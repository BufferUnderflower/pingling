//! `LogProcessor` — ensures `log.level` and `log.timestamp` are set.
//! Direct port of the dart equivalent.

use crate::attempt::ConfigRequest;
use crate::pipeline::ConfigProcessor;
use serde_json::{json, Value};

/// Ensures sensible defaults for sing-box's `log` section.
pub struct LogProcessor;

impl LogProcessor {
    /// Construct a fresh processor. Stateless.
    pub fn new() -> Self {
        Self
    }
}

impl Default for LogProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigProcessor for LogProcessor {
    fn name(&self) -> &str {
        "log"
    }

    fn process(&self, mut config: Value, _request: &ConfigRequest) -> Result<Value, String> {
        let root = config
            .as_object_mut()
            .ok_or_else(|| "config root is not an object".to_string())?;
        let log_section = root.entry("log").or_insert_with(|| json!({}));
        let log_section = log_section
            .as_object_mut()
            .ok_or_else(|| "log section is not an object".to_string())?;
        log_section.entry("level").or_insert_with(|| json!("info"));
        log_section
            .entry("timestamp")
            .or_insert_with(|| json!(true));
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempt::AttemptInfo;
    use crate::strategy::{ConnectionStrategy, ResolverType, RetryPolicy, StackType};
    use std::time::Duration;

    fn req() -> ConfigRequest {
        ConfigRequest {
            with_host_dns: false,
            default_dns_server: None,
            attempt: AttemptInfo {
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
        }
    }

    #[test]
    fn sets_default_log_section() {
        let out = LogProcessor::new().process(json!({}), &req()).unwrap();
        assert_eq!(out["log"]["level"], "info");
        assert_eq!(out["log"]["timestamp"], true);
    }

    #[test]
    fn preserves_existing_level() {
        let cfg = json!({"log": {"level": "debug"}});
        let out = LogProcessor::new().process(cfg, &req()).unwrap();
        assert_eq!(out["log"]["level"], "debug");
        assert_eq!(out["log"]["timestamp"], true);
    }
}
