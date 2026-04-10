//! Config validation hook — pre-flight check before connect and restart.
//!
//! [`ValidateBeforeStart`] implements `Hook<OpConnect>` and `Hook<OpRestart>`
//! using only the `before` phase: it calls [`VpnCore::validate_config`] before
//! the handler runs. If validation fails, the operation is short-circuited —
//! `core.start()` is never called.
//!
//! # Why a hook, not hardcoded
//!
//! - Some cores validate internally (e.g. a statically linked library).
//! - Mock cores in tests may skip validation entirely.
//! - Users may override validation with a plugin that decrypts first.
//! - An FFI core may validate differently than a subprocess core.
//!
//! # Pipeline position
//!
//! Registered early so validation runs before any other `before` hook that
//! might trigger expensive operations (network calls, file writes, etc.).
//! `LoggingHook` should be registered before `ValidateBeforeStart` so logging
//! fires outermost and captures the full lifecycle including the validation
//! error path.
//!
//! # Config content integration
//!
//! If the [`ConfigContentLoader`] hook is also registered, `before` hooks run
//! in registration order. Register `ConfigContentLoader` before
//! `ValidateBeforeStart` so the config content is available when plugins
//! inspect `ValidateConfigInput.config_content`.

use crate::CoreRegistry;
use domain::ops::*;
use domain::pipeline::Hook;
use domain::VpnError;
use std::sync::{Arc, Mutex};

/// Validates the config file before connecting or restarting.
///
/// If validation fails, the operation is short-circuited (handler never runs).
pub struct ValidateBeforeStart {
    registry: Arc<Mutex<CoreRegistry>>,
}

impl ValidateBeforeStart {
    pub fn new(registry: Arc<Mutex<CoreRegistry>>) -> Self {
        Self { registry }
    }
}

// Shared validation logic extracted to avoid duplication.
fn validate(registry: &Arc<Mutex<CoreRegistry>>, config_path: &str) -> Result<(), VpnError> {
    let mut reg = registry.lock().unwrap_or_else(|e| e.into_inner());
    let core = reg
        .active_core()
        .ok_or_else(|| VpnError::CoreNotFound("no active core".into()))?;
    core.validate_config(config_path)
    // Lock released here — the handler will acquire its own lock.
}

impl Hook<OpConnect> for ValidateBeforeStart {
    fn name(&self) -> &str {
        "builtin:validate"
    }

    /// Validates `config_path` before the connect handler starts the tunnel.
    ///
    /// Returns `Err` (short-circuit) if validation fails. The connect handler
    /// never runs and all `on_error` hooks fire.
    fn before(&self, input: &mut ConnectInput) -> Result<(), VpnError> {
        validate(&self.registry, &input.config_path)
    }
}

impl Hook<OpRestart> for ValidateBeforeStart {
    fn name(&self) -> &str {
        "builtin:validate"
    }

    /// Validates `config_path` before the restart handler stops and restarts
    /// the tunnel. Returns `Err` (short-circuit) if validation fails.
    fn before(&self, input: &mut RestartInput) -> Result<(), VpnError> {
        validate(&self.registry, &input.config_path)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use domain::pipeline::{Handler, Pipeline};
    use domain::{
        ConnectionState, CoreDescriptor, CoreInfo, CoreSource, PrerequisiteCheck, VpnCore,
    };

    // -- Test core -------------------------------------------------------------

    struct TestCore {
        validate_fails: bool,
    }

    impl VpnCore for TestCore {
        fn start(&mut self, _: &str) -> Result<(), VpnError> {
            Ok(())
        }
        fn stop(&mut self) -> Result<(), VpnError> {
            Ok(())
        }
        fn kill(&mut self) -> Result<(), VpnError> {
            Ok(())
        }
        fn status(&self) -> ConnectionState {
            ConnectionState::Disconnected
        }
        fn info(&self) -> CoreInfo {
            CoreInfo {
                name: "test".into(),
                version: "0".into(),
                supported_protocols: vec![],
            }
        }
        fn validate_config(&self, _: &str) -> Result<(), VpnError> {
            if self.validate_fails {
                Err(VpnError::ValidationError("bad config".into()))
            } else {
                Ok(())
            }
        }
        fn check_prerequisites(&self) -> Vec<PrerequisiteCheck> {
            vec![]
        }
        fn subscribe(&self) -> Option<std::sync::mpsc::Receiver<domain::CoreEvent>> {
            None
        }
    }

    // -- Registry helper -------------------------------------------------------

    fn registry_with(validate_fails: bool) -> Arc<Mutex<CoreRegistry>> {
        let mut reg = CoreRegistry::new();
        reg.register(
            CoreDescriptor {
                core_type: "test".into(),
                display_name: "Test".into(),
                source: CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(TestCore { validate_fails }),
        );
        Arc::new(Mutex::new(reg))
    }

    // -- Connect ---------------------------------------------------------------

    struct OkConnectHandler;
    impl Handler<OpConnect> for OkConnectHandler {
        fn handle(&self, input: ConnectInput) -> Result<ConnectOutput, VpnError> {
            Ok(ConnectOutput {
                connection_info: None,
                metadata: input.metadata,
            })
        }
    }

    fn connect_input() -> ConnectInput {
        ConnectInput {
            config_path: "/ok.json".into(),
            core_type: "test".into(),
            state: ConnectionState::Disconnected,
            metadata: Default::default(),
        }
    }

    #[test]
    fn connect_validation_passes_through() {
        let registry = registry_with(false);
        let mut pipeline = Pipeline::<OpConnect>::new(Box::new(OkConnectHandler));
        pipeline.push_hook(Box::new(ValidateBeforeStart::new(registry)));

        assert!(pipeline.execute(connect_input()).is_ok());
    }

    #[test]
    fn connect_validation_failure_short_circuits() {
        let handler_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        struct TrackHandler(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Handler<OpConnect> for TrackHandler {
            fn handle(&self, _: ConnectInput) -> Result<ConnectOutput, VpnError> {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(ConnectOutput {
                    connection_info: None,
                    metadata: Default::default(),
                })
            }
        }

        let registry = registry_with(true);
        let mut pipeline = Pipeline::<OpConnect>::new(Box::new(TrackHandler(handler_ran.clone())));
        pipeline.push_hook(Box::new(ValidateBeforeStart::new(registry)));

        let result = pipeline.execute(connect_input());
        assert!(matches!(result, Err(VpnError::ValidationError(_))));
        assert!(
            !handler_ran.load(std::sync::atomic::Ordering::SeqCst),
            "handler must not run"
        );
    }

    #[test]
    fn connect_validation_failure_fires_on_error() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let on_error_fired = std::sync::Arc::new(AtomicBool::new(false));
        let flag = on_error_fired.clone();

        use domain::pipeline::FnHook;
        let registry = registry_with(true);
        let mut pipeline = Pipeline::<OpConnect>::new(Box::new(OkConnectHandler));
        pipeline.push_hook(Box::new(ValidateBeforeStart::new(registry)));
        pipeline.push_hook(Box::new(
            FnHook::<OpConnect>::new("observer")
                .on_error(move |_, _| flag.store(true, Ordering::SeqCst)),
        ));

        assert!(pipeline.execute(connect_input()).is_err());
        assert!(on_error_fired.load(Ordering::SeqCst));
    }

    #[test]
    fn connect_without_validate_hook_handler_runs_directly() {
        let pipeline = Pipeline::<OpConnect>::new(Box::new(OkConnectHandler));
        assert!(pipeline.execute(connect_input()).is_ok());
    }

    // -- Restart ---------------------------------------------------------------

    struct OkRestartHandler;
    impl Handler<OpRestart> for OkRestartHandler {
        fn handle(&self, _: RestartInput) -> Result<RestartOutput, VpnError> {
            Ok(RestartOutput {
                connection_info: None,
                metadata: Default::default(),
            })
        }
    }

    fn restart_input() -> RestartInput {
        RestartInput {
            config_path: "/ok.json".into(),
            core_type: "test".into(),
            state: ConnectionState::Connected,
            metadata: Default::default(),
        }
    }

    #[test]
    fn restart_validation_passes_through() {
        let registry = registry_with(false);
        let mut pipeline = Pipeline::<OpRestart>::new(Box::new(OkRestartHandler));
        pipeline.push_hook(Box::new(ValidateBeforeStart::new(registry)));

        assert!(pipeline.execute(restart_input()).is_ok());
    }

    #[test]
    fn restart_validation_failure_short_circuits() {
        let registry = registry_with(true);
        let mut pipeline = Pipeline::<OpRestart>::new(Box::new(OkRestartHandler));
        pipeline.push_hook(Box::new(ValidateBeforeStart::new(registry)));

        let result = pipeline.execute(restart_input());
        assert!(matches!(result, Err(VpnError::ValidationError(_))));
    }

    // -- Arc sharing -----------------------------------------------------------

    #[test]
    fn validate_hook_can_be_shared_via_arc() {
        let registry = registry_with(false);
        let hook = std::sync::Arc::new(ValidateBeforeStart::new(registry));

        let mut p_connect = Pipeline::<OpConnect>::new(Box::new(OkConnectHandler));
        p_connect.push_hook(Box::new(std::sync::Arc::clone(&hook)));

        let mut p_restart = Pipeline::<OpRestart>::new(Box::new(OkRestartHandler));
        p_restart.push_hook(Box::new(std::sync::Arc::clone(&hook)));

        assert!(p_connect.execute(connect_input()).is_ok());
        assert!(p_restart.execute(restart_input()).is_ok());
    }
}
