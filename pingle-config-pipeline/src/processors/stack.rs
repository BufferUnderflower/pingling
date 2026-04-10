//! `StackProcessor` — sets `inbounds[type=tun].stack` according to the
//! current strategy. Direct port of the dart equivalent.

use crate::attempt::ConfigRequest;
use crate::pipeline::ConfigProcessor;
use serde_json::Value;

/// Sets `tun.stack` on every TUN inbound based on the current strategy.
pub struct StackProcessor;

impl StackProcessor {
    /// Construct a fresh processor. Stateless.
    pub fn new() -> Self {
        Self
    }
}

impl Default for StackProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigProcessor for StackProcessor {
    fn name(&self) -> &str {
        "stack"
    }

    fn process(&self, mut config: Value, request: &ConfigRequest) -> Result<Value, String> {
        let stack = request.attempt.strategy.stack.as_singbox_str();

        let Some(inbounds) = config.get_mut("inbounds").and_then(|v| v.as_array_mut()) else {
            log::debug!("stack: no inbounds, skipping");
            return Ok(config);
        };

        let mut updated = 0usize;
        for inbound in inbounds.iter_mut() {
            let Some(obj) = inbound.as_object_mut() else {
                continue;
            };
            if obj.get("type").and_then(|v| v.as_str()) == Some("tun") {
                obj.insert("stack".into(), Value::String(stack.into()));
                updated += 1;
            }
        }
        log::debug!("stack: set tun.stack={stack} on {updated} inbounds");
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempt::AttemptInfo;
    use crate::strategy::{ConnectionStrategy, ResolverType, RetryPolicy, StackType};
    use serde_json::json;
    use std::time::Duration;

    fn req(stack: StackType) -> ConfigRequest {
        ConfigRequest {
            with_host_dns: false,
            default_dns_server: None,
            attempt: AttemptInfo {
                strategy: ConnectionStrategy {
                    id: "test".into(),
                    stack,
                    resolver_type: ResolverType::Doh,
                    total_timeout: Duration::from_secs(30),
                    retry: RetryPolicy::NoRetry,
                },
                attempt_number: 1,
                previous_error: None,
            },
        }
    }

    fn cfg() -> Value {
        json!({
            "inbounds": [
                {"type": "tun", "tag": "tun-in"},
                {"type": "mixed", "tag": "mixed-in"}
            ]
        })
    }

    #[test]
    fn sets_system_stack() {
        let out = StackProcessor::new()
            .process(cfg(), &req(StackType::System))
            .unwrap();
        assert_eq!(out["inbounds"][0]["stack"], "system");
        // mixed inbound is untouched
        assert!(out["inbounds"][1].get("stack").is_none());
    }

    #[test]
    fn sets_gvisor_stack() {
        let out = StackProcessor::new()
            .process(cfg(), &req(StackType::GVisor))
            .unwrap();
        assert_eq!(out["inbounds"][0]["stack"], "gvisor");
    }

    #[test]
    fn sets_mixed_stack() {
        let out = StackProcessor::new()
            .process(cfg(), &req(StackType::Mixed))
            .unwrap();
        assert_eq!(out["inbounds"][0]["stack"], "mixed");
    }

    #[test]
    fn no_inbounds_passes_through() {
        let out = StackProcessor::new()
            .process(json!({}), &req(StackType::System))
            .unwrap();
        assert_eq!(out, json!({}));
    }
}
