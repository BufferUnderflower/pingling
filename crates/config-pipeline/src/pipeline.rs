//! Linear pipeline of [`ConfigProcessor`]s + a runner that walks them
//! in order, with optional step instrumentation for debugging.
//!
//! Direct port of the dart `ProcessorPipeline` shape from
//! existing native/mobile clients.

use crate::attempt::ConfigRequest;
use serde_json::Value;

/// One step in the config processing pipeline.
///
/// Implementations transform a sing-box config JSON given the per-attempt
/// `ConfigRequest`. They MUST be deterministic for the same input — the
/// strategy retry loop re-runs the pipeline on every attempt.
pub trait ConfigProcessor: Send + Sync {
    /// Stable name used in logs, in error context, and as the key the
    /// pipeline plugin's `post_<name>` stage matches against.
    fn name(&self) -> &str;

    /// Transform `config` given `request`. Return the transformed config.
    ///
    /// # Errors
    ///
    /// Implementations return an error string when the input is
    /// structurally invalid in a way that can't be passed through (e.g.
    /// the `dns` section is missing entirely when this processor needs
    /// it). On error, the runner stops and returns the error to the
    /// caller.
    fn process(&self, config: Value, request: &ConfigRequest) -> Result<Value, String>;
}

/// One captured step from a pipeline run, used by the optional step
/// callback for instrumented debugging.
#[derive(Debug, Clone)]
pub struct ProcessorStep {
    pub processor_name: String,
    pub input: Value,
    pub output: Value,
}

/// Linear runner over a list of [`ConfigProcessor`]s.
///
/// Construct with [`ProcessorPipeline::new`] and add processors via
/// [`push`](Self::push). Run with [`process`](Self::process), optionally
/// passing a step callback for per-step observability.
pub struct ProcessorPipeline {
    processors: Vec<Box<dyn ConfigProcessor>>,
}

impl ProcessorPipeline {
    /// Create an empty pipeline.
    pub fn new() -> Self {
        Self {
            processors: Vec::new(),
        }
    }

    /// Append a processor to the chain. Order matters.
    pub fn push(&mut self, processor: Box<dyn ConfigProcessor>) -> &mut Self {
        self.processors.push(processor);
        self
    }

    /// Names of all registered processors, in order.
    pub fn names(&self) -> Vec<&str> {
        self.processors.iter().map(|p| p.name()).collect()
    }

    /// Whether the pipeline has zero processors.
    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }

    /// Run the pipeline. Each processor receives the output of the
    /// previous one. On any processor error, returns the error with the
    /// processor name prefixed.
    pub fn process(&self, config: Value, request: &ConfigRequest) -> Result<Value, String> {
        self.process_with(&mut |_| {}, config, request)
    }

    /// Same as [`process`](Self::process) but invokes `on_step` after
    /// each processor with the input/output snapshot. Used for
    /// instrumented debugging — the strategy retry loop calls this with
    /// a logging callback when verbose mode is on.
    pub fn process_with(
        &self,
        on_step: &mut dyn FnMut(ProcessorStep),
        mut config: Value,
        request: &ConfigRequest,
    ) -> Result<Value, String> {
        for processor in &self.processors {
            let input_snapshot = config.clone();
            let output = processor
                .process(config, request)
                .map_err(|e| format!("processor[{}]: {e}", processor.name()))?;
            on_step(ProcessorStep {
                processor_name: processor.name().into(),
                input: input_snapshot,
                output: output.clone(),
            });
            config = output;
        }
        Ok(config)
    }
}

impl Default for ProcessorPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempt::AttemptInfo;
    use crate::strategy::{ConnectionStrategy, ResolverType, RetryPolicy, StackType};
    use serde_json::json;
    use std::time::Duration;

    fn sample_request() -> ConfigRequest {
        ConfigRequest {
            with_host_dns: false,
            default_dns_server: None,
            attempt: AttemptInfo {
                strategy: ConnectionStrategy {
                    id: "test".into(),
                    stack: StackType::System,
                    resolver_type: ResolverType::Doh,
                    total_timeout: Duration::from_secs(30),
                    retry: RetryPolicy::NoRetry,
                },
                attempt_number: 1,
                previous_error: None,
            },
        }
    }

    /// Trivial test processor: appends a key with its name to the config.
    struct AppendKey {
        name: &'static str,
    }
    impl ConfigProcessor for AppendKey {
        fn name(&self) -> &str {
            self.name
        }
        fn process(&self, mut config: Value, _r: &ConfigRequest) -> Result<Value, String> {
            if let Some(obj) = config.as_object_mut() {
                obj.insert(self.name.into(), json!(true));
            }
            Ok(config)
        }
    }

    /// Test processor that always errors with a known message.
    struct AlwaysError;
    impl ConfigProcessor for AlwaysError {
        fn name(&self) -> &str {
            "always_error"
        }
        fn process(&self, _c: Value, _r: &ConfigRequest) -> Result<Value, String> {
            Err("boom".into())
        }
    }

    #[test]
    fn empty_pipeline_returns_input_unchanged() {
        let pipeline = ProcessorPipeline::new();
        let input = json!({"hello": "world"});
        let out = pipeline.process(input.clone(), &sample_request()).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn processors_run_in_order() {
        let mut pipeline = ProcessorPipeline::new();
        pipeline
            .push(Box::new(AppendKey { name: "first" }))
            .push(Box::new(AppendKey { name: "second" }))
            .push(Box::new(AppendKey { name: "third" }));

        let input = json!({});
        let out = pipeline.process(input, &sample_request()).unwrap();

        assert_eq!(out["first"], json!(true));
        assert_eq!(out["second"], json!(true));
        assert_eq!(out["third"], json!(true));
    }

    #[test]
    fn error_short_circuits_with_processor_name_prefix() {
        let mut pipeline = ProcessorPipeline::new();
        pipeline
            .push(Box::new(AppendKey { name: "first" }))
            .push(Box::new(AlwaysError))
            .push(Box::new(AppendKey { name: "third" }));

        let result = pipeline.process(json!({}), &sample_request());
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("processor[always_error]"), "got: {msg}");
        assert!(msg.contains("boom"), "got: {msg}");
    }

    #[test]
    fn step_callback_fires_per_processor() {
        let mut pipeline = ProcessorPipeline::new();
        pipeline
            .push(Box::new(AppendKey { name: "a" }))
            .push(Box::new(AppendKey { name: "b" }));

        let mut steps = Vec::new();
        let _ = pipeline
            .process_with(&mut |s| steps.push(s), json!({}), &sample_request())
            .unwrap();

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].processor_name, "a");
        assert_eq!(steps[1].processor_name, "b");
        // The second step's input has the first step's "a" key set.
        assert_eq!(steps[1].input["a"], json!(true));
    }

    #[test]
    fn names_returns_processors_in_order() {
        let mut pipeline = ProcessorPipeline::new();
        pipeline
            .push(Box::new(AppendKey { name: "x" }))
            .push(Box::new(AppendKey { name: "y" }));
        assert_eq!(pipeline.names(), vec!["x", "y"]);
    }

    #[test]
    fn is_empty_reports_correctly() {
        let mut pipeline = ProcessorPipeline::new();
        assert!(pipeline.is_empty());
        pipeline.push(Box::new(AppendKey { name: "x" }));
        assert!(!pipeline.is_empty());
    }
}
