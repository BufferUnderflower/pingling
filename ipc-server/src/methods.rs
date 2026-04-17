//! Method dispatcher — maps incoming JSON-RPC method names to [`VpnManager`]
//! calls and produces serializable result values.
//!
//! ## Adding a new method
//!
//! 1. Add a `match` arm in [`dispatch`] for the new method name.
//! 2. Parse params from `req.params` (use [`parse_params`] for typed structs).
//! 3. Call the appropriate `VpnManager` method.
//! 4. Convert the result into a `serde_json::Value`.
//! 5. Convert errors with [`vpn_error_to_rpc`].
//!
//! Method naming follows the `<namespace>.<verb>` convention from the
//! architecture spec: `vpn.*`, `core.*`, `config.*`, `event.*`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use service::VpnManager;
use std::sync::Arc;

#[cfg(target_os = "macos")]
use core_libbox_macos::sysext;

use super::broadcaster::EventBroadcaster;
use super::protocol::{
    Notification, Request, Response, RpcError, APPLICATION_ERROR, INVALID_PARAMS, METHOD_NOT_FOUND,
};
use super::protocol_constants::{events, methods as m};

const DEFAULT_SYSEXT_BUNDLE_ID: &str = "one.pingle.vpn.system-extension";

// The IPC dispatcher splits into two halves:
//
//   1. Built-in `vpn.*` / `core.*` / `config.*` / `outbounds.*` /
//      `daemon.*` arms — hardcoded below, return `Ok(...)` /
//      `Err(...)` directly.
//
//   2. Plugin fall-through — anything the built-in arms don't claim
//      is forwarded to `vpn.plugin().handle_ipc(method, params)`. The
//      plugin defines its own method namespace; the daemon does not
//      enumerate, validate, or document it. Method names like
//      `auth.login`, `profile.bootstrap`, `account.config` belong
//      entirely to the plugin's vocabulary — see
//      `docs/architecture-plugin.md`.
//
// `MethodNotFound` is the result of BOTH halves missing. There is no
// hardcoded auth dispatch in this file by design.

/// Dispatch a single request against `vpn`. Returns `None` if the request was
/// a notification (no response should be sent).
///
/// The broadcaster is threaded through so handlers that mutate daemon state
/// can publish a corresponding push event to every subscribed client. Every
/// JSON-RPC client (TUI, Flutter, the future tray-driven UI) gets the same
/// view of the world this way — the daemon is the only source of truth.
pub fn dispatch(
    vpn: &Arc<VpnManager>,
    broadcaster: &Arc<EventBroadcaster>,
    req: Request,
) -> Option<Response> {
    let id = req.id_or_null();

    if req.is_notification() {
        // We currently support no client-to-server notifications. Drop them
        // silently per JSON-RPC spec.
        let _ = call(vpn, broadcaster, &req);
        return None;
    }

    Some(match call(vpn, broadcaster, &req) {
        Ok(value) => Response::ok(id, value),
        Err(err) => Response {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(err),
        },
    })
}

/// Inner dispatch — returns `Result<Value, RpcError>` for both success and
/// error paths so [`dispatch`] can wrap them uniformly.
fn call(
    vpn: &Arc<VpnManager>,
    broadcaster: &Arc<EventBroadcaster>,
    req: &Request,
) -> Result<Value, RpcError> {
    match req.method.as_str() {
        // ----- VPN lifecycle -----------------------------------------------
        x if x == m::VPN_CONNECT => {
            vpn.connect().map_err(vpn_error_to_rpc)?;
            publish_state(broadcaster, vpn);
            Ok(json!({ "state": vpn.get_status().to_string() }))
        }
        x if x == m::VPN_DISCONNECT => {
            vpn.disconnect().map_err(vpn_error_to_rpc)?;
            publish_state(broadcaster, vpn);
            Ok(json!({ "state": vpn.get_status().to_string() }))
        }
        x if x == m::VPN_RESTART => {
            vpn.restart().map_err(vpn_error_to_rpc)?;
            publish_state(broadcaster, vpn);
            Ok(json!({ "state": vpn.get_status().to_string() }))
        }
        x if x == m::VPN_STATUS => Ok(json!({
            "state": vpn.get_status().to_string(),
            "running": vpn.is_running(),
            "core": vpn.active_core_type().unwrap_or_default(),
        })),

        // ----- Core registry -----------------------------------------------
        x if x == m::CORE_LIST => {
            let cores: Vec<CoreDescriptorDto> = vpn
                .list_cores()
                .iter()
                .map(CoreDescriptorDto::from)
                .collect();
            Ok(serde_json::to_value(cores).unwrap_or(Value::Null))
        }
        x if x == m::CORE_ACTIVE => Ok(json!({ "core": vpn.active_core_type() })),
        x if x == m::CORE_SWITCH => {
            let p: CoreSwitchParams = parse_params(&req.params)?;
            vpn.switch_core(&p.core_type).map_err(vpn_error_to_rpc)?;
            // Broadcast: every connected client refetches its core info,
            // capabilities, and status to reflect the new active engine.
            broadcaster.publish(Notification::new(
                events::CORE_CHANGED,
                json!({
                    "core": vpn.active_core_type().unwrap_or_default(),
                    "capabilities": vpn.capabilities(),
                }),
            ));
            publish_state(broadcaster, vpn);
            Ok(json!({ "core": vpn.active_core_type() }))
        }

        // ----- Core introspection ------------------------------------------
        x if x == m::CORE_INFO => {
            // Active core metadata (name, version, supported protocols).
            // Lets TUI show a "Core" panel with the engine's self-reported info.
            let info = vpn.get_core_info();
            Ok(json!({
                "name": info.name,
                "version": info.version,
                "supported_protocols": info.supported_protocols,
            }))
        }
        x if x == m::CORE_PREREQS => {
            // Prerequisite checks for the active core (binary exists, TUN
            // device available, entitlements present, etc.). Lets TUI show
            // green/red badges next to each required capability.
            let checks = vpn.check_prerequisites();
            let items: Vec<serde_json::Value> = checks
                .iter()
                .map(|c| {
                    json!({
                        "name": c.name,
                        "passed": c.passed,
                        "message": c.message,
                    })
                })
                .collect();
            Ok(json!({ "checks": items }))
        }
        x if x == m::CORE_CAPABILITIES => {
            // Which optional pipelines (list_outbounds, select_outbound,
            // test_latency) are registered. The presence of a pipeline IS
            // the capability declaration — clients can use this to
            // enable/disable UI affordances.
            Ok(json!({ "capabilities": vpn.capabilities() }))
        }
        x if x == m::CORE_ENSURE_FIREWALL_RULES => core_ensure_firewall_rules(vpn),
        x if x == m::SYSTEM_EXTENSION_STATUS => {
            let params: SystemExtensionParams = parse_params(&req.params).or_else(|_| {
                Ok::<SystemExtensionParams, RpcError>(SystemExtensionParams::default())
            })?;
            let bundle_id = params
                .bundle_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_SYSEXT_BUNDLE_ID);
            system_extension_status(vpn, bundle_id)
        }
        x if x == m::SYSTEM_EXTENSION_INSTALL => {
            let params: SystemExtensionParams = parse_params(&req.params).or_else(|_| {
                Ok::<SystemExtensionParams, RpcError>(SystemExtensionParams::default())
            })?;
            let bundle_id = params
                .bundle_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_SYSEXT_BUNDLE_ID);
            system_extension_install(bundle_id)
        }
        x if x == m::SYSTEM_EXTENSION_UNINSTALL => {
            let params: SystemExtensionParams = parse_params(&req.params).or_else(|_| {
                Ok::<SystemExtensionParams, RpcError>(SystemExtensionParams::default())
            })?;
            let bundle_id = params
                .bundle_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_SYSEXT_BUNDLE_ID);
            system_extension_uninstall(bundle_id)
        }
        x if x == m::SYSTEM_SETTINGS_OPEN_FULL_DISK_ACCESS => open_full_disk_access_settings(),

        // ----- Config / settings -------------------------------------------
        x if x == m::CONFIG_GET => {
            let p: ConfigKeyParams = parse_params(&req.params)?;
            let value = vpn.get_setting(&p.key).map_err(vpn_error_to_rpc)?;
            Ok(json!({ "key": p.key, "value": value }))
        }
        x if x == m::CONFIG_SET => {
            let p: ConfigSetParams = parse_params(&req.params)?;
            vpn.set_setting(&p.key, &p.value)
                .map_err(vpn_error_to_rpc)?;
            // Daemon is the source of truth — every other client (TUI,
            // Flutter, the next dashboard) finds out via this push event.
            broadcaster.publish(Notification::new(
                events::CONFIG_CHANGED,
                json!({ "key": p.key, "value": p.value }),
            ));
            Ok(json!({ "ok": true }))
        }
        x if x == m::CONFIG_INFO => {
            let path = vpn
                .get_setting("config_path")
                .ok()
                .flatten()
                .unwrap_or_default();
            Ok(json!({
                "core_type": vpn.active_core_type().unwrap_or_default(),
                "config_path": path,
            }))
        }
        x if x == m::CONFIG_VALIDATE => {
            // Run the OpValidateConfig pipeline against the given path
            // (or the currently-configured one). Exercises plugin hooks
            // on the validate pipeline and returns ok/error uniformly.
            let p: ConfigValidateParams = parse_params(&req.params).or_else(|_| {
                Ok::<ConfigValidateParams, RpcError>(ConfigValidateParams { path: None })
            })?;
            let path = match p.path {
                Some(p) => p,
                None => vpn
                    .get_setting("config_path")
                    .map_err(vpn_error_to_rpc)?
                    .unwrap_or_default(),
            };
            if path.is_empty() {
                return Err(RpcError {
                    code: INVALID_PARAMS,
                    message: "config.validate: no config_path set and no path parameter".into(),
                    data: None,
                });
            }
            vpn.validate_config(&path).map_err(vpn_error_to_rpc)?;
            broadcaster.publish(Notification::new(
                events::CONFIG_VALIDATED,
                json!({ "path": path, "ok": true }),
            ));
            Ok(json!({ "ok": true, "path": path }))
        }

        // ----- Outbounds (capability-gated) --------------------------------
        x if x == m::OUTBOUNDS_LIST => {
            // Returns every outbound the active core exposes. Requires the
            // `list_outbounds` capability pipeline — otherwise returns an
            // empty list (not an error) so clients can treat it uniformly.
            let list = vpn.list_outbounds().map_err(vpn_error_to_rpc)?;
            let items: Vec<serde_json::Value> = list.iter().map(outbound_to_json).collect();
            Ok(json!({ "outbounds": items }))
        }
        x if x == m::OUTBOUNDS_SELECT => {
            let p: OutboundSelectParams = parse_params(&req.params)?;
            vpn.select_outbound(&p.outbound_id)
                .map_err(vpn_error_to_rpc)?;
            broadcaster.publish(Notification::new(
                events::OUTBOUND_SELECTED,
                json!({ "outbound_id": p.outbound_id }),
            ));
            Ok(json!({ "ok": true, "outbound_id": p.outbound_id }))
        }
        x if x == m::OUTBOUNDS_TEST_LATENCY => {
            let p: TestLatencyParams = parse_params(&req.params).or_else(|_| {
                Ok::<TestLatencyParams, RpcError>(TestLatencyParams {
                    outbound_ids: vec![],
                })
            })?;
            let results = vpn
                .test_latency(&p.outbound_ids)
                .map_err(vpn_error_to_rpc)?;
            Ok(json!({ "results": results }))
        }

        // ----- Daemon meta -------------------------------------------------
        x if x == m::DAEMON_INFO => Ok(json!({
            "name": "pingle",
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "protocol_version": crate::PROTOCOL_VERSION,
            "capabilities": vpn.capabilities(),
            "active_core": vpn.active_core_type().unwrap_or_default(),
            // Plugin metadata: name + authenticator status. Lets clients
            // render "Plugin: pingle-hub-userapi · alice" in their chrome
            // without dispatching a separate IPC call. Always present
            // (`null` when no plugin is installed) so the wire shape is
            // stable.
            "plugin": plugin_meta_for_daemon_info(vpn),
        })),
        x if x == m::DAEMON_PING => Ok(json!({ "pong": true })),

        x if x == m::DAEMON_INSTALL_ID => match vpn.install_id() {
            Ok(id) => Ok(json!({ "install_id": id })),
            Err(e) => Err(vpn_error_to_rpc(e)),
        },

        // ----- Profile management ------------------------------------------
        //
        // Profiles are the encrypted config source. Clients can put,
        // list, activate, delete — but never read the config body back
        // over IPC. See the design spec in
        // docs/superpowers/specs/2026-04-11-profiles-deeplink-encrypted-storage.md
        x if x == m::PROFILE_LIST => match vpn.list_profiles() {
            Ok(metas) => Ok(json!({ "profiles": metas })),
            Err(e) => Err(vpn_error_to_rpc(e)),
        },

        x if x == m::PROFILE_GET => {
            let params: ProfileGetParams = parse_params(&req.params)?;
            match vpn.get_profile(&params.id) {
                Ok(Some(meta)) => Ok(json!({ "profile": meta })),
                Ok(None) => Ok(json!({ "profile": null })),
                Err(e) => Err(vpn_error_to_rpc(e)),
            }
        }

        x if x == m::PROFILE_PUT => {
            let params: ProfilePutParams = parse_params(&req.params)?;
            let profile = domain::Profile {
                id: params.id.unwrap_or_default(),
                name: params.name,
                core_type: params.core_type,
                source: params
                    .source
                    .unwrap_or(domain::ProfileSource::Imported { filename: None }),
                metadata: params.metadata.unwrap_or_default(),
                created_at: std::time::SystemTime::now(),
                last_used_at: None,
            };
            match vpn.put_profile(profile, &params.config_json) {
                Ok(p) => {
                    broadcaster.publish(Notification::new(
                        crate::protocol_constants::events::PROFILE_CHANGED,
                        json!({ "id": p.id, "action": "put" }),
                    ));
                    Ok(json!({ "id": p.id }))
                }
                Err(e) => Err(vpn_error_to_rpc(e)),
            }
        }

        x if x == m::PROFILE_DELETE => {
            let params: ProfileGetParams = parse_params(&req.params)?;
            match vpn.delete_profile(&params.id) {
                Ok(()) => {
                    broadcaster.publish(Notification::new(
                        crate::protocol_constants::events::PROFILE_CHANGED,
                        json!({ "id": params.id, "action": "delete" }),
                    ));
                    Ok(json!({}))
                }
                Err(e) => Err(vpn_error_to_rpc(e)),
            }
        }

        x if x == m::PROFILE_ACTIVE => match vpn.active_profile() {
            Ok(id) => Ok(json!({ "id": id })),
            Err(e) => Err(vpn_error_to_rpc(e)),
        },

        x if x == m::PROFILE_ACTIVATE => {
            let params: ProfileGetParams = parse_params(&req.params)?;
            match vpn.set_active_profile(&params.id) {
                Ok(()) => {
                    broadcaster.publish(Notification::new(
                        crate::protocol_constants::events::PROFILE_CHANGED,
                        json!({ "id": params.id, "action": "activate" }),
                    ));
                    Ok(json!({ "active_id": params.id }))
                }
                Err(e) => Err(vpn_error_to_rpc(e)),
            }
        }

        x if x == m::PROFILE_CLEAR_ACTIVE => match vpn.clear_active_profile() {
            Ok(()) => {
                broadcaster.publish(Notification::new(
                    crate::protocol_constants::events::PROFILE_CHANGED,
                    json!({ "action": "clear_active" }),
                ));
                Ok(json!({}))
            }
            Err(e) => Err(vpn_error_to_rpc(e)),
        },

        // ----- Deep-link handling ------------------------------------------
        //
        // Parses `pingle://...`, dispatches to the loaded plugin's
        // `deeplink.resolve` method (if any), falls back to the
        // built-in resolver (handles `pingle://import?config=<base64>`),
        // and applies the result: stores a new profile + optionally
        // activates + optionally connects based on the next_action
        // hint in the resolution.
        x if x == m::DEEPLINK_HANDLE => {
            let params: DeeplinkHandleParams = parse_params(&req.params)?;
            match crate::deeplink::handle_deeplink(vpn, &params.url) {
                Ok(outcome) => {
                    // On a successful import, publish profileChanged
                    // so clients can refresh their lists.
                    if outcome.profile_id.is_some() {
                        broadcaster.publish(Notification::new(
                            crate::protocol_constants::events::PROFILE_CHANGED,
                            json!({
                                "id": outcome.profile_id,
                                "action": "deeplink",
                            }),
                        ));
                    }
                    Ok(serde_json::to_value(&outcome).unwrap_or_else(|_| json!({})))
                }
                Err(e) => Err(vpn_error_to_rpc(e)),
            }
        }

        // ----- Event subscription ------------------------------------------
        // Subscribing is implicit: every connection is auto-subscribed by
        // the server. These two are kept so old clients that send them get
        // a clean ack instead of MethodNotFound.
        x if x == m::EVENT_SUBSCRIBE => Ok(json!({ "subscribed": true })),
        x if x == m::EVENT_UNSUBSCRIBE => Ok(json!({ "unsubscribed": true })),

        // ----- Plugin fall-through -----------------------------------------
        //
        // Anything the built-in arms above didn't claim is forwarded to
        // the installed `Plugin`. The plugin defines its own method
        // namespace (`auth.login`, `profile.bootstrap`, …) and the
        // daemon doesn't enumerate it. If no plugin is installed OR the
        // plugin returns `None` (doesn't claim this method either), we
        // fall through to a clean `MethodNotFound`.
        other => match vpn.plugin() {
            Some(plugin) => match plugin.handle_ipc(other, &req.params) {
                Some(Ok(value)) => {
                    // The plugin may push side-effects through the daemon's
                    // event broadcaster too, but the broadcaster is exposed
                    // via the same dispatch chain — wiring host functions
                    // for events is a future arc.
                    Ok(value)
                }
                Some(Err(err)) => Err(vpn_error_to_rpc(err)),
                None => Err(RpcError {
                    code: METHOD_NOT_FOUND,
                    message: format!("method not found: {other}"),
                    data: None,
                }),
            },
            None => Err(RpcError {
                code: METHOD_NOT_FOUND,
                message: format!("method not found: {other}"),
                data: None,
            }),
        },
    }
}

/// Build the `plugin` field rendered into the `daemon.info` response.
/// Returns `Value::Null` when no plugin is installed; otherwise an
/// object with the plugin name and (when the plugin exposes one) the
/// authenticator snapshot. Pure observation — never calls
/// `handle_ipc`, so it's safe to call from the synchronous dispatch
/// path.
fn plugin_meta_for_daemon_info(vpn: &Arc<VpnManager>) -> Value {
    let Some(plugin) = vpn.plugin() else {
        return Value::Null;
    };
    let mut out = json!({ "name": plugin.name() });
    if let Some(auth) = plugin.authenticator() {
        out["authenticator"] = json!({
            "is_authenticated": auth.is_authenticated(),
            "user_id": auth.user_id(),
        });
    }
    out
}

fn core_ensure_firewall_rules(_vpn: &Arc<VpnManager>) -> Result<Value, RpcError> {
    #[cfg(all(target_os = "windows", feature = "libbox-windows"))]
    {
        core_libbox_windows::prereqs::ensure_firewall_rules_for_current_exe()
            .map_err(vpn_error_to_rpc)?;
        let checks = _vpn.check_prerequisites();
        let items: Vec<serde_json::Value> = checks
            .iter()
            .map(|c| {
                json!({
                    "name": c.name,
                    "passed": c.passed,
                    "message": c.message,
                })
            })
            .collect();
        Ok(json!({ "ok": true, "checks": items }))
    }

    #[cfg(not(all(target_os = "windows", feature = "libbox-windows")))]
    {
        Err(vpn_error_to_rpc(domain::VpnError::PrerequisiteMissing(
            "firewall rule management is only available in Windows libbox builds".into(),
        )))
    }
}

/// Publish the current VPN status as a `event.stateChanged` push event.
/// Called from the `vpn.connect`/disconnect/restart handlers (and the
/// background polling thread) so every subscribed client sees state
/// transitions even if they didn't initiate the action themselves.
fn publish_state(broadcaster: &Arc<EventBroadcaster>, vpn: &Arc<VpnManager>) {
    let core = vpn.active_core_type().unwrap_or_default();
    let running = vpn.is_running();
    let state = vpn.get_status().to_string();
    broadcaster.publish_state(&state, running, &core);
}

/// Convert a [`domain::VpnError`] into a JSON-RPC error object.
///
/// The stable error code goes into `error.data.code` so clients can handle
/// specific failures (already_connected, prerequisite_missing, …) without
/// regex-matching the human message.
pub fn vpn_error_to_rpc(err: domain::VpnError) -> RpcError {
    RpcError {
        code: APPLICATION_ERROR,
        message: err.to_string(),
        data: Some(json!({
            "code": err.code(),
            "recoverable": err.recoverable(),
        })),
    }
}

/// Helper for typed param parsing. Converts serde errors to InvalidParams.
fn parse_params<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, RpcError> {
    serde_json::from_value(value.clone()).map_err(|e| RpcError {
        code: INVALID_PARAMS,
        message: format!("invalid params: {e}"),
        data: None,
    })
}

// ---------------------------------------------------------------------------
// Param structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CoreSwitchParams {
    #[serde(rename = "coreType", alias = "core_type")]
    core_type: String,
}

#[derive(Debug, Deserialize)]
struct ConfigKeyParams {
    key: String,
}

#[derive(Debug, Deserialize)]
struct ConfigSetParams {
    key: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct ConfigValidateParams {
    /// Optional explicit path. If absent, use whatever `config_path` is in settings.
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OutboundSelectParams {
    #[serde(rename = "outboundId", alias = "outbound_id")]
    outbound_id: String,
}

#[derive(Debug, Deserialize)]
struct TestLatencyParams {
    /// Empty vec = test all outbounds.
    #[serde(default, rename = "outboundIds", alias = "outbound_ids")]
    outbound_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct SystemExtensionParams {
    #[serde(default, rename = "bundleId", alias = "bundle_id")]
    bundle_id: Option<String>,
}

/// Single-id param used by `profile.get`, `profile.delete`, `profile.activate`.
#[derive(Debug, Deserialize)]
struct ProfileGetParams {
    id: String,
}

/// Params for `deeplink.handle`.
#[derive(Debug, Deserialize)]
struct DeeplinkHandleParams {
    url: String,
}

/// Create/update params for `profile.put`.
///
/// The `id` field is optional — when absent the daemon generates a
/// fresh UUID. Clients that want stable ids (reproducible integration
/// tests) can set it explicitly.
///
/// The `config_json` field is the plaintext config body. Once the
/// daemon acknowledges the `put`, the client should forget it — the
/// next `profile.get` returns metadata only.
#[derive(Debug, Deserialize)]
struct ProfilePutParams {
    #[serde(default)]
    id: Option<String>,
    name: String,
    #[serde(rename = "coreType", alias = "core_type")]
    core_type: String,
    #[serde(rename = "configJson", alias = "config_json")]
    config_json: String,
    #[serde(default)]
    source: Option<domain::ProfileSource>,
    #[serde(default)]
    metadata: Option<std::collections::BTreeMap<String, String>>,
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// Wire-format core descriptor (snake_case JSON, stable for clients).
#[derive(Debug, Clone, Serialize)]
struct CoreDescriptorDto {
    core_type: String,
    display_name: String,
    source: String,
    binary_path: Option<String>,
    available: bool,
}

impl From<&domain::CoreDescriptor> for CoreDescriptorDto {
    fn from(d: &domain::CoreDescriptor) -> Self {
        Self {
            core_type: d.core_type.clone(),
            display_name: d.display_name.clone(),
            source: d.source.to_string(),
            binary_path: d.binary_path.clone(),
            available: d.available,
        }
    }
}

/// Serialize a [`domain::Outbound`] into a wire-format JSON value.
///
/// Kept here (not `impl Serialize for Outbound`) so the `domain` crate stays
/// serde-free. The wire shape uses snake_case and omits fields the TUI
/// doesn't need (metadata is flattened to a simple map).
fn outbound_to_json(o: &domain::Outbound) -> Value {
    json!({
        "id": o.id,
        "name": o.name,
        "protocol": o.protocol.as_str(),
        "transport": format!("{:?}", o.transport).to_lowercase(),
        "country_code": o.country_code,
        "location": o.location,
        "latency_ms": o.latency_ms,
        "selected": o.selected,
        "metadata": o.metadata,
    })
}

fn prerequisite_check_to_json(check: &domain::PrerequisiteCheck) -> Value {
    json!({
        "name": check.name,
        "passed": check.passed,
        "message": check.message,
    })
}

#[cfg(target_os = "macos")]
fn system_extension_record_to_json(record: &sysext::SystemExtensionRecord) -> Value {
    json!({
        "team_id": record.team_id,
        "bundle_id": record.bundle_id,
        "version": record.version,
        "build_version": record.build_version,
        "display_name": record.display_name,
        "state": record.state,
    })
}

fn system_extension_checks_to_json(vpn: &Arc<VpnManager>) -> Vec<Value> {
    vpn.check_prerequisites()
        .iter()
        .map(prerequisite_check_to_json)
        .collect()
}

fn rpc_error(message: impl Into<String>, stable_code: &'static str) -> RpcError {
    RpcError {
        code: APPLICATION_ERROR,
        message: message.into(),
        data: Some(json!({
            "code": stable_code,
            "recoverable": false,
        })),
    }
}

#[cfg(target_os = "macos")]
fn system_extension_status(vpn: &Arc<VpnManager>, bundle_id: &str) -> Result<Value, RpcError> {
    let status = sysext::status(bundle_id).map_err(|error| {
        rpc_error(
            format!("system extension status failed: {error}"),
            "system_extension_status_failed",
        )
    })?;
    let record = status.record.as_ref().map(system_extension_record_to_json);
    let records: Vec<Value> = status
        .records
        .iter()
        .map(system_extension_record_to_json)
        .collect();
    Ok(json!({
        "bundle_id": status.bundle_id,
        "embedded_bundle": status
            .embedded_bundle
            .as_ref()
            .map(|path| path.display().to_string()),
        "installed": status.is_installed(),
        "version": status.version(),
        "build_version": status.build_version(),
        "state": status
            .record
            .as_ref()
            .map(|record| record.state.clone())
            .unwrap_or_else(|| "not installed".to_string()),
        "record": record,
        "records": records,
        "prereqs": system_extension_checks_to_json(vpn),
    }))
}

#[cfg(not(target_os = "macos"))]
fn system_extension_status(_vpn: &Arc<VpnManager>, _bundle_id: &str) -> Result<Value, RpcError> {
    Err(rpc_error(
        "system extension control is only available on macOS",
        "system_extension_unsupported_platform",
    ))
}

#[cfg(target_os = "macos")]
fn system_extension_install(bundle_id: &str) -> Result<Value, RpcError> {
    sysext::prompt_install(bundle_id)
        .map(|outcome| json!({ "bundle_id": bundle_id, "message": outcome.message }))
        .map_err(|error| {
            rpc_error(
                format!("system extension install failed: {error}"),
                "system_extension_install_failed",
            )
        })
}

#[cfg(not(target_os = "macos"))]
fn system_extension_install(_bundle_id: &str) -> Result<Value, RpcError> {
    Err(rpc_error(
        "system extension control is only available on macOS",
        "system_extension_unsupported_platform",
    ))
}

#[cfg(target_os = "macos")]
fn system_extension_uninstall(bundle_id: &str) -> Result<Value, RpcError> {
    sysext::request_uninstall(bundle_id)
        .map(|outcome| json!({ "bundle_id": bundle_id, "message": outcome.message }))
        .map_err(|error| {
            rpc_error(
                format!("system extension uninstall failed: {error}"),
                "system_extension_uninstall_failed",
            )
        })
}

#[cfg(not(target_os = "macos"))]
fn system_extension_uninstall(_bundle_id: &str) -> Result<Value, RpcError> {
    Err(rpc_error(
        "system extension control is only available on macOS",
        "system_extension_unsupported_platform",
    ))
}

#[cfg(target_os = "macos")]
fn open_full_disk_access_settings() -> Result<Value, RpcError> {
    core_libbox_macos::sysext::open_full_disk_access_settings()
        .map(|outcome| {
            json!({
                "ok": true,
                "url": outcome.url,
                "message": outcome.message,
            })
        })
        .map_err(|error| {
            rpc_error(
                format!("open full disk access settings failed: {error}"),
                "open_full_disk_access_settings_failed",
            )
        })
}

#[cfg(not(target_os = "macos"))]
fn open_full_disk_access_settings() -> Result<Value, RpcError> {
    Err(rpc_error(
        "full disk access settings are only available on macOS",
        "open_full_disk_access_settings_unsupported_platform",
    ))
}
