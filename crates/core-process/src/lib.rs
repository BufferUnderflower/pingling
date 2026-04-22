use log::{info, warn};
use pingling_domain::{ConnectionState, CoreEvent, CoreInfo, PrerequisiteCheck, VpnCore, VpnError};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopMode {
    Kill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCoreSpec {
    pub id: String,
    pub display_name: String,
    pub version: String,
    pub binary: String,
    pub start_args: Vec<String>,
    pub validate_args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub supported_protocols: Vec<String>,
    pub stop_mode: StopMode,
    pub reaper_interval: Duration,
}

impl ProcessCoreSpec {
    pub fn new(id: impl Into<String>, binary: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            display_name: id.clone(),
            version: "process".to_owned(),
            binary: binary.into(),
            start_args: vec!["{config}".to_owned()],
            validate_args: Vec::new(),
            env: BTreeMap::new(),
            supported_protocols: Vec::new(),
            stop_mode: StopMode::Kill,
            reaper_interval: Duration::from_millis(500),
            id,
        }
    }

    pub fn with_display_name(mut self, value: impl Into<String>) -> Self {
        self.display_name = value.into();
        self
    }

    pub fn with_version(mut self, value: impl Into<String>) -> Self {
        self.version = value.into();
        self
    }

    pub fn with_start_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.start_args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_validate_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.validate_args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_supported_protocols(
        mut self,
        protocols: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.supported_protocols = protocols.into_iter().map(Into::into).collect();
        self
    }
}

pub struct ProcessCore {
    spec: ProcessCoreSpec,
    state: Arc<Mutex<ConnectionState>>,
    child: Arc<Mutex<Option<Child>>>,
    event_tx: Arc<Mutex<mpsc::Sender<CoreEvent>>>,
    event_rx: Mutex<Option<mpsc::Receiver<CoreEvent>>>,
}

impl ProcessCore {
    pub fn new(spec: ProcessCoreSpec) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            spec,
            state: Arc::new(Mutex::new(ConnectionState::Disconnected)),
            child: Arc::new(Mutex::new(None)),
            event_tx: Arc::new(Mutex::new(tx)),
            event_rx: Mutex::new(Some(rx)),
        }
    }

    pub fn spec(&self) -> &ProcessCoreSpec {
        &self.spec
    }

    fn resolve_binary(&self) -> Result<String, VpnError> {
        let path = &self.spec.binary;
        if path.contains('/') || path.contains('\\') {
            if Path::new(path).exists() {
                Ok(path.clone())
            } else {
                Err(VpnError::PrerequisiteMissing(format!(
                    "binary not found: {path}"
                )))
            }
        } else {
            pingling_paths::which(path)
                .ok_or_else(|| VpnError::PrerequisiteMissing(format!("{path} not found in PATH")))
        }
    }

    fn expand_args(&self, args: &[String], config_path: &str) -> Vec<String> {
        args.iter()
            .map(|arg| arg.replace("{config}", config_path))
            .collect()
    }

    fn command(&self, binary: &str, args: &[String]) -> Command {
        let mut command = Command::new(binary);
        command.args(args);
        command.envs(&self.spec.env);
        command
    }

    fn spawn_process(&self, args: &[String]) -> Result<Child, VpnError> {
        let binary = self.resolve_binary()?;
        let mut child = self
            .command(&binary, args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VpnError::ProcessStartFailed(format!("{binary}: {e}")))?;

        if let Some(stdout) = child.stdout.take() {
            let tx = self.event_tx.clone();
            let name = self.spec.id.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    info!("[{name}] {line}");
                    let _ = tx
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .send(CoreEvent::Log(line));
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let tx = self.event_tx.clone();
            let name = self.spec.id.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    warn!("[{name}] {line}");
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

impl VpnCore for ProcessCore {
    fn start(&mut self, config_path: &str) -> Result<(), VpnError> {
        if config_path.is_empty() {
            return Err(VpnError::InvalidConfiguration(
                "config_path must not be empty".into(),
            ));
        }

        if self
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return Err(VpnError::AlreadyConnected);
        }

        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = ConnectionState::Connecting;
        let args = self.expand_args(&self.spec.start_args, config_path);
        let child = match self.spawn_process(&args) {
            Ok(child) => child,
            Err(error) => {
                *self.state.lock().unwrap_or_else(|e| e.into_inner()) =
                    ConnectionState::Disconnected;
                return Err(error);
            }
        };
        *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = ConnectionState::Connected;

        let child_watch = self.child.clone();
        let state_watch = self.state.clone();
        let tx_watch = self.event_tx.clone();
        let interval = self.spec.reaper_interval;
        thread::spawn(move || loop {
            thread::sleep(interval);
            let mut guard = child_watch.lock().unwrap_or_else(|e| e.into_inner());
            match guard.as_mut() {
                None => break,
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
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => break,
                },
            }
        });

        Ok(())
    }

    fn stop(&mut self) -> Result<(), VpnError> {
        let mut child_guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let mut child = child_guard.take().ok_or(VpnError::NotConnected)?;
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = ConnectionState::Disconnecting;

        match self.spec.stop_mode {
            StopMode::Kill => child
                .kill()
                .map_err(|e| VpnError::ProcessStopFailed(e.to_string()))?,
        }

        let _ = child.wait();
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = ConnectionState::Disconnected;
        Ok(())
    }

    fn kill(&mut self) -> Result<(), VpnError> {
        self.stop()
    }

    fn status(&self) -> ConnectionState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn info(&self) -> CoreInfo {
        CoreInfo {
            name: self.spec.display_name.clone(),
            version: self.spec.version.clone(),
            supported_protocols: self.spec.supported_protocols.clone(),
        }
    }

    fn validate_config(&self, config_path: &str) -> Result<(), VpnError> {
        if config_path.is_empty() {
            return Err(VpnError::InvalidConfiguration(
                "config_path must not be empty".into(),
            ));
        }
        if self.spec.validate_args.is_empty() {
            return Ok(());
        }
        if !Path::new(config_path).exists() {
            return Err(VpnError::InvalidConfiguration(format!(
                "config file not found: {config_path}"
            )));
        }

        let binary = self.resolve_binary()?;
        let args = self.expand_args(&self.spec.validate_args, config_path);
        let output = self
            .command(&binary, &args)
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
        vec![PrerequisiteCheck {
            name: "binary_exists".into(),
            passed: binary_result.is_ok(),
            message: match binary_result {
                Ok(path) => format!("found at {path}"),
                Err(error) => error.to_string(),
            },
        }]
    }

    fn subscribe(&self) -> Option<mpsc::Receiver<CoreEvent>> {
        self.event_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_defaults_to_config_arg() {
        let spec = ProcessCoreSpec::new("demo", "demo-bin");
        assert_eq!(spec.start_args, vec!["{config}"]);
    }

    #[test]
    fn missing_binary_reports_prerequisite() {
        let core = ProcessCore::new(ProcessCoreSpec::new("demo", "/definitely/missing"));
        let checks = core.check_prerequisites();
        assert_eq!(checks.len(), 1);
        assert!(!checks[0].passed);
    }

    #[test]
    fn start_failure_restores_disconnected_state() {
        let mut core = ProcessCore::new(ProcessCoreSpec::new("demo", "/definitely/missing"));

        assert!(core.start("ignored").is_err());

        assert_eq!(core.status(), ConnectionState::Disconnected);
    }

    #[cfg(unix)]
    #[test]
    fn can_start_and_stop_sleep() {
        let spec = ProcessCoreSpec::new("sleep", "/bin/sleep").with_start_args(["60"]);
        let mut core = ProcessCore::new(spec);
        core.start("ignored").unwrap();
        assert_eq!(core.status(), ConnectionState::Connected);
        core.stop().unwrap();
        assert_eq!(core.status(), ConnectionState::Disconnected);
    }

    #[cfg(unix)]
    #[test]
    fn validate_runs_configurable_command() {
        let spec = ProcessCoreSpec::new("echo", "/bin/echo").with_validate_args(["{config}"]);
        let core = ProcessCore::new(spec);
        assert!(core.validate_config("/bin/echo").is_ok());
    }
}
