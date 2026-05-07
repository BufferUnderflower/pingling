//! Slot-chain observer that logs and broadcasts IPC events.
//!
//! The daemon passes one instance of [`BroadcastingSlotObserver`] into
//! every [`pingling_domain::run_slot_chain_observed`] call. It fans each
//! observation out to two sinks:
//!
//! 1. **`log::debug!` / `log::warn!`** on the `ipc_server::slot` target
//!    so operators can trace slot dispatch with `RUST_LOG=ipc_server::slot=debug`.
//!    Errors go through `warn!` so they survive default filters.
//!
//! 2. **IPC `event.slot.*` notifications** broadcast through the same
//!    [`EventBroadcaster`] used for `event.stateChanged`. Subscribers
//!    see `event.slot.enter`, `event.slot.unchanged`, `event.slot.continue`,
//!    `event.slot.halt`, `event.slot.unhandled`, `event.slot.skipped`,
//!    and `event.slot.error` notifications. The notification's
//!    `params` carry `{slot, phase, wire_version, invocation_id,
//!    payload, error?}` so a listener can replay phase transitions
//!    without needing a side channel.
//!
//! ## Why broadcast every phase transition
//!
//! Two reasons. First, **observability for free**: as long as a
//! client is listening, it can count method calls, measure phase
//! latency by pairing `enter`/`continue`, or watch for plugin
//! errors. No plugin needed. Second, **future mTLS credential
//! rotation** and other cross-cutting features can subscribe to
//! `event.slot.vpn.connect.before` and inject a fresh token before
//! `exec` runs without touching any daemon code.
//!
//! ## Toggle
//!
//! Always-on broadcasting may be too chatty for some deployments.
//! The observer exposes a `set_broadcast_enabled(bool)` and a
//! `set_log_enabled(bool)` switch so the daemon can react to config
//! changes at runtime without replacing the Arc. Default: both
//! enabled. For noisy operational slots consider wiring the
//! `PINGLING_SLOT_BROADCAST` env var to turn broadcasting off in
//! production builds until a listener is actually expected.

use crate::broadcaster::EventBroadcaster;
use pingling_domain::{SlotObservation, SlotObserver};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Observer that logs every slot phase and publishes an IPC event
/// describing it. Cheap to clone (it's an `Arc<Inner>`); one instance
/// is created at daemon startup and passed into every slot dispatch.
#[derive(Clone)]
pub struct BroadcastingSlotObserver {
    inner: Arc<Inner>,
}

struct Inner {
    broadcaster: Arc<EventBroadcaster>,
    log_enabled: AtomicBool,
    broadcast_enabled: AtomicBool,
}

impl BroadcastingSlotObserver {
    /// Construct with both log + broadcast enabled by default. Call
    /// [`set_log_enabled`] / [`set_broadcast_enabled`] afterwards to
    /// tune per deployment.
    pub fn new(broadcaster: Arc<EventBroadcaster>) -> Self {
        Self {
            inner: Arc::new(Inner {
                broadcaster,
                log_enabled: AtomicBool::new(true),
                broadcast_enabled: AtomicBool::new(true),
            }),
        }
    }

    /// Toggle the log sink. Use `false` on hot paths to avoid
    /// serialization overhead when RUST_LOG filters would drop the
    /// message anyway.
    pub fn set_log_enabled(&self, enabled: bool) {
        self.inner.log_enabled.store(enabled, Ordering::Relaxed);
    }

    /// Toggle the IPC broadcast sink. Set `false` when no client is
    /// subscribed; set `true` at least once for testing so
    /// `daemon-v0.1.3` users can watch phase transitions with a
    /// JSON-RPC client.
    pub fn set_broadcast_enabled(&self, enabled: bool) {
        self.inner
            .broadcast_enabled
            .store(enabled, Ordering::Relaxed);
    }
}

impl SlotObserver for BroadcastingSlotObserver {
    fn observe(&self, o: SlotObservation<'_>) {
        // -------- log sink --------
        if self.inner.log_enabled.load(Ordering::Relaxed) {
            match o.event {
                "error" => log::warn!(
                    target: "ipc_server::slot",
                    "slot={} phase={} event=error msg={}",
                    o.slot,
                    o.phase,
                    o.error_message.unwrap_or("(none)")
                ),
                "halt" | "continue" => log::debug!(
                    target: "ipc_server::slot",
                    "slot={} phase={} event={} id={}",
                    o.slot,
                    o.phase,
                    o.event,
                    o.invocation_id
                ),
                _ => log::trace!(
                    target: "ipc_server::slot",
                    "slot={} phase={} event={} id={}",
                    o.slot,
                    o.phase,
                    o.event,
                    o.invocation_id
                ),
            }
        }

        // -------- broadcast sink --------
        if self.inner.broadcast_enabled.load(Ordering::Relaxed) {
            // One method per event kind so subscribers can filter
            // with a simple method-name match. The params carry the
            // full observation so listeners can correlate without a
            // lookup table.
            let method = format!("event.slot.{}", o.event);
            let mut params = json!({
                "slot": o.slot,
                "phase": o.phase,
                "wire_version": o.wire_version,
                "invocation_id": o.invocation_id,
                "payload": o.payload_json,
            });
            if let Some(msg) = o.error_message {
                params["error"] = Value::String(msg.to_string());
            }
            self.inner.broadcaster.publish_custom(&method, params);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pingling_domain::SlotObservation;

    #[test]
    fn broadcasts_every_event_kind_as_its_own_method() {
        let broadcaster = Arc::new(EventBroadcaster::new());
        let rx = broadcaster.subscribe();
        let observer = BroadcastingSlotObserver::new(broadcaster.clone());

        let payload = json!({"n": 1});
        for event in [
            "enter",
            "unchanged",
            "continue",
            "halt",
            "unhandled",
            "skipped",
            "error",
        ] {
            observer.observe(SlotObservation {
                slot: "test.slot",
                phase: "exec",
                wire_version: 1,
                invocation_id: "inv-1",
                event,
                payload_json: &payload,
                error_message: if event == "error" { Some("boom") } else { None },
            });
        }

        // Drain the subscriber and verify each event became a
        // distinct IPC notification with the expected method name.
        let mut received = Vec::new();
        while let Ok(n) = rx.try_recv() {
            received.push(n.method);
        }
        assert_eq!(
            received,
            vec![
                "event.slot.enter",
                "event.slot.unchanged",
                "event.slot.continue",
                "event.slot.halt",
                "event.slot.unhandled",
                "event.slot.skipped",
                "event.slot.error",
            ]
        );
    }

    #[test]
    fn broadcast_disabled_produces_no_events() {
        let broadcaster = Arc::new(EventBroadcaster::new());
        let rx = broadcaster.subscribe();
        let observer = BroadcastingSlotObserver::new(broadcaster.clone());
        observer.set_broadcast_enabled(false);

        observer.observe(SlotObservation {
            slot: "s",
            phase: "exec",
            wire_version: 1,
            invocation_id: "x",
            event: "enter",
            payload_json: &json!({}),
            error_message: None,
        });

        assert!(rx.try_recv().is_err(), "no events when broadcast disabled");
    }

    #[test]
    fn error_event_attaches_error_message_to_params() {
        let broadcaster = Arc::new(EventBroadcaster::new());
        let rx = broadcaster.subscribe();
        let observer = BroadcastingSlotObserver::new(broadcaster.clone());
        observer.observe(SlotObservation {
            slot: "s",
            phase: "exec",
            wire_version: 1,
            invocation_id: "x",
            event: "error",
            payload_json: &json!({}),
            error_message: Some("bad config"),
        });
        let n = rx.recv().unwrap();
        assert_eq!(n.method, "event.slot.error");
        assert_eq!(n.params["error"], "bad config");
    }
}
