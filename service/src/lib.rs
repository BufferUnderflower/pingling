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

use domain::ops::*;
use domain::pipeline::Pipeline;
use domain::{
    ConnectionState, CoreDescriptor, CoreInfo, CoreSource, Plugin, PrerequisiteCheck,
    SettingsStorage, VpnCore, VpnError,
};
use handlers::*;
use log::info;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

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

        Self {
            connect: Mutex::new(Pipeline::new(Box::new(ConnectHandler {
                registry: registry.clone(),
            }))),
            disconnect: Mutex::new(Pipeline::new(Box::new(DisconnectHandler {
                registry: registry.clone(),
            }))),
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
            registry,
            storage,
        }
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

    fn get_config_path(&self) -> Result<String, VpnError> {
        self.storage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_string("config_path")?
            .ok_or_else(|| {
                VpnError::InvalidConfiguration("config_path not found in settings".into())
            })
    }

    // -- lifecycle operations -----------------------------------------------

    pub fn connect(&self) -> Result<(), VpnError> {
        let config_path = self.get_config_path()?;
        let input = ConnectInput {
            config_path,
            core_type: self.active_core_type_str(),
            state: self.get_status(),
            metadata: BTreeMap::new(),
        };
        self.connect
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute(input)?;
        Ok(())
    }

    pub fn disconnect(&self) -> Result<(), VpnError> {
        let input = DisconnectInput {
            core_type: self.active_core_type_str(),
            state: self.get_status(),
            metadata: BTreeMap::new(),
        };
        self.disconnect
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute(input)?;
        Ok(())
    }

    pub fn force_kill(&self) -> Result<(), VpnError> {
        info!("Force-killing");
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let core = registry.active_core().ok_or(VpnError::NotConnected)?;
        core.kill()
    }

    pub fn restart(&self) -> Result<(), VpnError> {
        let config_path = self.get_config_path()?;
        let input = RestartInput {
            config_path,
            core_type: self.active_core_type_str(),
            state: self.get_status(),
            metadata: BTreeMap::new(),
        };
        self.restart
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute(input)?;
        Ok(())
    }

    pub fn validate_config(&self, config_path: &str) -> Result<(), VpnError> {
        let input = ValidateConfigInput {
            config_path: config_path.to_string(),
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

    // -- capability operations (pipeline-gated) -----------------------------

    /// List outbounds if the capability is registered. Returns empty if not.
    pub fn list_outbounds(&self) -> Result<Vec<domain::Outbound>, VpnError> {
        let guard = self
            .list_outbounds
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(pipeline) => {
                let input = ListOutboundsInput {
                    core_type: self.active_core_type_str(),
                    config_path: self.get_config_path().ok(),
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
                let input = SelectOutboundInput {
                    outbound_id: outbound_id.to_string(),
                    core_type: self.active_core_type_str(),
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use data::MemorySettingsStorage;
    use domain::pipeline::{FnHook, FnWrapHook};

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

    fn manager_with_config() -> VpnManager {
        let mut storage = MemorySettingsStorage::new();
        storage
            .set_string("config_path", "/fake/config.json")
            .unwrap();
        VpnManager::new(test_registry(), Box::new(storage))
    }

    fn manager_connected() -> VpnManager {
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
        let mut storage = MemorySettingsStorage::new();
        storage
            .set_string("config_path", "/fake/config.json")
            .unwrap();
        VpnManager::new(reg, Box::new(storage))
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
}
