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
use domain::{ConnectionState, ProfileStorage, TempConfigPath, VpnError};
use log::{info, warn};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Connect
// ---------------------------------------------------------------------------

pub struct ConnectHandler {
    pub(crate) registry: Arc<Mutex<CoreRegistry>>,
    /// Optional profile storage. When `Some`, the handler tries to load
    /// the active profile's decrypted config into a temp file and starts
    /// the core with that path. The legacy `input.config_path` is used
    /// only as a fallback when no active profile is set.
    profile_storage: Option<Arc<dyn ProfileStorage>>,
    /// Shared slot that holds the decrypted config temp file for the
    /// lifetime of the connection. Populated by this handler, consumed
    /// by the matching [`DisconnectHandler`]. The slot is a shared
    /// `Arc<Mutex<_>>` so both handlers see the same state.
    active_temp_config: Arc<Mutex<Option<TempConfigPath>>>,
}

impl ConnectHandler {
    pub fn new(
        registry: Arc<Mutex<CoreRegistry>>,
        profile_storage: Option<Arc<dyn ProfileStorage>>,
        active_temp_config: Arc<Mutex<Option<TempConfigPath>>>,
    ) -> Self {
        Self {
            registry,
            profile_storage,
            active_temp_config,
        }
    }
}

impl Handler<OpConnect> for ConnectHandler {
    fn handle(&self, input: ConnectInput) -> Result<ConnectOutput, VpnError> {
        // Resolve the config path. Profile storage wins when it has an
        // active profile; otherwise fall back to the legacy input path.
        //
        // The `TempConfigPath` returned by the storage is stashed in a
        // shared `Arc<Mutex<_>>` slot so the disconnect handler can
        // drop it (and delete the temp file) when the core stops.
        let resolved_path: String = match self.profile_storage.as_ref() {
            Some(storage) => match storage.load_active_for_core_start() {
                Ok(temp) => {
                    let path = temp.path().to_string_lossy().into_owned();
                    info!(
                        "connect: using active profile's decrypted config at {}",
                        path
                    );
                    // Park the RAII handle so the temp file stays on
                    // disk until disconnect.
                    *self
                        .active_temp_config
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(temp);
                    path
                }
                Err(VpnError::NotConnected) => {
                    info!(
                        "connect: no active profile, falling back to legacy config_path: {}",
                        input.config_path
                    );
                    input.config_path.clone()
                }
                Err(e) => {
                    warn!("connect: profile storage failed to load active: {e}");
                    return Err(e);
                }
            },
            None => {
                info!(
                    "connect: no profile storage wired, using legacy config_path: {}",
                    input.config_path
                );
                input.config_path.clone()
            }
        };

        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let core = registry
            .active_core()
            .ok_or_else(|| VpnError::CoreNotFound("no active core".into()))?;

        // Only start — validation is a separate middleware concern.
        // Push ValidateBeforeStart middleware onto the connect pipeline
        // if you want pre-flight validation.
        core.start(&resolved_path)?;

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
    /// Matching slot to [`ConnectHandler::active_temp_config`]. On a
    /// successful disconnect the handler `.take()`s the value so its
    /// `Drop` runs and the decrypted temp file is deleted.
    active_temp_config: Arc<Mutex<Option<TempConfigPath>>>,
}

impl DisconnectHandler {
    pub fn new(
        registry: Arc<Mutex<CoreRegistry>>,
        active_temp_config: Arc<Mutex<Option<TempConfigPath>>>,
    ) -> Self {
        Self {
            registry,
            active_temp_config,
        }
    }
}

impl Handler<OpDisconnect> for DisconnectHandler {
    fn handle(&self, input: DisconnectInput) -> Result<DisconnectOutput, VpnError> {
        info!("Disconnecting");
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let core = registry.active_core().ok_or(VpnError::NotConnected)?;
        core.stop()?;
        // Drop the decrypted temp file, if any. `Drop` on TempConfigPath
        // deletes the file — we just need to remove it from the slot so
        // the Drop fires.
        let _ = self
            .active_temp_config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
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
