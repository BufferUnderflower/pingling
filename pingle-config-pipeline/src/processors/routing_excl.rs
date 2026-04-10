//! `RoutingExclusionsProcessor` — adds RFC1918 / link-local / loopback
//! prefixes as `direct` route rules so LAN traffic doesn't accidentally
//! route through the tunnel.
//!
//! Direct port of the dart `RoutingExclusionsProcessor`. Idempotent: if
//! a rule with the same `tag` is already present, leaves it alone.

use crate::attempt::ConfigRequest;
use crate::pipeline::ConfigProcessor;
use serde_json::{json, Value};

const EXCLUSION_TAG: &str = "pingle-lan-exclusions";

/// CIDR prefixes to exclude. Standard private address space + loopback
/// + link-local for both IPv4 and IPv6.
const EXCLUSIONS: &[&str] = &[
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "::1/128",
    "fe80::/10",
    "fc00::/7",
];

/// Adds standard LAN exclusions to `route.rules` so traffic to private
/// address space stays on the host network.
pub struct RoutingExclusionsProcessor;

impl RoutingExclusionsProcessor {
    /// Construct a fresh processor. Stateless.
    pub fn new() -> Self {
        Self
    }
}

impl Default for RoutingExclusionsProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigProcessor for RoutingExclusionsProcessor {
    fn name(&self) -> &str {
        "routing_excl"
    }

    fn process(&self, mut config: Value, _request: &ConfigRequest) -> Result<Value, String> {
        let route = config
            .as_object_mut()
            .ok_or_else(|| "config root is not an object".to_string())?
            .entry("route")
            .or_insert_with(|| json!({}));
        let route = route
            .as_object_mut()
            .ok_or_else(|| "route section is not an object".to_string())?;
        let rules = route.entry("rules").or_insert_with(|| json!([]));
        let rules = rules
            .as_array_mut()
            .ok_or_else(|| "route.rules is not an array".to_string())?;

        // If a rule with our tag already exists, leave it alone.
        let already_present = rules
            .iter()
            .any(|r| r.get("tag").and_then(|t| t.as_str()) == Some(EXCLUSION_TAG));
        if already_present {
            log::debug!("routing_excl: rule already present, skipping");
            return Ok(config);
        }

        // Insert at the front so it takes precedence over other rules.
        let new_rule = json!({
            "tag": EXCLUSION_TAG,
            "ip_cidr": EXCLUSIONS,
            "outbound": "direct"
        });
        rules.insert(0, new_rule);
        log::debug!("routing_excl: added {} exclusions", EXCLUSIONS.len());
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

    #[test]
    fn adds_rule_to_empty_route() {
        let p = RoutingExclusionsProcessor::new();
        let out = p.process(json!({}), &req()).unwrap();
        let rules = out["route"]["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["tag"], EXCLUSION_TAG);
        assert_eq!(rules[0]["outbound"], "direct");
        let cidrs = rules[0]["ip_cidr"].as_array().unwrap();
        assert_eq!(cidrs.len(), EXCLUSIONS.len());
    }

    #[test]
    fn idempotent_when_rule_already_present() {
        let p = RoutingExclusionsProcessor::new();
        let cfg = json!({
            "route": {
                "rules": [
                    {"tag": EXCLUSION_TAG, "ip_cidr": ["10.0.0.0/8"], "outbound": "direct"}
                ]
            }
        });
        let out = p.process(cfg.clone(), &req()).unwrap();
        assert_eq!(out, cfg);
    }

    #[test]
    fn inserts_at_front_so_takes_precedence() {
        let p = RoutingExclusionsProcessor::new();
        let cfg = json!({
            "route": {
                "rules": [
                    {"tag": "user-rule", "outbound": "proxy"}
                ]
            }
        });
        let out = p.process(cfg, &req()).unwrap();
        let rules = out["route"]["rules"].as_array().unwrap();
        assert_eq!(rules[0]["tag"], EXCLUSION_TAG);
        assert_eq!(rules[1]["tag"], "user-rule");
    }
}
