//! `PipelinePlugin` trait + `PluginError` enum.
//!
//! The trait is the daemon-facing interface. The canonical implementation
//! is `crate::extism_plugin::ExtismPipelinePlugin`, but tests use a
//! hand-rolled `RecordingPipelinePlugin` and the `StrategyRetryWrap` may
//! use other shapes in the future (e.g. a sidecar process).

use crate::protocol::{
    PipelineCapabilities, PipelineStage, ProcessConfigInput, ProcessConfigOutput,
};

/// Daemon-facing pipeline plugin interface.
///
/// `Send + Sync` because the connect path may run on any worker thread.
pub trait PipelinePlugin: Send + Sync {
    /// Plugin name. Used in logs and `daemon.info`.
    fn name(&self) -> &str;

    /// Static capabilities probed once at load time.
    fn capabilities(&self) -> &PipelineCapabilities;

    /// Process one stage of the pipeline.
    ///
    /// `stage` matches one of the entries in
    /// `capabilities().stages`. The wrap caller already filters by
    /// claimed stages, so plugins are never called for stages they
    /// don't claim.
    ///
    /// # Errors
    ///
    /// Implementations return [`PluginError`] for any failure mode.
    /// The wrap caller logs the error at warn level and falls back to
    /// the native (unmodified) input — a misbehaving plugin must NEVER
    /// break connect.
    fn process_config(
        &self,
        stage: PipelineStage,
        input: ProcessConfigInput,
    ) -> Result<ProcessConfigOutput, PluginError>;
}

/// All ways a pipeline plugin call can fail.
///
/// **Non-exhaustive on purpose** so the strategy retry wrap can match
/// against known classes and fall through unknown future variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PluginError {
    /// Plugin's `pipeline_capabilities` returned a wire version the
    /// daemon doesn't speak.
    WireVersionMismatch { plugin_says: u32, daemon_uses: u32 },
    /// A required wasm export is missing.
    MissingExport { fn_name: String },
    /// The wasm runtime returned an error (host call failed, OOM, etc).
    Wasm(String),
    /// The plugin returned a JSON value that didn't deserialize into
    /// the expected `ProcessConfigOutput` shape.
    InvalidJson(String),
    /// The plugin explicitly rejected the call (returned a typed
    /// rejection in its output). Reserved for future use.
    Rejected(String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WireVersionMismatch {
                plugin_says,
                daemon_uses,
            } => write!(
                f,
                "wire version mismatch: plugin={plugin_says}, daemon={daemon_uses}"
            ),
            Self::MissingExport { fn_name } => write!(f, "missing wasm export: {fn_name}"),
            Self::Wasm(msg) => write!(f, "wasm error: {msg}"),
            Self::InvalidJson(msg) => write!(f, "invalid json: {msg}"),
            Self::Rejected(msg) => write!(f, "plugin rejected: {msg}"),
        }
    }
}

impl std::error::Error for PluginError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test fixture: records every `process_config` call.
    pub struct RecordingPipelinePlugin {
        capabilities: PipelineCapabilities,
        seen: Mutex<Vec<(PipelineStage, ProcessConfigInput)>>,
    }

    impl RecordingPipelinePlugin {
        pub fn new(capabilities: PipelineCapabilities) -> Self {
            Self {
                capabilities,
                seen: Mutex::new(Vec::new()),
            }
        }

        pub fn calls(&self) -> Vec<(PipelineStage, ProcessConfigInput)> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl PipelinePlugin for RecordingPipelinePlugin {
        fn name(&self) -> &str {
            "recording"
        }
        fn capabilities(&self) -> &PipelineCapabilities {
            &self.capabilities
        }
        fn process_config(
            &self,
            stage: PipelineStage,
            input: ProcessConfigInput,
        ) -> Result<ProcessConfigOutput, PluginError> {
            self.seen.lock().unwrap().push((stage, input.clone()));
            Ok(ProcessConfigOutput {
                config: input.config,
                diagnostics: vec![],
            })
        }
    }

    use crate::protocol::{CoreInfo, PipelineStage as Stage, ProcessConfigAttempt, WIRE_VERSION};
    use core_config_processor::{ConnectionStrategy, ResolverType, RetryPolicy, StackType};
    use serde_json::json;
    use std::time::Duration;

    fn sample_input(stage: Stage) -> ProcessConfigInput {
        ProcessConfigInput {
            wire_version: WIRE_VERSION,
            core: CoreInfo {
                kind: "mock".into(),
                version: "0.0.0".into(),
                platform: "linux".into(),
            },
            attempt: ProcessConfigAttempt {
                stage,
                strategy: ConnectionStrategy {
                    id: "x".into(),
                    stack: StackType::System,
                    resolver_type: ResolverType::Doh,
                    total_timeout: Duration::from_secs(30),
                    retry: RetryPolicy::NoRetry,
                },
                attempt_number: 1,
                previous_error: None,
            },
            config: json!({"hi": "there"}),
        }
    }

    #[test]
    fn recording_plugin_records_calls_and_passes_through() {
        let plugin = RecordingPipelinePlugin::new(PipelineCapabilities::default());
        let input = sample_input(Stage::PostPipeline);
        let out = plugin
            .process_config(Stage::PostPipeline, input.clone())
            .unwrap();
        assert_eq!(out.config, json!({"hi": "there"}));
        assert!(out.diagnostics.is_empty());

        let calls = plugin.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, Stage::PostPipeline);
        assert_eq!(calls[0].1, input);
    }

    #[test]
    fn plugin_error_display() {
        assert_eq!(
            PluginError::WireVersionMismatch {
                plugin_says: 2,
                daemon_uses: 1
            }
            .to_string(),
            "wire version mismatch: plugin=2, daemon=1"
        );
        assert_eq!(
            PluginError::MissingExport {
                fn_name: "process_config".into()
            }
            .to_string(),
            "missing wasm export: process_config"
        );
    }
}
