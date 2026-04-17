//! Pingle — Tauri headless daemon.
//!
//! Runs as a background process with a system tray. Has **no webview window**.
//! The Flutter UI (separate repository) connects via a JSON-RPC 2.0 server
//! on a Unix domain socket (`$TMPDIR/pingle.sock`) or Windows named pipe.
//!
//! Wires together:
//! - `domain` traits ([`VpnCore`](domain::VpnCore), [`SettingsStorage`](domain::SettingsStorage))
//! - `service` [`VpnManager`](service::VpnManager) orchestrator + [`CoreRegistry`](service::CoreRegistry)
//! - System tray: status icon (red/yellow/green), core selector, config picker, connection controls
//! - Tauri `#[command]` handlers (used by a future in-process admin interface, not by Flutter)
//!
//! # IPC for Flutter
//! Flutter does **not** use Tauri's built-in `invoke()` bridge — that requires a webview.
//! Instead, this daemon will expose a JSON-RPC 2.0 server (see `ARCHITECTURE.md`).
//! The `#[tauri::command]` handlers below are kept for internal tooling / admin UI use.

#[cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use app_config::{ConfigLoader, PinglingConfig};
use domain::VpnCore;
use ipc_server as ipc;
use serde::Serialize;
use service::{CoreRegistry, VpnManager};
use std::sync::Arc;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, Submenu},
    tray::TrayIconBuilder,
    Manager,
};

/// Application state shared across the system tray, Tauri commands, and the JSON-RPC IPC server.
///
/// Wrapped in `Arc<AppState>` and injected into Tauri via `app.manage()`.
/// The JSON-RPC server (when implemented) will clone the `Arc<VpnManager>` from here.
struct AppState {
    vpn: Arc<VpnManager>,
    icon_disconnected: tauri::image::Image<'static>,
    icon_connecting: tauri::image::Image<'static>,
    icon_connected: tauri::image::Image<'static>,
    /// Push-event broadcaster shared with the IPC server. The tray refresh
    /// loop publishes `event.stateChanged` here so subscribed clients
    /// (Flutter UI, standalone TUI) react in real time.
    ipc_broadcaster: Arc<ipc::EventBroadcaster>,
}

// ---------------------------------------------------------------------------
// Serializable types for IPC
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct CoreDescriptorDto {
    core_type: String,
    display_name: String,
    source: String,
    binary_path: Option<String>,
    available: bool,
}

impl From<&domain::CoreDescriptor> for CoreDescriptorDto {
    fn from(d: &domain::CoreDescriptor) -> Self {
        Self {
            core_type: d.core_type.clone(),
            display_name: d.display_name.clone(),
            source: d.source.to_string(),
            binary_path: d.binary_path.clone(),
            available: d.available,
        }
    }
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

fn load_config() -> PinglingConfig {
    match ConfigLoader::from_env() {
        Ok(cfg) => {
            log::info!("config loaded: core_type={}", cfg.core_type);
            cfg
        }
        Err(e) => {
            log::warn!("config load failed ({e}), using defaults");
            PinglingConfig::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin discovery
// ---------------------------------------------------------------------------

/// Default hostnames wasm plugins are allowed to reach via
/// `extism_pdk::http::request(...)`.
///
/// Empty by default — the OSS build has no hardcoded endpoints. Add
/// hosts at runtime via the `PINGLE_PLUGIN_ALLOWED_HOSTS` env var
/// (comma-separated). A plugin with no allowed host can still load,
/// it just cannot reach any network service.
const DEFAULT_PLUGIN_ALLOWED_HOSTS: &[&str] = &[];

/// Parse the `PINGLE_PLUGIN_ALLOWED_HOSTS` env var into an allowed-hosts
/// list. Falls back to `DEFAULT_PLUGIN_ALLOWED_HOSTS` when unset.
fn resolve_plugin_allowed_hosts() -> Vec<String> {
    std::env::var("PINGLE_PLUGIN_ALLOWED_HOSTS")
        .map(|s| {
            s.split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|_| {
            DEFAULT_PLUGIN_ALLOWED_HOSTS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        })
}

/// Resolve the directory the daemon scans for `.wasm` plugins.
///
/// Priority:
/// 1. `PinglingConfig::plugins.plugins_dir` (explicit path from config file or
///    `PINGLING_PLUGINS_DIR` env var) if non-empty.
/// 2. The XDG-style platform default: `$XDG_CONFIG_HOME/pingle/plugins` on
///    Linux/BSD, `~/.config/pingle/plugins` if XDG_CONFIG_HOME is unset, or
///    `~/Library/Application Support/pingle/plugins` on macOS.
fn resolve_plugins_dir(cfg: &PinglingConfig) -> Option<std::path::PathBuf> {
    if !cfg.plugins.plugins_dir.is_empty() {
        return Some(std::path::PathBuf::from(&cfg.plugins.plugins_dir));
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(std::path::PathBuf::from(home).join("Library/Application Support/pingle/plugins"))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA")?;
        Some(std::path::PathBuf::from(appdata).join("pingle\\plugins"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(xdg) => std::path::PathBuf::from(xdg),
            None => {
                let home = std::env::var_os("HOME")?;
                std::path::PathBuf::from(home).join(".config")
            }
        };
        Some(base.join("pingle/plugins"))
    }
}

/// Scan the plugins dir for `.wasm` files, try to load each via
/// [`plugin_extism::plugin_adapter::PluginAdapter`], and return the
/// first one that exposes `plugin_handle_ipc` (the only required
/// export). Returns `None` if the directory is missing, empty, or
/// contains nothing that satisfies the contract.
///
/// Honours `cfg.plugins.enabled` as a filename filter when non-empty
/// (matching the existing convention from PluginsConfig). All wasm
/// files are tried in directory order otherwise.
///
/// We deliberately stop at the first match because the
/// `VpnManager::set_plugin` slot only holds one plugin at a time. To
/// switch plugins the user removes the old `.wasm` and drops in a
/// new one.
fn discover_plugin(cfg: &PinglingConfig) -> Option<std::sync::Arc<dyn domain::Plugin>> {
    let dir = resolve_plugins_dir(cfg)?;
    if !dir.exists() {
        log::debug!("plugin: plugin dir {} does not exist", dir.display());
        return None;
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(it) => it,
        Err(e) => {
            log::warn!("plugin: read_dir({}) failed: {e}", dir.display());
            return None;
        }
    };

    let allowed_hosts = resolve_plugin_allowed_hosts();

    let enabled_filter = &cfg.plugins.enabled;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("wasm") {
            continue;
        }
        if !enabled_filter.is_empty() {
            let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !enabled_filter.iter().any(|allowed| allowed == fname) {
                continue;
            }
        }
        log::info!("plugin: trying {}", path.display());
        match plugin_extism::plugin_adapter::PluginAdapter::load(&path, allowed_hosts.clone()) {
            Ok(adapter) => {
                log::info!(
                    "plugin: installed {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
                return Some(adapter);
            }
            Err(e) => {
                log::warn!("plugin: {} did not load: {e}", path.display());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Core setup
// ---------------------------------------------------------------------------

fn build_registry(cfg: &PinglingConfig) -> CoreRegistry {
    let mut registry = CoreRegistry::new();

    // -- libbox (in-process FFI, Windows) — registered first = default ------
    //
    // When libbox.dll is present next to the daemon binary (or at the
    // path pointed to by PINGLE_LIBBOX_WINDOWS_DIR), this core drives
    // sing-box in-process via the gobind C API — same mechanism the
    // macOS build uses via Libbox.xcframework. When the DLL is absent,
    // the crate compiles in cfg(libbox_stub) mode and every VpnCore
    // method returns PrerequisiteMissing, so the core is registered as
    // `available: false` and the registry falls through to the next one.
    #[cfg(feature = "libbox-windows")]
    {
        let core = core_libbox_windows::LibboxCoreWindows::new();
        let prereqs = core.check_prerequisites();
        let available = core_libbox_windows::runtime_available(&prereqs);
        log::info!("registering libbox-windows core (available: {available})");
        registry.register(
            domain::CoreDescriptor {
                core_type: "libbox".into(),
                display_name: "Libbox (in-process)".into(),
                source: domain::CoreSource::Linked("libbox.dll".into()),
                binary_path: None,
                available,
            },
            Box::new(core),
        );
    }

    // -- sing-box standalone (subprocess) — fallback on all platforms --------
    #[cfg(feature = "sing-box")]
    {
        let binary_path = if cfg.core_binary_path.is_empty() {
            util::which("sing-box").unwrap_or_default()
        } else {
            cfg.core_binary_path.clone()
        };
        let available = !binary_path.is_empty() && std::path::Path::new(&binary_path).exists();

        if available {
            log::info!("registering sing-box subprocess core at: {binary_path}");
        } else {
            log::info!("sing-box binary not found (searched PATH + config) — core registered as unavailable");
        }

        let core = core_singbox_standalone::SingboxStandalone::new(&binary_path);
        registry.register(
            domain::CoreDescriptor {
                core_type: "sing-box".into(),
                display_name: "Sing-Box (subprocess)".into(),
                source: if cfg.core_binary_path.is_empty() {
                    domain::CoreSource::System
                } else {
                    domain::CoreSource::Linked(cfg.core_binary_path.clone())
                },
                binary_path: if binary_path.is_empty() {
                    None
                } else {
                    Some(binary_path)
                },
                available,
            },
            Box::new(core),
        );
    }

    // -- mock (always available in debug builds) ----------------------------
    #[cfg(debug_assertions)]
    {
        registry.register(
            domain::CoreDescriptor {
                core_type: "mock".into(),
                display_name: "Mock (Debug)".into(),
                source: domain::CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(core_mock::MockCore::new()),
        );
    }

    // If nothing registered (release build, no features), add mock as last resort
    if registry.list().is_empty() {
        registry.register(
            domain::CoreDescriptor {
                core_type: "mock".into(),
                display_name: "Mock (fallback)".into(),
                source: domain::CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(core_mock::MockCore::new()),
        );
    }

    registry.discover();
    registry
}

// ---------------------------------------------------------------------------
// Menu building
// ---------------------------------------------------------------------------

/// Build the tray menu dynamically based on current state.
fn build_tray_menu(app: &tauri::AppHandle, vpn: &VpnManager) -> tauri::Result<Menu<tauri::Wry>> {
    let active_core = vpn.active_core_type().unwrap_or_else(|| "none".into());
    let cores = vpn.list_cores();
    let running = vpn.is_running();

    // -- Core selector submenu --
    let mut core_items: Vec<Box<dyn tauri::menu::IsMenuItem<tauri::Wry>>> = Vec::new();
    for core in &cores {
        let label = if core.available {
            core.display_name.clone()
        } else {
            format!("{} (not found)", core.display_name)
        };
        let item = CheckMenuItem::with_id(
            app,
            format!("core:{}", core.core_type),
            &label,
            core.available && !running, // can't switch core while connected
            core.core_type == active_core,
            None::<&str>,
        )?;
        core_items.push(Box::new(item));
    }
    if core_items.is_empty() {
        let item = MenuItem::with_id(app, "core:none", "No cores found", false, None::<&str>)?;
        core_items.push(Box::new(item));
    }
    let core_refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> =
        core_items.iter().map(|b| b.as_ref()).collect();
    let cores_submenu = Submenu::with_items(app, "Core", true, &core_refs)?;

    // -- Config path display --
    let config_path = vpn
        .get_setting("config_path")
        .ok()
        .flatten()
        .unwrap_or_default();
    let config_label = if config_path.is_empty() {
        "Set core config\u{2026}".to_string()
    } else {
        let filename = std::path::Path::new(&config_path)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| config_path.clone());
        format!("Config: {filename}")
    };
    let config_item = MenuItem::with_id(app, "pick_config", &config_label, !running, None::<&str>)?;

    // -- Status line --
    let state_label = if running {
        format!("\u{25cf} connected ({active_core})")
    } else {
        format!("\u{25cb} disconnected ({active_core})")
    };
    let status_item = MenuItem::with_id(app, "status_label", &state_label, false, None::<&str>)?;

    // -- Connection controls --
    let connect = MenuItem::with_id(app, "connect", "Connect", !running, None::<&str>)?;
    let disconnect = MenuItem::with_id(app, "disconnect", "Disconnect", running, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "Restart", running, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "Show Window", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    Menu::with_items(
        app,
        &[
            &status_item,
            &MenuItem::with_id(
                app,
                "sep1",
                "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                false,
                None::<&str>,
            )?,
            &cores_submenu,
            &config_item,
            &MenuItem::with_id(
                app,
                "sep2",
                "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                false,
                None::<&str>,
            )?,
            &connect,
            &disconnect,
            &restart,
            &MenuItem::with_id(
                app,
                "sep3",
                "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                false,
                None::<&str>,
            )?,
            &show,
            &quit,
        ],
    )
}

/// Publish a `event.stateChanged` notification to every IPC client subscribed
/// via the broadcaster. Called from tray menu handlers right after a state
/// change so external clients (Flutter UI, standalone TUI) react immediately
/// instead of waiting for the next 500ms tray refresh poll.
fn push_state_event(state: &AppState) {
    let core = state.vpn.active_core_type().unwrap_or_default();
    let running = state.vpn.is_running();
    let label = state.vpn.get_status().to_string();
    state.ipc_broadcaster.publish_state(&label, running, &core);
}

/// Rebuild the tray menu and update icon after a state change.
fn refresh_tray(app: &tauri::AppHandle) {
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let state = app.state::<AppState>();
    let vpn = &state.vpn;

    let running = vpn.is_running();
    let status_text = vpn.get_status().to_string();
    let icon = if running {
        &state.icon_connected
    } else if status_text.contains("connect") {
        &state.icon_connecting
    } else {
        &state.icon_disconnected
    };
    let _ = tray.set_icon(Some(icon.clone()));

    let core = vpn.active_core_type().unwrap_or_else(|| "none".into());
    let state_str = if running { "connected" } else { "disconnected" };
    let _ = tray.set_tooltip(Some(&format!("{state_str} ({core})")));

    if let Ok(menu) = build_tray_menu(app, vpn) {
        let _ = tray.set_menu(Some(menu));
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let log_path = std::env::temp_dir().join("pingling.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();

    env_logger::Builder::new()
        .filter_module("pingling", log::LevelFilter::Debug)
        .filter_module("app", log::LevelFilter::Debug)
        .format_target(false)
        .format_timestamp(None)
        .init();

    if let Some(file) = log_file {
        eprintln!("pingling logs -> {}", log_path.display());
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            unsafe { libc::dup2(file.as_raw_fd(), 2) };
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        // Register the `pingle://` URL scheme so the OS routes
        // deep-links to this process. The actual handler is wired
        // below in the .setup() closure via `app.deep_link().on_open_url`.
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            let cfg = load_config();

            // -- Build registry + manager --
            let registry = build_registry(&cfg);
            let mut storage: Box<dyn domain::SettingsStorage> =
                Box::new(data::MemorySettingsStorage::new());

            // Set config path from config file / env
            let config_path = if cfg.core_config_path.is_empty() {
                if cfg.core_type == "mock" {
                    let default_path = std::env::temp_dir().join("pingling-mock.json");
                    let path_str = default_path.to_string_lossy().to_string();
                    let _ = std::fs::write(&default_path, "{}");
                    log::info!("mock core: using default config at {path_str}");
                    path_str
                } else {
                    String::new()
                }
            } else {
                cfg.core_config_path.clone()
            };

            if !config_path.is_empty() {
                if let Err(e) = storage.set_string("config_path", &config_path) {
                    log::error!("failed to set config_path: {e}");
                }
            }

            // -- Wire encrypted profile storage + install-id provider --
            //
            // The profile store is the new primary config source: when
            // an active profile is set, the connect handler loads and
            // decrypts it into a temp file. The legacy `config_path`
            // setting is used only as a fallback. On first launch the
            // store generates a 32-byte AES-GCM key in the OS keychain
            // and a UUID install-id that plugins read via the
            // `daemon.installId` IPC method.
            //
            // If keychain access fails (e.g. in some headless CI
            // environments), we log the error and proceed without
            // profile support — the daemon still works with the
            // legacy flow.
            let vpn_base = VpnManager::new(registry, storage);
            let vpn = match data::EncryptedProfileStore::default_path() {
                Ok(store) => {
                    log::info!("profile store: initialized at OS config dir");
                    let store = std::sync::Arc::new(store);
                    Arc::new(vpn_base.with_profile_storage(
                        store.clone() as std::sync::Arc<dyn domain::ProfileStorage>,
                        store as std::sync::Arc<dyn domain::InstallIdProvider>,
                    ))
                }
                Err(e) => {
                    log::warn!(
                        "profile store: failed to initialize ({e}); running without profile support"
                    );
                    Arc::new(vpn_base)
                }
            };

            // -- Install the wasm plugin, if one is on disk --
            //
            // Scans the configured plugins dir (or the platform default)
            // for the first `.wasm` file that exports `plugin_handle_ipc`,
            // wraps it as `Arc<dyn domain::Plugin>` via
            // `plugin_extism::plugin_adapter::PluginAdapter::load`, and
            // installs it on the manager via `vpn.set_plugin(adapter)`.
            // See docs/architecture-plugin.md for the wire contract.
            //
            // No plugin → daemon runs in headless mode. JSON-RPC method
            // names that aren't in the daemon's built-in dispatch table
            // (vpn.* / core.* / config.* / outbounds.* / daemon.*)
            // return MethodNotFound cleanly.
            match discover_plugin(&cfg) {
                Some(adapter) => {
                    vpn.set_plugin(adapter);
                }
                None => {
                    log::info!("plugin: no .wasm in plugins dir");
                }
            }

            // -- Start IPC server (UDS + TCP loopback + UDP discovery beacon) --
            // The handle exposes the broadcaster so other parts of the daemon
            // can publish push events. Failure here is non-fatal — the daemon
            // continues with tray-only operation.
            let ipc_broadcaster = match ipc::start(vpn.clone()) {
                Ok(handle) => {
                    log::info!(
                        "ipc: server up (uds={:?}, tcp={:?})",
                        handle.uds_path,
                        handle.tcp_addr
                    );
                    handle.broadcaster
                }
                Err(e) => {
                    log::error!("ipc: server failed to start: {e}");
                    Arc::new(ipc::EventBroadcaster::new())
                }
            };

            log::info!(
                "active core: {}",
                vpn.active_core_type().unwrap_or_else(|| "none".into())
            );
            for core in vpn.list_cores() {
                log::info!(
                    "  core: {} ({}) available={}",
                    core.core_type,
                    core.source,
                    core.available
                );
            }
            log::info!(
                "config_path: {}",
                vpn.get_setting("config_path")
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "<not set>".into())
            );

            app.manage(AppState {
                vpn: vpn.clone(),
                icon_disconnected: tauri::image::Image::from_bytes(include_bytes!(
                    "../icons/tray-disconnected.png"
                ))
                .expect("valid icon"),
                icon_connecting: tauri::image::Image::from_bytes(include_bytes!(
                    "../icons/tray-connecting.png"
                ))
                .expect("valid icon"),
                icon_connected: tauri::image::Image::from_bytes(include_bytes!(
                    "../icons/tray-connected.png"
                ))
                .expect("valid icon"),
                ipc_broadcaster: ipc_broadcaster.clone(),
            });

            // -- Register the `pingle://` URL scheme handler --
            //
            // tauri-plugin-deep-link delivers URLs here when the OS
            // routes a `pingle://...` click to this process. We forward
            // each URL to `ipc::deeplink::handle_deeplink` which parses,
            // resolves (via plugin or built-in), and applies the result.
            //
            // The handler runs on a Tauri worker thread; we pass a
            // cloned Arc<VpnManager> in via move capture so the closure
            // has no borrow lifetime issues.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let dl_vpn = vpn.clone();
                let dl_bc = ipc_broadcaster.clone();
                app.deep_link().on_open_url(move |event| {
                    let urls = event.urls();
                    log::info!("deeplink: received {} url(s) from OS", urls.len());
                    for url in urls {
                        let url_str = url.to_string();
                        log::info!("deeplink: handling {}", url_str);
                        match ipc::deeplink::handle_deeplink(&dl_vpn, &url_str) {
                            Ok(outcome) => {
                                log::info!("deeplink: {} ({})", outcome.kind, outcome.message);
                                // Push to IPC subscribers so GUI clients
                                // can react (e.g. show a toast).
                                if let Ok(payload) = serde_json::to_value(&outcome) {
                                    dl_bc.publish(ipc::protocol::Notification::new(
                                        "event.deeplinkHandled",
                                        payload,
                                    ));
                                }
                            }
                            Err(e) => {
                                log::error!("deeplink: handle failed: {e}");
                            }
                        }
                    }
                });

                // On Linux + Windows, register the scheme at runtime so
                // users who ran the binary directly (without the installer
                // doing the registry write) still get deep-links routed.
                // No-op on macOS where Info.plist handles registration.
                #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
                {
                    if let Err(e) = app.deep_link().register("pingle") {
                        log::warn!("deeplink: failed to register scheme: {e}");
                    }
                }
            }

            // -- Background tray refresh loop --
            // Detects when the VPN process exits unexpectedly (e.g. killed
            // externally) and refreshes the tray icon/menu. Also publishes a
            // `event.stateChanged` IPC notification so any subscribed external
            // client (Flutter UI, standalone TUI) reacts in real time.
            {
                let poll_app = app.handle().clone();
                let poll_vpn = vpn.clone();
                let poll_bc = ipc_broadcaster.clone();
                std::thread::spawn(move || {
                    let mut last_running = false;
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let running = poll_vpn.is_running();
                        if running != last_running {
                            last_running = running;
                            refresh_tray(&poll_app);
                            let core = poll_vpn.active_core_type().unwrap_or_default();
                            let state = poll_vpn.get_status().to_string();
                            poll_bc.publish_state(&state, running, &core);
                        }
                    }
                });
            }

            // -- System tray --
            let tray_state = app.state::<AppState>();
            let menu = build_tray_menu(app.handle(), &vpn)?;
            let tooltip = format!(
                "{status} ({core})",
                status = vpn.get_status(),
                core = vpn.active_core_type().unwrap_or_else(|| "none".into())
            );

            let _tray = TrayIconBuilder::with_id("main")
                .icon(tray_state.icon_disconnected.clone())
                .menu(&menu)
                .tooltip(&tooltip)
                .on_menu_event(|app, event| {
                    let id = event.id().as_ref();

                    // -- Core selection --
                    if let Some(core_type) = id.strip_prefix("core:") {
                        if core_type == "none" {
                            return;
                        }
                        let state = app.state::<AppState>();
                        match state.vpn.switch_core(core_type) {
                            Ok(()) => {
                                log::info!("Switched core to: {core_type}");
                                refresh_tray(app);
                            }
                            Err(e) => log::error!("Core switch failed: {e}"),
                        }
                        return;
                    }

                    let state = app.state::<AppState>();
                    match id {
                        // -- Config file picker --
                        "pick_config" => {
                            let app_handle = app.clone();
                            tauri::async_runtime::spawn(async move {
                                use tauri_plugin_dialog::DialogExt;
                                let path = app_handle
                                    .dialog()
                                    .file()
                                    .add_filter("Config", &["json", "yaml", "yml"])
                                    .set_title("Select core config file")
                                    .blocking_pick_file();

                                if let Some(file_path) = path {
                                    let path_str = file_path.to_string();
                                    log::info!("Selected config: {path_str}");

                                    let state = app_handle.state::<AppState>();
                                    match state.vpn.set_setting("config_path", &path_str) {
                                        Ok(()) => {
                                            log::info!("Config path set to: {path_str}");
                                            refresh_tray(&app_handle);
                                        }
                                        Err(e) => log::error!("Failed to set config: {e}"),
                                    }
                                }
                            });
                        }
                        // -- Connection controls --
                        "connect" => {
                            let core = state.vpn.active_core_type().unwrap_or_else(|| "?".into());
                            let cfg_path = state
                                .vpn
                                .get_setting("config_path")
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| "<none>".into());
                            log::info!("Connect: core={core} config={cfg_path}");
                            refresh_tray(app);
                            match state.vpn.connect() {
                                Ok(()) => log::info!("Connected ({core})"),
                                Err(e) => log::error!("Connect failed: {e}"),
                            }
                            refresh_tray(app);
                            push_state_event(&state);
                        }
                        "disconnect" => match state.vpn.disconnect() {
                            Ok(()) => {
                                log::info!("Disconnected");
                                refresh_tray(app);
                                push_state_event(&state);
                            }
                            Err(e) => log::error!("Disconnect failed: {e}"),
                        },
                        "restart" => {
                            refresh_tray(app);
                            match state.vpn.restart() {
                                Ok(()) => {
                                    log::info!("Restarted");
                                    refresh_tray(app);
                                    push_state_event(&state);
                                }
                                Err(e) => log::error!("Restart failed: {e}"),
                            }
                        }
                        "show" => {
                            if let Some(win) = app.get_webview_window("main") {
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        }
                        "quit" => {
                            if let Err(e) = state.vpn.force_kill() {
                                log::warn!("Kill on quit: {e}");
                            }
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            // Auto-connect if configured
            if cfg.auto_connect {
                log::info!("auto-connect enabled, connecting...");
                if let Err(e) = vpn.connect() {
                    log::error!("auto-connect failed: {e}");
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            vpn_connect,
            vpn_disconnect,
            vpn_status,
            vpn_restart,
            core_list,
            core_active,
            core_switch,
            config_get_path,
            config_set_path,
            config_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ---------------------------------------------------------------------------
// IPC commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn vpn_connect(state: tauri::State<AppState>) -> Result<String, String> {
    state.vpn.connect().map_err(|e| e.to_string())?;
    Ok(state.vpn.get_status().to_string())
}

#[tauri::command]
fn vpn_disconnect(state: tauri::State<AppState>) -> Result<String, String> {
    state.vpn.disconnect().map_err(|e| e.to_string())?;
    Ok(state.vpn.get_status().to_string())
}

#[tauri::command]
fn vpn_status(state: tauri::State<AppState>) -> String {
    state.vpn.get_status().to_string()
}

#[tauri::command]
fn vpn_restart(state: tauri::State<AppState>) -> Result<String, String> {
    state.vpn.restart().map_err(|e| e.to_string())?;
    Ok(state.vpn.get_status().to_string())
}

#[tauri::command]
fn core_list(state: tauri::State<AppState>) -> Vec<CoreDescriptorDto> {
    state.vpn.list_cores().iter().map(|d| d.into()).collect()
}

#[tauri::command]
fn core_active(state: tauri::State<AppState>) -> Option<String> {
    state.vpn.active_core_type()
}

#[tauri::command]
fn core_switch(state: tauri::State<AppState>, core_type: String) -> Result<(), String> {
    state.vpn.switch_core(&core_type).map_err(|e| e.to_string())
}

#[tauri::command]
fn config_get_path(state: tauri::State<AppState>) -> Result<Option<String>, String> {
    state
        .vpn
        .get_setting("config_path")
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn config_set_path(state: tauri::State<AppState>, path: String) -> Result<(), String> {
    state
        .vpn
        .set_setting("config_path", &path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn config_info(state: tauri::State<AppState>) -> serde_json::Value {
    let config_path = state
        .vpn
        .get_setting("config_path")
        .ok()
        .flatten()
        .unwrap_or_default();
    serde_json::json!({
        "core_type": state.vpn.active_core_type().unwrap_or_default(),
        "config_path": config_path,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `PinglingConfig` whose `plugins.plugins_dir` points at the
    /// supplied directory and nothing else differs from the default.
    fn cfg_with_plugins_dir(dir: &std::path::Path) -> PinglingConfig {
        let mut cfg = PinglingConfig::default();
        cfg.plugins.plugins_dir = dir.to_string_lossy().into_owned();
        cfg
    }

    #[test]
    fn resolve_plugins_dir_uses_explicit_when_present() {
        let cfg = cfg_with_plugins_dir(std::path::Path::new("/tmp/explicit/pingle/plugins"));
        let resolved = resolve_plugins_dir(&cfg).expect("explicit dir always resolves");
        assert_eq!(
            resolved,
            std::path::PathBuf::from("/tmp/explicit/pingle/plugins")
        );
    }

    #[test]
    fn resolve_plugins_dir_falls_back_to_platform_default() {
        // Empty plugins_dir → platform default. The test only asserts the
        // returned path *ends with* the canonical suffix so it works on
        // both macOS and Linux without forking on cfg flags here.
        let cfg = PinglingConfig::default();
        let resolved = resolve_plugins_dir(&cfg).expect("HOME is set in test env");
        let s = resolved.to_string_lossy();
        assert!(
            s.ends_with("pingle/plugins"),
            "platform default should land at .../pingle/plugins, got {s}"
        );
    }

    #[test]
    fn discover_plugin_returns_none_when_dir_missing() {
        let cfg = cfg_with_plugins_dir(std::path::Path::new(
            "/nonexistent/path/that/should/never/exist/pingle",
        ));
        let plugin = discover_plugin(&cfg);
        assert!(plugin.is_none(), "missing dir must yield None, not panic");
    }

    #[test]
    fn discover_plugin_returns_none_for_empty_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = cfg_with_plugins_dir(tmp.path());
        let plugin = discover_plugin(&cfg);
        assert!(plugin.is_none(), "empty dir → no plugin installed");
    }

    #[test]
    fn discover_plugin_skips_garbage_wasm_files() {
        // Drop a file that *looks* like a wasm plugin (correct extension)
        // but contains garbage. The discovery loop must catch the
        // extism load error, log a warning, and return None — NOT panic.
        // This is the failure mode that matters most in prod: a half-
        // downloaded plugin shouldn't kill the daemon.
        let tmp = tempfile::tempdir().expect("tempdir");
        let bogus = tmp.path().join("not-a-real-plugin.wasm");
        std::fs::write(&bogus, b"not actually a wasm module").expect("write bogus wasm");
        let cfg = cfg_with_plugins_dir(tmp.path());
        let plugin = discover_plugin(&cfg);
        assert!(plugin.is_none(), "garbage .wasm must not install a plugin");
    }

    #[test]
    fn discover_plugin_honours_enabled_filter() {
        // When `plugins.enabled` is non-empty, files NOT in the list
        // are skipped before we even try to load them. We don't have
        // a real wasm module here, so all we assert is that the bogus
        // file gets ignored when its name is not in the filter — i.e.
        // discovery returns None without ever surfacing a load error.
        let tmp = tempfile::tempdir().expect("tempdir");
        let bogus = tmp.path().join("forbidden.wasm");
        std::fs::write(&bogus, b"not actually a wasm module").expect("write bogus wasm");
        let mut cfg = cfg_with_plugins_dir(tmp.path());
        cfg.plugins.enabled = vec!["only-this-one.wasm".to_string()];
        let plugin = discover_plugin(&cfg);
        assert!(plugin.is_none(), "filter excludes the only file → None");
    }
}
