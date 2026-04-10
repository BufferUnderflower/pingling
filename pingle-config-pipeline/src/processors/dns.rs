//! `DnsProcessor` — port of the dart `DnsProcessor`.
//!
//! Mutates `dns.servers[tag=dns-local]` according to the request:
//!
//! - `with_host_dns=true`  → `type=local`, remove `server` field.
//! - `with_host_dns=false` and `type != "local"` → ensure `server`
//!   field is set; fall back to `request.default_dns_server`
//!   (or `"8.8.8.8"`) if missing.
//! - `with_host_dns=false` and `type == "local"` → no change.
//!
//! No-op (with a debug log) when the config has no `dns` section, no
//! `dns.servers` array, or no server tagged `dns-local`.

use crate::attempt::ConfigRequest;
use crate::pipeline::ConfigProcessor;
use serde_json::Value;

/// Default DNS server when `with_host_dns=false` and the existing
/// `dns-local` server has no `server` field set and the request also
/// has no `default_dns_server`.
pub const DEFAULT_DNS_SERVER: &str = "8.8.8.8";

/// DNS processor — see module-level doc for the rules.
pub struct DnsProcessor;

impl DnsProcessor {
    /// Construct a fresh DNS processor. Stateless.
    pub fn new() -> Self {
        Self
    }
}

impl Default for DnsProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigProcessor for DnsProcessor {
    fn name(&self) -> &str {
        "dns"
    }

    fn process(&self, mut config: Value, request: &ConfigRequest) -> Result<Value, String> {
        let Some(dns) = config.get_mut("dns").and_then(|v| v.as_object_mut()) else {
            log::debug!("dns: no dns section, skipping");
            return Ok(config);
        };
        let Some(servers) = dns.get_mut("servers").and_then(|v| v.as_array_mut()) else {
            log::debug!("dns: no dns.servers array, skipping");
            return Ok(config);
        };

        let dns_local = servers.iter_mut().find(|server| {
            server
                .as_object()
                .and_then(|o| o.get("tag"))
                .and_then(|t| t.as_str())
                == Some("dns-local")
        });

        let Some(dns_local) = dns_local else {
            log::debug!("dns: no server tagged dns-local, skipping");
            return Ok(config);
        };
        let Some(dns_local) = dns_local.as_object_mut() else {
            return Ok(config);
        };

        if request.with_host_dns {
            dns_local.insert("type".into(), Value::String("local".into()));
            dns_local.remove("server");
            log::debug!("dns: with_host_dns=true, type=local, server removed");
        } else {
            let current_type = dns_local
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if current_type != "local" && !current_type.is_empty() {
                let server_present = dns_local
                    .get("server")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
                if !server_present {
                    let fallback = request
                        .default_dns_server
                        .clone()
                        .unwrap_or_else(|| DEFAULT_DNS_SERVER.into());
                    dns_local.insert("server".into(), Value::String(fallback));
                    log::debug!("dns: added missing server field");
                }
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

    fn req(with_host_dns: bool, default_dns: Option<&str>) -> ConfigRequest {
        ConfigRequest {
            with_host_dns,
            default_dns_server: default_dns.map(|s| s.into()),
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

    fn config_with_dns_local(initial_type: &str, server: Option<&str>) -> Value {
        let mut server_obj = json!({"tag": "dns-local", "type": initial_type});
        if let Some(s) = server {
            server_obj["server"] = Value::String(s.into());
        }
        json!({
            "dns": {
                "servers": [server_obj]
            }
        })
    }

    #[test]
    fn with_host_dns_true_sets_local_and_removes_server() {
        let p = DnsProcessor::new();
        let cfg = config_with_dns_local("https", Some("https://1.1.1.1/dns-query"));
        let out = p.process(cfg, &req(true, None)).unwrap();
        let dns_local = &out["dns"]["servers"][0];
        assert_eq!(dns_local["type"], "local");
        assert!(dns_local.get("server").is_none());
    }

    #[test]
    fn with_host_dns_false_adds_default_server_when_missing() {
        let p = DnsProcessor::new();
        let cfg = config_with_dns_local("https", None);
        let out = p.process(cfg, &req(false, None)).unwrap();
        assert_eq!(out["dns"]["servers"][0]["server"], DEFAULT_DNS_SERVER);
    }

    #[test]
    fn with_host_dns_false_uses_request_default_dns_server() {
        let p = DnsProcessor::new();
        let cfg = config_with_dns_local("https", None);
        let out = p.process(cfg, &req(false, Some("9.9.9.9"))).unwrap();
        assert_eq!(out["dns"]["servers"][0]["server"], "9.9.9.9");
    }

    #[test]
    fn with_host_dns_false_preserves_existing_server() {
        let p = DnsProcessor::new();
        let cfg = config_with_dns_local("https", Some("https://existing.example/dns-query"));
        let out = p.process(cfg, &req(false, None)).unwrap();
        assert_eq!(
            out["dns"]["servers"][0]["server"],
            "https://existing.example/dns-query"
        );
    }

    #[test]
    fn type_already_local_no_change() {
        let p = DnsProcessor::new();
        let cfg = config_with_dns_local("local", None);
        let out = p.process(cfg.clone(), &req(false, None)).unwrap();
        assert_eq!(out, cfg);
    }

    #[test]
    fn no_dns_section_passes_through() {
        let p = DnsProcessor::new();
        let cfg = json!({"outbounds": []});
        let out = p.process(cfg.clone(), &req(false, None)).unwrap();
        assert_eq!(out, cfg);
    }

    #[test]
    fn no_dns_local_server_passes_through() {
        let p = DnsProcessor::new();
        let cfg = json!({"dns": {"servers": [{"tag": "google", "type": "https"}]}});
        let out = p.process(cfg.clone(), &req(true, None)).unwrap();
        assert_eq!(out, cfg);
    }
}
