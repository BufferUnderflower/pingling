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

use super::broadcaster::EventBroadcaster;
use super::protocol::{
    Notification, Request, Response, RpcError, APPLICATION_ERROR, INVALID_PARAMS, METHOD_NOT_FOUND,
};
use super::protocol_constants::{events, methods as m};

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

