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
//! (comma-separated); defaults to `panel.example.com` for the
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
    let storage: Box<dyn domain::SettingsStorage> = Box::new(data::MemorySettingsStorage::new());
    // Pre-seed a config_path so vpn.connect() doesn't fail on InvalidConfiguration.
    let mut storage = storage;
    let _ = storage.set_string("config_path", "/tmp/headless-mock.json");

    let vpn = Arc::new(service::VpnManager::new(registry, storage));

    // Optional plugin install — only when PINGLE_PLUGIN_WASM is set.
    // The same wasm wire contract as `app/src/main.rs::discover_plugin`
    // (both go through `PluginAdapter::load`), just with the path
    // sourced from an env var instead of a directory scan, so the
    // headless binary can be parameterised by tests + smoke scripts
    // without dropping files into the user's plugins dir.
    if let Some(wasm_path) = std::env::var_os("PINGLE_PLUGIN_WASM") {
        let allowed_hosts: Vec<String> = std::env::var("PINGLE_PLUGIN_ALLOWED_HOSTS")
            .unwrap_or_else(|_| "panel.example.com".to_string())
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

    let handle = start(vpn).expect("ipc-server failed to start");

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
