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

use crate::CoreRegistry;
use core_clash_api::ClashApiClient;
use domain::ops::{
    ListOutboundsInput, ListOutboundsOutput, OpListOutbounds, OpSelectOutbound,
    SelectOutboundInput, SelectOutboundOutput,
};
use domain::pipeline::Handler;
use domain::types::{Outbound, OutboundProtocol, OutboundTransport};
use domain::VpnError;
use domain::{Profile, ProfileMeta, ProfileStorage, SettingsStorage};
use log::warn;
use std::sync::{Arc, Mutex};

/// Parses a sing-box JSON config to extract outbounds.
///
/// The `config_path` is taken from the pipeline input's `config_path` field,
/// or falls back to the path stored in settings.
pub struct SingboxConfigHandler {
    /// Fallback config path (from settings).
    fallback_config_path: String,
}

impl SingboxConfigHandler {
    const DEFAULT_SELECTOR_TAG: &'static str = "🌐 Proxy";

    pub fn new(config_path: &str) -> Self {
        Self {
            fallback_config_path: config_path.to_string(),
        }
    }

    fn parse_root(json: &str) -> Result<serde_json::Value, VpnError> {
        serde_json::from_str(json)
            .map_err(|e| VpnError::InvalidConfiguration(format!("JSON parse error: {e}")))
    }

    fn outbounds_array<'a>(
        root: &'a serde_json::Value,
    ) -> Result<&'a [serde_json::Value], VpnError> {
        root.get("outbounds")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .ok_or_else(|| VpnError::InvalidConfiguration("missing 'outbounds' array".into()))
    }

    fn selector_tag(selector_tag: Option<&str>) -> &str {
        selector_tag
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(Self::DEFAULT_SELECTOR_TAG)
    }

    fn selector_index(
        outbounds: &[serde_json::Value],
        selector_tag: Option<&str>,
    ) -> Option<usize> {
        let preferred = Self::selector_tag(selector_tag);
        outbounds
            .iter()
            .enumerate()
            .find(|(_, entry)| {
                matches!(
                    entry.get("type").and_then(|v| v.as_str()),
                    Some("selector" | "urltest")
                ) && entry.get("tag").and_then(|v| v.as_str()) == Some(preferred)
            })
            .map(|(index, _)| index)
            .or_else(|| {
                outbounds.iter().enumerate().find_map(|(index, entry)| {
                    matches!(
                        entry.get("type").and_then(|v| v.as_str()),
                        Some("selector" | "urltest")
                    )
                    .then_some(index)
                })
            })
    }

    fn selector_member_names(selector: &serde_json::Value) -> Vec<String> {
        selector
            .get("outbounds")
            .and_then(|v| v.as_array())
            .into_iter()
            .flat_map(|items| items.iter())
            .filter_map(|value| value.as_str())
            .map(ToOwned::to_owned)
            .collect()
    }

    fn build_outbound(entry: Option<&serde_json::Value>, tag: &str, selected: bool) -> Outbound {
        let kind = entry
            .and_then(|value| value.get("type"))
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let server = entry
            .and_then(|value| value.get("server"))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let protocol: OutboundProtocol = kind.parse().unwrap_or(OutboundProtocol::Direct);
        let transport = entry
            .and_then(|value| value.get("transport"))
            .and_then(|value| value.get("type"))
            .and_then(|value| value.as_str())
            .unwrap_or("tcp")
            .parse()
            .unwrap_or(OutboundTransport::Tcp);
        let mut metadata = std::collections::BTreeMap::new();
        if !server.is_empty() {
            metadata.insert("server".into(), server.to_string());
        }
        Outbound {
            id: tag.to_string(),
            name: tag.to_string(),
            protocol,
            transport,
            country_code: None,
            location: None,
            latency_ms: None,
            selected,
            metadata,
        }
    }

    fn parse_outbounds(json: &str) -> Result<Vec<Outbound>, VpnError> {
        // Minimal JSON parsing without serde — domain is serde-free,
        // but service CAN use serde. We use serde_json here.
        let root = Self::parse_root(json)?;
        let outbounds = Self::outbounds_array(&root)?;

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

    fn parse_selector_outbounds(
        json: &str,
        selector_tag: Option<&str>,
        live_selection: Option<&str>,
    ) -> Result<Vec<Outbound>, VpnError> {
        let root = Self::parse_root(json)?;
        let outbounds = Self::outbounds_array(&root)?;
        let Some(selector_index) = Self::selector_index(outbounds, selector_tag) else {
            return Self::parse_outbounds(json);
        };
        let selector = &outbounds[selector_index];
        let configured_default = selector.get("default").and_then(|value| value.as_str());
        let selected_id = live_selection
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or(configured_default)
            .map(ToOwned::to_owned);
        let members = Self::selector_member_names(selector);
        if members.is_empty() {
            return Self::parse_outbounds(json);
        }
        let mut result = Vec::with_capacity(members.len());
        for member in members {
            let entry = outbounds.iter().find(|value| {
                value.get("tag").and_then(|candidate| candidate.as_str()) == Some(member.as_str())
            });
            result.push(Self::build_outbound(
                entry,
                &member,
                selected_id.as_deref() == Some(member.as_str()),
            ));
        }
        Ok(result)
    }

    fn replace_selector_default(
        json: &str,
        selector_tag: Option<&str>,
        outbound_id: &str,
    ) -> Result<serde_json::Value, VpnError> {
        let mut root = Self::parse_root(json)?;
        let outbounds = root
            .get_mut("outbounds")
            .and_then(|value| value.as_array_mut())
            .ok_or_else(|| VpnError::InvalidConfiguration("missing 'outbounds' array".into()))?;
        let selector_index = Self::selector_index(outbounds, selector_tag).ok_or_else(|| {
            VpnError::OutboundNotFound(format!(
                "selector '{}' not found",
                Self::selector_tag(selector_tag)
            ))
        })?;
        let selector = &mut outbounds[selector_index];
        let members = Self::selector_member_names(selector);
        if !members.iter().any(|candidate| candidate == outbound_id) {
            return Err(VpnError::OutboundNotFound(outbound_id.to_string()));
        }
        selector["default"] = serde_json::Value::String(outbound_id.to_string());
        Ok(root)
    }

    fn clash_controller(root: &serde_json::Value) -> Option<String> {
        root.get("experimental")
            .and_then(|value| value.get("clash_api"))
            .and_then(|value| value.get("external_controller"))
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
    }

    fn live_selected_outbound(
        json: &str,
        selector_tag: Option<&str>,
    ) -> Result<Option<String>, VpnError> {
        let root = Self::parse_root(json)?;
        let Some(controller) = Self::clash_controller(&root) else {
            return Ok(None);
        };
        clash_client(&controller)?
            .get_active_proxy(Self::selector_tag(selector_tag))
            .map_err(map_clash_error)
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

        let live_selection = Self::live_selected_outbound(&json, None).ok().flatten();
        let outbounds = Self::parse_selector_outbounds(&json, None, live_selection.as_deref())
            .unwrap_or_else(|e| {
                warn!("failed to parse sing-box outbounds: {e}");
                vec![]
            });

        Ok(ListOutboundsOutput {
            outbounds,
            metadata: input.metadata,
        })
    }
}

struct SourceConfig {
    json: String,
    legacy_path: Option<String>,
    profile: Option<Profile>,
}

pub struct SingboxSelectOutboundHandler {
    registry: Arc<Mutex<CoreRegistry>>,
    storage: Arc<Mutex<Box<dyn SettingsStorage>>>,
    profile_storage: Option<Arc<dyn ProfileStorage>>,
    selector_tag: String,
}

impl SingboxSelectOutboundHandler {
    pub fn new(
        registry: Arc<Mutex<CoreRegistry>>,
        storage: Arc<Mutex<Box<dyn SettingsStorage>>>,
        profile_storage: Option<Arc<dyn ProfileStorage>>,
    ) -> Self {
        Self {
            registry,
            storage,
            profile_storage,
            selector_tag: SingboxConfigHandler::DEFAULT_SELECTOR_TAG.into(),
        }
    }

    fn read_source_config(&self) -> Result<SourceConfig, VpnError> {
        if let Some(store) = self.profile_storage.as_ref() {
            if let Some(active_id) = store.active()? {
                let meta = store.get_meta(&active_id)?.ok_or_else(|| {
                    VpnError::StorageError(format!("active profile {active_id} not found"))
                })?;
                let temp = store.load_active_for_core_start()?;
                let json = std::fs::read_to_string(temp.path()).map_err(|e| {
                    VpnError::StorageError(format!(
                        "read active profile config {}: {e}",
                        temp.path().display()
                    ))
                })?;
                return Ok(SourceConfig {
                    json,
                    legacy_path: None,
                    profile: Some(profile_from_meta(meta)),
                });
            }
        }

        let legacy_path = self
            .storage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_string("config_path")?
            .ok_or_else(|| {
                VpnError::InvalidConfiguration("config_path not found in settings".into())
            })?;
        let json = std::fs::read_to_string(&legacy_path).map_err(|e| {
            VpnError::StorageError(format!("read legacy config {legacy_path}: {e}"))
        })?;
        Ok(SourceConfig {
            json,
            legacy_path: Some(legacy_path),
            profile: None,
        })
    }

    fn persist_source_config(
        &self,
        source: SourceConfig,
        updated: &serde_json::Value,
    ) -> Result<(), VpnError> {
        let rendered = serde_json::to_string_pretty(updated)
            .map_err(|e| VpnError::StorageError(format!("serialize selector config: {e}")))?;
        if let Some(profile) = source.profile {
            let store = self
                .profile_storage
                .as_ref()
                .ok_or_else(|| VpnError::StorageError("profile storage not configured".into()))?;
            store.put(profile, &rendered)?;
            return Ok(());
        }
        if let Some(path) = source.legacy_path {
            std::fs::write(&path, rendered)
                .map_err(|e| VpnError::StorageError(format!("write legacy config {path}: {e}")))?;
            return Ok(());
        }
        Err(VpnError::InvalidConfiguration(
            "no writable config source available".into(),
        ))
    }

    fn maybe_apply_live_selection(
        &self,
        config_path: Option<&str>,
        outbound_id: &str,
    ) -> Result<(), VpnError> {
        let running = self
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_core()
            .map(|core| core.running())
            .unwrap_or(false);
        if !running {
            return Ok(());
        }
        let Some(path) = config_path.filter(|value| !value.trim().is_empty()) else {
            return Ok(());
        };
        let json = std::fs::read_to_string(path)
            .map_err(|e| VpnError::StorageError(format!("read runtime config {path}: {e}")))?;
        let root = SingboxConfigHandler::parse_root(&json)?;
        let controller = SingboxConfigHandler::clash_controller(&root)
            .unwrap_or_else(|| "127.0.0.1:9090".into());
        clash_client(&controller)?
            .set_active_proxy(&self.selector_tag, outbound_id)
            .map_err(map_clash_error)
    }
}

impl Handler<OpSelectOutbound> for SingboxSelectOutboundHandler {
    fn handle(&self, input: SelectOutboundInput) -> Result<SelectOutboundOutput, VpnError> {
        let source = self.read_source_config()?;
        let updated = SingboxConfigHandler::replace_selector_default(
            &source.json,
            Some(&self.selector_tag),
            &input.outbound_id,
        )?;
        self.persist_source_config(source, &updated)?;
        self.maybe_apply_live_selection(input.config_path.as_deref(), &input.outbound_id)?;
        Ok(SelectOutboundOutput {
            metadata: input.metadata,
        })
    }
}

fn profile_from_meta(meta: ProfileMeta) -> Profile {
    Profile {
        id: meta.id,
        name: meta.name,
        core_type: meta.core_type,
        source: meta.source,
        metadata: meta.metadata,
        created_at: meta.created_at,
        last_used_at: meta.last_used_at,
    }
}

fn clash_client(controller: &str) -> Result<ClashApiClient, VpnError> {
    ClashApiClient::new(controller).map_err(|error| {
        VpnError::InvalidConfiguration(format!("invalid clash controller '{controller}': {error}"))
    })
}

fn map_clash_error(error: core_clash_api::ClashApiError) -> VpnError {
    VpnError::Unknown(format!("clash api: {error}"))
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

    const SAMPLE_SELECTOR_CONFIG: &str = r#"{
        "outbounds": [
            { "type": "direct", "tag": "↔️ Direct" },
            {
                "type": "selector",
                "tag": "🌐 Proxy",
                "default": "🇳🇱 Netherlands",
                "interrupt_exist_connections": true,
                "outbounds": [
                    "🇳🇱 Netherlands",
                    "🇩🇪 Germany"
                ]
            },
            { "type": "vless", "tag": "🇳🇱 Netherlands", "server": "nl.example.com" },
            { "type": "vless", "tag": "🇩🇪 Germany", "server": "de.example.com" }
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
    fn selector_members_use_config_default_when_live_state_is_missing() {
        let outbounds = SingboxConfigHandler::parse_selector_outbounds(
            SAMPLE_SELECTOR_CONFIG,
            Some("🌐 Proxy"),
            None,
        )
        .unwrap();

        assert_eq!(outbounds.len(), 2);
        assert_eq!(outbounds[0].id, "🇳🇱 Netherlands");
        assert!(outbounds[0].selected);
        assert!(!outbounds[1].selected);
    }

    #[test]
    fn selector_members_prefer_live_selection_over_config_default() {
        let outbounds = SingboxConfigHandler::parse_selector_outbounds(
            SAMPLE_SELECTOR_CONFIG,
            Some("🌐 Proxy"),
            Some("🇩🇪 Germany"),
        )
        .unwrap();

        assert!(!outbounds[0].selected);
        assert!(outbounds[1].selected);
    }

    #[test]
    fn replacing_selector_default_updates_only_the_requested_selector() {
        let updated = SingboxConfigHandler::replace_selector_default(
            SAMPLE_SELECTOR_CONFIG,
            Some("🌐 Proxy"),
            "🇩🇪 Germany",
        )
        .unwrap();

        assert_eq!(
            updated["outbounds"][1]["default"],
            serde_json::Value::String("🇩🇪 Germany".into())
        );
        assert_eq!(
            updated["outbounds"][2]["tag"],
            serde_json::Value::String("🇳🇱 Netherlands".into())
        );
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
