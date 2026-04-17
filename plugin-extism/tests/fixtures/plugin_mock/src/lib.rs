//! Mock plugin — exercises the generic Plugin contract AND the
//! middleware-style slot chain convention.
//!
//! Used by `plugin-extism/tests/plugin_adapter_smoke.rs` and
//! `plugin-extism/tests/slot_chain_smoke.rs` to verify the adapter
//! end-to-end without depending on a real backend. Doubles as a
//! worked example for plugin authors — the smallest possible plugin
//! that satisfies both the legacy flat-method contract and the new
//! `slot.<name>.<phase>` contract.
//!
//! ## Wire shape
//!
//! Two exports — see `plugin-extism/src/plugin_adapter.rs` for the
//! canonical contract.
//!
//! - `plugin_handle_ipc({"method": "...", "params": <json>})` →
//!   `{"handled": true, "result": <json>}` /
//!   `{"handled": true, "error": "..."}` /
//!   `{"handled": false}`.
//!
//! - `plugin_authenticator_status(null)` → `{"is_authenticated": bool, "user_id": "..."}`.
//!
//! ## Legacy method names (still recognized)
//!
//! `auth.login`, `auth.logout`, `profile.bootstrap`, `auth.fail` —
//! pre-slot-convention vocabulary. The daemon now prefers the slot
//! chain but keeps these working for tests + migration.
//!
//! `debug.config` — returns selected plugin config values so the host
//! adapter tests can prove manifest config reaches wasm.
//!
//! ## Slot chain method names (new)
//!
//! | Method                         | Outcome                                       |
//! |--------------------------------|-----------------------------------------------|
//! | `slot.demo.observe.before`     | Unchanged (claim the slot, no mutation)       |
//! | `slot.demo.observe.after`      | Unchanged                                     |
//! | `slot.demo.transform.exec`     | Continue with `{bump: ctx.payload.bump + 1}`  |
//! | `slot.demo.halt.before`        | Halt with a canned payload; exec+after skipped|
//! | `slot.demo.error.exec`         | Error("simulated slot failure")               |
//! | any other `slot.*` method      | Unhandled — chain advances normally           |
//!
//! Note: there is no fixed list of method names baked into either the
//! daemon or the adapter. The plugin author defines its own
//! vocabulary; clients learn it from the plugin's own documentation.

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
///
/// Two dispatch layers:
///
/// 1. **Slot chain**: methods of the form `slot.<slot_name>.<phase>`
///    route to [`dispatch_slot_phase`], which returns a canonical
///    [`domain::SlotOutcome`]-shaped JSON the host can fold.
///
/// 2. **Legacy flat methods**: everything else (e.g. `auth.login`)
///    uses the pre-slot envelope for backwards compatibility.
#[plugin_fn]
pub fn plugin_handle_ipc(input: String) -> FnResult<String> {
    let req: HandleInput = serde_json::from_str(&input)
        .map_err(|e| Error::msg(format!("plugin_mock: bad input: {e}")))?;

    // Slot-chain dispatch: `slot.<name>.<phase>`
    if let Some(stripped) = req.method.strip_prefix("slot.") {
        let outcome = dispatch_slot_phase(stripped, &req.params);
        return Ok(serde_json::to_string(&HandleOutput::ok(outcome))
            .map_err(|e| Error::msg(format!("plugin_mock: serialize: {e}")))?);
    }

    // Legacy flat-method dispatch.
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
        "debug.config" => {
            let plugin_target_os = config::get("plugin_target_os")
                .ok()
                .flatten();
            HandleOutput::ok(serde_json::json!({
                "plugin_target_os": plugin_target_os,
            }))
        }

        // Deliberately failing endpoint to exercise the error envelope.
        "auth.fail" => HandleOutput::err("simulated failure from plugin_mock"),

        // Anything else: pass. The daemon then returns MethodNotFound
        // to the client.
        _ => HandleOutput::unhandled(),
    };
    Ok(serde_json::to_string(&out)
        .map_err(|e| Error::msg(format!("plugin_mock: serialize: {e}")))?)
}

/// Given a `slot.<x>` method suffix (the bit after `slot.`) and the
/// raw SlotContext envelope from the host, return a canonical
/// `SlotOutcome`-shaped JSON value. The tests below consume these.
///
/// Pulled out so test authors can see each demo slot's behavior in
/// one small block without scrolling through the auth branches.
fn dispatch_slot_phase(suffix: &str, ctx_value: &serde_json::Value) -> serde_json::Value {
    // Extract the payload from the context envelope. Slot tests that
    // need to see the payload reach into this field; slot tests that
    // don't care ignore it.
    let payload = ctx_value
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    match suffix {
        // demo.observe — claim before+after as pure observers.
        "demo.observe.before" | "demo.observe.after" => {
            serde_json::json!({"kind": "unchanged"})
        }

        // demo.transform — exec mutates the payload's `bump` counter.
        "demo.transform.exec" => {
            let bump = payload.get("bump").and_then(|v| v.as_u64()).unwrap_or(0);
            serde_json::json!({
                "kind": "continue",
                "payload": {"bump": bump + 1}
            })
        }

        // demo.halt — before returns Halt with a canned payload.
        // Exec and after must never fire (covered by tests).
        "demo.halt.before" => serde_json::json!({
            "kind": "halt",
            "payload": {"halted": true, "reason": "canned halt from plugin_mock"}
        }),

        // demo.error — exec returns Error to exercise propagation.
        "demo.error.exec" => serde_json::json!({
            "kind": "error",
            "message": "simulated slot failure"
        }),

        // Any other slot phase: explicit Unhandled so the host
        // advances to the next phase without inferring intent.
        _ => serde_json::json!({"kind": "unhandled"}),
    }
}

/// Optional authenticator probe. The mock fixture always reports
/// "logged in as mock-1" so the daemon's authenticator wiring is
/// exercised. Real plugins flip this to `false` between login and
/// logout based on their internal token cache.
#[plugin_fn]
pub fn plugin_authenticator_status(_: ()) -> FnResult<String> {
    Ok(r#"{"is_authenticated":true,"user_id":"mock-1"}"#.to_string())
}
