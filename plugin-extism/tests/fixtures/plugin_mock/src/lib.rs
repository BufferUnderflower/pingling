//! Mock plugin — exercises the new generic Plugin contract.
//!
//! Used by `plugin-extism/tests/plugin_adapter_smoke.rs` to verify the
//! adapter end-to-end without depending on a real backend. Doubles as
//! a worked example for plugin authors: it's the smallest possible
//! plugin that satisfies `looks_like_plugin` and round-trips through
//! `PluginAdapter`.
//!
//! ## Wire shape
//!
//! Two exports — see
//! `plugin-extism/src/plugin_adapter.rs` for the canonical
//! contract.
//!
//! - `plugin_handle_ipc({"method": "...", "params": <json>})` →
//!   `{"handled": true, "result": <json>}` /
//!   `{"handled": true, "error": "..."}` /
//!   `{"handled": false}`.
//!
//! - `plugin_authenticator_status(null)` → `{"is_authenticated": bool, "user_id": "..."}`.
//!
//! Note: there is no fixed list of method names baked into either the
//! daemon or the adapter. The plugin author defines its own
//! vocabulary inside `plugin_handle_ipc` and clients learn it from
//! the plugin's own documentation.

use extism_pdk::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct HandleInput {
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Serialize)]
struct HandleOutput {
    handled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl HandleOutput {
    fn unhandled() -> Self {
        Self {
            handled: false,
            result: None,
            error: None,
        }
    }
    fn ok(value: serde_json::Value) -> Self {
        Self {
            handled: true,
            result: Some(value),
            error: None,
        }
    }
    fn err(message: impl Into<String>) -> Self {
        Self {
            handled: true,
            result: None,
            error: Some(message.into()),
        }
    }
}

/// Generic IPC dispatcher — the plugin's own little router. The
/// daemon hands us `(method, params)` and we either claim it (with
/// `Ok` / `Err`) or pass (return `unhandled`). The set of method
/// names below is **the plugin's vocabulary**, not the daemon's.
#[plugin_fn]
pub fn plugin_handle_ipc(input: String) -> FnResult<String> {
    let req: HandleInput = serde_json::from_str(&input)
        .map_err(|e| Error::msg(format!("plugin_mock: bad input: {e}")))?;

    let out = match req.method.as_str() {
        // Auth flow — fictional. The daemon never names these.
        "auth.login" => HandleOutput::ok(serde_json::json!({
            "token": "mock-tok",
            "account_id": "mock-1",
            "display_name": "Mock User",
            "is_new": true,
            "echoed_params": req.params,
        })),
        "auth.logout" => HandleOutput::ok(serde_json::json!({"ok": true})),

        // Profile / bootstrap.
        "profile.bootstrap" => HandleOutput::ok(serde_json::json!({
            "account_id": "mock-1",
            "display_name": "Mock User",
            "wallet": {"balance_units": 1000, "currency": "USD"},
            "features": {"is_mock": true},
        })),

        // Deliberately failing endpoint to exercise the error envelope.
        "auth.fail" => HandleOutput::err("simulated failure from plugin_mock"),

        // Anything else: pass. The daemon then returns MethodNotFound
        // to the client.
        _ => HandleOutput::unhandled(),
    };
    Ok(serde_json::to_string(&out)
        .map_err(|e| Error::msg(format!("plugin_mock: serialize: {e}")))?)
}

/// Optional authenticator probe. The mock fixture always reports
/// "logged in as mock-1" so the daemon's authenticator wiring is
/// exercised. Real plugins flip this to `false` between login and
/// logout based on their internal token cache.
#[plugin_fn]
pub fn plugin_authenticator_status(_: ()) -> FnResult<String> {
    Ok(r#"{"is_authenticated":true,"user_id":"mock-1"}"#.to_string())
}
