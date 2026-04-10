//! `PlatformProcessor` — sets `inbounds[type=tun]` platform-specific
//! options based on the host OS. Direct port of the dart equivalent.

use crate::attempt::ConfigRequest;
use crate::pipeline::ConfigProcessor;
use serde_json::Value;

/// Sets per-platform TUN defaults: `auto_route`, `strict_route`, and
/// the OS-specific interface name.
pub struct PlatformProcessor;

impl PlatformProcessor {
    /// Construct a fresh processor. Stateless.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PlatformProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigProcessor for PlatformProcessor {
    fn name(&self) -> &str {
        "platform"
    }

    fn process(&self, mut config: Value, _request: &ConfigRequest) -> Result<Value, String> {
        let Some(inbounds) = config.get_mut("inbounds").and_then(|v| v.as_array_mut()) else {
            return Ok(config);
        };
        for inbound in inbounds.iter_mut() {
            let Some(obj) = inbound.as_object_mut() else {
                continue;
            };
            if obj.get("type").and_then(|v| v.as_str()) != Some("tun") {
                continue;
            }
            // Defaults that work cross-platform.
            obj.entry("auto_route").or_insert_with(|| Value::Bool(true));
            obj.entry("strict_route")
                .or_insert_with(|| Value::Bool(true));
            // Per-platform interface name default — only set if missing.
            if obj.get("interface_name").is_none() {
                let default_name = if cfg!(target_os = "macos") {
                    "utun42"
                } else if cfg!(target_os = "windows") {
                    "pingle-tun"
                } else {
                    "tun0"
                };
                obj.insert(
                    "interface_name".into(),
                    Value::String(default_name.into()),
                );
            }
        }
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
    fn sets_auto_route_and_strict_route_on_tun_inbound() {
        let cfg = json!({"inbounds": [{"type": "tun"}]});
        let out = PlatformProcessor::new().process(cfg, &req()).unwrap();
        assert_eq!(out["inbounds"][0]["auto_route"], true);
        assert_eq!(out["inbounds"][0]["strict_route"], true);
        assert!(out["inbounds"][0]["interface_name"].is_string());
    }

    #[test]
    fn preserves_user_interface_name() {
        let cfg = json!({"inbounds": [{"type": "tun", "interface_name": "user-tun"}]});
        let out = PlatformProcessor::new().process(cfg, &req()).unwrap();
        assert_eq!(out["inbounds"][0]["interface_name"], "user-tun");
    }

    #[test]
    fn ignores_non_tun_inbounds() {
        let cfg = json!({"inbounds": [{"type": "mixed"}]});
        let out = PlatformProcessor::new().process(cfg, &req()).unwrap();
        assert!(out["inbounds"][0].get("auto_route").is_none());
    }
}
