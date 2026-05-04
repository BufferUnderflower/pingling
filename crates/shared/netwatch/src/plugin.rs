//! Optional in-process hook between the [`Watcher`](crate::watcher::Watcher)
//! backend and downstream subscribers.
//!
//! ## Why
//!
//! The hook exists for **debugging and policy injection**, not for the
//! platform abstraction itself. A wasm guest registered here can:
//!
//! - Log every interface change to a remote sink without recompiling the daemon
//! - Suppress noisy events from a known-flapping interface
//! - Inject synthetic events for testing
//! - Apply per-host policy ("on this corporate laptop, ignore Bluetooth NIC")
//!
//! ## Passthrough by default
//!
//! [`PassthroughPlugin`] is the no-op default. The daemon constructs it
//! when no wasm plugin is loaded. Zero allocation, zero serialization,
//! events flow straight through.

use crate::watcher::UpdateEvent;

/// In-process hook between the watcher backend and downstream subscribers.
///
/// Implementations receive every event the backend emits and can:
/// - Pass it through unchanged (default)
/// - Modify the event (e.g. fill in a missing interface name)
/// - Suppress the event entirely (return an empty `Vec`)
/// - Emit one event as multiple events (e.g. split a "modified with both
///   addrs_added and addrs_removed" into two cleaner events)
pub trait NetwatchPlugin: Send + Sync {
    /// Plugin name. Used in logs.
    fn name(&self) -> &str;

    /// Process one event from the backend. Return zero or more events to
    /// forward to subscribers.
    ///
    /// The default implementation forwards unchanged.
    fn process(&self, event: UpdateEvent) -> Vec<UpdateEvent> {
        vec![event]
    }
}

/// The no-op default plugin. Forwards every event unchanged.
///
/// Used when no wasm netwatch plugin is loaded. The daemon constructs one
/// of these as the always-present innermost stage of the chain so the
/// downstream call site doesn't need a `None` branch.
pub struct PassthroughPlugin;

impl NetwatchPlugin for PassthroughPlugin {
    fn name(&self) -> &str {
        "passthrough"
    }
    // process() defaults to forwarding unchanged.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::IfaceSnapshot;

    fn sample_added() -> UpdateEvent {
        UpdateEvent::Added(IfaceSnapshot {
            index: 1,
            name: "en0".into(),
            hw_addr: "00:11:22:33:44:55".into(),
            ips: vec![],
        })
    }

    #[test]
    fn passthrough_forwards_event_unchanged() {
        let plugin = PassthroughPlugin;
        let event = sample_added();
        let out = plugin.process(event.clone());
        assert_eq!(out, vec![event]);
    }

    #[test]
    fn passthrough_name_is_passthrough() {
        assert_eq!(PassthroughPlugin.name(), "passthrough");
    }

    /// Stub plugin that records every call. Used as a fixture.
    struct RecordingPlugin {
        seen: std::sync::Mutex<Vec<UpdateEvent>>,
    }

    impl NetwatchPlugin for RecordingPlugin {
        fn name(&self) -> &str {
            "recording"
        }
        fn process(&self, event: UpdateEvent) -> Vec<UpdateEvent> {
            self.seen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(event.clone());
            vec![event]
        }
    }

    #[test]
    fn custom_plugin_sees_events() {
        let plugin = RecordingPlugin {
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let event = sample_added();
        plugin.process(event.clone());
        let seen = plugin.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], event);
    }
}
