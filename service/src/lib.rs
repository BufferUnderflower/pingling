//! Pingle service layer — VPN orchestration via typed pipelines.
//!
//! Every interaction flows through a [`Pipeline`]: a middleware chain wrapped
//! around a terminal [`Handler`]. The presence or absence of a pipeline for
//! a capability operation (like outbound listing) IS the capability declaration.
//!
//! # Architecture
//!
//! ```text
//! VpnManager
//! ├── connect_pipeline:       Pipeline<OpConnect>
//! ├── disconnect_pipeline:    Pipeline<OpDisconnect>
//! ├── restart_pipeline:       Pipeline<OpRestart>
//! ├── validate_pipeline:      Pipeline<OpValidateConfig>
//! ├── status_pipeline:        Pipeline<OpGetStatus>
//! ├── list_outbounds_pipeline: Option<Pipeline<OpListOutbounds>>  ← capability
//! ├── select_outbound_pipeline: Option<Pipeline<OpSelectOutbound>> ← capability
//! └── test_latency_pipeline:  Option<Pipeline<OpTestLatency>>     ← capability
//! ```
//!
//! # Consumers
//! - `app/src/main.rs` — Tauri daemon: wraps `VpnManager` in `Arc`
//! - `cli/src/main.rs` — headless binary: creates per-invocation
//!
//! # Modules
//! - [`handlers`] — Terminal handlers that call `VpnCore` methods
//! - [`middleware`] — Built-in middleware (logging, geo-filter, singbox config parser)

pub mod defaults;
pub mod handlers;
pub mod middleware;
// Keep plugins.rs around for backward compat but it's just re-exports now
pub mod plugins;
mod runtime_config;

use core_config_processor::{classify_error, ConfigRequest, ErrorKind, StrategyPlan};
use domain::ops::*;
use domain::pipeline::Pipeline;
use domain::{
    ConnectionState, CoreDescriptor, CoreEvent, CoreInfo, CoreSource, InstallIdProvider, Plugin,
    PrerequisiteCheck, Profile, ProfileMeta, ProfileStorage, SettingsStorage, TempConfigPath,
    VpnCore, VpnError,
};
use handlers::*;
use log::{info, warn};
use runtime_config::PreparedRuntimeConfig;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// CoreRegistry (unchanged)
// ---------------------------------------------------------------------------

/// Manages VPN core discovery, resolution, and switching.
pub struct CoreRegistry {
    cores: BTreeMap<String, Box<dyn VpnCore>>,
    descriptors: BTreeMap<String, CoreDescriptor>,
    active: Option<String>,
}

impl CoreRegistry {
    pub fn new() -> Self {
        Self {
            cores: BTreeMap::new(),
            descriptors: BTreeMap::new(),
            active: None,
        }
    }

    pub fn register(&mut self, descriptor: CoreDescriptor, core: Box<dyn VpnCore>) {
        let key = descriptor.core_type.clone();
        if self.active.is_none() {
            self.active = Some(key.clone());
        }
        self.descriptors.insert(key.clone(), descriptor);
        self.cores.insert(key, core);
    }

    pub fn discover(&mut self) {
        self.discover_system_cores();
        if self.active.is_none() {
            self.active = self.cores.keys().next().cloned();
        }
    }

    fn discover_system_cores(&mut self) {
        let known_cores: &[(&str, &str)] = &[("sing-box", "Sing-Box"), ("xray", "Xray")];
        for (core_type, display_name) in known_cores {
            if self.descriptors.contains_key(*core_type) {
                continue;
            }
            if let Some(path) = util::which(core_type) {
                let available = std::path::Path::new(&path).exists();
                let descriptor = CoreDescriptor {
                    core_type: core_type.to_string(),
                    display_name: display_name.to_string(),
                    source: CoreSource::System,
                    binary_path: Some(path),
                    available,
                };
                if available {
                    info!("discovered system core: {core_type}");
                }
                self.descriptors.insert(core_type.to_string(), descriptor);
            }
        }
    }

    pub fn list(&self) -> Vec<&CoreDescriptor> {
        self.descriptors.values().collect()
    }

    pub fn descriptor(&self, core_type: &str) -> Option<&CoreDescriptor> {
        self.descriptors.get(core_type)
    }

    pub fn active_type(&self) -> Option<&str> {
        self.active.as_deref()
    }

    pub fn active_core(&mut self) -> Option<&mut Box<dyn VpnCore>> {
        let key = self.active.as_ref()?;
        self.cores.get_mut(key.as_str())
    }

    pub fn get_core(&mut self, core_type: &str) -> Option<&mut Box<dyn VpnCore>> {
        self.cores.get_mut(core_type)
    }

    pub fn switch(&mut self, core_type: &str) -> Result<(), VpnError> {
        let desc = self
            .descriptors
            .get(core_type)
            .ok_or_else(|| VpnError::CoreNotFound(core_type.to_string()))?;
        if !desc.available {
            return Err(VpnError::PrerequisiteMissing(format!(
                "core '{}' is not available",
                core_type
            )));
        }
        if !self.cores.contains_key(core_type) {
            return Err(VpnError::CoreNotFound(format!(
                "{core_type} (no registered instance)"
            )));
        }
        self.active = Some(core_type.to_string());
        info!("switched active core to: {core_type}");
        Ok(())
    }

    pub fn set_binary_path(&mut self, core_type: &str, path: &str) -> Result<(), VpnError> {
        let desc = self
            .descriptors
            .get_mut(core_type)
            .ok_or_else(|| VpnError::CoreNotFound(core_type.to_string()))?;
        desc.binary_path = Some(path.to_string());
        desc.available = std::path::Path::new(path).exists();
        desc.source = CoreSource::Linked(path.to_string());
        info!("updated {core_type} binary path: {path}");
        Ok(())
    }
}

impl Default for CoreRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSessionInfo {
    pub core_type: String,
    pub source_kind: String,
    pub config_path: Option<String>,
    pub effective_config_path: Option<String>,
    pub active_profile_id: Option<String>,
    pub active_profile_name: Option<String>,
    pub active_profile_core_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedConfig {
    pub path: String,
    pub source_path: String,
    pub source_kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeMetricsSnapshot {
    pub available: bool,
    pub controller: Option<String>,
    pub clash_version: Option<String>,
    pub upload_bps: Option<u64>,
    pub download_bps: Option<u64>,
    pub upload_total: Option<u64>,
    pub download_total: Option<u64>,
    pub connections_count: Option<usize>,
    pub memory_bytes: Option<u64>,
}

struct EffectiveConfigPath {
    path: String,
    _temp_path: Option<TempConfigPath>,
}

struct ConnectSourceConfig {
    path: String,
    _temp_path: Option<TempConfigPath>,
}

// ---------------------------------------------------------------------------
// VpnManager
// ---------------------------------------------------------------------------

/// Orchestrates VPN operations through typed middleware pipelines.
///
/// Lifecycle pipelines (connect, disconnect, restart, validate, status) are
/// always present. Capability pipelines (list_outbounds, select_outbound,
/// test_latency) are `Option` — their presence IS the capability declaration.
///
/// All pipelines are behind `Mutex` for thread-safe middleware registration.
pub struct VpnManager {
    registry: Arc<Mutex<CoreRegistry>>,
    storage: Arc<Mutex<Box<dyn SettingsStorage>>>,

    // Lifecycle pipelines — always present
    connect: Mutex<Pipeline<OpConnect>>,
    disconnect: Mutex<Pipeline<OpDisconnect>>,
    restart: Mutex<Pipeline<OpRestart>>,
    validate: Mutex<Pipeline<OpValidateConfig>>,
    /// Status pipeline — reserved for middleware that enriches status queries.
    /// Currently bypassed by `get_status()` which reads the registry directly
    /// for zero-overhead polling from the tray refresh loop.
    #[allow(dead_code)]
    status: Mutex<Pipeline<OpGetStatus>>,

    // Capability pipelines — present only if the active core supports them
    list_outbounds: Mutex<Option<Pipeline<OpListOutbounds>>>,
    select_outbound: Mutex<Option<Pipeline<OpSelectOutbound>>>,
    test_latency: Mutex<Option<Pipeline<OpTestLatency>>>,

    /// Optional generic [`Plugin`] slot — populated at daemon startup
    /// from a wasm `.wasm` file via `plugin-extism::PluginAdapter`. The
    /// plugin handles whatever JSON-RPC method names it claims via
    /// `Plugin::handle_ipc`; the daemon's IPC layer falls through to
    /// it after exhausting its built-in `vpn.*`/`core.*`/`config.*`
    /// dispatch table. When the slot is empty, unknown methods return
    /// `MethodNotFound` cleanly and the rest of the daemon works
    /// normally. See `domain::traits::plugin` for the trait surface
    /// and `docs/architecture-plugin.md` for the rationale.
    plugin: Mutex<Option<Arc<dyn Plugin>>>,

    /// Encrypted profile storage. When present, the connect handler
    /// prefers the active profile over the legacy `config_path`
    /// setting. When absent, the daemon falls back to legacy behavior.
    /// This is set via [`VpnManager::with_profile_storage`] at the
    /// composition root; the default [`VpnManager::new`] leaves it empty
    /// so existing tests and the headless CLI keep working unchanged.
    profile_storage: Option<Arc<dyn ProfileStorage>>,

    /// Optional install-ID provider. Usually the same object that
    /// implements [`ProfileStorage`] (the encrypted profile store reads
    /// both entries from the same keychain), but kept as a separate
    /// trait object so tests can mock them independently.
    install_id_provider: Option<Arc<dyn InstallIdProvider>>,

    /// Shared temp-config slot for the runtime config handed to the
    /// active core. `connect()` / `restart()` materialize a processed
    /// temp config before middleware runs, then stash it here on
    /// success so the disconnect handler can drop it on stop.
    active_temp_config: Arc<Mutex<Option<TempConfigPath>>>,

    /// Latest live Clash API metrics observed by the IPC-side runtime monitor.
    /// Empty/default when the active core is not running or the controller
    /// is unavailable.
    runtime_metrics: Arc<Mutex<RuntimeMetricsSnapshot>>,

    /// Optional slot-chain observer. When `Some`, every slot dispatch
    /// (`slot.vpn.connect.*`, `slot.vpn.disconnect.*`, etc.) flows
    /// through the observer so it can log and broadcast IPC events.
    /// When `None`, slot dispatches use [`domain::NullSlotObserver`]
    /// internally — still functional, just silent.
    ///
    /// Behind a `Mutex` (not `RwLock`) because `Arc<dyn SlotObserver>`
    /// cloning is already cheap, and the set happens once at startup
    /// from the composition root. Read path is `.lock() → clone Arc`
    /// which is O(1).
    slot_observer: Mutex<Option<Arc<dyn domain::SlotObserver>>>,
}

impl VpnManager {
    /// Create a manager with lifecycle pipelines wired to the registry.
    ///
    /// Pipelines start with just the core handlers — no middleware.
    /// The composition root (app/main.rs) decides what to push:
    ///
    /// ```rust,ignore
    /// let mgr = VpnManager::new(registry, storage);
    /// defaults::register(&mgr);  // logging + validation + ...
    /// // or wire selectively:
    /// mgr.connect_pipeline().push(Box::new(MyCustomMiddleware));
    /// ```
    pub fn new(registry: CoreRegistry, storage: Box<dyn SettingsStorage>) -> Self {
        let registry = Arc::new(Mutex::new(registry));
        let storage = Arc::new(Mutex::new(storage));
        let active_temp_config: Arc<Mutex<Option<TempConfigPath>>> = Arc::new(Mutex::new(None));
        let runtime_metrics: Arc<Mutex<RuntimeMetricsSnapshot>> =
            Arc::new(Mutex::new(RuntimeMetricsSnapshot::default()));

        Self {
            connect: Mutex::new(Pipeline::new(Box::new(ConnectHandler::new(
                registry.clone(),
                None,
                active_temp_config.clone(),
            )))),
            disconnect: Mutex::new(Pipeline::new(Box::new(DisconnectHandler::new(
                registry.clone(),
                active_temp_config.clone(),
            )))),
            restart: Mutex::new(Pipeline::new(Box::new(RestartHandler {
                registry: registry.clone(),
            }))),
            validate: Mutex::new(Pipeline::new(Box::new(ValidateConfigHandler {
                registry: registry.clone(),
            }))),
            status: Mutex::new(Pipeline::new(Box::new(GetStatusHandler {
                registry: registry.clone(),
            }))),
            list_outbounds: Mutex::new(None),
            select_outbound: Mutex::new(None),
            test_latency: Mutex::new(None),
            plugin: Mutex::new(None),
            profile_storage: None,
            install_id_provider: None,
            active_temp_config: active_temp_config.clone(),
            runtime_metrics,
            slot_observer: Mutex::new(None),
            registry,
            storage,
        }
    }

    /// Attach a [`domain::SlotObserver`] so slot-chain dispatches in
    /// this manager emit observations (log lines, IPC broadcasts).
    /// Pass `ipc_server::BroadcastingSlotObserver` at the composition
    /// root for the standard behavior. If never called, slot chains
    /// still execute — they just don't emit observations.
    ///
    /// Takes `&self` (not `self`) because the composition root
    /// typically wraps the manager in `Arc` before the broadcaster
    /// is available, and can't easily consume `self` afterwards.
    pub fn set_slot_observer(&self, observer: Arc<dyn domain::SlotObserver>) {
        let mut guard = self.slot_observer.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(observer);
    }

    /// Dispatch a slot chain through this manager's loaded plugin
    /// (if any) and observer. Returns `Ok(Some(payload))` when a
    /// phase claimed the slot, `Ok(None)` when every phase returned
    /// Unhandled / None, and `Err(VpnError)` on plugin or serde
    /// error. Callers use `None` to mean "default daemon behavior
    /// applies, no plugin intervention".
    ///
    /// Safe to call even when no plugin is loaded — in that case the
    /// function short-circuits and returns `Ok(None)` without any
    /// overhead.
    ///
    /// `pub` because the IPC layer needs to invoke the slot chain
    /// for `ipc.dispatch` around every JSON-RPC method call —
    /// that happens in a sibling crate (`ipc-server`).
    pub fn run_slot<P>(
        &self,
        slot: &str,
        wire_version: u32,
        invocation_id: &str,
        payload: P,
    ) -> Result<Option<P>, VpnError>
    where
        P: serde::Serialize + for<'de> serde::Deserialize<'de> + Clone,
    {
        let guard = self.plugin.lock().unwrap_or_else(|e| e.into_inner());
        let plugin_ref = match guard.as_ref() {
            Some(p) => p.clone(),
            None => return Ok(None),
        };
        drop(guard);

        let observer_guard = self.slot_observer.lock().unwrap_or_else(|e| e.into_inner());
        let observer = observer_guard.as_ref().cloned();
        drop(observer_guard);

        match observer {
            Some(obs) => domain::run_slot_chain_observed(
                plugin_ref.as_ref(),
                slot,
                wire_version,
                invocation_id,
                payload,
                obs.as_ref(),
            ),
            None => domain::run_slot_chain(
                plugin_ref.as_ref(),
                slot,
                wire_version,
                invocation_id,
                payload,
            ),
        }
    }

    /// Attach a [`ProfileStorage`] (and matching [`InstallIdProvider`])
    /// to this manager. Call this at the composition root before any
    /// `vpn.connect` is issued — the connect handler clones the `Arc`
    /// at construction time.
    ///
    /// The two trait objects are usually the same underlying store
    /// (`data::EncryptedProfileStore` implements both), but the API
    /// accepts separate `Arc`s so tests can mock them independently.
    pub fn with_profile_storage(
        mut self,
        profile_storage: Arc<dyn ProfileStorage>,
        install_id_provider: Arc<dyn InstallIdProvider>,
    ) -> Self {
        // Rebuild connect + disconnect handlers with the new storage.
        let active_temp_config: Arc<Mutex<Option<TempConfigPath>>> = Arc::new(Mutex::new(None));
        self.connect = Mutex::new(Pipeline::new(Box::new(ConnectHandler::new(
            self.registry.clone(),
            Some(profile_storage.clone()),
            active_temp_config.clone(),
        ))));
        self.disconnect = Mutex::new(Pipeline::new(Box::new(DisconnectHandler::new(
            self.registry.clone(),
            active_temp_config.clone(),
        ))));
        self.profile_storage = Some(profile_storage);
        self.install_id_provider = Some(install_id_provider);
        self.active_temp_config = active_temp_config;
        self
    }

    pub fn runtime_metrics(&self) -> RuntimeMetricsSnapshot {
        self.runtime_metrics
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_runtime_metrics(&self, snapshot: RuntimeMetricsSnapshot) {
        let mut guard = self
            .runtime_metrics
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = snapshot;
    }

    pub fn clear_runtime_metrics(&self) {
        self.set_runtime_metrics(RuntimeMetricsSnapshot::default());
    }

    // -- Pipeline access (for middleware registration) -----------------------

    /// Access the connect pipeline to push middleware.
    pub fn connect_pipeline(&self) -> std::sync::MutexGuard<'_, Pipeline<OpConnect>> {
        self.connect.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Access the disconnect pipeline to push middleware.
    pub fn disconnect_pipeline(&self) -> std::sync::MutexGuard<'_, Pipeline<OpDisconnect>> {
        self.disconnect.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Access the restart pipeline to push middleware.
    pub fn restart_pipeline(&self) -> std::sync::MutexGuard<'_, Pipeline<OpRestart>> {
        self.restart.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Access the validate pipeline to push middleware.
    pub fn validate_pipeline(&self) -> std::sync::MutexGuard<'_, Pipeline<OpValidateConfig>> {
        self.validate.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Set the list-outbounds capability pipeline.
    ///
    /// Call this with a pipeline whose terminal handler knows how to
    /// extract outbounds from the active core's config (e.g.
    /// [`SingboxConfigHandler`](crate::middleware::singbox_config::SingboxConfigHandler)).
    pub fn set_list_outbounds(&self, pipeline: Pipeline<OpListOutbounds>) {
        *self
            .list_outbounds
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(pipeline);
    }

    /// Set the select-outbound capability pipeline.
    pub fn set_select_outbound(&self, pipeline: Pipeline<OpSelectOutbound>) {
        *self
            .select_outbound
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(pipeline);
    }

    /// Set the test-latency capability pipeline.
    pub fn set_test_latency(&self, pipeline: Pipeline<OpTestLatency>) {
        *self.test_latency.lock().unwrap_or_else(|e| e.into_inner()) = Some(pipeline);
    }

    /// Install a [`Plugin`]. Typically called once at daemon startup
    /// from `app/src/main.rs` after `discover_plugin` finds a `.wasm`
    /// file in the plugins dir. Calling it again replaces the
    /// previous plugin; the old `Arc` is dropped along with whatever
    /// internal state it held (sessions, caches, …).
    pub fn set_plugin(&self, plugin: Arc<dyn Plugin>) {
        *self.plugin.lock().unwrap_or_else(|e| e.into_inner()) = Some(plugin);
    }

    /// Borrow the installed [`Plugin`], cloning the `Arc` so the
    /// caller can hold the reference past the mutex guard. Returns
    /// `None` if no plugin is installed — the daemon's IPC layer
    /// then routes unknown methods to `MethodNotFound`.
    pub fn plugin(&self) -> Option<Arc<dyn Plugin>> {
        self.plugin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    // -- Profile storage accessors -------------------------------------------

    /// Borrow the attached [`ProfileStorage`], if any.
    ///
    /// The IPC dispatcher uses this to implement `profile.list`,
    /// `profile.put`, `profile.activate`, etc. When no storage is
    /// attached (composition root skipped `with_profile_storage`),
    /// callers should return `StorageError("profile storage not
    /// configured")` so clients get a clear message.
    pub fn profile_storage(&self) -> Option<Arc<dyn ProfileStorage>> {
        self.profile_storage.clone()
    }

    /// Borrow the attached [`InstallIdProvider`], if any. Used by
    /// the `daemon.installId` IPC method.
    pub fn install_id_provider(&self) -> Option<Arc<dyn InstallIdProvider>> {
        self.install_id_provider.clone()
    }

    // -- Profile CRUD convenience wrappers -----------------------------------
    //
    // These wrap the trait methods with clear error handling for the
    // "no storage configured" case. The IPC dispatcher calls these
    // instead of reaching into `profile_storage()` directly so there's
    // exactly one place that emits the "not configured" error.

    /// List all stored profiles. Returns an empty vec if storage is
    /// not configured — clients interpret "no storage" as "no
    /// profiles yet", which matches the legacy behavior.
    pub fn list_profiles(&self) -> Result<Vec<ProfileMeta>, VpnError> {
        match self.profile_storage() {
            Some(s) => s.list(),
            None => Ok(Vec::new()),
        }
    }

    /// Get one profile's metadata.
    pub fn get_profile(&self, id: &str) -> Result<Option<ProfileMeta>, VpnError> {
        match self.profile_storage() {
            Some(s) => s.get_meta(id),
            None => Ok(None),
        }
    }

    /// Insert or update a profile. Errors with `StorageError` when
    /// no storage is configured.
    pub fn put_profile(&self, profile: Profile, config_json: &str) -> Result<Profile, VpnError> {
        match self.profile_storage() {
            Some(s) => s.put(profile, config_json),
            None => Err(VpnError::StorageError(
                "profile storage not configured".to_string(),
            )),
        }
    }

    /// Delete a profile by id. No-op when storage is not configured.
    pub fn delete_profile(&self, id: &str) -> Result<(), VpnError> {
        match self.profile_storage() {
            Some(s) => s.delete(id),
            None => Ok(()),
        }
    }

    /// Get the active profile id, if any.
    pub fn active_profile(&self) -> Result<Option<String>, VpnError> {
        match self.profile_storage() {
            Some(s) => s.active(),
            None => Ok(None),
        }
    }

    /// Set the active profile. Errors with `StorageError` when no
    /// storage is configured.
    pub fn set_active_profile(&self, id: &str) -> Result<(), VpnError> {
        match self.profile_storage() {
            Some(s) => s.set_active(id),
            None => Err(VpnError::StorageError(
                "profile storage not configured".to_string(),
            )),
        }
    }

    /// Clear the active profile pointer.
    pub fn clear_active_profile(&self) -> Result<(), VpnError> {
        match self.profile_storage() {
            Some(s) => s.clear_active(),
            None => Ok(()),
        }
    }

    /// Return the daemon's install ID, generating one on first call.
    /// Errors if no [`InstallIdProvider`] is configured.
    pub fn install_id(&self) -> Result<String, VpnError> {
        match self.install_id_provider() {
            Some(p) => p.install_id(),
            None => Err(VpnError::StorageError(
                "install id provider not configured".to_string(),
            )),
        }
    }

    /// Which capability pipelines are registered.
    pub fn capabilities(&self) -> Vec<&'static str> {
        let mut caps = Vec::new();
        if self
            .list_outbounds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            caps.push("list_outbounds");
        }
        if self
            .select_outbound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            caps.push("select_outbound");
        }
        if self
            .test_latency
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            caps.push("test_latency");
        }
        caps
    }

    /// Shared reference to the core registry.
    pub fn registry(&self) -> Arc<Mutex<CoreRegistry>> {
        self.registry.clone()
    }

    /// Shared reference to settings storage.
    pub fn storage(&self) -> Arc<Mutex<Box<dyn SettingsStorage>>> {
        self.storage.clone()
    }

    // -- internal helpers ---------------------------------------------------

    fn active_core_type_str(&self) -> String {
        let registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.active_type().unwrap_or("none").to_string()
    }

    fn active_core_target(&self) -> core_config_processor_impls::CoreCompatTarget {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let core_type = registry.active_type().unwrap_or("unknown").to_string();
        let version = registry
            .active_core()
            .map(|core| core.info().version)
            .unwrap_or_default();
        core_config_processor_impls::CoreCompatTarget::new(core_type, version, std::env::consts::OS)
    }

    fn get_config_path(&self) -> Result<String, VpnError> {
        self.storage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_string("config_path")?
            .ok_or_else(|| {
                VpnError::InvalidConfiguration("config_path not found in settings".into())
            })
    }

    fn materialize_runtime_config(
        &self,
        source_path: &str,
    ) -> Result<PreparedRuntimeConfig, VpnError> {
        let request = runtime_config::default_request();
        self.materialize_runtime_config_for_request(source_path, &request)
    }

    fn materialize_runtime_config_for_request(
        &self,
        source_path: &str,
        request: &ConfigRequest,
    ) -> Result<PreparedRuntimeConfig, VpnError> {
        runtime_config::materialize_runtime_config(
            source_path,
            &util::paths::ruleset_cache_dir(),
            &util::paths::active_config_temp_dir(),
            self.active_core_target(),
            request,
        )
    }

    fn resolve_connect_source_config(&self) -> Result<ConnectSourceConfig, VpnError> {
        if let Some(storage) = self.profile_storage() {
            match storage.load_active_for_core_start() {
                Ok(temp) => {
                    let path = temp.path().to_string_lossy().into_owned();
                    return Ok(ConnectSourceConfig {
                        path,
                        _temp_path: Some(temp),
                    });
                }
                Err(VpnError::NotConnected) => {}
                Err(error) => return Err(error),
            }
        }

        Ok(ConnectSourceConfig {
            path: self.get_config_path()?,
            _temp_path: None,
        })
    }

    fn prepare_connect_runtime_config(&self) -> Result<PreparedRuntimeConfig, VpnError> {
        let source = self.resolve_connect_source_config()?;
        self.materialize_runtime_config(&source.path)
    }

    fn current_runtime_config_path(&self) -> Option<String> {
        self.active_temp_config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|temp| temp.path().to_string_lossy().into_owned())
    }

    fn resolve_effective_runtime_config(&self) -> Result<EffectiveConfigPath, VpnError> {
        if let Some(path) = self.current_runtime_config_path() {
            return Ok(EffectiveConfigPath {
                path,
                _temp_path: None,
            });
        }

        let prepared = self.prepare_connect_runtime_config()?;
        Ok(EffectiveConfigPath {
            path: prepared.path,
            _temp_path: Some(prepared.temp_path),
        })
    }

    fn active_profile_summary(&self) -> Result<(Option<String>, Option<ProfileMeta>), VpnError> {
        let active_profile_id = self.active_profile()?;
        let active_profile = match active_profile_id.as_deref() {
            Some(id) => self.get_profile(id)?,
            None => None,
        };
        Ok((active_profile_id, active_profile))
    }

    fn source_kind(active_profile_id: &Option<String>, config_path: &Option<String>) -> String {
        if active_profile_id.is_some() {
            "active_profile".into()
        } else if config_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
        {
            "config_path".into()
        } else {
            "none".into()
        }
    }

    fn inspect_label(info: &ConfigSessionInfo) -> String {
        let candidate = if info.source_kind == "active_profile" {
            info.active_profile_name
                .as_deref()
                .or(info.active_profile_id.as_deref())
                .unwrap_or("active-profile")
        } else if info.source_kind == "config_path" {
            info.config_path
                .as_deref()
                .and_then(|path| std::path::Path::new(path).file_stem())
                .and_then(|stem| stem.to_str())
                .unwrap_or("config-path")
        } else {
            "effective-config"
        };
        sanitize_label(candidate)
    }

    pub fn current_clash_controller(&self) -> Result<Option<String>, VpnError> {
        let Some(path) = self.current_runtime_config_path() else {
            return Ok(None);
        };
        let json = std::fs::read_to_string(&path)
            .map_err(|e| VpnError::StorageError(format!("read runtime config {path}: {e}")))?;
        let root: serde_json::Value = serde_json::from_str(&json).map_err(|e| {
            VpnError::InvalidConfiguration(format!("parse runtime config {path}: {e}"))
        })?;
        Ok(root
            .get("experimental")
            .and_then(|value| value.get("clash_api"))
            .and_then(|value| value.get("external_controller"))
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned))
    }

    fn resolve_connect_strategy_plan(&self) -> StrategyPlan {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let Some(core) = registry.active_core() else {
            return StrategyPlan {
                strategies: vec![runtime_config::default_strategy()],
                global_timeout: None,
            };
        };

        let Some(bytes) = core.default_strategy_plan() else {
            return StrategyPlan {
                strategies: vec![runtime_config::default_strategy()],
                global_timeout: None,
            };
        };

        match serde_json::from_slice::<StrategyPlan>(&bytes) {
            Ok(plan) if !plan.strategies.is_empty() => plan,
            Ok(_) => StrategyPlan {
                strategies: vec![runtime_config::default_strategy()],
                global_timeout: None,
            },
            Err(error) => {
                warn!("connect: invalid strategy plan from core: {error}");
                StrategyPlan {
                    strategies: vec![runtime_config::default_strategy()],
                    global_timeout: None,
                }
            }
        }
    }

    fn run_connect_attempt(
        &self,
        source_path: &str,
        core_type: &str,
        request: &ConfigRequest,
    ) -> Result<TempConfigPath, VpnError> {
        let prepared = self.materialize_runtime_config_for_request(source_path, request)?;
        let input = ConnectInput {
            config_path: prepared.path.clone(),
            core_type: core_type.to_string(),
            state: self.get_status(),
            metadata: BTreeMap::new(),
        };
        self.connect
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute(input)?;
        Ok(prepared.temp_path)
    }

    fn strategy_plan_timeout_error(limit: Duration) -> VpnError {
        VpnError::Unknown(format!(
            "connect strategy plan timed out after {} ms",
            limit.as_millis()
        ))
    }

    fn execute_connect_strategy_plan(
        &self,
        source_path: &str,
        core_type: &str,
        plan: &StrategyPlan,
    ) -> Result<TempConfigPath, VpnError> {
        let started_at = Instant::now();
        let mut last_error: Option<VpnError> = None;

        for strategy in &plan.strategies {
            let max_attempts = strategy.retry.max_attempts();
            for attempt_number in 1..=max_attempts {
                if let Some(limit) = plan.global_timeout {
                    if started_at.elapsed() >= limit {
                        return Err(Self::strategy_plan_timeout_error(limit));
                    }
                }

                let previous_error = last_error.as_ref().map(classify_error);
                let request =
                    runtime_config::request_for_strategy(strategy, attempt_number, previous_error);

                info!(
                    "connect: strategy={} attempt={}/{} stack={:?} resolver={:?}",
                    strategy.id,
                    attempt_number,
                    max_attempts,
                    strategy.stack,
                    strategy.resolver_type
                );

                match self.run_connect_attempt(source_path, core_type, &request) {
                    Ok(temp_path) => return Ok(temp_path),
                    Err(error) => {
                        let classified = classify_error(&error);
                        warn!(
                            "connect: strategy={} attempt={}/{} failed: {} ({:?})",
                            strategy.id, attempt_number, max_attempts, error, classified.kind
                        );

                        if matches!(
                            error,
                            VpnError::AlreadyConnected
                                | VpnError::Cancelled
                                | VpnError::StorageError(_)
                        ) {
                            return Err(error);
                        }

                        let retry_within_strategy = matches!(
                            classified.kind,
                            ErrorKind::DnsFailure
                                | ErrorKind::TcpTimeout
                                | ErrorKind::TcpRefused
                                | ErrorKind::TlsHandshake
                                | ErrorKind::HttpError
                                | ErrorKind::Timeout
                                | ErrorKind::Unknown
                        );
                        let bail_immediately = matches!(
                            classified.kind,
                            ErrorKind::AuthFailure
                                | ErrorKind::TunDevice
                                | ErrorKind::PermissionDenied
                                | ErrorKind::PrerequisiteMissing
                        );

                        if bail_immediately {
                            return Err(error);
                        }

                        last_error = Some(error);

                        if retry_within_strategy && attempt_number < max_attempts {
                            let delay = strategy.retry.delay_for(attempt_number + 1);
                            if !delay.is_zero() {
                                if let Some(limit) = plan.global_timeout {
                                    if started_at.elapsed().saturating_add(delay) >= limit {
                                        return Err(Self::strategy_plan_timeout_error(limit));
                                    }
                                }
                                std::thread::sleep(delay);
                            }
                            continue;
                        }

                        break;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            VpnError::Unknown("connect strategy plan completed without a successful attempt".into())
        }))
    }

    pub fn get_config_info(&self) -> Result<ConfigSessionInfo, VpnError> {
        let config_path = self.get_setting("config_path")?;
        let (active_profile_id, active_profile) = self.active_profile_summary()?;
        Ok(ConfigSessionInfo {
            core_type: self.active_core_type_str(),
            source_kind: Self::source_kind(&active_profile_id, &config_path),
            config_path,
            effective_config_path: self.current_runtime_config_path(),
            active_profile_id,
            active_profile_name: active_profile.as_ref().map(|profile| profile.name.clone()),
            active_profile_core_type: active_profile
                .as_ref()
                .map(|profile| profile.core_type.clone()),
        })
    }

    pub fn validate_current_config(&self) -> Result<String, VpnError> {
        let effective = self.resolve_effective_runtime_config()?;
        let input = ValidateConfigInput {
            config_path: effective.path.clone(),
            core_type: self.active_core_type_str(),
            config_content: None,
            metadata: BTreeMap::new(),
        };
        self.validate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute(input)?;
        Ok(effective.path)
    }

    pub fn export_current_config(&self) -> Result<ExportedConfig, VpnError> {
        let effective = self.resolve_effective_runtime_config()?;
        let info = self.get_config_info()?;
        let inspect_dir = util::paths::config_inspect_dir();
        std::fs::create_dir_all(&inspect_dir).map_err(|e| {
            VpnError::StorageError(format!("create inspect dir {}: {e}", inspect_dir.display()))
        })?;

        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_secs();
        let export_path =
            inspect_dir.join(format!("{}-{}.json", Self::inspect_label(&info), suffix));
        let bytes = std::fs::read(&effective.path).map_err(|e| {
            VpnError::StorageError(format!("read effective config {}: {e}", effective.path))
        })?;
        std::fs::write(&export_path, bytes).map_err(|e| {
            VpnError::StorageError(format!(
                "write inspect config {}: {e}",
                export_path.display()
            ))
        })?;

        Ok(ExportedConfig {
            path: export_path.to_string_lossy().into_owned(),
            source_path: effective.path,
            source_kind: info.source_kind,
        })
    }

    // -- lifecycle operations -----------------------------------------------

    pub fn connect(&self) -> Result<(), VpnError> {
        let source = self.resolve_connect_source_config()?;
        let core_type = self.active_core_type_str();
        let strategy_plan = self.resolve_connect_strategy_plan();

        // slot.vpn.connect.* — middleware chain.
        //
        // Fires around the pipeline execution: the `before` / `exec`
        // phases see a payload with `result: None`; the `after`
        // phase sees one with `result: Some(ConnectResult { ... })`.
        // A plugin that returns `Halt` from `before` short-circuits
        // the whole connect with the halt payload (used for quota
        // enforcement, subscription gating). If no plugin claims the
        // slot, behavior is exactly the same as before this slot
        // existed — the pipeline executes unchanged.
        let invocation_id = domain::new_invocation_id();
        let mut slot_payload = domain::VpnConnectPayload {
            core_type: core_type.clone(),
            config_path: Some(source.path.clone()),
            hint: None,
            result: None,
        };
        if let Some(halted) = self.run_slot(
            domain::slot_names::VPN_CONNECT,
            domain::VPN_CONNECT_WIRE_VERSION,
            &invocation_id,
            slot_payload.clone(),
        )? {
            slot_payload = halted;
            // If the plugin pre-filled a result (e.g. explicit
            // halt with a refusal), respect its decision and skip
            // the real pipeline call.
            if let Some(result) = slot_payload.result.as_ref() {
                if !result.started {
                    if let Some(msg) = result.error.clone() {
                        return Err(VpnError::Unknown(msg));
                    }
                }
            }
        }

        let start_ts = std::time::Instant::now();
        let pipeline_result =
            self.execute_connect_strategy_plan(&source.path, &core_type, &strategy_plan);
        let duration_ms = start_ts.elapsed().as_millis() as u64;

        // Stamp the result into the slot payload and fire the after
        // chain so observers see the outcome.
        slot_payload.result = Some(domain::ConnectResult {
            started: pipeline_result.is_ok(),
            duration_ms,
            error: pipeline_result.as_ref().err().map(|e| e.to_string()),
        });
        // After-fire is best-effort: any plugin error here becomes a
        // debug log, not a failure cause for the connect itself
        // (the real connect already succeeded/failed). We intentionally
        // don't propagate slot chain errors back up from the after
        // phase.
        let _ = self.run_slot(
            domain::slot_names::VPN_CONNECT,
            domain::VPN_CONNECT_WIRE_VERSION,
            &invocation_id,
            slot_payload,
        );

        let active_temp_config = pipeline_result?;
        *self
            .active_temp_config
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(active_temp_config);
        Ok(())
    }

    pub fn disconnect(&self) -> Result<(), VpnError> {
        let core_type = self.active_core_type_str();
        let input = DisconnectInput {
            core_type: core_type.clone(),
            state: self.get_status(),
            metadata: BTreeMap::new(),
        };

        // slot.vpn.disconnect.* — same middleware shape as connect.
        // The `after` phase is the natural place for session metrics
        // flushing and token rotation.
        let invocation_id = domain::new_invocation_id();
        let mut slot_payload = domain::VpnDisconnectPayload {
            core_type: core_type.clone(),
            reason: None,
            result: None,
        };
        if let Some(updated) = self.run_slot(
            domain::slot_names::VPN_DISCONNECT,
            domain::VPN_DISCONNECT_WIRE_VERSION,
            &invocation_id,
            slot_payload.clone(),
        )? {
            slot_payload = updated;
        }

        let start_ts = std::time::Instant::now();
        let pipeline_result = self
            .disconnect
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute(input);
        let duration_ms = start_ts.elapsed().as_millis() as u64;

        slot_payload.result = Some(domain::DisconnectResult {
            stopped: pipeline_result.is_ok(),
            duration_ms,
            error: pipeline_result.as_ref().err().map(|e| e.to_string()),
        });
        let _ = self.run_slot(
            domain::slot_names::VPN_DISCONNECT,
            domain::VPN_DISCONNECT_WIRE_VERSION,
            &invocation_id,
            slot_payload,
        );

        pipeline_result?;
        Ok(())
    }

    pub fn force_kill(&self) -> Result<(), VpnError> {
        info!("Force-killing");
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let core = registry.active_core().ok_or(VpnError::NotConnected)?;
        core.kill()
    }

    pub fn restart(&self) -> Result<(), VpnError> {
        let prepared = self.prepare_connect_runtime_config()?;
        let input = RestartInput {
            config_path: prepared.path.clone(),
            core_type: self.active_core_type_str(),
            state: self.get_status(),
            metadata: BTreeMap::new(),
        };
        self.restart
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute(input)?;
        *self
            .active_temp_config
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(prepared.temp_path);
        Ok(())
    }

    pub fn validate_config(&self, config_path: &str) -> Result<(), VpnError> {
        let prepared = self.materialize_runtime_config(config_path)?;
        let input = ValidateConfigInput {
            config_path: prepared.path,
            core_type: self.active_core_type_str(),
            config_content: None,
            metadata: BTreeMap::new(),
        };
        self.validate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute(input)?;
        Ok(())
    }

    // -- status -------------------------------------------------------------

    pub fn get_status(&self) -> ConnectionState {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        match registry.active_core() {
            Some(core) => core.status(),
            None => ConnectionState::Disconnected,
        }
    }

    pub fn is_running(&self) -> bool {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        match registry.active_core() {
            Some(core) => core.running(),
            None => false,
        }
    }

    pub fn get_core_info(&self) -> CoreInfo {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        match registry.active_core() {
            Some(core) => core.info(),
            None => CoreInfo {
                name: "none".into(),
                version: "N/A".into(),
                supported_protocols: vec![],
            },
        }
    }

    pub fn check_prerequisites(&self) -> Vec<PrerequisiteCheck> {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        match registry.active_core() {
            Some(core) => core.check_prerequisites(),
            None => vec![],
        }
    }

    pub fn subscribe_active_core_events(&self) -> Option<std::sync::mpsc::Receiver<CoreEvent>> {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.active_core().and_then(|core| core.subscribe())
    }

    // -- capability operations (pipeline-gated) -----------------------------

    /// List outbounds if the capability is registered. Returns empty if not.
    pub fn list_outbounds(&self) -> Result<Vec<domain::Outbound>, VpnError> {
        let guard = self
            .list_outbounds
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(pipeline) => {
                let effective = self.resolve_effective_runtime_config().ok();
                let input = ListOutboundsInput {
                    core_type: self.active_core_type_str(),
                    config_path: effective.as_ref().map(|resolved| resolved.path.clone()),
                    metadata: BTreeMap::new(),
                };
                let output = pipeline.execute(input)?;
                Ok(output.outbounds)
            }
            None => Ok(vec![]),
        }
    }

    /// Select an outbound. Returns error if capability not registered.
    pub fn select_outbound(&self, outbound_id: &str) -> Result<(), VpnError> {
        let guard = self
            .select_outbound
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(pipeline) => {
                let effective = self.resolve_effective_runtime_config().ok();
                let input = SelectOutboundInput {
                    outbound_id: outbound_id.to_string(),
                    core_type: self.active_core_type_str(),
                    config_path: effective.as_ref().map(|resolved| resolved.path.clone()),
                    metadata: BTreeMap::new(),
                };
                pipeline.execute(input)?;
                Ok(())
            }
            None => Err(VpnError::Unknown(
                "outbound selection not supported by this core".into(),
            )),
        }
    }

    /// Test latency. Returns error if capability not registered.
    pub fn test_latency(&self, outbound_ids: &[String]) -> Result<BTreeMap<String, u32>, VpnError> {
        let guard = self.test_latency.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(pipeline) => {
                let input = TestLatencyInput {
                    outbound_ids: outbound_ids.to_vec(),
                    core_type: self.active_core_type_str(),
                    metadata: BTreeMap::new(),
                };
                let output = pipeline.execute(input)?;
                Ok(output.results)
            }
            None => Err(VpnError::Unknown(
                "latency testing not supported by this core".into(),
            )),
        }
    }

    // -- core registry delegation -------------------------------------------

    pub fn list_cores(&self) -> Vec<CoreDescriptor> {
        let registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.list().into_iter().cloned().collect()
    }

    pub fn active_core_type(&self) -> Option<String> {
        let registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.active_type().map(|s| s.to_string())
    }

    pub fn switch_core(&self, core_type: &str) -> Result<(), VpnError> {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.switch(core_type)
    }

    pub fn register_core(&self, descriptor: CoreDescriptor, core: Box<dyn VpnCore>) {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.register(descriptor, core);
    }

    pub fn set_core_binary_path(&self, core_type: &str, path: &str) -> Result<(), VpnError> {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.set_binary_path(core_type, path)
    }

    pub fn discover_cores(&self) {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.discover();
    }

    // -- settings delegation ------------------------------------------------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, VpnError> {
        self.storage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_string(key)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), VpnError> {
        self.storage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_string(key, value)
    }

    pub fn remove_setting(&self, key: &str) -> Result<(), VpnError> {
        self.storage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key)
    }

    pub fn list_setting_keys(&self) -> Result<Vec<String>, VpnError> {
        self.storage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
    }
}

fn sanitize_label(input: &str) -> String {
    let mut rendered = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if mapped == '-' {
            if last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        rendered.push(mapped);
        if rendered.len() >= 48 {
            break;
        }
    }
    let trimmed = rendered.trim_matches('-');
    if trimmed.is_empty() {
        "config".into()
    } else {
        trimmed.into()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use core_config_processor::ConnectionStrategy;
    use core_config_processor_impls::RulesetCache;
    use data::MemorySettingsStorage;
    use domain::pipeline::{FnHook, FnWrapHook};
    use serial_test::serial;
    use std::io::{Read, Write};
    use tempfile::TempDir;

    // -- MockVpnCore --------------------------------------------------------

    struct MockVpnCore {
        state: ConnectionState,
        start_should_fail: bool,
        stop_should_fail: bool,
        validate_should_fail: bool,
    }

    impl MockVpnCore {
        fn new() -> Self {
            Self {
                state: ConnectionState::Disconnected,
                start_should_fail: false,
                stop_should_fail: false,
                validate_should_fail: false,
            }
        }
        fn connected() -> Self {
            let mut m = Self::new();
            m.state = ConnectionState::Connected;
            m
        }
    }

    impl VpnCore for MockVpnCore {
        fn start(&mut self, config_path: &str) -> Result<(), VpnError> {
            if config_path.is_empty() {
                return Err(VpnError::InvalidConfiguration("empty".into()));
            }
            if self.start_should_fail {
                return Err(VpnError::ProcessStartFailed("mock".into()));
            }
            self.state = ConnectionState::Connected;
            Ok(())
        }
        fn stop(&mut self) -> Result<(), VpnError> {
            if self.stop_should_fail {
                return Err(VpnError::ProcessStopFailed("mock".into()));
            }
            self.state = ConnectionState::Disconnected;
            Ok(())
        }
        fn kill(&mut self) -> Result<(), VpnError> {
            self.state = ConnectionState::Disconnected;
            Ok(())
        }
        fn status(&self) -> ConnectionState {
            self.state.clone()
        }
        fn info(&self) -> CoreInfo {
            CoreInfo {
                name: "mock".into(),
                version: "0.0.0".into(),
                supported_protocols: vec![],
            }
        }
        fn validate_config(&self, config_path: &str) -> Result<(), VpnError> {
            if config_path.is_empty() {
                return Err(VpnError::InvalidConfiguration("empty".into()));
            }
            if self.validate_should_fail {
                return Err(VpnError::ValidationError("bad config".into()));
            }
            Ok(())
        }
        fn check_prerequisites(&self) -> Vec<PrerequisiteCheck> {
            vec![PrerequisiteCheck {
                name: "mock".into(),
                passed: true,
                message: "ok".into(),
            }]
        }
        fn subscribe(&self) -> Option<std::sync::mpsc::Receiver<domain::CoreEvent>> {
            None
        }
    }

    fn test_registry() -> CoreRegistry {
        let mut reg = CoreRegistry::new();
        reg.register(
            CoreDescriptor {
                core_type: "mock".into(),
                display_name: "Mock".into(),
                source: CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(MockVpnCore::new()),
        );
        reg
    }

    fn runtime_config_json(marker: &str) -> String {
        serde_json::json!({
            "marker": marker,
            "inbounds": [{"type": "tun"}]
        })
        .to_string()
    }

    fn selector_runtime_config_json(default_tag: &str) -> String {
        serde_json::json!({
            "outbounds": [
                { "type": "direct", "tag": "↔️ Direct" },
                {
                    "type": "selector",
                    "tag": "🌐 Proxy",
                    "default": default_tag,
                    "interrupt_exist_connections": true,
                    "outbounds": [
                        "🇳🇱 Netherlands",
                        "🇩🇪 Germany"
                    ]
                },
                { "type": "vless", "tag": "🇳🇱 Netherlands", "server": "nl.example.com" },
                { "type": "vless", "tag": "🇩🇪 Germany", "server": "de.example.com" }
            ]
        })
        .to_string()
    }

    fn write_raw_config(json: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "pingle-service-raw-config-{}-{nanos}.json",
            std::process::id()
        ));
        std::fs::write(&path, json).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn write_runtime_config(marker: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "pingle-service-config-{}-{nanos}.json",
            std::process::id()
        ));
        std::fs::write(&path, runtime_config_json(marker)).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn manager_with_config() -> VpnManager {
        let mut storage = MemorySettingsStorage::new();
        storage
            .set_string("config_path", &write_runtime_config("default"))
            .unwrap();
        VpnManager::new(test_registry(), Box::new(storage))
    }

    fn manager_connected() -> VpnManager {
        let mut storage = MemorySettingsStorage::new();
        storage
            .set_string("config_path", &write_runtime_config("connected"))
            .unwrap();
        manager_connected_with_storage(storage)
    }

    fn manager_connected_with_storage(storage: MemorySettingsStorage) -> VpnManager {
        let mut reg = CoreRegistry::new();
        reg.register(
            CoreDescriptor {
                core_type: "mock".into(),
                display_name: "Mock".into(),
                source: CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(MockVpnCore::connected()),
        );
        VpnManager::new(reg, Box::new(storage))
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.as_ref() {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn install_runtime_env(root: &std::path::Path) -> Vec<EnvGuard> {
        let mut guards = Vec::new();
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "APPDATA",
            "LOCALAPPDATA",
            "TMPDIR",
            "TEMP",
            "TMP",
        ] {
            guards.push(EnvGuard::set(key, root));
        }
        guards
    }

    // -- Lifecycle tests ----------------------------------------------------

    #[test]
    fn connect_success() {
        let mgr = manager_with_config();
        assert_eq!(mgr.get_status(), ConnectionState::Disconnected);
        assert!(mgr.connect().is_ok());
        assert_eq!(mgr.get_status(), ConnectionState::Connected);
    }

    #[test]
    fn connect_no_config() {
        let mgr = VpnManager::new(test_registry(), Box::new(MemorySettingsStorage::new()));
        assert!(matches!(
            mgr.connect(),
            Err(VpnError::InvalidConfiguration(_))
        ));
    }

    #[test]
    fn disconnect_success() {
        let mgr = manager_connected();
        assert!(mgr.disconnect().is_ok());
        assert_eq!(mgr.get_status(), ConnectionState::Disconnected);
    }

    #[test]
    fn restart_success() {
        let mgr = manager_connected();
        assert!(mgr.restart().is_ok());
        assert_eq!(mgr.get_status(), ConnectionState::Connected);
    }

    #[test]
    fn force_kill() {
        let mgr = manager_connected();
        assert!(mgr.force_kill().is_ok());
        assert_eq!(mgr.get_status(), ConnectionState::Disconnected);
    }

    // -- Middleware integration tests ----------------------------------------

    #[test]
    fn middleware_can_rewrite_config_path() {
        let mgr = manager_with_config();
        // WrapHook: receives `next`, rewrites input, delegates to inner chain.
        mgr.connect_pipeline()
            .push_wrap(Box::new(FnWrapHook::<OpConnect, _>::new(
                "rewrite",
                |mut input, next| {
                    input.config_path = "/rewritten/config.json".into();
                    next.handle(input)
                },
            )));
        // MockVpnCore accepts any non-empty path.
        assert!(mgr.connect().is_ok());
    }

    #[test]
    fn middleware_can_block_connect() {
        let mgr = manager_with_config();
        // Hook: before() rejects without calling the handler.
        mgr.connect_pipeline().push_hook(Box::new(
            FnHook::<OpConnect>::new("block")
                .before(|_input| Err(VpnError::Unknown("policy block".into()))),
        ));
        let result = mgr.connect();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("policy block"));
        assert_eq!(mgr.get_status(), ConnectionState::Disconnected);
    }

    struct StartConfigRecordingCore {
        state: ConnectionState,
        started_config: Arc<Mutex<Option<String>>>,
    }

    impl StartConfigRecordingCore {
        fn new() -> (Self, Arc<Mutex<Option<String>>>) {
            let started_config = Arc::new(Mutex::new(None));
            (
                Self {
                    state: ConnectionState::Disconnected,
                    started_config: started_config.clone(),
                },
                started_config,
            )
        }
    }

    impl VpnCore for StartConfigRecordingCore {
        fn start(&mut self, config_path: &str) -> Result<(), VpnError> {
            let config = std::fs::read_to_string(config_path).map_err(|e| {
                VpnError::InvalidConfiguration(format!("read started config {config_path}: {e}"))
            })?;
            *self
                .started_config
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(config);
            self.state = ConnectionState::Connected;
            Ok(())
        }

        fn stop(&mut self) -> Result<(), VpnError> {
            self.state = ConnectionState::Disconnected;
            Ok(())
        }

        fn kill(&mut self) -> Result<(), VpnError> {
            self.state = ConnectionState::Disconnected;
            Ok(())
        }

        fn status(&self) -> ConnectionState {
            self.state.clone()
        }

        fn info(&self) -> CoreInfo {
            CoreInfo {
                name: "recording-start-config".into(),
                version: "0.0.0".into(),
                supported_protocols: vec![],
            }
        }

        fn validate_config(&self, config_path: &str) -> Result<(), VpnError> {
            if config_path.is_empty() {
                return Err(VpnError::InvalidConfiguration("empty".into()));
            }
            Ok(())
        }

        fn check_prerequisites(&self) -> Vec<PrerequisiteCheck> {
            vec![]
        }

        fn subscribe(&self) -> Option<std::sync::mpsc::Receiver<domain::CoreEvent>> {
            None
        }
    }

    fn manager_with_start_config_recording_core(
        config_path: &str,
    ) -> (VpnManager, Arc<Mutex<Option<String>>>) {
        let (core, started_config) = StartConfigRecordingCore::new();
        let mut reg = CoreRegistry::new();
        reg.register(
            CoreDescriptor {
                core_type: "recording-start-config".into(),
                display_name: "RecordingStartConfig".into(),
                source: CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(core),
        );
        let mut storage = MemorySettingsStorage::new();
        storage.set_string("config_path", config_path).unwrap();
        (VpnManager::new(reg, Box::new(storage)), started_config)
    }

    #[test]
    #[serial]
    fn connect_localizes_remote_rulesets_before_core_start() {
        let runtime_root = TempDir::new().unwrap();
        let _guards = install_runtime_env(runtime_root.path());
        let runtime_paths = util::paths::RuntimePaths::current();

        let url = "https://storage.yandexcloud.net/srs-v3/ru-app-packages.srs";
        let cache = RulesetCache::new(runtime_paths.ruleset_cache_dir.clone()).unwrap();
        let cached_path = cache.put(url, "binary", b"SRS-CACHED").unwrap();

        let config_dir = TempDir::new().unwrap();
        let config_path = config_dir.path().join("config.json");
        std::fs::write(
            &config_path,
            serde_json::json!({
                "inbounds": [{"type": "tun"}],
                "route": {
                    "rule_set": [{
                        "type": "remote",
                        "tag": "ru-app-packages",
                        "format": "binary",
                        "url": url
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();

        let (mgr, started_config) =
            manager_with_start_config_recording_core(&config_path.to_string_lossy());

        mgr.connect().unwrap();

        let started_config = started_config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("core.start should see a config");
        let started_json: serde_json::Value = serde_json::from_str(&started_config).unwrap();
        let localized = &started_json["route"]["rule_set"][0];
        assert_eq!(localized["type"], "local");
        assert_eq!(localized["tag"], "ru-app-packages");
        assert_eq!(localized["format"], "binary");
        assert_eq!(localized["path"], cached_path.to_string_lossy().to_string());
        assert!(localized.get("url").is_none());
    }

    struct StrategyAwareRecordingCore {
        state: ConnectionState,
        started_stacks: Arc<Mutex<Vec<String>>>,
    }

    impl StrategyAwareRecordingCore {
        fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
            let started_stacks = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    state: ConnectionState::Disconnected,
                    started_stacks: started_stacks.clone(),
                },
                started_stacks,
            )
        }
    }

    impl VpnCore for StrategyAwareRecordingCore {
        fn start(&mut self, config_path: &str) -> Result<(), VpnError> {
            let config = std::fs::read_to_string(config_path).map_err(|e| {
                VpnError::InvalidConfiguration(format!("read started config {config_path}: {e}"))
            })?;
            let json: serde_json::Value = serde_json::from_str(&config).map_err(|e| {
                VpnError::InvalidConfiguration(format!("parse started config {config_path}: {e}"))
            })?;
            let stack = json["inbounds"][0]["stack"]
                .as_str()
                .unwrap_or("missing")
                .to_string();
            self.started_stacks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(stack.clone());
            if stack == "system" {
                return Err(VpnError::ProcessStartFailed("dial tcp: i/o timeout".into()));
            }
            self.state = ConnectionState::Connected;
            Ok(())
        }

        fn stop(&mut self) -> Result<(), VpnError> {
            self.state = ConnectionState::Disconnected;
            Ok(())
        }

        fn kill(&mut self) -> Result<(), VpnError> {
            self.state = ConnectionState::Disconnected;
            Ok(())
        }

        fn status(&self) -> ConnectionState {
            self.state.clone()
        }

        fn info(&self) -> CoreInfo {
            CoreInfo {
                name: "strategy-aware-recording".into(),
                version: "1.13.7".into(),
                supported_protocols: vec![],
            }
        }

        fn validate_config(&self, config_path: &str) -> Result<(), VpnError> {
            if config_path.is_empty() {
                return Err(VpnError::InvalidConfiguration("empty".into()));
            }
            Ok(())
        }

        fn check_prerequisites(&self) -> Vec<PrerequisiteCheck> {
            vec![]
        }

        fn subscribe(&self) -> Option<std::sync::mpsc::Receiver<domain::CoreEvent>> {
            None
        }

        fn default_strategy_plan(&self) -> Option<Vec<u8>> {
            Some(
                serde_json::to_vec(&StrategyPlan {
                    strategies: vec![
                        ConnectionStrategy {
                            id: "system-first".into(),
                            stack: core_config_processor::StackType::System,
                            resolver_type: core_config_processor::ResolverType::System,
                            total_timeout: Duration::from_secs(10),
                            retry: core_config_processor::RetryPolicy::Fixed {
                                max_attempts: 2,
                                delay: Duration::ZERO,
                            },
                        },
                        ConnectionStrategy {
                            id: "gvisor-fallback".into(),
                            stack: core_config_processor::StackType::GVisor,
                            resolver_type: core_config_processor::ResolverType::System,
                            total_timeout: Duration::from_secs(10),
                            retry: core_config_processor::RetryPolicy::NoRetry,
                        },
                    ],
                    global_timeout: Some(Duration::from_secs(30)),
                })
                .expect("strategy plan json"),
            )
        }
    }

    fn manager_with_strategy_recording_core(
        config_path: &str,
    ) -> (VpnManager, Arc<Mutex<Vec<String>>>) {
        let (core, started_stacks) = StrategyAwareRecordingCore::new();
        let mut reg = CoreRegistry::new();
        reg.register(
            CoreDescriptor {
                core_type: "libbox".into(),
                display_name: "StrategyAwareRecording".into(),
                source: CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(core),
        );
        let mut storage = MemorySettingsStorage::new();
        storage.set_string("config_path", config_path).unwrap();
        (VpnManager::new(reg, Box::new(storage)), started_stacks)
    }

    #[test]
    #[serial]
    fn connect_retries_within_strategy_and_advances_to_next_strategy() {
        let runtime_root = TempDir::new().unwrap();
        let _guards = install_runtime_env(runtime_root.path());
        let config_path = write_runtime_config("strategy-retry");
        let (mgr, started_stacks) = manager_with_strategy_recording_core(&config_path);

        mgr.connect().unwrap();

        let stacks = started_stacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(stacks, vec!["system", "system", "gvisor"]);
        assert_eq!(mgr.get_status(), ConnectionState::Connected);
    }

    // -- Capability tests ---------------------------------------------------

    #[test]
    fn no_capabilities_by_default() {
        let mgr = manager_with_config();
        assert!(mgr.capabilities().is_empty());
        assert!(mgr.list_outbounds().unwrap().is_empty());
        assert!(mgr.select_outbound("x").is_err());
        assert!(mgr.test_latency(&[]).is_err());
    }

    #[test]
    fn capabilities_reflect_registered_pipelines() {
        let mgr = manager_with_config();

        // Register a static outbound handler
        struct StaticOutbounds;
        impl domain::Handler<OpListOutbounds> for StaticOutbounds {
            fn handle(&self, _: ListOutboundsInput) -> Result<ListOutboundsOutput, VpnError> {
                Ok(ListOutboundsOutput {
                    outbounds: vec![domain::Outbound {
                        id: "test-1".into(),
                        name: "Test".into(),
                        protocol: domain::OutboundProtocol::Vless,
                        transport: domain::OutboundTransport::Tcp,
                        country_code: Some("JP".into()),
                        location: None,
                        latency_ms: None,
                        selected: false,
                        metadata: Default::default(),
                    }],
                    metadata: Default::default(),
                })
            }
        }

        mgr.set_list_outbounds(Pipeline::new(Box::new(StaticOutbounds)));
        assert_eq!(mgr.capabilities(), vec!["list_outbounds"]);

        let outbounds = mgr.list_outbounds().unwrap();
        assert_eq!(outbounds.len(), 1);
        assert_eq!(outbounds[0].id, "test-1");
    }

    #[test]
    fn builtin_outbound_controls_list_selector_members() {
        let config_path = write_raw_config(&selector_runtime_config_json("🇳🇱 Netherlands"));
        let mut storage = MemorySettingsStorage::new();
        storage.set_string("config_path", &config_path).unwrap();
        let mgr = VpnManager::new(test_registry(), Box::new(storage));

        crate::defaults::register_builtin_outbound_controls(&mgr);

        let outbounds = mgr.list_outbounds().unwrap();
        assert_eq!(outbounds.len(), 2);
        assert_eq!(outbounds[0].id, "🇳🇱 Netherlands");
        assert!(outbounds[0].selected);
        assert!(!outbounds[1].selected);
    }

    #[test]
    fn builtin_outbound_controls_persist_selector_default_in_legacy_config() {
        let config_path = write_raw_config(&selector_runtime_config_json("🇳🇱 Netherlands"));
        let mut storage = MemorySettingsStorage::new();
        storage.set_string("config_path", &config_path).unwrap();
        let mgr = VpnManager::new(test_registry(), Box::new(storage));

        crate::defaults::register_builtin_outbound_controls(&mgr);
        mgr.select_outbound("🇩🇪 Germany").unwrap();

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(updated["outbounds"][1]["default"], "🇩🇪 Germany");
    }

    #[test]
    fn builtin_outbound_controls_prefer_live_clash_selection_when_connected() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0_u8; 4096];
            let read = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..read]);
            assert!(request.starts_with("GET /proxies/"));
            let body = r#"{"now":"🇩🇪 Germany","all":["🇳🇱 Netherlands","🇩🇪 Germany"]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut config: serde_json::Value =
            serde_json::from_str(&selector_runtime_config_json("🇳🇱 Netherlands")).unwrap();
        config["experimental"] = serde_json::json!({
            "clash_api": { "external_controller": format!("127.0.0.1:{}", addr.port()) }
        });
        let config_path = write_raw_config(&serde_json::to_string(&config).unwrap());
        let mut storage = MemorySettingsStorage::new();
        storage.set_string("config_path", &config_path).unwrap();
        let mgr = manager_connected_with_storage(storage);

        crate::defaults::register_builtin_outbound_controls(&mgr);

        let outbounds = mgr.list_outbounds().unwrap();
        assert_eq!(outbounds.len(), 2);
        assert!(!outbounds[0].selected);
        assert!(outbounds[1].selected);
        server.join().unwrap();
    }

    #[test]
    fn builtin_outbound_controls_apply_live_clash_selection_when_connected() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(String::new()));
        let captured_server = captured.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0_u8; 4096];
            let read = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..read]).to_string();
            *captured_server.lock().unwrap() = request;
            let response =
                "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut config: serde_json::Value =
            serde_json::from_str(&selector_runtime_config_json("🇳🇱 Netherlands")).unwrap();
        config["experimental"] = serde_json::json!({
            "clash_api": { "external_controller": format!("127.0.0.1:{}", addr.port()) }
        });
        let config_path = write_raw_config(&serde_json::to_string(&config).unwrap());
        let mut storage = MemorySettingsStorage::new();
        storage.set_string("config_path", &config_path).unwrap();
        let mgr = manager_connected_with_storage(storage);

        crate::defaults::register_builtin_outbound_controls(&mgr);
        mgr.select_outbound("🇩🇪 Germany").unwrap();

        server.join().unwrap();
        let request = captured.lock().unwrap().clone();
        assert!(request.starts_with("PUT /proxies/"));
        assert!(request.contains(r#"{"name":"🇩🇪 Germany"}"#));
    }

    // -- Registry tests -----------------------------------------------------

    #[test]
    fn registry_switch() {
        let mut reg = CoreRegistry::new();
        reg.register(
            CoreDescriptor {
                core_type: "a".into(),
                display_name: "A".into(),
                source: CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(MockVpnCore::new()),
        );
        reg.switch("a").unwrap();
        assert_eq!(reg.active_type(), Some("a"));
    }

    #[test]
    fn registry_switch_unknown() {
        let mut reg = CoreRegistry::new();
        assert!(matches!(
            reg.switch("nonexistent"),
            Err(VpnError::CoreNotFound(_))
        ));
    }

    // -- Plugin slot --------------------------------------------------------

    /// Smallest possible plugin: claims one method (`stub.echo`),
    /// no authenticator. Lets us prove the slot's set/get/replace
    /// semantics without depending on any concrete plugin
    /// implementation.
    struct StubPlugin {
        tag: &'static str,
    }

    impl domain::Plugin for StubPlugin {
        fn name(&self) -> &str {
            self.tag
        }
        fn authenticator(&self) -> Option<&dyn domain::Authenticator> {
            None
        }
        fn handle_ipc(
            &self,
            method: &str,
            params: &serde_json::Value,
        ) -> Option<Result<serde_json::Value, VpnError>> {
            match method {
                "stub.echo" => Some(Ok(serde_json::json!({
                    "from": self.tag,
                    "params": params,
                }))),
                _ => None,
            }
        }
    }

    #[test]
    fn plugin_slot_is_none_by_default() {
        let mgr = manager_with_config();
        assert!(mgr.plugin().is_none());
    }

    #[test]
    fn set_plugin_stores_plugin_reachable_via_getter() {
        let mgr = manager_with_config();
        mgr.set_plugin(Arc::new(StubPlugin { tag: "first" }));
        let plugin = mgr.plugin().expect("plugin installed");
        // Exercise the plugin through the trait so the dispatch path
        // is end-to-end-tested.
        let r = plugin
            .handle_ipc("stub.echo", &serde_json::json!({"hi": "there"}))
            .expect("stub.echo is claimed")
            .expect("stub.echo returned ok");
        assert_eq!(r["from"], "first");
        assert_eq!(r["params"]["hi"], "there");
        // Methods the plugin doesn't recognise → None.
        assert!(plugin
            .handle_ipc("stub.unknown", &serde_json::Value::Null)
            .is_none());
    }

    #[test]
    fn set_plugin_replaces_existing() {
        // Installing a second plugin replaces the first; the first
        // plugin's Arc should be dropped.
        let mgr = manager_with_config();
        mgr.set_plugin(Arc::new(StubPlugin { tag: "first" }));
        mgr.set_plugin(Arc::new(StubPlugin { tag: "second" }));
        let plugin = mgr.plugin().expect("plugin installed");
        let r = plugin
            .handle_ipc("stub.echo", &serde_json::Value::Null)
            .unwrap()
            .unwrap();
        assert_eq!(r["from"], "second");
    }

    // -- Settings tests -----------------------------------------------------

    #[test]
    fn settings_roundtrip() {
        let mgr = manager_with_config();
        mgr.set_setting("key", "val").unwrap();
        assert_eq!(mgr.get_setting("key").unwrap(), Some("val".into()));
        mgr.remove_setting("key").unwrap();
        assert_eq!(mgr.get_setting("key").unwrap(), None);
    }

    // -- Poison resilience --------------------------------------------------

    #[test]
    fn survives_poisoned_registry() {
        let mgr = Arc::new(manager_with_config());
        let registry = mgr.registry();
        let _ = std::panic::catch_unwind(|| {
            let _guard = registry.lock().unwrap();
            panic!("intentional poison");
        });
        assert_eq!(mgr.get_status(), ConnectionState::Disconnected);
    }

    #[test]
    fn survives_poisoned_storage() {
        let mgr = Arc::new(manager_with_config());
        let storage = mgr.storage();
        let _ = std::panic::catch_unwind(|| {
            let _guard = storage.lock().unwrap();
            panic!("intentional poison");
        });
        assert!(mgr.get_setting("config_path").is_ok());
    }

    // -- Profile storage integration ----------------------------------------
    //
    // These tests verify that wiring a ProfileStorage into VpnManager
    // causes the connect handler to prefer the active profile's
    // decrypted config over the legacy `config_path` setting, and that
    // disconnecting drops the decrypted temp file.
    //
    // We use a tiny in-memory ProfileStorage impl instead of pulling in
    // data::EncryptedProfileStore (which would create a cyclic dep
    // from service → data → service via MemorySettingsStorage).

    use domain::{InstallIdProvider, Profile, ProfileMeta, ProfileStorage, TempConfigPath};
    use std::collections::HashMap;
    use std::time::SystemTime;

    /// In-memory profile storage for tests. Not encrypted — stores
    /// plaintext bytes in a HashMap and writes decrypted temp files
    /// to `std::env::temp_dir()`.
    struct TestProfileStorage {
        inner: Mutex<TestProfileInner>,
    }

    #[derive(Default)]
    struct TestProfileInner {
        profiles: HashMap<String, (ProfileMeta, Vec<u8>)>,
        active: Option<String>,
    }

    impl TestProfileStorage {
        fn new() -> Self {
            Self {
                inner: Mutex::new(TestProfileInner::default()),
            }
        }
    }

    impl ProfileStorage for TestProfileStorage {
        fn list(&self) -> Result<Vec<ProfileMeta>, VpnError> {
            let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let active = g.active.clone();
            Ok(g.profiles
                .values()
                .map(|(m, _)| {
                    let mut m = m.clone();
                    m.is_active = Some(&m.id) == active.as_ref();
                    m
                })
                .collect())
        }

        fn get_meta(&self, id: &str) -> Result<Option<ProfileMeta>, VpnError> {
            let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            Ok(g.profiles.get(id).map(|(m, _)| {
                let mut m = m.clone();
                m.is_active = Some(&m.id) == g.active.as_ref();
                m
            }))
        }

        fn put(&self, mut profile: Profile, config_json: &str) -> Result<Profile, VpnError> {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if profile.id.is_empty() {
                profile.id = format!("test-{}", g.profiles.len());
            }
            let meta = ProfileMeta {
                id: profile.id.clone(),
                name: profile.name.clone(),
                core_type: profile.core_type.clone(),
                source: profile.source.clone(),
                metadata: profile.metadata.clone(),
                created_at: profile.created_at,
                last_used_at: profile.last_used_at,
                is_active: false,
            };
            g.profiles
                .insert(profile.id.clone(), (meta, config_json.as_bytes().to_vec()));
            Ok(profile)
        }

        fn delete(&self, id: &str) -> Result<(), VpnError> {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            g.profiles.remove(id);
            if g.active.as_deref() == Some(id) {
                g.active = None;
            }
            Ok(())
        }

        fn active(&self) -> Result<Option<String>, VpnError> {
            Ok(self
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active
                .clone())
        }

        fn set_active(&self, id: &str) -> Result<(), VpnError> {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if !g.profiles.contains_key(id) {
                return Err(VpnError::CoreNotFound(format!("profile {id} not found")));
            }
            g.active = Some(id.to_string());
            Ok(())
        }

        fn clear_active(&self) -> Result<(), VpnError> {
            self.inner.lock().unwrap_or_else(|e| e.into_inner()).active = None;
            Ok(())
        }

        fn load_active_for_core_start(&self) -> Result<TempConfigPath, VpnError> {
            let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let id = g.active.clone().ok_or(VpnError::NotConnected)?;
            let bytes = g
                .profiles
                .get(&id)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| VpnError::StorageError("active profile missing".into()))?;
            // Nanos suffix so parallel test runs with the same profile
            // id never collide on the same path.
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "pingle-test-profile-{}-{}-{}.json",
                std::process::id(),
                id,
                nanos
            ));
            std::fs::write(&path, &bytes)
                .map_err(|e| VpnError::StorageError(format!("write temp: {e}")))?;
            Ok(TempConfigPath::new(path))
        }
    }

    struct TestInstallIdProvider {
        id: String,
    }

    impl InstallIdProvider for TestInstallIdProvider {
        fn install_id(&self) -> Result<String, VpnError> {
            Ok(self.id.clone())
        }
    }

    /// Recording core: captures the last `config_path` it was asked to
    /// start with, so tests can assert which path the handler resolved.
    struct RecordingCore {
        state: ConnectionState,
        last_start_path: Arc<Mutex<Option<String>>>,
    }

    impl RecordingCore {
        fn new() -> (Self, Arc<Mutex<Option<String>>>) {
            let recorded = Arc::new(Mutex::new(None));
            let core = Self {
                state: ConnectionState::Disconnected,
                last_start_path: recorded.clone(),
            };
            (core, recorded)
        }
    }

    impl VpnCore for RecordingCore {
        fn start(&mut self, config_path: &str) -> Result<(), VpnError> {
            *self
                .last_start_path
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(config_path.to_string());
            self.state = ConnectionState::Connected;
            Ok(())
        }
        fn stop(&mut self) -> Result<(), VpnError> {
            self.state = ConnectionState::Disconnected;
            Ok(())
        }
        fn kill(&mut self) -> Result<(), VpnError> {
            self.state = ConnectionState::Disconnected;
            Ok(())
        }
        fn status(&self) -> ConnectionState {
            self.state.clone()
        }
        fn info(&self) -> CoreInfo {
            CoreInfo {
                name: "recording".into(),
                version: "0.0.0".into(),
                supported_protocols: vec![],
            }
        }
        fn validate_config(&self, _: &str) -> Result<(), VpnError> {
            Ok(())
        }
        fn check_prerequisites(&self) -> Vec<PrerequisiteCheck> {
            vec![]
        }
        fn subscribe(&self) -> Option<std::sync::mpsc::Receiver<domain::CoreEvent>> {
            None
        }
    }

    fn manager_with_recording_core_and_profile() -> (
        VpnManager,
        Arc<Mutex<Option<String>>>,
        Arc<TestProfileStorage>,
    ) {
        let (core, recorded) = RecordingCore::new();
        let mut reg = CoreRegistry::new();
        reg.register(
            CoreDescriptor {
                core_type: "recording".into(),
                display_name: "Recording".into(),
                source: CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(core),
        );
        let mut storage = MemorySettingsStorage::new();
        storage
            .set_string("config_path", &write_runtime_config("legacy"))
            .unwrap();

        let profile_storage = Arc::new(TestProfileStorage::new());
        let id_provider: Arc<dyn InstallIdProvider> = Arc::new(TestInstallIdProvider {
            id: "test-install-id".into(),
        });
        let mgr = VpnManager::new(reg, Box::new(storage))
            .with_profile_storage(profile_storage.clone(), id_provider);
        (mgr, recorded, profile_storage)
    }

    fn manager_with_recording_core_and_profile_no_legacy_config() -> (
        VpnManager,
        Arc<Mutex<Option<String>>>,
        Arc<TestProfileStorage>,
    ) {
        let (core, recorded) = RecordingCore::new();
        let mut reg = CoreRegistry::new();
        reg.register(
            CoreDescriptor {
                core_type: "recording".into(),
                display_name: "Recording".into(),
                source: CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(core),
        );

        let profile_storage = Arc::new(TestProfileStorage::new());
        let id_provider: Arc<dyn InstallIdProvider> = Arc::new(TestInstallIdProvider {
            id: "test-install-id".into(),
        });
        let mgr = VpnManager::new(reg, Box::new(MemorySettingsStorage::new()))
            .with_profile_storage(profile_storage.clone(), id_provider);
        (mgr, recorded, profile_storage)
    }

    fn sample_profile(name: &str) -> Profile {
        Profile {
            id: String::new(),
            name: name.into(),
            core_type: "sing-box".into(),
            source: domain::ProfileSource::Imported { filename: None },
            metadata: std::collections::BTreeMap::new(),
            created_at: SystemTime::now(),
            last_used_at: None,
        }
    }

    #[test]
    fn connect_uses_active_profile_when_present() {
        let (mgr, recorded, store) = manager_with_recording_core_and_profile();
        let p = store
            .put(sample_profile("Home"), &runtime_config_json("profile"))
            .unwrap();
        store.set_active(&p.id).unwrap();

        mgr.connect().unwrap();

        let path = recorded
            .lock()
            .unwrap()
            .clone()
            .expect("core.start was called");
        let started_config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(started_config["marker"], "profile");
        assert!(
            path.contains("runtime-config-"),
            "expected runtime temp path, got: {path}"
        );
    }

    #[test]
    fn connect_falls_back_to_legacy_config_path_when_no_active_profile() {
        let (mgr, recorded, _store) = manager_with_recording_core_and_profile();
        // No active profile set — should fall through to legacy path.
        mgr.connect().unwrap();
        let path = recorded.lock().unwrap().clone().unwrap();
        let started_config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(started_config["marker"], "legacy");
    }

    #[test]
    fn connect_works_with_active_profile_without_legacy_config_path() {
        let (mgr, recorded, store) = manager_with_recording_core_and_profile_no_legacy_config();
        let p = store
            .put(sample_profile("Home"), &runtime_config_json("profile"))
            .unwrap();
        store.set_active(&p.id).unwrap();

        mgr.connect().unwrap();

        let path = recorded
            .lock()
            .unwrap()
            .clone()
            .expect("core.start was called");
        let started_config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(started_config["marker"], "profile");
        assert!(
            path.contains("runtime-config-"),
            "expected runtime temp path, got: {path}"
        );
    }

    #[test]
    fn connect_falls_back_when_profile_storage_returns_not_connected() {
        // Active profile set but the store returns NotConnected —
        // e.g. a store that never had put() called. ConnectHandler
        // should still fall back instead of bubbling the error.
        let (mgr, recorded, _store) = manager_with_recording_core_and_profile();
        // Don't put anything, don't set active — store's active()
        // returns None, load_active_for_core_start returns NotConnected,
        // handler falls back.
        mgr.connect().unwrap();
        let path = recorded.lock().unwrap().clone().unwrap();
        let started_config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(started_config["marker"], "legacy");
    }

    #[test]
    fn disconnect_deletes_decrypted_temp_file() {
        let (mgr, recorded, store) = manager_with_recording_core_and_profile();
        let p = store
            .put(sample_profile("Home"), &runtime_config_json("profile"))
            .unwrap();
        store.set_active(&p.id).unwrap();
        mgr.connect().unwrap();

        let temp_path = recorded.lock().unwrap().clone().unwrap();
        assert!(std::path::Path::new(&temp_path).exists());

        mgr.disconnect().unwrap();
        // After disconnect, the TempConfigPath is dropped and the
        // file should be gone.
        assert!(
            !std::path::Path::new(&temp_path).exists(),
            "disconnect should have deleted {temp_path}"
        );
    }

    #[test]
    fn profile_crud_via_manager() {
        let (mgr, _rec, _store) = manager_with_recording_core_and_profile();

        assert!(mgr.list_profiles().unwrap().is_empty());

        let p = mgr.put_profile(sample_profile("Home"), "{}").unwrap();
        assert!(!p.id.is_empty());

        let list = mgr.list_profiles().unwrap();
        assert_eq!(list.len(), 1);

        mgr.set_active_profile(&p.id).unwrap();
        assert_eq!(
            mgr.active_profile().unwrap().as_deref(),
            Some(p.id.as_str())
        );

        mgr.delete_profile(&p.id).unwrap();
        assert!(mgr.active_profile().unwrap().is_none());
    }

    #[test]
    fn list_profiles_returns_empty_when_storage_not_configured() {
        let mgr = manager_with_config();
        assert!(mgr.list_profiles().unwrap().is_empty());
    }

    #[test]
    fn put_profile_errors_when_storage_not_configured() {
        let mgr = manager_with_config();
        assert!(matches!(
            mgr.put_profile(sample_profile("x"), "{}"),
            Err(VpnError::StorageError(_))
        ));
    }

    #[test]
    fn install_id_returns_configured_value() {
        let (mgr, _, _) = manager_with_recording_core_and_profile();
        assert_eq!(mgr.install_id().unwrap(), "test-install-id");
    }

    #[test]
    fn install_id_errors_when_provider_not_configured() {
        let mgr = manager_with_config();
        assert!(matches!(mgr.install_id(), Err(VpnError::StorageError(_))));
    }
}
