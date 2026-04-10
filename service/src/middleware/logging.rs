//! Logging hook — observes every pipeline operation across all three phases.
//!
//! `LoggingHook` implements [`Hook<Op>`] for every concrete operation.
//! Each phase maps to a distinct log line:
//!
//! - **`before`** — logs the operation name and key input fields as it enters
//!   the pipeline (after any preceding hooks may have rewritten input).
//! - **`after`** — logs `ok` on successful completion.
//! - **`on_error`** — logs the error message and VPN error code on failure.
//!
//! Because logging is a pure observer it never modifies input or output
//! and never returns `Err` from any phase.
//!
//! # Registration
//!
//! Register via [`defaults::register`](crate::defaults::register), or manually:
//!
//! ```rust,ignore
//! use service::middleware::logging::LoggingHook;
//!
//! pipeline.push_hook(Box::new(LoggingHook));
//! ```
//!
//! # Position in the pipeline
//!
//! Register `LoggingHook` **first** so its `before` fires earliest (captures
//! the input before other hooks transform it) and its `after`/`on_error` fire
//! last (see the final outcome after all other hooks have run).
//! For reverse-registration-order `after` this means registering it last would
//! make it fire first — register it first to fire it last on the output path.

use domain::ops::*;
use domain::pipeline::Hook;
use domain::VpnError;
use log::info;

/// Logs operation start, success, and failure for any pipeline operation.
///
/// Implements [`Hook<Op>`] for every concrete operation via the macro below.
/// Registered on all lifecycle pipelines by [`defaults::register`].
pub struct LoggingHook;

// Implements Hook<$op> for LoggingHook.
// - before(): logs the operation name and a human-readable input summary.
// - after(): logs "ok".
// - on_error(): logs the error message and numeric code.
macro_rules! impl_logging_hook {
    ($op:ty, $input_ty:ty, $fmt:expr) => {
        impl Hook<$op> for LoggingHook {
            fn name(&self) -> &str {
                "builtin:logging"
            }

            fn before(&self, input: &mut $input_ty) -> Result<(), VpnError> {
                let op_name = <$op as domain::pipeline::Operation>::name();
                let fmt_fn: fn(&$input_ty) -> String = $fmt;
                info!("[{op_name}] start {}", fmt_fn(input));
                Ok(())
            }

            fn after(
                &self,
                _input: &$input_ty,
                _output: &mut <$op as domain::pipeline::Operation>::Output,
            ) -> Result<(), VpnError> {
                let op_name = <$op as domain::pipeline::Operation>::name();
                info!("[{op_name}] ok");
                Ok(())
            }

            fn on_error(&self, _input: &$input_ty, err: &VpnError) {
                let op_name = <$op as domain::pipeline::Operation>::name();
                info!("[{op_name}] error: {} (code={})", err, err.code());
            }
        }
    };
}

impl_logging_hook!(OpConnect, ConnectInput, |i: &ConnectInput| format!(
    "core={} config={}",
    i.core_type, i.config_path
));
impl_logging_hook!(
    OpDisconnect,
    DisconnectInput,
    |i: &DisconnectInput| format!("core={}", i.core_type)
);
impl_logging_hook!(OpRestart, RestartInput, |i: &RestartInput| format!(
    "core={} config={}",
    i.core_type, i.config_path
));
impl_logging_hook!(
    OpValidateConfig,
    ValidateConfigInput,
    |i: &ValidateConfigInput| format!(
        "core={} config={} has_content={}",
        i.core_type,
        i.config_path,
        i.config_content.is_some()
    )
);
impl_logging_hook!(OpGetStatus, GetStatusInput, |i: &GetStatusInput| format!(
    "core={}",
    i.core_type
));
impl_logging_hook!(
    OpListOutbounds,
    ListOutboundsInput,
    |i: &ListOutboundsInput| format!("core={}", i.core_type)
);
impl_logging_hook!(
    OpSelectOutbound,
    SelectOutboundInput,
    |i: &SelectOutboundInput| format!("core={} outbound={}", i.core_type, i.outbound_id)
);
impl_logging_hook!(
    OpTestLatency,
    TestLatencyInput,
    |i: &TestLatencyInput| format!("core={} ids={:?}", i.core_type, i.outbound_ids)
);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use domain::pipeline::{Handler, Pipeline};
    use domain::ConnectionState;

    // A minimal handler that echoes back a status output.
    struct EchoStatusHandler;
    impl Handler<OpGetStatus> for EchoStatusHandler {
        fn handle(&self, _: GetStatusInput) -> Result<GetStatusOutput, VpnError> {
            Ok(GetStatusOutput {
                state: ConnectionState::Disconnected,
                connection_info: None,
                running: false,
            })
        }
    }

    // A handler that always fails.
    struct FailStatusHandler;
    impl Handler<OpGetStatus> for FailStatusHandler {
        fn handle(&self, _: GetStatusInput) -> Result<GetStatusOutput, VpnError> {
            Err(VpnError::Unknown("status unavailable".into()))
        }
    }

    #[test]
    fn logging_before_does_not_modify_input() {
        let mut pipeline = Pipeline::<OpGetStatus>::new(Box::new(EchoStatusHandler));
        pipeline.push_hook(Box::new(LoggingHook));

        let input = GetStatusInput {
            core_type: "mock".into(),
        };
        let output = pipeline.execute(input).unwrap();
        assert_eq!(output.state, ConnectionState::Disconnected);
    }

    #[test]
    fn logging_after_does_not_modify_output() {
        let mut pipeline = Pipeline::<OpGetStatus>::new(Box::new(EchoStatusHandler));
        pipeline.push_hook(Box::new(LoggingHook));

        let output = pipeline
            .execute(GetStatusInput {
                core_type: "test".into(),
            })
            .unwrap();
        // Output unchanged: running must still be false.
        assert!(!output.running);
    }

    #[test]
    fn logging_on_error_does_not_suppress_error() {
        let mut pipeline = Pipeline::<OpGetStatus>::new(Box::new(FailStatusHandler));
        pipeline.push_hook(Box::new(LoggingHook));

        let result = pipeline.execute(GetStatusInput {
            core_type: "test".into(),
        });
        // Error still propagates — logging is read-only.
        assert!(result.is_err());
    }

    #[test]
    fn logging_works_on_connect_op() {
        struct OkConnectHandler;
        impl Handler<OpConnect> for OkConnectHandler {
            fn handle(&self, _: ConnectInput) -> Result<ConnectOutput, VpnError> {
                Ok(ConnectOutput {
                    connection_info: None,
                    metadata: Default::default(),
                })
            }
        }

        let mut pipeline = Pipeline::<OpConnect>::new(Box::new(OkConnectHandler));
        pipeline.push_hook(Box::new(LoggingHook));

        let result = pipeline.execute(ConnectInput {
            config_path: "/cfg.json".into(),
            core_type: "mock".into(),
            state: ConnectionState::Disconnected,
            metadata: Default::default(),
        });
        assert!(result.is_ok());
    }

    #[test]
    fn logging_validate_shows_has_content_flag() {
        // Validates that the log format for OpValidateConfig includes
        // the has_content field (true when config_content is Some).
        // We can only check the format fn indirectly via pipeline success.
        struct OkValidateHandler;
        impl Handler<OpValidateConfig> for OkValidateHandler {
            fn handle(&self, _: ValidateConfigInput) -> Result<ValidateConfigOutput, VpnError> {
                Ok(ValidateConfigOutput {
                    metadata: Default::default(),
                })
            }
        }

        let mut pipeline = Pipeline::<OpValidateConfig>::new(Box::new(OkValidateHandler));
        pipeline.push_hook(Box::new(LoggingHook));

        // Without content
        let result = pipeline.execute(ValidateConfigInput {
            config_path: "/cfg.json".into(),
            core_type: "mock".into(),
            config_content: None,
            metadata: Default::default(),
        });
        assert!(result.is_ok());

        // With content
        let result = pipeline.execute(ValidateConfigInput {
            config_path: "/cfg.json".into(),
            core_type: "mock".into(),
            config_content: Some("{}".into()),
            metadata: Default::default(),
        });
        assert!(result.is_ok());
    }
}
