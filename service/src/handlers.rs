//! Core handlers — the bottom of each lifecycle pipeline.
//!
//! Each handler holds an `Arc<Mutex<CoreRegistry>>` and delegates to the
//! active [`VpnCore`] implementation. How that core executes the operation
//! (child process, FFI to a linked library, in-process mock) is entirely
//! the core's concern — handlers are transport-agnostic.
//!
//! These are constructed by [`VpnManager::new()`](crate::VpnManager::new)
//! and sit at the bottom of every pipeline. Middleware wraps around them.

use crate::CoreRegistry;
use domain::ops::*;
use domain::pipeline::Handler;
use domain::{ConnectionState, VpnError};
use log::info;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Connect
// ---------------------------------------------------------------------------

pub struct ConnectHandler {
    pub(crate) registry: Arc<Mutex<CoreRegistry>>,
}

impl Handler<OpConnect> for ConnectHandler {
    fn handle(&self, input: ConnectInput) -> Result<ConnectOutput, VpnError> {
        info!("Connecting with config: {}", input.config_path);
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let core = registry
            .active_core()
            .ok_or_else(|| VpnError::CoreNotFound("no active core".into()))?;

        // Only start — validation is a separate middleware concern.
        // Push ValidateBeforeStart middleware onto the connect pipeline
        // if you want pre-flight validation.
        core.start(&input.config_path)?;

        Ok(ConnectOutput {
            connection_info: None,
            metadata: input.metadata,
        })
    }
}

// ---------------------------------------------------------------------------
// Disconnect
// ---------------------------------------------------------------------------

pub struct DisconnectHandler {
    pub(crate) registry: Arc<Mutex<CoreRegistry>>,
}

impl Handler<OpDisconnect> for DisconnectHandler {
    fn handle(&self, input: DisconnectInput) -> Result<DisconnectOutput, VpnError> {
        info!("Disconnecting");
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let core = registry.active_core().ok_or(VpnError::NotConnected)?;
        core.stop()?;
        Ok(DisconnectOutput {
            metadata: input.metadata,
        })
    }
}

// ---------------------------------------------------------------------------
// Restart
// ---------------------------------------------------------------------------

pub struct RestartHandler {
    pub(crate) registry: Arc<Mutex<CoreRegistry>>,
}

impl Handler<OpRestart> for RestartHandler {
    fn handle(&self, input: RestartInput) -> Result<RestartOutput, VpnError> {
        info!("Restarting with config: {}", input.config_path);
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let core = registry
            .active_core()
            .ok_or_else(|| VpnError::CoreNotFound("no active core".into()))?;
        core.restart(&input.config_path)?;
        Ok(RestartOutput {
            connection_info: None,
            metadata: input.metadata,
        })
    }
}

// ---------------------------------------------------------------------------
// Validate
// ---------------------------------------------------------------------------

pub struct ValidateConfigHandler {
    pub(crate) registry: Arc<Mutex<CoreRegistry>>,
}

impl Handler<OpValidateConfig> for ValidateConfigHandler {
    fn handle(&self, input: ValidateConfigInput) -> Result<ValidateConfigOutput, VpnError> {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let core = registry
            .active_core()
            .ok_or_else(|| VpnError::CoreNotFound("no active core".into()))?;
        core.validate_config(&input.config_path)?;
        Ok(ValidateConfigOutput {
            metadata: input.metadata,
        })
    }
}

// ---------------------------------------------------------------------------
// GetStatus
// ---------------------------------------------------------------------------

pub struct GetStatusHandler {
    pub(crate) registry: Arc<Mutex<CoreRegistry>>,
}

impl Handler<OpGetStatus> for GetStatusHandler {
    fn handle(&self, _input: GetStatusInput) -> Result<GetStatusOutput, VpnError> {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        match registry.active_core() {
            Some(core) => Ok(GetStatusOutput {
                state: core.status(),
                running: core.running(),
                connection_info: None,
            }),
            None => Ok(GetStatusOutput {
                state: ConnectionState::Disconnected,
                running: false,
                connection_info: None,
            }),
        }
    }
}
