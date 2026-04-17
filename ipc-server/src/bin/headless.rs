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

use std::sync::Arc;

use ipc_server::start;

fn main() {
    // Default to debug logging unless RUST_LOG already set.
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "ipc_server=debug,info");
    }
    env_logger::init();

    // Build a VpnManager backed by the mock core. No real binary needed.
    let mut registry = service::CoreRegistry::new();
    registry.register(
        domain::CoreDescriptor {
            core_type: "mock".into(),
            display_name: "Mock (headless)".into(),
            source: domain::CoreSource::Mocked,
            binary_path: None,
            available: true,
        },
        Box::new(core_mock::MockCore::new()),
    );

    // libbox (Windows) — registered only when the binary was built
    // with `--features libbox-windows`. Stays present on non-Windows
    // hosts too (the core-libbox-windows crate is a no-op stub there),
    // but reports PrerequisiteMissing until Windows + a real libbox.dll.
    // Calling libbox.Version() proves the linker resolved the DLL
    // symbols end-to-end: stub mode returns null, linked mode returns
    // "sing-box-<version>".
    #[cfg(feature = "libbox-windows")]
    {
        use domain::VpnCore;
        let core = core_libbox_windows::LibboxCoreWindows::new();
        let info = core.info();
        log::info!(
            "headless: libbox core registered, reports version = {:?}",
            info.version
        );
        registry.register(
            domain::CoreDescriptor {
                core_type: "libbox".into(),
                display_name: "libbox (Windows)".into(),
                source: domain::CoreSource::Linked("libbox.dll".into()),
                binary_path: None,
                available: true,
            },
            Box::new(core),
        );
    }
    let storage: Box<dyn domain::SettingsStorage> = Box::new(data::MemorySettingsStorage::new());
    // Pre-seed a config_path so vpn.connect() doesn't fail on InvalidConfiguration.
    let mut storage = storage;
    let _ = storage.set_string("config_path", "/tmp/headless-mock.json");

    let vpn = Arc::new(service::VpnManager::new(registry, storage));

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
