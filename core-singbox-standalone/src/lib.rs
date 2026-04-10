//! Standalone [`VpnCore`] implementation for sing-box.
//!
//! Wraps the sing-box binary via `std::process::Command` — no Tauri dependency.
//! This single implementation is used by all consumers:
//!
//! - `app` (Tauri daemon): resolves the bundled sidecar path via Tauri's
//!   `externalBin` mechanism, then passes it to [`SingboxStandalone::new`]
//! - `cli`: resolves the binary from PATH or `--binary` flag
//! - Tests: uses `/bin/sleep` or `/bin/echo` as a stand-in binary
//!
//! # Process lifecycle
//! `start()` spawns the sing-box child process and launches three background threads:
//! - **stdout reader** — forwards log lines as [`CoreEvent::Log`]
//! - **stderr reader** — forwards log lines as [`CoreEvent::ErrorLog`]
//! - **reaper** — polls `child.try_wait()` every 500 ms; on unexpected exit,
//!   sets state to [`ConnectionState::Disconnected`] and emits [`CoreEvent::Crashed`]
//!
//! The reaper ensures the Tauri daemon and Flutter clients learn about external
//! process deaths (e.g. `kill -9`) within 500 ms, triggering tray refresh and
//! a `event.coreCrashed` JSON-RPC push to all connected Flutter clients.

use domain::{ConnectionState, CoreEvent, CoreInfo, PrerequisiteCheck, VpnCore, VpnError};
use log::{info, warn};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

/// Standalone sing-box core managed via `std::process::Command`.
///
/// Spawns the sing-box binary as a child process, captures stdout/stderr
/// via background threads, and tracks connection state.
pub struct SingboxStandalone {
    binary_path: String,
    state: Arc<Mutex<ConnectionState>>,
    child: Arc<Mutex<Option<Child>>>,
    event_tx: Arc<Mutex<mpsc::Sender<CoreEvent>>>,
    event_rx: Mutex<Option<mpsc::Receiver<CoreEvent>>>,
}

impl SingboxStandalone {
    /// Create a new standalone core with the given binary path.
    ///
    /// If `binary_path` is empty, will attempt to find `sing-box` in PATH.
    pub fn new(binary_path: &str) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            binary_path: if binary_path.is_empty() {
                "sing-box".into()
            } else {
                binary_path.into()
            },
            state: Arc::new(Mutex::new(ConnectionState::Disconnected)),
            child: Arc::new(Mutex::new(None)),
            event_tx: Arc::new(Mutex::new(tx)),
            event_rx: Mutex::new(Some(rx)),
        }
    }

    /// Resolve the binary to an absolute path, or use as-is if not found.
    fn resolve_binary(&self) -> Result<String, VpnError> {
        let path = &self.binary_path;
        if path.contains('/') {
            // Already an absolute or relative path
            if std::path::Path::new(path).exists() {
                Ok(path.clone())
            } else {
                Err(VpnError::PrerequisiteMissing(format!(
                    "binary not found: {path}"
                )))
            }
        } else {
            // Search PATH via shared utility
            util::which(path)
                .ok_or_else(|| VpnError::PrerequisiteMissing(format!("{path} not found in PATH")))
        }
    }

    /// Spawn the process and set up background I/O threads.
    fn spawn_process(&self, args: &[&str]) -> Result<Child, VpnError> {
        let binary = self.resolve_binary()?;

        let mut child = Command::new(&binary)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VpnError::ProcessStartFailed(format!("{binary}: {e}")))?;

        // Capture stdout
        if let Some(stdout) = child.stdout.take() {
            let tx = self.event_tx.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    info!("[sing-box] {line}");
                    let _ = tx
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .send(CoreEvent::Log(line));
                }
                // State is managed by stop()/kill() or the reaper thread.
            });
        }

        // Capture stderr
        if let Some(stderr) = child.stderr.take() {
            let tx = self.event_tx.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    warn!("[sing-box] {line}");
                    let _ = tx
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .send(CoreEvent::ErrorLog(line));
                }
            });
        }

        Ok(child)
    }
}

impl VpnCore for SingboxStandalone {
    fn start(&mut self, config_path: &str) -> Result<(), VpnError> {
        if config_path.is_empty() {
            return Err(VpnError::InvalidConfiguration(
                "config_path must not be empty".into(),
            ));
        }

        {
            let child = self.child.lock().unwrap_or_else(|e| e.into_inner());
            if child.is_some() {
                return Err(VpnError::AlreadyConnected);
            }
        }

        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = ConnectionState::Connecting;

        let child = self.spawn_process(&["run", "-c", config_path])?;

        *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = ConnectionState::Connected;

        // Reaper thread: detect unexpected process exit and clean up.
        let child_watch = self.child.clone();
        let state_watch = self.state.clone();
        let tx_watch = self.event_tx.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(std::time::Duration::from_millis(500));
                let mut guard = child_watch.lock().unwrap_or_else(|e| e.into_inner());
                match guard.as_mut() {
                    None => break, // stop()/kill() already cleaned up
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => {
                            let code = status.code().unwrap_or(-1);
                            *guard = None;
                            drop(guard);
                            *state_watch.lock().unwrap_or_else(|e| e.into_inner()) =
                                ConnectionState::Disconnected;
                            let _ = tx_watch.lock().unwrap_or_else(|e| e.into_inner()).send(
                                CoreEvent::Crashed(format!("exited unexpectedly (code {code})")),
                            );
                            warn!("sing-box exited unexpectedly (code {code})");
                            break;
                        }
                        Ok(None) => {} // still running
                        Err(_) => break,
                    },
                }
            }
        });

        info!("sing-box started with config: {config_path}");
        Ok(())
    }

    fn stop(&mut self) -> Result<(), VpnError> {
        let mut child_guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let mut child = child_guard.take().ok_or(VpnError::NotConnected)?;

        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = ConnectionState::Disconnecting;

        child.kill().map_err(|e| {
            *self.state.lock().unwrap_or_else(|e| e.into_inner()) =
                ConnectionState::Error(e.to_string());
            VpnError::ProcessStopFailed(format!("{e}"))
        })?;

        let _ = child.wait();
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = ConnectionState::Disconnected;
        info!("sing-box stopped");
        Ok(())
    }

    fn kill(&mut self) -> Result<(), VpnError> {
        let mut child_guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let mut child = child_guard.take().ok_or(VpnError::NotConnected)?;

        child
            .kill()
            .map_err(|e| VpnError::ProcessKillFailed(format!("{e}")))?;

        let _ = child.wait();
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = ConnectionState::Disconnected;
        info!("sing-box killed");
        Ok(())
    }

    fn status(&self) -> ConnectionState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn info(&self) -> CoreInfo {
        CoreInfo {
            name: "sing-box".into(),
            version: "standalone".into(),
            supported_protocols: vec![
                "vmess".into(),
                "vless".into(),
                "trojan".into(),
                "shadowsocks".into(),
                "wireguard".into(),
                "hysteria".into(),
                "hysteria2".into(),
                "tuic".into(),
            ],
        }
    }

    fn validate_config(&self, config_path: &str) -> Result<(), VpnError> {
        if config_path.is_empty() {
            return Err(VpnError::InvalidConfiguration(
                "config_path must not be empty".into(),
            ));
        }

        if !std::path::Path::new(config_path).exists() {
            return Err(VpnError::InvalidConfiguration(format!(
                "config file not found: {config_path}"
            )));
        }

        let binary = self.resolve_binary()?;
        let output = Command::new(&binary)
            .args(["check", "-c", config_path])
            .output()
            .map_err(|e| VpnError::ValidationError(format!("{binary}: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VpnError::ValidationError(stderr.trim().to_string()));
        }

        Ok(())
    }

    fn check_prerequisites(&self) -> Vec<PrerequisiteCheck> {
        let binary_result = self.resolve_binary();
        let mut checks = vec![PrerequisiteCheck {
            name: "binary_exists".into(),
            passed: binary_result.is_ok(),
            message: match &binary_result {
                Ok(path) => format!("found at {path}"),
                Err(e) => format!("{e}"),
            },
        }];

        // Check if binary is executable
        if let Ok(path) = &binary_result {
            let executable = std::path::Path::new(path)
                .metadata()
                .map(|m| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        m.permissions().mode() & 0o111 != 0
                    }
                    #[cfg(not(unix))]
                    {
                        true
                    }
                })
                .unwrap_or(false);

            checks.push(PrerequisiteCheck {
                name: "binary_executable".into(),
                passed: executable,
                message: if executable {
                    "binary is executable".into()
                } else {
                    format!("{path} is not executable")
                },
            });
        }

        checks
    }

    fn subscribe(&self) -> Option<mpsc::Receiver<CoreEvent>> {
        self.event_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Per-core default strategy plan tuned for the standalone sing-box
    /// CLI subprocess. Two strategies (DoH → TCP) with a 60-second
    /// global cap. Lighter than the libbox plans because the standalone
    /// path is the fallback engine, not the primary one — anyone using
    /// it knows what they're doing and a heavy retry loop wastes time.
    fn default_strategy_plan(&self) -> Option<Vec<u8>> {
        Some(default_singbox_standalone_strategy_plan_json())
    }
}

/// Build the default strategy plan for the standalone sing-box CLI
/// core and serialize to JSON bytes.
pub(crate) fn default_singbox_standalone_strategy_plan_json() -> Vec<u8> {
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
                id: "fallback-tcp".into(),
                stack: StackType::System,
                resolver_type: ResolverType::Tcp,
                total_timeout: Duration::from_secs(25),
                retry: RetryPolicy::Fixed {
                    max_attempts: 2,
                    delay: Duration::from_secs(3),
                },
            },
        ],
        global_timeout: Some(Duration::from_secs(60)),
    };
    serde_json::to_vec(&plan).expect("default singbox-standalone strategy plan must serialize")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- basic construction ---------------------------------------------------

    #[test]
    fn new_with_path() {
        let core = SingboxStandalone::new("/usr/bin/sing-box");
        assert_eq!(core.status(), ConnectionState::Disconnected);
    }

    #[test]
    fn default_strategy_plan_round_trips_through_pingle_config_pipeline() {
        use pingle_config_pipeline::strategy::{ResolverType, StackType, StrategyPlan};

        let core = SingboxStandalone::new("/usr/bin/sing-box");
        let bytes = core.default_strategy_plan().expect("plan present");
        let plan: StrategyPlan = serde_json::from_slice(&bytes).expect("valid json");
        assert_eq!(plan.strategies.len(), 2);
        assert_eq!(plan.strategies[0].id, "default-doh");
        assert_eq!(plan.strategies[0].stack, StackType::System);
        assert_eq!(plan.strategies[0].resolver_type, ResolverType::Doh);
        assert_eq!(plan.strategies[1].id, "fallback-tcp");
        assert_eq!(plan.global_timeout.unwrap().as_secs(), 60);
    }

    #[test]
    fn new_empty_path_uses_default() {
        let core = SingboxStandalone::new("");
        assert_eq!(core.status(), ConnectionState::Disconnected);
    }

    #[test]
    fn info_returns_metadata() {
        let core = SingboxStandalone::new("sing-box");
        let info = core.info();
        assert_eq!(info.name, "sing-box");
        assert_eq!(info.version, "standalone");
        assert!(info.supported_protocols.contains(&"vmess".into()));
        assert!(info.supported_protocols.contains(&"wireguard".into()));
    }

    // -- start errors ---------------------------------------------------------

    #[test]
    fn start_empty_config_path() {
        let mut core = SingboxStandalone::new("/bin/echo");
        let result = core.start("");
        assert!(matches!(result, Err(VpnError::InvalidConfiguration(_))));
    }

    #[test]
    fn start_nonexistent_binary() {
        let mut core = SingboxStandalone::new("/nonexistent/sing-box");
        let result = core.start("/tmp/config.json");
        assert!(matches!(result, Err(VpnError::PrerequisiteMissing(_))));
    }

    #[cfg(unix)]
    #[test]
    fn start_already_connected() {
        let mut core = SingboxStandalone::new("/bin/sleep");
        core.start("10").unwrap();
        let result = core.start("10");
        assert!(matches!(result, Err(VpnError::AlreadyConnected)));
        let _ = core.kill();
    }

    // -- stop errors ----------------------------------------------------------

    #[test]
    fn stop_not_connected() {
        let mut core = SingboxStandalone::new("/bin/echo");
        let result = core.stop();
        assert!(matches!(result, Err(VpnError::NotConnected)));
    }

    // -- kill errors ----------------------------------------------------------

    #[test]
    fn kill_not_connected() {
        let mut core = SingboxStandalone::new("/bin/echo");
        let result = core.kill();
        assert!(matches!(result, Err(VpnError::NotConnected)));
    }

    // -- lifecycle with real processes ----------------------------------------

    #[cfg(unix)]
    #[test]
    fn start_and_stop_with_sleep() {
        let mut core = SingboxStandalone::new("/bin/sleep");
        core.start("60").unwrap();
        assert_eq!(core.status(), ConnectionState::Connected);

        core.stop().unwrap();
        assert_eq!(core.status(), ConnectionState::Disconnected);
    }

    #[cfg(unix)]
    #[test]
    fn start_and_kill_with_sleep() {
        let mut core = SingboxStandalone::new("/bin/sleep");
        core.start("60").unwrap();
        assert_eq!(core.status(), ConnectionState::Connected);

        core.kill().unwrap();
        assert_eq!(core.status(), ConnectionState::Disconnected);
    }

    #[cfg(unix)]
    #[test]
    fn restart_lifecycle() {
        let mut core = SingboxStandalone::new("/bin/sleep");
        core.start("60").unwrap();
        assert_eq!(core.status(), ConnectionState::Connected);

        core.restart("30").unwrap();
        assert_eq!(core.status(), ConnectionState::Connected);

        core.stop().unwrap();
        assert_eq!(core.status(), ConnectionState::Disconnected);
    }

    // -- validate_config ------------------------------------------------------

    #[test]
    fn validate_empty_path() {
        let core = SingboxStandalone::new("/bin/echo");
        let result = core.validate_config("");
        assert!(matches!(result, Err(VpnError::InvalidConfiguration(_))));
    }

    #[test]
    fn validate_missing_file() {
        let core = SingboxStandalone::new("/bin/echo");
        let result = core.validate_config("/nonexistent/config.json");
        assert!(matches!(result, Err(VpnError::InvalidConfiguration(_))));
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[cfg(unix)]
    #[test]
    fn validate_existing_file_with_echo() {
        // /bin/echo always exits 0 — simulates a passing validation
        let core = SingboxStandalone::new("/bin/echo");
        // Use /bin/echo itself as a file that exists
        let result = core.validate_config("/bin/echo");
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn validate_failing_binary() {
        // A binary that always exits 1 — simulates a failing validation.
        // On macOS `false` lives at /usr/bin/false; on Linux at /bin/false.
        #[cfg(target_os = "macos")]
        let false_bin = "/usr/bin/false";
        #[cfg(not(target_os = "macos"))]
        let false_bin = "/bin/false";

        // Use the binary itself as the "config file" (it exists on disk).
        let core = SingboxStandalone::new(false_bin);
        let result = core.validate_config(false_bin);
        assert!(matches!(result, Err(VpnError::ValidationError(_))));
    }

    // -- check_prerequisites --------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn prereqs_binary_in_path() {
        // /bin/echo should be findable
        let core = SingboxStandalone::new("echo");
        let checks = core.check_prerequisites();
        assert!(checks.iter().any(|c| c.name == "binary_exists" && c.passed));
    }

    #[test]
    fn prereqs_binary_missing() {
        let core = SingboxStandalone::new("/nonexistent/sing-box");
        let checks = core.check_prerequisites();
        let binary_check = checks.iter().find(|c| c.name == "binary_exists").unwrap();
        assert!(!binary_check.passed);
    }

    #[cfg(unix)]
    #[test]
    fn prereqs_absolute_path_exists() {
        let core = SingboxStandalone::new("/bin/echo");
        let checks = core.check_prerequisites();
        assert!(checks.iter().any(|c| c.name == "binary_exists" && c.passed));
    }

    #[test]
    fn prereqs_absolute_path_missing() {
        let core = SingboxStandalone::new("/does/not/exist");
        let checks = core.check_prerequisites();
        assert!(!checks.iter().any(|c| c.name == "binary_exists" && c.passed));
    }

    // -- subscribe ------------------------------------------------------------

    #[test]
    fn subscribe_returns_receiver_once() {
        let core = SingboxStandalone::new("/bin/echo");
        assert!(core.subscribe().is_some());
        assert!(core.subscribe().is_none()); // second call returns None
    }

    #[cfg(unix)]
    #[test]
    fn subscribe_receives_log_events() {
        let mut core = SingboxStandalone::new("/bin/echo");
        let rx = core.subscribe().unwrap();

        core.start("hello world").unwrap();

        let event = rx.recv_timeout(std::time::Duration::from_secs(2));
        assert!(matches!(event, Ok(CoreEvent::Log(s)) if s.contains("hello")));

        core.stop().unwrap();
    }
}
