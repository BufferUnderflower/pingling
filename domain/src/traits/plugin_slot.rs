//! Plugin slot framework — middleware-style extension points.
//!
//! A **slot** is a named extension point in the daemon where a plugin
//! can inject behavior in three phases: `before`, `exec`, `after`. The
//! host walks the phases in order, feeding each phase's output payload
//! into the next phase's input. Any phase the plugin doesn't claim is
//! skipped; any phase can short-circuit the chain.
//!
//! ## Why three phases
//!
//! It maps cleanly onto middleware / aspect-oriented patterns:
//!
//! | Phase    | Typical use                                             |
//! |----------|---------------------------------------------------------|
//! | `before` | validation, authorization, rate-limit, tracing span open |
//! | `exec`   | the actual operation (transform, resolve, compute)      |
//! | `after`  | response mutation, telemetry emit, tracing span close   |
//!
//! A plugin can claim any subset. A pure **observability** plugin
//! claims only `before` + `after` across every slot and lets the
//! daemon's default behaviour run the `exec`. A **feature-replacement**
//! plugin claims `exec` and returns [`SlotOutcome::Halt`] from
//! `before` or `after`. A **thin interceptor** claims only `before`
//! and mutates the payload on the way through.
//!
//! ## Wire protocol
//!
//! This module **does not change the wasm ABI**. Plugins still expose
//! a single [`Plugin::handle_ipc`] dispatcher (one wasm export). The
//! slot convention is purely on top of it: the host calls `handle_ipc`
//! three times per slot invocation with method names of the form
//!
//! ```text
//! slot.<slot_name>.before
//! slot.<slot_name>.exec
//! slot.<slot_name>.after
//! ```
//!
//! so plugins participate by dispatching on those strings in their
//! existing handler. Legacy single-method names (e.g. `config.process`)
//! keep working — adapters first try the chain and fall back to the
//! flat name if the plugin returned [`SlotOutcome::Unhandled`] (or
//! `None`) for every phase.
//!
//! ## Typed payloads
//!
//! Each slot defines its own payload type. The host uses
//! [`run_slot_chain`] which is generic over `P: Serialize +
//! DeserializeOwned + Clone`, so slot consumers get strongly typed
//! inputs + outputs without every slot reinventing the envelope.
//!
//! ## Outcome semantics
//!
//! [`SlotOutcome`] is a tagged serde enum that every phase's response
//! deserializes into:
//!
//! | Variant      | Host reaction                                                      |
//! |--------------|--------------------------------------------------------------------|
//! | `Unchanged`  | Phase observed but didn't transform; chain continues with same payload |
//! | `Continue`   | Phase returned a new payload; chain continues with the new one    |
//! | `Halt`       | Chain terminates immediately, host uses the payload as final result |
//! | `Error`      | Chain terminates with error, host surfaces to its own caller      |
//! | `Unhandled`  | Phase didn't claim the slot; host advances to the next phase      |
//!
//! ## Well-known slot names
//!
//! See [`slot_names`] for the canonical list. Slot owners pick the
//! short machine-readable name (`config.process`, `deeplink.resolve`,
//! ...) and document the payload type next to their adapter code.

use crate::{Plugin, VpnError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Canonical slot names the daemon dispatches around. Each entry has a
/// corresponding adapter site in the codebase; documented there.
pub mod slot_names {
    /// Config processor pipeline — transforms a sing-box config JSON
    /// between attempts. Payload: `{config, request}`. See
    /// `core-config-processor/src/processors/extism.rs`.
    pub const CONFIG_PROCESS: &str = "config.process";

    /// Deeplink resolver — turns a `pingle://` URL into a typed
    /// action (profile import, login token, etc.). Payload: the
    /// parsed URL envelope. See `ipc-server/src/deeplink.rs`.
    pub const DEEPLINK_RESOLVE: &str = "deeplink.resolve";

    /// Auth session probe — "do we have a valid session cached?".
    /// Payload: empty `{}`. Returns session descriptor. Used by
    /// the daemon's authenticator to avoid forcing a round-trip
    /// through the auth plugin on every status check.
    pub const AUTH_SESSION: &str = "auth.session";

    /// VPN connect lifecycle — wraps `VpnManager::connect`. `before`
    /// for pre-flight checks (licence, quota); `exec` is currently
    /// daemon-native; `after` for post-start telemetry emission.
    /// Payload: connect request + result envelope.
    pub const VPN_CONNECT: &str = "vpn.connect";

    /// VPN disconnect lifecycle — mirror of [`VPN_CONNECT`]. `after`
    /// is the natural place to flush metrics or rotate tokens.
    pub const VPN_DISCONNECT: &str = "vpn.disconnect";

    /// Generic IPC method dispatch. Every JSON-RPC method call passes
    /// through here as a cross-cutting hook, so telemetry plugins can
    /// record every client interaction without knowing the methods.
    /// Payload: `{method, params}`.
    pub const IPC_DISPATCH: &str = "ipc.dispatch";

    /// Invoked once when the daemon finishes loading a plugin.
    /// `exec` payload is empty — the plugin uses `after` to publish
    /// its capability declarations or schedule a warm-up job.
    pub const PLUGIN_LOAD: &str = "plugin.load";
}

/// Phase identifiers. Slot dispatches use method names of the form
/// `slot.<slot_name>.<phase>`.
pub mod phase {
    pub const BEFORE: &str = "before";
    pub const EXEC: &str = "exec";
    pub const AFTER: &str = "after";

    /// Canonical ordered list — the host walks slots in this order.
    pub const ORDER: [&str; 3] = [BEFORE, EXEC, AFTER];
}

/// Envelope the host passes to a plugin for a single slot phase.
///
/// Plugins deserialize this (ignoring fields they don't care about)
/// and dispatch on [`slot`](Self::slot) + [`phase`](Self::phase) inside
/// their `plugin_handle_ipc` handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotContext<P> {
    /// Canonical slot name, e.g. `"config.process"`. Never the
    /// fully-qualified `slot.config.process.exec` method string —
    /// the plugin receives the slot name and phase separately.
    pub slot: String,

    /// Phase within the slot: `"before"`, `"exec"`, or `"after"`.
    pub phase: String,

    /// Wire protocol version for the payload shape. Bumped when a slot
    /// owner changes its payload incompatibly. Plugins that don't
    /// recognize a wire version should return [`SlotOutcome::Error`]
    /// with a clear message rather than silently misinterpret data.
    pub wire_version: u32,

    /// Correlates phases of the same logical invocation across all
    /// three calls. Useful for telemetry plugins that pair `before`
    /// with `after` to measure latency. Format is slot-owner's choice;
    /// the helper in this module generates UUID-like strings.
    pub invocation_id: String,

    /// Slot-specific payload. Shape is defined by the slot owner and
    /// documented next to the adapter code.
    pub payload: P,
}

/// Response a plugin returns from any phase.
///
/// Tagged serde enum with snake-case `kind` discriminant so wire
/// messages look like `{"kind": "continue", "payload": {...}}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SlotOutcome<P> {
    /// Phase observed the call but made no payload change. Chain
    /// continues with the same payload fed into this phase.
    Unchanged,

    /// Phase returns a possibly-modified payload. Chain continues
    /// with this payload as the next phase's input.
    Continue {
        payload: P,
    },

    /// Phase short-circuits the chain. Later phases are skipped; the
    /// caller uses this payload as the final result of the whole
    /// slot invocation.
    Halt {
        payload: P,
    },

    /// Phase recognized the slot but ran into a recoverable error.
    /// Chain terminates; the caller surfaces the error message to its
    /// own caller.
    Error {
        message: String,
    },

    /// Plugin doesn't claim this phase. Host advances to the next
    /// phase as if the plugin weren't registered for it. Equivalent
    /// to the plugin's `handle_ipc` returning `None`, but explicit so
    /// plugins can distinguish "I looked and decided to pass" from
    /// "I didn't recognize the method at all" in telemetry.
    Unhandled,
}

/// Result of running a full slot chain.
///
/// `Ok(Some(payload))` — at least one phase handled the slot; `payload`
/// is the folded result after `after` (or whatever phase terminated).
///
/// `Ok(None)` — plugin ignored every phase of this slot. The caller
/// should fall back to its default behaviour (legacy method name, or
/// daemon-native logic).
///
/// `Err(VpnError)` — some phase returned [`SlotOutcome::Error`] or
/// the host hit a serde error while encoding/decoding the envelope.
pub type SlotChainResult<P> = Result<Option<P>, VpnError>;

/// Walk the `before` → `exec` → `after` phases of a slot, folding
/// each phase's outcome into the next phase's input.
///
/// Uses the plugin's existing [`Plugin::handle_ipc`] dispatcher under
/// the hood — no new wasm export required. Plugins participate by
/// matching on `slot.<slot_name>.<phase>` inside their existing
/// handler and returning a [`SlotOutcome`]-shaped JSON.
///
/// ## Payload requirements
///
/// `P` must be `Serialize + DeserializeOwned + Clone`. Clone is
/// needed because the envelope sent to each phase takes ownership of
/// the payload, and we need a copy to fall back on if the phase
/// returns [`SlotOutcome::Unhandled`] (the original payload continues
/// to the next phase unchanged).
///
/// ## Error handling
///
/// Any serde error (envelope build or outcome parse) is surfaced as
/// [`VpnError::Unknown`] so the caller can propagate it through its
/// usual error path — typically turning into a JSON-RPC
/// `APPLICATION_ERROR` at the IPC layer.
pub fn run_slot_chain<P>(
    plugin: &dyn Plugin,
    slot: &str,
    wire_version: u32,
    invocation_id: &str,
    initial_payload: P,
) -> SlotChainResult<P>
where
    P: Serialize + DeserializeOwned + Clone,
{
    let mut payload = initial_payload;
    let mut any_handled = false;

    for phase_name in phase::ORDER {
        let method = format!("slot.{slot}.{phase_name}");
        let ctx = SlotContext {
            slot: slot.to_string(),
            phase: phase_name.to_string(),
            wire_version,
            invocation_id: invocation_id.to_string(),
            payload: payload.clone(),
        };
        let ctx_value = serde_json::to_value(&ctx).map_err(|e| {
            VpnError::Unknown(format!("slot {slot}.{phase_name}: serialize context: {e}"))
        })?;

        match plugin.handle_ipc(&method, &ctx_value) {
            // `None` — plugin didn't claim the method at all. Recover
            // the payload and move on (it's the same as the one we
            // fed in, since we cloned before the call).
            None => continue,

            // Explicit plugin error from handle_ipc itself (extism
            // crash, bad JSON, etc.). Surface it.
            Some(Err(e)) => return Err(e),

            // Plugin returned a JSON blob — parse as SlotOutcome.
            Some(Ok(raw)) => {
                let outcome: SlotOutcome<P> = serde_json::from_value(raw).map_err(|e| {
                    VpnError::Unknown(format!(
                        "slot {slot}.{phase_name}: parse outcome: {e}"
                    ))
                })?;
                match outcome {
                    SlotOutcome::Unhandled => {
                        // Plugin explicitly opted out of this phase.
                        // Equivalent to None, but logged separately
                        // in telemetry if a plugin wants to track it.
                        continue;
                    }
                    SlotOutcome::Unchanged => {
                        any_handled = true;
                        // Keep `payload` as-is (already cloned into ctx).
                        continue;
                    }
                    SlotOutcome::Continue {
                        payload: new_payload,
                    } => {
                        any_handled = true;
                        payload = new_payload;
                        continue;
                    }
                    SlotOutcome::Halt {
                        payload: final_payload,
                    } => {
                        return Ok(Some(final_payload));
                    }
                    SlotOutcome::Error { message } => {
                        return Err(VpnError::Unknown(format!(
                            "slot {slot}.{phase_name}: {message}"
                        )));
                    }
                }
            }
        }
    }

    if any_handled {
        Ok(Some(payload))
    } else {
        Ok(None)
    }
}

/// Generate a short invocation id for slot-chain correlation. Uses
/// the low 64 bits of a monotonic counter seeded from a random start
/// so two daemon runs don't collide in shared telemetry backends.
///
/// Not cryptographically unique — just enough to pair `before`/`after`
/// phases in a busy trace log.
pub fn new_invocation_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{:x}-{:x}", seed.wrapping_add(n), n)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Authenticator;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    /// Fake plugin that records every method it's called with and
    /// returns whatever `responses` says for each phase. Used below
    /// to pin the slot-chain semantics down.
    struct RecordingPlugin {
        calls: Mutex<Vec<(String, Value)>>,
        responses: Mutex<Vec<Option<Result<Value, VpnError>>>>,
    }

    impl RecordingPlugin {
        fn new(responses: Vec<Option<Result<Value, VpnError>>>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(responses),
            }
        }
    }

    impl Plugin for RecordingPlugin {
        fn name(&self) -> &str {
            "recording"
        }
        fn authenticator(&self) -> Option<&dyn Authenticator> {
            None
        }
        fn handle_ipc(&self, method: &str, params: &Value) -> Option<Result<Value, VpnError>> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_string(), params.clone()));
            self.responses.lock().unwrap().remove(0)
        }
    }

    /// Payload stand-in. Any `{"n": u32}` will do.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Pay {
        n: u32,
    }

    fn outcome_unchanged() -> Value {
        json!({"kind": "unchanged"})
    }
    fn outcome_continue(n: u32) -> Value {
        json!({"kind": "continue", "payload": {"n": n}})
    }
    fn outcome_halt(n: u32) -> Value {
        json!({"kind": "halt", "payload": {"n": n}})
    }
    fn outcome_error(msg: &str) -> Value {
        json!({"kind": "error", "message": msg})
    }
    fn outcome_unhandled() -> Value {
        json!({"kind": "unhandled"})
    }

    #[test]
    fn chain_with_no_phases_returns_none() {
        let plug = RecordingPlugin::new(vec![None, None, None]);
        let result = run_slot_chain(&plug, "test.slot", 1, "inv-1", Pay { n: 42 });
        assert_eq!(result.unwrap(), None);
        let calls = plug.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].0, "slot.test.slot.before");
        assert_eq!(calls[1].0, "slot.test.slot.exec");
        assert_eq!(calls[2].0, "slot.test.slot.after");
    }

    #[test]
    fn unchanged_phase_preserves_payload_and_marks_handled() {
        let plug = RecordingPlugin::new(vec![Some(Ok(outcome_unchanged())), None, None]);
        let result = run_slot_chain(&plug, "s", 1, "inv", Pay { n: 7 });
        assert_eq!(result.unwrap(), Some(Pay { n: 7 }));
    }

    #[test]
    fn continue_phase_folds_new_payload_into_next() {
        let plug = RecordingPlugin::new(vec![
            Some(Ok(outcome_continue(10))),  // before: n=7 → n=10
            Some(Ok(outcome_continue(100))), // exec: n=10 → n=100
            Some(Ok(outcome_unchanged())),   // after: observe only
        ]);
        let result = run_slot_chain(&plug, "s", 1, "inv", Pay { n: 7 });
        assert_eq!(result.unwrap(), Some(Pay { n: 100 }));

        // Confirm the exec phase saw the payload emitted by before.
        let calls = plug.calls.lock().unwrap();
        let exec_ctx: SlotContext<Pay> = serde_json::from_value(calls[1].1.clone()).unwrap();
        assert_eq!(exec_ctx.payload.n, 10);
    }

    #[test]
    fn halt_in_before_skips_exec_and_after() {
        let plug = RecordingPlugin::new(vec![
            Some(Ok(outcome_halt(999))),
            None, // never called
            None, // never called
        ]);
        let result = run_slot_chain(&plug, "s", 1, "inv", Pay { n: 1 });
        assert_eq!(result.unwrap(), Some(Pay { n: 999 }));
        // Only `before` was dispatched — the other two Nones in our
        // vec remain consumed, but the fn should not have advanced.
        let calls = plug.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn unhandled_variant_is_equivalent_to_none() {
        let plug = RecordingPlugin::new(vec![
            Some(Ok(outcome_unhandled())),
            None,
            None,
        ]);
        let result = run_slot_chain(&plug, "s", 1, "inv", Pay { n: 3 });
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn error_variant_stops_chain_with_error() {
        let plug = RecordingPlugin::new(vec![
            Some(Ok(outcome_unchanged())),
            Some(Ok(outcome_error("boom"))),
            None, // never called
        ]);
        let result = run_slot_chain(&plug, "s", 1, "inv", Pay { n: 1 });
        assert!(matches!(result, Err(VpnError::Unknown(_))));
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("boom"));
    }

    #[test]
    fn mixed_phases_produce_final_payload() {
        let plug = RecordingPlugin::new(vec![
            None,                                 // before skipped
            Some(Ok(outcome_continue(50))),       // exec transforms
            Some(Ok(outcome_unchanged())),        // after observes
        ]);
        let result = run_slot_chain(&plug, "s", 1, "inv", Pay { n: 1 });
        assert_eq!(result.unwrap(), Some(Pay { n: 50 }));
    }

    #[test]
    fn invocation_ids_are_unique() {
        let ids: Vec<String> = (0..10).map(|_| new_invocation_id()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().cloned().collect();
        assert_eq!(unique.len(), 10);
    }

    /// Shared sanity: a real-looking payload — two slots chained back
    /// to back should not interfere. Not a regression test of the
    /// implementation but a guard that I haven't put global state
    /// somewhere stupid.
    #[test]
    fn two_independent_chains_do_not_share_state() {
        let p1 = Arc::new(RecordingPlugin::new(vec![
            None,
            Some(Ok(outcome_continue(2))),
            None,
        ]));
        let p2 = Arc::new(RecordingPlugin::new(vec![
            None,
            Some(Ok(outcome_continue(3))),
            None,
        ]));
        let r1 = run_slot_chain(p1.as_ref(), "a", 1, "x", Pay { n: 1 });
        let r2 = run_slot_chain(p2.as_ref(), "b", 1, "y", Pay { n: 1 });
        assert_eq!(r1.unwrap(), Some(Pay { n: 2 }));
        assert_eq!(r2.unwrap(), Some(Pay { n: 3 }));
    }
}
