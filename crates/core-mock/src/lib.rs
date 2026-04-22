//! Mock [`VpnCore`] implementation for development and testing.
//!
//! Provides a portable stand-in for real process or platform cores that:
//! - Tracks lifecycle state without spawning external processes
//! - Emits lifecycle and log events through the standard [`CoreEvent`] channel
//! - Requires no sing-box binary
//!
//! Used in unit tests throughout the workspace and can be selected at runtime
//! via `PINGLING_CORE_TYPE=mock` for UI development without a real VPN binary.
//! Downstream hosts receive the same `VpnCore` state and event shapes as they
//! would from a real core.
//!
//! [`CoreEvent`]: pingling_domain::CoreEvent

use log::info;
use pingling_domain::{ConnectionState, CoreEvent, CoreInfo, PrerequisiteCheck, VpnCore, VpnError};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Mock VPN core for development and testing.
pub struct MockCore {
    state: Arc<Mutex<ConnectionState>>,
    event_tx: Arc<Mutex<mpsc::Sender<CoreEvent>>>,
    event_rx: Mutex<Option<mpsc::Receiver<CoreEvent>>>,
}

impl MockCore {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            state: Arc::new(Mutex::new(ConnectionState::Disconnected)),
            event_tx: Arc::new(Mutex::new(tx)),
            event_rx: Mutex::new(Some(rx)),
        }
    }

    /// Emit a log event through the event channel.
    fn log(&self, msg: &str) {
        let _ = self
            .event_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(CoreEvent::Log(msg.into()));
    }

    fn emit_state(&self, state: ConnectionState) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state.clone();
        let _ = self
            .event_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(CoreEvent::StateChanged(state));
    }
}

impl Default for MockCore {
    fn default() -> Self {
        Self::new()
    }
}

impl VpnCore for MockCore {
    fn start(&mut self, config_path: &str) -> Result<(), VpnError> {
        if config_path.is_empty() {
            return Err(VpnError::InvalidConfiguration(
                "config_path must not be empty".into(),
            ));
        }

        if self.status().is_active() {
            return Err(VpnError::AlreadyConnected);
        }

        self.log("[mock] start requested");
        self.emit_state(ConnectionState::Connecting);

        // Emit simulated activity log lines through the event channel. The
        // mock stays fully in-process so it works on any CI runner without a
        // shell utility such as `sleep`.
        self.log("[mock] initializing core engine");
        self.log("[mock] binding to 127.0.0.1:1080 (socks)");
        self.log("[mock] binding to 127.0.0.1:8080 (http)");
        self.log("[mock] connected (simulated)");

        self.emit_state(ConnectionState::Connected);

        info!("mock core started");
        Ok(())
    }

    fn stop(&mut self) -> Result<(), VpnError> {
        if !self.status().is_active() {
            return Err(VpnError::NotConnected);
        }

        self.log("[mock] stop requested");
        self.emit_state(ConnectionState::Disconnecting);

        self.emit_state(ConnectionState::Disconnected);
        self.log("[mock] disconnected");

        info!("mock core stopped");
        Ok(())
    }

    fn kill(&mut self) -> Result<(), VpnError> {
        if !self.status().is_active() {
            return Err(VpnError::NotConnected);
        }

        self.log("[mock] force kill requested");
        self.emit_state(ConnectionState::Disconnected);
        self.log("[mock] killed");

        info!("mock core killed");
        Ok(())
    }

    fn status(&self) -> ConnectionState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn info(&self) -> CoreInfo {
        CoreInfo {
            name: "mock".into(),
            version: "dev".into(),
            supported_protocols: vec!["mock-socks".into(), "mock-http".into(), "mock-tun".into()],
        }
    }

    fn validate_config(&self, config_path: &str) -> Result<(), VpnError> {
        if config_path.is_empty() {
            return Err(VpnError::InvalidConfiguration(
                "config_path must not be empty".into(),
            ));
        }
        // Mock always validates successfully
        Ok(())
    }

    fn check_prerequisites(&self) -> Vec<PrerequisiteCheck> {
        vec![PrerequisiteCheck {
            name: "mock_binary".into(),
            passed: true,
            message: "mock core — no binary required".into(),
        }]
    }

    fn subscribe(&self) -> Option<mpsc::Receiver<CoreEvent>> {
        self.event_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_disconnected_core() {
        let core = MockCore::new();
        assert_eq!(core.status(), ConnectionState::Disconnected);
    }

    #[test]
    fn info_returns_mock_metadata() {
        let core = MockCore::new();
        let info = core.info();
        assert_eq!(info.name, "mock");
        assert_eq!(info.version, "dev");
        assert!(!info.supported_protocols.is_empty());
    }

    #[test]
    fn validate_config_empty_path() {
        let core = MockCore::new();
        let result = core.validate_config("");
        assert!(matches!(result, Err(VpnError::InvalidConfiguration(_))));
    }

    #[test]
    fn validate_config_any_path() {
        let core = MockCore::new();
        assert!(core.validate_config("/any/path.json").is_ok());
    }

    #[test]
    fn check_prerequisites() {
        let core = MockCore::new();
        let checks = core.check_prerequisites();
        assert!(!checks.is_empty());
        // We deliberately no longer ship the `terminal_emulator` check —
        // the daemon must never assume a desktop environment is available.
        assert!(checks.iter().all(|c| c.name.starts_with("mock")));
    }

    #[test]
    fn subscribe_returns_receiver() {
        let core = MockCore::new();
        assert!(core.subscribe().is_some());
    }

    #[test]
    fn start_empty_config() {
        let mut core = MockCore::new();
        let result = core.start("");
        assert!(matches!(result, Err(VpnError::InvalidConfiguration(_))));
    }

    #[test]
    fn start_and_stop_lifecycle() {
        let mut core = MockCore::new();
        let _rx = core.subscribe().unwrap();

        core.start("/test/config.json").unwrap();
        assert_eq!(core.status(), ConnectionState::Connected);

        core.stop().unwrap();
        assert_eq!(core.status(), ConnectionState::Disconnected);
    }

    #[test]
    fn start_already_connected() {
        let mut core = MockCore::new();
        core.start("/test.json").unwrap();
        let result = core.start("/test.json");
        assert!(matches!(result, Err(VpnError::AlreadyConnected)));
        let _ = core.kill();
    }

    #[test]
    fn stop_not_connected() {
        let mut core = MockCore::new();
        let result = core.stop();
        assert!(matches!(result, Err(VpnError::NotConnected)));
    }

    #[test]
    fn kill_not_connected() {
        let mut core = MockCore::new();
        let result = core.kill();
        assert!(matches!(result, Err(VpnError::NotConnected)));
    }

    #[test]
    fn start_emits_events() {
        let mut core = MockCore::new();
        let rx = core.subscribe().unwrap();

        core.start("/test.json").unwrap();

        // Should receive at least Started and some log events
        let mut got_log = false;
        let mut got_state = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                CoreEvent::Log(msg) if msg.contains("[mock]") => got_log = true,
                CoreEvent::StateChanged(ConnectionState::Connected) => got_state = true,
                _ => {}
            }
        }
        assert!(got_log, "should have received log events");
        assert!(got_state, "should have received Connected state change");

        core.stop().unwrap();
    }
}
