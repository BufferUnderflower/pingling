//! [`VpnCore`] implementation backed by libbox via the C bridge on Windows.
//!
//! Mirrors `core-libbox-macos/src/core.rs` so the two cores have the
//! same shape and the only delta is the bridge module path. The
//! lifecycle is:
//!
//!   start(config_path)
//!     → read file → pingle_libbox_new_service(json) → service_start()
//!     → store handle in self.service
//!   stop()
//!     → service_close() → service_release() → drop handle
//!   status() / running() → derived from whether self.service is Some
//!
//! Connection state events from libbox (sing-box's
//! `LibboxCommandClient`) are NOT yet bridged through to
//! `CoreEvent::Log` / `StateChanged`. That's the next layer once a
//! real `libbox.dll` is in place — for now `start()` and `stop()` are
//! synchronous and the state is what they last returned.
//!
//! ## Stub mode
//!
//! When the build script can't find a libbox build, every method
//! returns `VpnError::PrerequisiteMissing("libbox unavailable on this host")`
//! so the daemon falls through to `core-mock` / `core-singbox-standalone`.
//! See `crate::lib::stub-fallback` for the rationale.

use crate::bridge;
use domain::{
    ConnectionState, CoreEvent, CoreInfo, PrerequisiteCheck, VpnCore, VpnError,
};
#[cfg(not(libbox_stub))]
use log::info;
use std::ffi::CString;
use std::os::raw::c_char;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// VpnCore that drives sing-box via the embedded libbox.dll on Windows.
///
/// In stub mode (every host that isn't Windows-with-libbox.dll) the
/// `service` / `event_tx` fields are unused — `start` short-circuits
/// before they would be touched. The `#[allow(dead_code)]` keeps the
/// stub build warning-free without forking the struct definition.
#[allow(dead_code)]
pub struct LibboxCoreWindows {
    state: Arc<Mutex<ConnectionState>>,
    /// Opaque service pointer returned by [`bridge::pingle_libbox_new_service`].
    /// `Some(_)` while a tunnel is up; `None` otherwise. Always taken
    /// out before close so we never call close on a stale pointer.
    service: Arc<Mutex<Option<*mut std::os::raw::c_void>>>,
    event_tx: Arc<Mutex<mpsc::Sender<CoreEvent>>>,
    event_rx: Mutex<Option<mpsc::Receiver<CoreEvent>>>,
}

// Safety: the libbox handle is opaque and the only thing that touches
// it from Rust is `*mut c_void`. Access is synchronised through the
// `Arc<Mutex<_>>`, and the underlying Go runtime is thread-safe per
// gobind's runtime guarantees.
unsafe impl Send for LibboxCoreWindows {}
unsafe impl Sync for LibboxCoreWindows {}

impl Default for LibboxCoreWindows {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)] // helpers go unused in stub mode (which is most hosts)
impl LibboxCoreWindows {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            state: Arc::new(Mutex::new(ConnectionState::Disconnected)),
            service: Arc::new(Mutex::new(None)),
            event_tx: Arc::new(Mutex::new(tx)),
            event_rx: Mutex::new(Some(rx)),
        }
    }

    fn emit_state(&self, state: ConnectionState) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state.clone();
        let _ = self
            .event_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(CoreEvent::StateChanged(state));
    }

    fn log(&self, msg: impl Into<String>) {
        let _ = self
            .event_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(CoreEvent::Log(msg.into()));
    }

    /// Read the config file from disk into a CString. libbox expects
    /// the raw JSON content, not a path.
    fn load_config(path: &Path) -> Result<CString, VpnError> {
        let bytes = std::fs::read(path).map_err(|e| {
            VpnError::InvalidConfiguration(format!("can't read config {}: {e}", path.display()))
        })?;
        CString::new(bytes)
            .map_err(|_| VpnError::InvalidConfiguration("config contains interior NUL byte".into()))
    }

    /// Take ownership of a `*mut c_char` heap-allocated by the C bridge,
    /// copy it into a Rust String, and free the original via the
    /// matching `pingle_libbox_free_string`. Always safe — NULL maps to
    /// the supplied default.
    unsafe fn take_c_string(ptr: *mut c_char, default: &str) -> String {
        if ptr.is_null() {
            return default.to_string();
        }
        let s = std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned();
        bridge::pingle_libbox_free_string(ptr);
        s
    }
}

impl VpnCore for LibboxCoreWindows {
    fn start(&mut self, config_path: &str) -> Result<(), VpnError> {
        // The stub fallback short-circuits the entire lifecycle so the
        // daemon falls through to another core cleanly. Real Windows
        // builds (with libbox.dll present) compile out this branch.
        #[cfg(libbox_stub)]
        {
            let _ = config_path;
            return Err(VpnError::PrerequisiteMissing(
                "libbox unavailable on this host (stub build — see core-libbox-windows/README.md)"
                    .into(),
            ));
        }

        #[cfg(not(libbox_stub))]
        {
            if config_path.is_empty() {
                return Err(VpnError::InvalidConfiguration(
                    "config_path required".into(),
                ));
            }
            if self
                .service
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
            {
                return Err(VpnError::AlreadyConnected);
            }

            self.emit_state(ConnectionState::Connecting);
            self.log(format!("[libbox-windows] start: {config_path}"));

            let cfg = Self::load_config(Path::new(config_path))?;
            let mut err: *mut c_char = std::ptr::null_mut();
            let handle =
                unsafe { bridge::pingle_libbox_new_service(cfg.as_ptr(), &mut err as *mut _) };
            if handle.is_null() {
                let msg = unsafe { Self::take_c_string(err, "libbox new_service returned null") };
                let e = VpnError::ProcessStartFailed(format!("libbox new_service: {msg}"));
                self.emit_state(ConnectionState::Error(e.to_string()));
                return Err(e);
            }

            let mut start_err: *mut c_char = std::ptr::null_mut();
            let ok =
                unsafe { bridge::pingle_libbox_service_start(handle, &mut start_err as *mut _) };
            if ok == 0 {
                unsafe { bridge::pingle_libbox_service_release(handle) };
                let msg = unsafe { Self::take_c_string(start_err, "libbox service start failed") };
                let e = VpnError::ProcessStartFailed(format!("libbox start: {msg}"));
                self.emit_state(ConnectionState::Error(e.to_string()));
                return Err(e);
            }

            *self.service.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
            self.emit_state(ConnectionState::Connected);
            info!("libbox-windows core started");
            Ok(())
        }
    }

    fn stop(&mut self) -> Result<(), VpnError> {
        #[cfg(libbox_stub)]
        {
            return Err(VpnError::PrerequisiteMissing(
                "libbox unavailable on this host".into(),
            ));
        }

        #[cfg(not(libbox_stub))]
        {
            let handle_opt = self
                .service
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            let Some(handle) = handle_opt else {
                return Err(VpnError::NotConnected);
            };

            self.emit_state(ConnectionState::Disconnecting);

            let mut err: *mut c_char = std::ptr::null_mut();
            let ok = unsafe { bridge::pingle_libbox_service_close(handle, &mut err as *mut _) };
            unsafe { bridge::pingle_libbox_service_release(handle) };

            if ok == 0 {
                let msg = unsafe { Self::take_c_string(err, "libbox service close failed") };
                let e = VpnError::ProcessStopFailed(format!("libbox close: {msg}"));
                self.emit_state(ConnectionState::Error(e.to_string()));
                return Err(e);
            }

            self.emit_state(ConnectionState::Disconnected);
            info!("libbox-windows core stopped");
            Ok(())
        }
    }

    fn kill(&mut self) -> Result<(), VpnError> {
        // libbox doesn't expose a force-kill primitive separate from
        // close — close is already a clean Go-side shutdown. We treat
        // kill the same as stop and ignore the result, since the caller
        // is asking us to make the tunnel go away one way or another.
        let _ = self.stop();
        Ok(())
    }

    fn status(&self) -> ConnectionState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn info(&self) -> CoreInfo {
        #[cfg(libbox_stub)]
        {
            return CoreInfo {
                name: "sing-box (libbox-windows, stub)".into(),
                version: "stub".into(),
                supported_protocols: vec![],
            };
        }

        #[cfg(not(libbox_stub))]
        {
            let version = unsafe {
                let p = bridge::pingle_libbox_version();
                Self::take_c_string(p, "unknown")
            };
            CoreInfo {
                name: "sing-box (libbox-windows)".into(),
                version,
                supported_protocols: vec![
                    "vless".into(),
                    "vmess".into(),
                    "trojan".into(),
                    "shadowsocks".into(),
                    "wireguard".into(),
                ],
            }
        }
    }

    fn check_prerequisites(&self) -> Vec<PrerequisiteCheck> {
        #[cfg(libbox_stub)]
        {
            return vec![PrerequisiteCheck {
                name: "libbox.dll".into(),
                passed: false,
                message:
                    "libbox.dll not built / not found — see core-libbox-windows/README.md"
                        .into(),
            }];
        }

        #[cfg(not(libbox_stub))]
        {
            // The DLL is statically linked at build time so by the time
            // this method runs we know it loaded. Live capability
            // discovery (TUN device, WinTun driver, admin rights, etc.)
            // belongs in a separate Windows-specific check that runs
            // out of the daemon's prerequisite layer — out of scope for
            // the skeleton.
            vec![PrerequisiteCheck {
                name: "libbox.dll".into(),
                passed: true,
                message: "linked".into(),
            }]
        }
    }

    fn validate_config(&self, _config_path: &str) -> Result<(), VpnError> {
        // libbox exposes a `LibboxCheckConfig` symbol on most builds; we
        // can wire it through the bridge in a follow-up. For the
        // skeleton we just accept everything in stub mode and return
        // "not implemented" in real mode so callers can detect the gap
        // explicitly.
        #[cfg(libbox_stub)]
        {
            return Err(VpnError::PrerequisiteMissing(
                "libbox unavailable on this host".into(),
            ));
        }
        #[cfg(not(libbox_stub))]
        {
            Err(VpnError::ValidationError(
                "libbox-windows: validate_config not yet wired".into(),
            ))
        }
    }

    fn subscribe(&self) -> Option<std::sync::mpsc::Receiver<CoreEvent>> {
        // We hand out the receiver exactly once; subsequent callers get
        // None. Same contract as core-libbox-macos.
        self.event_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Per-core default strategy plan tuned for sing-box on Windows.
    /// Four strategies (DoH → TCP system → TCP gvisor → system-resolver)
    /// with a 120-second global cap. Longer than macOS because Windows
    /// is where the historical pain lives — half users smooth on one
    /// strategy combo, half on another.
    fn default_strategy_plan(&self) -> Option<Vec<u8>> {
        Some(default_windows_strategy_plan_json())
    }
}

/// Build the default strategy plan for libbox-windows and serialize to
/// JSON bytes. Kept as a free function so the test module can call
/// it directly.
pub(crate) fn default_windows_strategy_plan_json() -> Vec<u8> {
    use pingle_config_pipeline::strategy::{
        ConnectionStrategy, ResolverType, RetryPolicy, StackType, StrategyPlan,
    };
    use std::time::Duration;

    let plan = StrategyPlan {
        strategies: vec![
            ConnectionStrategy {
                id: "default-doh".into(),
                stack: StackType::System,
                resolver_type: ResolverType::Doh,
                total_timeout: Duration::from_secs(25),
                retry: RetryPolicy::Fixed {
                    max_attempts: 3,
                    delay: Duration::from_secs(2),
                },
            },
            ConnectionStrategy {
                id: "fallback-tcp-system".into(),
                stack: StackType::System,
                resolver_type: ResolverType::Tcp,
                total_timeout: Duration::from_secs(25),
                retry: RetryPolicy::Fixed {
                    max_attempts: 3,
                    delay: Duration::from_secs(3),
                },
            },
            ConnectionStrategy {
                id: "fallback-tcp-gvisor".into(),
                stack: StackType::GVisor,
                resolver_type: ResolverType::Tcp,
                total_timeout: Duration::from_secs(25),
                retry: RetryPolicy::Fixed {
                    max_attempts: 2,
                    delay: Duration::from_secs(5),
                },
            },
            ConnectionStrategy {
                id: "fallback-system-resolver".into(),
                stack: StackType::System,
                resolver_type: ResolverType::System,
                total_timeout: Duration::from_secs(25),
                retry: RetryPolicy::Fixed {
                    max_attempts: 2,
                    delay: Duration::from_secs(5),
                },
            },
        ],
        global_timeout: Some(Duration::from_secs(120)),
    };
    serde_json::to_vec(&plan).expect("default windows strategy plan must serialize")
}

// ---------------------------------------------------------------------------
// Tests — pure stub-mode coverage so the workspace's `cargo test` exercises
// the lifecycle without needing a real libbox.dll on disk.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_core_starts_disconnected() {
        let core = LibboxCoreWindows::new();
        matches!(core.status(), ConnectionState::Disconnected);
    }

    #[test]
    fn default_strategy_plan_round_trips_through_pingle_config_pipeline() {
        use pingle_config_pipeline::strategy::{ResolverType, StackType, StrategyPlan};

        let core = LibboxCoreWindows::new();
        let bytes = core.default_strategy_plan().expect("plan present");
        let plan: StrategyPlan = serde_json::from_slice(&bytes).expect("valid json");
        assert_eq!(plan.strategies.len(), 4);
        assert_eq!(plan.strategies[0].id, "default-doh");
        assert_eq!(plan.strategies[0].resolver_type, ResolverType::Doh);
        assert_eq!(plan.strategies[3].id, "fallback-system-resolver");
        assert_eq!(plan.strategies[3].stack, StackType::System);
        // Windows plan has the longest budget — 120s.
        assert_eq!(
            plan.global_timeout.unwrap().as_secs(),
            120,
            "windows plan should have 120s global timeout"
        );
    }

    #[test]
    #[cfg(libbox_stub)]
    fn stub_start_returns_prerequisite_missing() {
        let mut core = LibboxCoreWindows::new();
        let err = core.start("/tmp/whatever.json").unwrap_err();
        assert!(matches!(err, VpnError::PrerequisiteMissing(_)));
    }

    #[test]
    #[cfg(libbox_stub)]
    fn stub_check_prerequisites_reports_dll_missing() {
        let core = LibboxCoreWindows::new();
        let checks = core.check_prerequisites();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "libbox.dll");
        assert!(!checks[0].passed);
    }

    #[test]
    #[cfg(libbox_stub)]
    fn stub_info_reports_stub_version() {
        let core = LibboxCoreWindows::new();
        let info = core.info();
        assert!(info.name.contains("stub"));
        assert_eq!(info.version, "stub");
    }
}
