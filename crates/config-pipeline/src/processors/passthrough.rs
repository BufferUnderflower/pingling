//! The built-in no-op processor.
//!
//! Returns the input config unchanged. Useful as:
//!
//! 1. **A sane default** when no extism plugin is loaded — the pipeline
//!    still has a processor in it so the runner doesn't special-case
//!    "empty pipeline".
//! 2. **A test fixture** — lets unit tests exercise the pipeline runner
//!    without depending on any real processor.
//! 3. **A debugging step** — can be inserted into a real pipeline to
//!    verify the surrounding stages are working independently.

use crate::attempt::ConfigRequest;
use crate::pipeline::ConfigProcessor;
use serde_json::Value;

/// No-op processor: `process(config, _)` returns `Ok(config)` unchanged.
#[derive(Debug, Default, Clone, Copy)]
pub struct PassthroughProcessor;

impl PassthroughProcessor {
    /// Construct a passthrough processor. Stateless — `Default::default()`
    /// works equivalently.
    pub const fn new() -> Self {
        Self
    }
}

impl ConfigProcessor for PassthroughProcessor {
    fn name(&self) -> &str {
        "passthrough"
    }

    fn process(&self, config: Value, _request: &ConfigRequest) -> Result<Value, String> {
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempt::AttemptInfo;
    use crate::strategy::{ConnectionStrategy, ResolverType, RetryPolicy, StackType};
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
                    total_timeout: Duration::from_secs(10),
                    retry: RetryPolicy::NoRetry,
                },
                attempt_number: 1,
                previous_error: None,
            },
        }
    }

    #[test]
    fn passthrough_returns_input_unchanged() {
        let processor = PassthroughProcessor::new();
        let input = serde_json::json!({
            "log": {"level": "info"},
            "dns": {"servers": []},
        });
        let output = processor.process(input.clone(), &sample_request()).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn passthrough_has_stable_name() {
        assert_eq!(PassthroughProcessor::new().name(), "passthrough");
    }

    #[test]
    fn passthrough_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PassthroughProcessor>();
    }
}
