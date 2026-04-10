//! Sing-box config parser — terminal handler for [`OpListOutbounds`].
//!
//! Reads a sing-box JSON configuration file and extracts outbound entries
//! as [`Outbound`] values. This is the canonical example of a core-specific
//! capability: only cores using sing-box register this handler.
//!
//! # How it works
//!
//! Sing-box configs have an `"outbounds"` array where each entry has:
//! ```json
//! { "type": "vless", "tag": "jp-tokyo-1", "server": "1.2.3.4", ... }
//! ```
//!
//! This handler parses the JSON, maps `type` → [`OutboundProtocol`],
//! and extracts the `tag` as the outbound ID.
//!
//! # Not a middleware
//!
//! This is a terminal [`Handler`], not a [`Middleware`]. It lives at the
//! bottom of the pipeline and is the source of truth for the outbound list.
//! Middleware (geo-filter, latency-bias) wraps around it.

use domain::ops::{ListOutboundsInput, ListOutboundsOutput, OpListOutbounds};
use domain::pipeline::Handler;
use domain::types::{Outbound, OutboundProtocol, OutboundTransport};
use domain::VpnError;
use log::warn;

/// Parses a sing-box JSON config to extract outbounds.
///
/// The `config_path` is taken from the pipeline input's `config_path` field,
/// or falls back to the path stored in settings.
pub struct SingboxConfigHandler {
    /// Fallback config path (from settings).
    fallback_config_path: String,
}

impl SingboxConfigHandler {
    pub fn new(config_path: &str) -> Self {
        Self {
            fallback_config_path: config_path.to_string(),
        }
    }

    fn parse_outbounds(json: &str) -> Result<Vec<Outbound>, VpnError> {
        // Minimal JSON parsing without serde — domain is serde-free,
        // but service CAN use serde. We use serde_json here.
        let root: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| VpnError::InvalidConfiguration(format!("JSON parse error: {e}")))?;

        let outbounds = root
            .get("outbounds")
            .and_then(|v| v.as_array())
            .ok_or_else(|| VpnError::InvalidConfiguration("missing 'outbounds' array".into()))?;

        let mut result = Vec::new();
        for entry in outbounds {
            let tag = entry
                .get("tag")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let kind = entry
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let server = entry
                .get("server")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            // Skip internal/meta outbounds
            if tag.is_empty() || kind == "dns" || kind == "block" {
                continue;
            }

            let protocol: OutboundProtocol = kind.parse().unwrap_or(OutboundProtocol::Direct);
            let transport = entry
                .get("transport")
                .and_then(|v| v.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("tcp")
                .parse()
                .unwrap_or(OutboundTransport::Tcp);

            let mut metadata = std::collections::BTreeMap::new();
            if !server.is_empty() {
                metadata.insert("server".into(), server.to_string());
            }
            if let Some(sni) = entry
                .get("tls")
                .and_then(|t| t.get("server_name"))
                .and_then(|v| v.as_str())
            {
                metadata.insert("sni".into(), sni.to_string());
            }

            result.push(Outbound {
                id: tag.to_string(),
                name: tag.to_string(),
                protocol,
                transport,
                country_code: None, // would need a GeoIP lookup or tag convention
                location: None,
                latency_ms: None,
                selected: false,
                metadata,
            });
        }

        Ok(result)
    }
}

impl Handler<OpListOutbounds> for SingboxConfigHandler {
    fn handle(&self, input: ListOutboundsInput) -> Result<ListOutboundsOutput, VpnError> {
        let config_path = input
            .config_path
            .as_deref()
            .unwrap_or(&self.fallback_config_path);

        if config_path.is_empty() {
            return Err(VpnError::InvalidConfiguration(
                "no config path available".into(),
            ));
        }

        let json = std::fs::read_to_string(config_path).map_err(|e| {
            VpnError::InvalidConfiguration(format!("cannot read {config_path}: {e}"))
        })?;

        let outbounds = Self::parse_outbounds(&json).unwrap_or_else(|e| {
            warn!("failed to parse sing-box outbounds: {e}");
            vec![]
        });

        Ok(ListOutboundsOutput {
            outbounds,
            metadata: input.metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CONFIG: &str = r#"{
        "outbounds": [
            {
                "type": "vless",
                "tag": "jp-tokyo-1",
                "server": "1.2.3.4",
                "tls": { "server_name": "example.jp" },
                "transport": { "type": "ws" }
            },
            {
                "type": "vmess",
                "tag": "us-east-1",
                "server": "5.6.7.8"
            },
            {
                "type": "trojan",
                "tag": "de-frankfurt-1",
                "server": "9.10.11.12",
                "transport": { "type": "grpc" }
            },
            {
                "type": "direct",
                "tag": "direct-out"
            },
            {
                "type": "selector",
                "tag": "proxy-select"
            },
            {
                "type": "dns",
                "tag": "dns-out"
            },
            {
                "type": "block",
                "tag": "block-out"
            }
        ]
    }"#;

    #[test]
    fn parses_outbounds_from_json() {
        let outbounds = SingboxConfigHandler::parse_outbounds(SAMPLE_CONFIG).unwrap();

        // dns and block are skipped → 5 remain
        assert_eq!(outbounds.len(), 5);

        let jp = outbounds.iter().find(|o| o.id == "jp-tokyo-1").unwrap();
        assert_eq!(jp.protocol, OutboundProtocol::Vless);
        assert_eq!(jp.transport, OutboundTransport::WebSocket);
        assert_eq!(
            jp.metadata.get("server").map(|s| s.as_str()),
            Some("1.2.3.4")
        );
        assert_eq!(
            jp.metadata.get("sni").map(|s| s.as_str()),
            Some("example.jp")
        );

        let us = outbounds.iter().find(|o| o.id == "us-east-1").unwrap();
        assert_eq!(us.protocol, OutboundProtocol::Vmess);
        assert_eq!(us.transport, OutboundTransport::Tcp); // default

        let de = outbounds.iter().find(|o| o.id == "de-frankfurt-1").unwrap();
        assert_eq!(de.protocol, OutboundProtocol::Trojan);
        assert_eq!(de.transport, OutboundTransport::Grpc);
    }

    #[test]
    fn skips_dns_and_block() {
        let outbounds = SingboxConfigHandler::parse_outbounds(SAMPLE_CONFIG).unwrap();
        assert!(outbounds.iter().all(|o| o.id != "dns-out"));
        assert!(outbounds.iter().all(|o| o.id != "block-out"));
    }

    #[test]
    fn keeps_selector_and_direct() {
        let outbounds = SingboxConfigHandler::parse_outbounds(SAMPLE_CONFIG).unwrap();
        assert!(outbounds.iter().any(|o| o.id == "direct-out"));
        assert!(outbounds.iter().any(|o| o.id == "proxy-select"));
    }

    #[test]
    fn handles_empty_outbounds() {
        let json = r#"{"outbounds": []}"#;
        let outbounds = SingboxConfigHandler::parse_outbounds(json).unwrap();
        assert!(outbounds.is_empty());
    }

    #[test]
    fn handles_missing_outbounds_array() {
        let json = r#"{"dns": {}}"#;
        let result = SingboxConfigHandler::parse_outbounds(json);
        assert!(result.is_err());
    }

    #[test]
    fn handles_invalid_json() {
        let result = SingboxConfigHandler::parse_outbounds("not json");
        assert!(result.is_err());
    }

    #[test]
    fn handler_reads_file() {
        let dir = std::env::temp_dir().join("pingle-test-singbox");
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.json");
        std::fs::write(&config_path, SAMPLE_CONFIG).unwrap();

        let handler = SingboxConfigHandler::new(config_path.to_str().unwrap());
        let output = handler
            .handle(ListOutboundsInput {
                core_type: "sing-box".into(),
                config_path: Some(config_path.to_string_lossy().into()),
                metadata: Default::default(),
            })
            .unwrap();

        assert_eq!(output.outbounds.len(), 5);

        std::fs::remove_dir_all(&dir).ok();
    }
}
