//! `ClashApiProcessor` — ensures `experimental.clash_api.external_controller`
//! is set so the daemon can talk to the running sing-box. Default port
//! 9090 unless already set.

use crate::attempt::ConfigRequest;
use crate::pipeline::ConfigProcessor;
use serde_json::{json, Value};

/// Ensures the clash-api external_controller default is set.
pub struct ClashApiProcessor;

impl ClashApiProcessor {
    /// Construct a fresh processor. Stateless.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClashApiProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigProcessor for ClashApiProcessor {
    fn name(&self) -> &str {
        "clash_api"
    }

    fn process(&self, mut config: Value, _request: &ConfigRequest) -> Result<Value, String> {
        let root = config
            .as_object_mut()
            .ok_or_else(|| "config root is not an object".to_string())?;
        let exp = root.entry("experimental").or_insert_with(|| json!({}));
        let exp = exp
            .as_object_mut()
            .ok_or_else(|| "experimental section is not an object".to_string())?;
        let clash = exp.entry("clash_api").or_insert_with(|| json!({}));
        let clash = clash
            .as_object_mut()
            .ok_or_else(|| "clash_api is not an object".to_string())?;
        clash
            .entry("external_controller")
            .or_insert_with(|| json!("127.0.0.1:9090"));
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
    fn sets_default_external_controller() {
        let out = ClashApiProcessor::new().process(json!({}), &req()).unwrap();
        assert_eq!(
            out["experimental"]["clash_api"]["external_controller"],
            "127.0.0.1:9090"
        );
    }

    #[test]
    fn preserves_existing_external_controller() {
        let cfg = json!({
            "experimental": {"clash_api": {"external_controller": "127.0.0.1:8888"}}
        });
        let out = ClashApiProcessor::new().process(cfg, &req()).unwrap();
        assert_eq!(
            out["experimental"]["clash_api"]["external_controller"],
            "127.0.0.1:8888"
        );
    }
}
