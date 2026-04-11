//! `pingle://` deep-link handling.
//!
//! Glue between the OS-delivered deep-link URL and the encrypted
//! profile store. Parses the URL, dispatches to either a
//! plugin-provided resolver or the built-in fallback, and (based on
//! the resolution's `next_action` hint) stores the profile, optionally
//! activates it, and optionally kicks off `vpn.connect`.
//!
//! # Flow
//!
//! 1. OS delivers a URL like `pingle://import?token=eyJ...` to the
//!    Tauri host binary via `tauri-plugin-deep-link`.
//! 2. `app/main.rs` forwards it to the IPC layer as a `deeplink.handle`
//!    JSON-RPC call (same dispatch table as every other method — so
//!    a client can also trigger a deeplink import programmatically
//!    for testing).
//! 3. The IPC dispatcher routes to [`handle_deeplink`] here.
//! 4. [`parse_request`] extracts scheme / action / subpath / query.
//! 5. [`resolve`] tries the loaded plugin's `deeplink.resolve` method
//!    first. When the plugin returns `{"kind":"unhandled"}` (or no
//!    plugin is loaded), [`builtin_resolve`] takes over.
//! 6. [`apply_resolution`] handles storage + activation + connect
//!    based on the resolution's `next_action`.
//!
//! # Built-in resolver scope
//!
//! The built-in resolver understands exactly one URL shape:
//!
//! ```text
//! pingle://import?name=<str>&core=<str>&config=<base64>&activate=<bool>
//! ```
//!
//! This is enough for the OSS build to import configs shared via
//! "copy this URL and send it to a friend" flows, without any
//! vendor-specific plugin. The vendor plugin extends the set of
//! understood URLs (token-based, magic-link, etc.) by claiming the
//! `import`, `auth`, and any other actions in its
//! `deeplink.capabilities` response.

use base64::Engine;
use domain::{Profile, ProfileSource, VpnError};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use service::VpnManager;

/// Wire-version this module speaks. Bumped only on breaking changes
/// to the `deeplink.resolve` plugin protocol.
pub const DEEPLINK_WIRE_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Request + Resolution types
// ---------------------------------------------------------------------------

/// Parsed `pingle://` URL in structured form. Forwarded to plugins
/// via their `deeplink.resolve` method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeeplinkRequest {
    /// The full URL as received from the OS.
    pub raw_url: String,
    /// The action keyword — first segment after the scheme.
    pub action: String,
    /// Path segments after the action.
    #[serde(default)]
    pub subpath: Vec<String>,
    /// Decoded query parameters.
    #[serde(default)]
    pub query: BTreeMap<String, String>,
}

/// What the daemon should do after resolving a deeplink.
///
/// The resolver (plugin or built-in) returns one of these variants.
/// [`apply_resolution`] then executes the indicated side effects.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeeplinkResolution {
    /// The URL carried (or resolved to) a profile. The daemon stores
    /// it and optionally activates + connects per `next_action`.
    Profile {
        name: String,
        #[serde(rename = "coreType", alias = "core_type", default = "default_core_type")]
        core_type: String,
        config: String,
        #[serde(default)]
        metadata: BTreeMap<String, String>,
        #[serde(default)]
        next_action: DeeplinkNextAction,
    },
    /// The URL was an auth-only link. Plugin updated its internal
    /// session; daemon does not store a profile.
    Auth {
        #[serde(default)]
        session: serde_json::Value,
    },
    /// Plugin recognized the URL but couldn't complete the resolution.
    Error {
        message: String,
        #[serde(default)]
        recoverable: bool,
    },
    /// Plugin does not claim this URL — daemon falls back to built-in.
    Unhandled,
}

/// What the daemon should do after storing a deeplink-imported profile.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeeplinkNextAction {
    /// Store the profile but don't activate it. Safest default.
    #[default]
    StoreOnly,
    /// Store and make it the active profile, but don't connect yet.
    Activate,
    /// Store, activate, and call `vpn.connect`.
    ActivateAndConnect,
}

fn default_core_type() -> String {
    "sing-box".to_string()
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a `pingle://...` URL into a [`DeeplinkRequest`].
///
/// Accepts any scheme prefix for resilience — the IPC caller already
/// checks that the URL is a `pingle://` link before calling us. We DO
/// require a non-empty action segment.
///
/// # Errors
/// - `VpnError::InvalidConfiguration` on malformed URL.
pub fn parse_request(url: &str) -> Result<DeeplinkRequest, VpnError> {
    let (_scheme, rest) = url.split_once("://").ok_or_else(|| {
        VpnError::InvalidConfiguration(format!("deeplink url missing scheme separator: {url}"))
    })?;

    let (path_part, query_part) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };

    let mut segments = path_part
        .split('/')
        .filter(|s| !s.is_empty())
        .map(decode_segment);
    let action = segments.next().ok_or_else(|| {
        VpnError::InvalidConfiguration(format!("deeplink url has no action: {url}"))
    })?;
    let subpath: Vec<String> = segments.collect();

    let mut query = BTreeMap::new();
    if let Some(q) = query_part {
        for pair in q.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (k, v) = match pair.split_once('=') {
                Some((k, v)) => (decode_segment(k), decode_segment(v)),
                None => (decode_segment(pair), String::new()),
            };
            query.insert(k, v);
        }
    }

    Ok(DeeplinkRequest {
        raw_url: url.to_string(),
        action,
        subpath,
        query,
    })
}

fn decode_segment(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Built-in resolver
// ---------------------------------------------------------------------------

/// Built-in fallback resolver for `pingle://` URLs.
///
/// Handles exactly one action — `import` — with a base64-encoded
/// inline config. Returns [`DeeplinkResolution::Unhandled`] for any
/// other action.
pub fn builtin_resolve(req: &DeeplinkRequest) -> DeeplinkResolution {
    if req.action != "import" {
        return DeeplinkResolution::Unhandled;
    }

    let config_b64 = match req.query.get("config") {
        Some(v) if !v.is_empty() => v,
        _ => {
            return DeeplinkResolution::Error {
                message: "pingle://import requires a `config` query param \
                          (base64-encoded config JSON)"
                    .to_string(),
                recoverable: false,
            }
        }
    };

    let config_bytes = match base64::engine::general_purpose::STANDARD.decode(config_b64) {
        Ok(b) => b,
        Err(_) => match base64::engine::general_purpose::URL_SAFE.decode(config_b64) {
            Ok(b) => b,
            Err(e) => {
                return DeeplinkResolution::Error {
                    message: format!("config param is not valid base64: {e}"),
                    recoverable: false,
                }
            }
        },
    };

    let config_json = match String::from_utf8(config_bytes) {
        Ok(s) => s,
        Err(e) => {
            return DeeplinkResolution::Error {
                message: format!("decoded config is not valid UTF-8: {e}"),
                recoverable: false,
            }
        }
    };

    let name = req
        .query
        .get("name")
        .cloned()
        .unwrap_or_else(|| "Imported profile".to_string());
    let core_type = req
        .query
        .get("core")
        .cloned()
        .unwrap_or_else(default_core_type);
    let activate = req
        .query
        .get("activate")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    let mut metadata = BTreeMap::new();
    metadata.insert("source".to_string(), "pingle-builtin-deeplink".to_string());

    DeeplinkResolution::Profile {
        name,
        core_type,
        config: config_json,
        metadata,
        next_action: if activate {
            DeeplinkNextAction::ActivateAndConnect
        } else {
            DeeplinkNextAction::StoreOnly
        },
    }
}

// ---------------------------------------------------------------------------
// Plugin resolver
// ---------------------------------------------------------------------------

/// Payload exchanged with the plugin over the `deeplink.resolve`
/// slot chain. Carried inside [`domain::SlotContext::payload`] for
/// each phase (`before`, `exec`, `after`) and returned inside any
/// [`domain::SlotOutcome::Continue`] / [`domain::SlotOutcome::Halt`].
///
/// `resolution` starts `None` from the host and is filled in by
/// whichever plugin phase decides to own the deeplink — typically
/// `exec`. `before` plugins can mutate `request` (e.g. rewrite a
/// legacy query string into a new shape); `after` plugins can
/// observe the final resolution for telemetry or persist stats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeeplinkResolvePayload {
    /// Original deeplink request, parsed from the pingle:// URL.
    pub request: DeeplinkRequest,
    /// Stable daemon install id — lets plugins match a deeplink
    /// against a specific installation (e.g. a login token generated
    /// for this device).
    pub install_id: String,
    /// Current platform: `"windows"`, `"macos"`, `"linux"`, ...
    /// Plugins that emit platform-specific imports use this.
    pub platform: String,
    /// Resolution chosen by a phase (initially `None`). If a phase
    /// returns `Continue { payload }` with this field populated, the
    /// host uses it as the deeplink outcome after the chain completes.
    pub resolution: Option<DeeplinkResolution>,
}

/// Ask the loaded plugin (if any) to resolve the URL.
///
/// Dispatches through the canonical [`domain::slot_names::DEEPLINK_RESOLVE`]
/// slot chain first. If no phase of the chain claims the slot, falls
/// back to the legacy flat `deeplink.resolve` method name for plugins
/// that haven't yet adopted the slot convention. Any plugin error
/// (wire, serde, or explicit) is converted to
/// [`DeeplinkResolution::Unhandled`] so the built-in resolver gets a
/// chance — plugins should NEVER break the deeplink path.
pub fn plugin_resolve(
    vpn: &VpnManager,
    req: &DeeplinkRequest,
) -> Option<DeeplinkResolution> {
    let plugin = vpn.plugin()?;
    let install_id = vpn.install_id().unwrap_or_default();

    // Try the slot-chain convention first.
    let payload = DeeplinkResolvePayload {
        request: req.clone(),
        install_id: install_id.clone(),
        platform: std::env::consts::OS.to_string(),
        resolution: None,
    };
    let invocation_id = domain::new_invocation_id();
    match domain::run_slot_chain(
        plugin.as_ref(),
        domain::slot_names::DEEPLINK_RESOLVE,
        DEEPLINK_WIRE_VERSION,
        &invocation_id,
        payload,
    ) {
        Ok(Some(final_payload)) => {
            // Chain handled it. The plugin either filled `resolution`
            // in one of the phases, or left it `None` (observed but
            // didn't claim) — treat the latter as Unhandled so the
            // builtin resolver picks up.
            return Some(
                final_payload
                    .resolution
                    .unwrap_or(DeeplinkResolution::Unhandled),
            );
        }
        Err(e) => {
            log::warn!("plugin deeplink.resolve slot chain errored: {e}");
            return Some(DeeplinkResolution::Unhandled);
        }
        Ok(None) => {
            // Fall through to legacy dispatch below. Covers
            // pre-slot plugins still in the field.
        }
    }

    // Legacy fallback: single-method dispatch with the pre-slot wire
    // shape. Dropped once every plugin has migrated.
    let legacy_input = serde_json::json!({
        "wire_version": DEEPLINK_WIRE_VERSION,
        "request": req,
        "install_id": install_id,
        "platform": std::env::consts::OS,
    });
    match plugin.handle_ipc("deeplink.resolve", &legacy_input) {
        Some(Ok(value)) => match serde_json::from_value::<DeeplinkResolution>(value.clone()) {
            Ok(res) => Some(res),
            Err(e) => {
                log::warn!(
                    "plugin deeplink.resolve (legacy) returned unparseable value: {e} (raw: {value})"
                );
                Some(DeeplinkResolution::Unhandled)
            }
        },
        Some(Err(e)) => {
            log::warn!("plugin deeplink.resolve (legacy) errored: {e}");
            Some(DeeplinkResolution::Unhandled)
        }
        None => {
            log::debug!("plugin does not claim deeplink.resolve (slot chain or legacy)");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Resolution dispatch
// ---------------------------------------------------------------------------

/// Resolver order: plugin first, then built-in.
pub fn resolve(vpn: &VpnManager, req: &DeeplinkRequest) -> DeeplinkResolution {
    if let Some(res) = plugin_resolve(vpn, req) {
        if !matches!(res, DeeplinkResolution::Unhandled) {
            return res;
        }
    }
    builtin_resolve(req)
}

// ---------------------------------------------------------------------------
// Apply resolution — the side-effect step
// ---------------------------------------------------------------------------

/// Outcome summary returned from [`handle_deeplink`] to the IPC caller.
#[derive(Debug, Clone, Serialize)]
pub struct DeeplinkOutcome {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub message: String,
}

/// Apply a [`DeeplinkResolution`] to the daemon state.
pub fn apply_resolution(
    vpn: &Arc<VpnManager>,
    req: &DeeplinkRequest,
    resolution: DeeplinkResolution,
) -> Result<DeeplinkOutcome, VpnError> {
    match resolution {
        DeeplinkResolution::Profile {
            name,
            core_type,
            config,
            metadata,
            next_action,
        } => {
            let profile = Profile {
                id: String::new(),
                name: name.clone(),
                core_type,
                source: ProfileSource::Deeplink {
                    url: req.raw_url.clone(),
                },
                metadata,
                created_at: SystemTime::now(),
                last_used_at: None,
            };
            let stored = vpn.put_profile(profile, &config)?;
            let id = stored.id.clone();
            let base_message = format!("Stored profile \"{name}\" ({id})");
            match next_action {
                DeeplinkNextAction::StoreOnly => Ok(DeeplinkOutcome {
                    kind: "profile_stored".into(),
                    profile_id: Some(id),
                    message: base_message,
                }),
                DeeplinkNextAction::Activate => {
                    vpn.set_active_profile(&id)?;
                    Ok(DeeplinkOutcome {
                        kind: "profile_activated".into(),
                        profile_id: Some(id),
                        message: format!("{base_message} (activated)"),
                    })
                }
                DeeplinkNextAction::ActivateAndConnect => {
                    vpn.set_active_profile(&id)?;
                    match vpn.connect() {
                        Ok(_) => Ok(DeeplinkOutcome {
                            kind: "profile_connected".into(),
                            profile_id: Some(id),
                            message: format!("{base_message} (activated + connected)"),
                        }),
                        Err(e) => Ok(DeeplinkOutcome {
                            kind: "profile_activated".into(),
                            profile_id: Some(id),
                            message: format!(
                                "{base_message} (activated, connect failed: {e})"
                            ),
                        }),
                    }
                }
            }
        }
        DeeplinkResolution::Auth { session } => Ok(DeeplinkOutcome {
            kind: "auth".into(),
            profile_id: None,
            message: format!("Authentication completed: {session}"),
        }),
        DeeplinkResolution::Error {
            message,
            recoverable,
        } => Ok(DeeplinkOutcome {
            kind: "error".into(),
            profile_id: None,
            message: if recoverable {
                format!("{message} (recoverable)")
            } else {
                message
            },
        }),
        DeeplinkResolution::Unhandled => Ok(DeeplinkOutcome {
            kind: "unhandled".into(),
            profile_id: None,
            message: format!(
                "No handler claimed this deeplink URL (action: {})",
                req.action
            ),
        }),
    }
}

/// Top-level entry point. Called by the IPC dispatcher when a
/// `deeplink.handle` JSON-RPC method arrives.
pub fn handle_deeplink(vpn: &Arc<VpnManager>, url: &str) -> Result<DeeplinkOutcome, VpnError> {
    let req = parse_request(url)?;
    log::info!(
        "deeplink: received url (action={}, query_keys={:?})",
        req.action,
        req.query.keys().collect::<Vec<_>>()
    );
    let resolution = resolve(vpn, &req);
    apply_resolution(vpn, &req, resolution)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_request tests -------------------------------------------------

    #[test]
    fn parse_simple_import_url() {
        let req = parse_request("pingle://import?name=Home&config=abc").unwrap();
        assert_eq!(req.action, "import");
        assert_eq!(req.subpath, Vec::<String>::new());
        assert_eq!(req.query.get("name"), Some(&"Home".to_string()));
        assert_eq!(req.query.get("config"), Some(&"abc".to_string()));
    }

    #[test]
    fn parse_url_with_subpath() {
        let req = parse_request("pingle://import/hub/v1?token=xyz").unwrap();
        assert_eq!(req.action, "import");
        assert_eq!(req.subpath, vec!["hub".to_string(), "v1".to_string()]);
        assert_eq!(req.query.get("token"), Some(&"xyz".to_string()));
    }

    #[test]
    fn parse_url_without_query() {
        let req = parse_request("pingle://disconnect").unwrap();
        assert_eq!(req.action, "disconnect");
        assert!(req.query.is_empty());
    }

    #[test]
    fn parse_url_with_percent_encoding() {
        let req = parse_request("pingle://import?name=Home%20WiFi").unwrap();
        assert_eq!(req.query.get("name"), Some(&"Home WiFi".to_string()));
    }

    #[test]
    fn parse_url_missing_scheme_errors() {
        let err = parse_request("import?config=abc").unwrap_err();
        assert!(matches!(err, VpnError::InvalidConfiguration(_)));
    }

    #[test]
    fn parse_url_missing_action_errors() {
        let err = parse_request("pingle://").unwrap_err();
        assert!(matches!(err, VpnError::InvalidConfiguration(_)));
    }

    #[test]
    fn parse_url_with_empty_query_value() {
        let req = parse_request("pingle://import?activate=&name=X").unwrap();
        assert_eq!(req.query.get("activate"), Some(&"".to_string()));
        assert_eq!(req.query.get("name"), Some(&"X".to_string()));
    }

    // -- builtin_resolve tests ----------------------------------------------

    fn base64_config(json: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(json.as_bytes())
    }

    #[test]
    fn builtin_resolve_unhandled_action() {
        let req = DeeplinkRequest {
            raw_url: "pingle://auth?token=x".into(),
            action: "auth".into(),
            subpath: vec![],
            query: BTreeMap::new(),
        };
        let res = builtin_resolve(&req);
        assert!(matches!(res, DeeplinkResolution::Unhandled));
    }

    #[test]
    fn builtin_resolve_import_without_config_errors() {
        let req = parse_request("pingle://import?name=Home").unwrap();
        match builtin_resolve(&req) {
            DeeplinkResolution::Error { message, .. } => {
                assert!(message.contains("config"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn builtin_resolve_import_with_valid_config() {
        let json = r#"{"log":{"level":"debug"}}"#;
        let url = format!("pingle://import?name=Test&config={}", base64_config(json));
        let req = parse_request(&url).unwrap();
        match builtin_resolve(&req) {
            DeeplinkResolution::Profile {
                name,
                core_type,
                config,
                next_action,
                ..
            } => {
                assert_eq!(name, "Test");
                assert_eq!(core_type, "sing-box");
                assert_eq!(config, json);
                assert_eq!(next_action, DeeplinkNextAction::StoreOnly);
            }
            other => panic!("expected Profile, got {other:?}"),
        }
    }

    #[test]
    fn builtin_resolve_with_activate_true() {
        let json = r#"{"log":{"level":"info"}}"#;
        let url = format!(
            "pingle://import?config={}&activate=true",
            base64_config(json)
        );
        let req = parse_request(&url).unwrap();
        match builtin_resolve(&req) {
            DeeplinkResolution::Profile { next_action, .. } => {
                assert_eq!(next_action, DeeplinkNextAction::ActivateAndConnect);
            }
            other => panic!("expected Profile with ActivateAndConnect, got {other:?}"),
        }
    }

    #[test]
    fn builtin_resolve_invalid_base64() {
        let req = parse_request("pingle://import?config=not-base64-!!!").unwrap();
        match builtin_resolve(&req) {
            DeeplinkResolution::Error { message, .. } => {
                assert!(message.contains("base64"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn builtin_resolve_default_name() {
        let json = r#"{}"#;
        let url = format!("pingle://import?config={}", base64_config(json));
        let req = parse_request(&url).unwrap();
        match builtin_resolve(&req) {
            DeeplinkResolution::Profile { name, .. } => {
                assert_eq!(name, "Imported profile");
            }
            other => panic!("expected Profile, got {other:?}"),
        }
    }

    #[test]
    fn builtin_resolve_custom_core_type() {
        let json = r#"{}"#;
        let url = format!("pingle://import?config={}&core=xray", base64_config(json));
        let req = parse_request(&url).unwrap();
        match builtin_resolve(&req) {
            DeeplinkResolution::Profile { core_type, .. } => {
                assert_eq!(core_type, "xray");
            }
            other => panic!("expected Profile, got {other:?}"),
        }
    }

    // -- DeeplinkResolution serde round trip --------------------------------

    #[test]
    fn resolution_profile_round_trip() {
        let res = DeeplinkResolution::Profile {
            name: "Test".into(),
            core_type: "sing-box".into(),
            config: r#"{"a":1}"#.into(),
            metadata: [("tag".to_string(), "foo".to_string())].into(),
            next_action: DeeplinkNextAction::Activate,
        };
        let json = serde_json::to_string(&res).unwrap();
        let round: DeeplinkResolution = serde_json::from_str(&json).unwrap();
        match round {
            DeeplinkResolution::Profile {
                name, next_action, ..
            } => {
                assert_eq!(name, "Test");
                assert_eq!(next_action, DeeplinkNextAction::Activate);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn resolution_unhandled_serializes_as_kind_only() {
        let res = DeeplinkResolution::Unhandled;
        let json = serde_json::to_string(&res).unwrap();
        assert_eq!(json, r#"{"kind":"unhandled"}"#);
    }

    #[test]
    fn resolution_plugin_wire_accepts_camel_or_snake_case() {
        let camel = r#"{"kind":"profile","name":"X","coreType":"sing-box","config":"{}","next_action":"activate"}"#;
        let res: DeeplinkResolution = serde_json::from_str(camel).unwrap();
        match res {
            DeeplinkResolution::Profile {
                core_type,
                next_action,
                ..
            } => {
                assert_eq!(core_type, "sing-box");
                assert_eq!(next_action, DeeplinkNextAction::Activate);
            }
            _ => panic!("wrong variant"),
        }
        let snake = r#"{"kind":"profile","name":"X","core_type":"xray","config":"{}"}"#;
        let res: DeeplinkResolution = serde_json::from_str(snake).unwrap();
        match res {
            DeeplinkResolution::Profile { core_type, .. } => assert_eq!(core_type, "xray"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn request_round_trip() {
        let req = DeeplinkRequest {
            raw_url: "pingle://import?token=xyz".into(),
            action: "import".into(),
            subpath: vec!["hub".into()],
            query: [("token".to_string(), "xyz".to_string())].into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let round: DeeplinkRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(round, req);
    }
}
