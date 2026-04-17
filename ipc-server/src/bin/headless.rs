//! Headless IPC server — boots a [`VpnManager`] backed by the in-process
//! mock core and exposes it over UDS + TCP + UDP discovery.
//!
//! This binary exists for two purposes:
//!
//! 1. **End-to-end testing**: The Dart `clients/tui` repo spawns this
//!    process to exercise the full client→server stack without needing the
//!    Tauri app, a tray, or sing-box installed.
//!
//! 2. **Manual smoke test**: A developer can run
//!    `cargo run -p ipc-server --features headless --bin ipc-server-headless`
//!    and then point any JSON-RPC client at the printed UDS / TCP endpoint.
//!
//! The mock core requires no real binary — it accepts any `start(config_path)`
//! call and reports `Connected` until `stop()` flips it back. Perfect for
//! exercising the protocol without spinning up a real VPN.
//!
//! ## Optional plugin loading (env-gated)
//!
//! Set `PINGLE_PLUGIN_WASM=/path/to/plugin.wasm` to install a wasm
//! plugin at startup via `plugin_extism::plugin_adapter::PluginAdapter`.
//! Allowed-hosts can be overridden via `PINGLE_PLUGIN_ALLOWED_HOSTS`
//! (comma-separated); defaults to `example.com` for the
//! canonical Pingle plugin. Used by hand-driven plugin smoke tests
//! and the Dart e2e harness.
//!
//! On startup the binary prints a single JSON line to stdout describing how
//! to connect, so test harnesses can parse it without scraping logs:
//!
//! ```text
//! {"uds":"/tmp/pingle.sock","tcp":"127.0.0.1:54321","pid":12345}
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use domain::{ConnectionState, CoreDescriptor, CoreEvent, CoreSource, InstallIdProvider, ProfileStorage, SettingsStorage};
#[cfg(feature = "libbox-windows")]
use domain::VpnCore;

fn main() {
    // Default to debug logging unless RUST_LOG already set.
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "ipc_server=debug,info");
    }
    env_logger::init();

    let registry = build_registry();
    let vpn = build_vpn_manager(registry);

    // --- Slot-chain observer wiring ----------------------------------
    //
    // Build a shared EventBroadcaster now so the slot observer and
    // the IPC server share the same push channel. Any subscriber
    // attaching to the IPC server sees both `event.stateChanged`
    // and `event.slot.*` notifications flowing out of the same
    // broadcaster. `PINGLING_SLOT_BROADCAST=0` turns the broadcast
    // sink off at boot for deployments where no listener is
    // expected; the log sink is always on at the trace level.
    let broadcaster = Arc::new(ipc_server::EventBroadcaster::new());
    let slot_observer = Arc::new(ipc_server::BroadcastingSlotObserver::new(
        broadcaster.clone(),
    ));
    if matches!(
        std::env::var("PINGLING_SLOT_BROADCAST").as_deref(),
        Ok("0") | Ok("false") | Ok("off")
    ) {
        slot_observer.set_broadcast_enabled(false);
    }
    vpn.set_slot_observer(slot_observer);
    spawn_core_event_bridge(vpn.clone(), broadcaster.clone());

    // Optional plugin install — only when PINGLE_PLUGIN_WASM is set.
    // The same wasm wire contract as `app/src/main.rs::discover_plugin`
    // (both go through `PluginAdapter::load`), just with the path
    // sourced from an env var instead of a directory scan, so the
    // headless binary can be parameterised by tests + smoke scripts
    // without dropping files into the user's plugins dir.
    if let Some(wasm_path) = std::env::var_os("PINGLE_PLUGIN_WASM") {
        let allowed_hosts: Vec<String> = std::env::var("PINGLE_PLUGIN_ALLOWED_HOSTS")
            .unwrap_or_else(|_| "example.com".to_string())
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().to_string())
            .collect();
        let path = std::path::PathBuf::from(&wasm_path);
        log::info!(
            "headless: loading plugin from {} (allowed_hosts={:?})",
            path.display(),
            allowed_hosts
        );
        match plugin_extism::plugin_adapter::PluginAdapter::load(&path, allowed_hosts) {
            Ok(plugin) => {
                vpn.set_plugin(plugin);
                log::info!("headless: plugin installed");
            }
            Err(e) => {
                log::error!("headless: plugin load failed: {e}");
            }
        }
    }

    let handle =
        ipc_server::start_with_broadcaster(vpn, broadcaster).expect("ipc-server failed to start");

    // Print machine-readable connect info on a single stdout line so test
    // harnesses can pick it up without parsing logs.
    let line = serde_json::json!({
        "uds": handle.uds_path,
        "tcp": handle.tcp_addr,
        "pid": std::process::id(),
    });
    println!("{line}");
    // Important: flush so the parent test process can read it immediately.
    use std::io::Write;
    std::io::stdout().flush().ok();

    log::info!("ipc-server-headless ready, waiting for clients");

    // Block forever — the listener threads handle everything.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

fn build_registry() -> service::CoreRegistry {
    let mut registry = service::CoreRegistry::new();
    #[cfg(feature = "libbox-windows")]
    let mut fallback_entries: Vec<(CoreDescriptor, Box<dyn VpnCore>)> = Vec::new();

    #[cfg(feature = "libbox-windows")]
    {
        let core = core_libbox_windows::LibboxCoreWindows::new();
        let prereqs = core.check_prerequisites();
        let available = core_libbox_windows::runtime_available(&prereqs);
        log::info!("headless: libbox prereqs = {:?}", prereqs);
        register_preferred_or_fallback(
            &mut registry,
            &mut fallback_entries,
            CoreDescriptor {
                core_type: "libbox".into(),
                display_name: "Libbox (Windows)".into(),
                source: CoreSource::Linked("libbox.dll".into()),
                binary_path: None,
                available,
            },
            Box::new(core),
        );
    }

    if registry.active_type().is_none() {
        registry.register(
            CoreDescriptor {
                core_type: "mock".into(),
                display_name: "Mock (headless fallback)".into(),
                source: CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(core_mock::MockCore::new()),
        );
    }

    #[cfg(feature = "libbox-windows")]
    for (descriptor, core) in fallback_entries {
        registry.register(descriptor, core);
    }

    registry
}

#[cfg(feature = "libbox-windows")]
fn register_preferred_or_fallback(
    registry: &mut service::CoreRegistry,
    fallback_entries: &mut Vec<(CoreDescriptor, Box<dyn VpnCore>)>,
    descriptor: CoreDescriptor,
    core: Box<dyn VpnCore>,
) {
    if descriptor.available && registry.active_type().is_none() {
        registry.register(descriptor, core);
        return;
    }

    fallback_entries.push((descriptor, core));
}

fn build_vpn_manager(registry: service::CoreRegistry) -> Arc<service::VpnManager> {
    let active_core = registry.active_type().map(str::to_string);
    let mut storage: Box<dyn SettingsStorage> = Box::new(data::MemorySettingsStorage::new());
    if let Some(config_path) = resolve_default_config_path(active_core.as_deref()) {
        let _ = storage.set_string("config_path", &config_path);
    }

    let vpn_base = service::VpnManager::new(registry, storage);
    match build_profile_store() {
        Ok(Some(store)) => {
            log::info!("headless: profile store initialized");
            let store: Arc<data::EncryptedProfileStore> = Arc::new(store);
            Arc::new(vpn_base.with_profile_storage(
                store.clone() as Arc<dyn ProfileStorage>,
                store as Arc<dyn InstallIdProvider>,
            ))
        }
        Ok(None) => Arc::new(vpn_base),
        Err(error) => {
            log::warn!("headless: profile store unavailable ({error})");
            Arc::new(vpn_base)
        }
    }
}

fn resolve_default_config_path(active_core: Option<&str>) -> Option<String> {
    if let Some(path) = std::env::var("PINGLING_CONFIG_PATH").ok().filter(|value| !value.is_empty()) {
        return Some(path);
    }

    if active_core == Some("mock") {
        let default_path = std::env::temp_dir().join("pingling-headless-mock.json");
        let path_str = default_path.to_string_lossy().to_string();
        if let Err(error) = std::fs::write(&default_path, "{}") {
            log::warn!("headless: failed to seed mock config at {path_str}: {error}");
        }
        return Some(path_str);
    }

    None
}

fn build_profile_store() -> Result<Option<data::EncryptedProfileStore>, domain::VpnError> {
    if matches!(
        std::env::var("PINGLING_PROFILE_STORE").as_deref(),
        Ok("0") | Ok("false") | Ok("off")
    ) {
        return Ok(None);
    }

    let override_dir = std::env::var_os("PINGLING_PROFILE_STORE_DIR").map(PathBuf::from);
    match override_dir {
        Some(base_dir) => data::EncryptedProfileStore::with_base_dir(base_dir).map(Some),
        None => data::EncryptedProfileStore::default_path().map(Some),
    }
}

fn spawn_core_event_bridge(vpn: Arc<service::VpnManager>, broadcaster: Arc<ipc_server::EventBroadcaster>) {
    let Some(rx) = vpn.subscribe_active_core_events() else {
        return;
    };

    std::thread::Builder::new()
        .name("pingle-headless-core-events".into())
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                match event {
                    CoreEvent::Log(message) => broadcaster.publish_log("info", &message),
                    CoreEvent::ErrorLog(message) => broadcaster.publish_log("error", &message),
                    CoreEvent::StateChanged(state) => publish_state_from_core(&broadcaster, &vpn, state),
                    CoreEvent::Started => publish_state_from_core(
                        &broadcaster,
                        &vpn,
                        ConnectionState::Connecting,
                    ),
                    CoreEvent::Stopped(_) => publish_state_from_core(
                        &broadcaster,
                        &vpn,
                        ConnectionState::Disconnected,
                    ),
                    CoreEvent::Crashed(message) => {
                        broadcaster.publish_log("error", &message);
                        publish_state_from_core(
                            &broadcaster,
                            &vpn,
                            ConnectionState::Error(message),
                        );
                    }
                }
            }
        })
        .ok();
}

fn publish_state_from_core(
    broadcaster: &Arc<ipc_server::EventBroadcaster>,
    vpn: &Arc<service::VpnManager>,
    state: ConnectionState,
) {
    let core = vpn.active_core_type().unwrap_or_else(|| "none".into());
    broadcaster.publish_state(&state.to_string(), state.is_active(), &core);
}

#[cfg(test)]
mod tests {
    use super::{build_profile_store, resolve_default_config_path};
    use tempfile::tempdir;

    #[test]
    fn resolve_default_config_path_only_seeds_mock() {
        let mock = resolve_default_config_path(Some("mock")).expect("mock config path");
        assert!(mock.contains("pingling-headless-mock.json"));
        assert!(resolve_default_config_path(Some("libbox")).is_none());
    }

    #[test]
    fn build_profile_store_honors_override_dir() {
        let dir = tempdir().unwrap();
        std::env::set_var("PINGLING_PROFILE_STORE_DIR", dir.path());
        std::env::remove_var("PINGLING_PROFILE_STORE");

        let store = build_profile_store().expect("profile store result");
        assert!(store.is_some(), "profile store should be enabled");

        std::env::remove_var("PINGLING_PROFILE_STORE_DIR");
    }
}
