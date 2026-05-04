//! Push-event broadcaster.
//!
//! Holds a list of channel senders, one per subscribed client. The daemon
//! calls [`EventBroadcaster::publish`] when state changes; each connection
//! thread reads its own receiver and writes the notification frame to its
//! socket.
//!
//! Disconnected clients are detected lazily — `send` on a closed receiver
//! fails and the entry is dropped on the next publish.

use crate::protocol::Notification;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{mpsc, Mutex};

/// A single subscriber's outbound queue.
pub type Subscriber = mpsc::Sender<Notification>;

/// Multi-producer, multi-subscriber broadcaster.
///
/// Cheap to clone (it's an `Arc<Mutex<Vec<…>>>` internally). Clone it into
/// every part of the daemon that needs to publish events.
pub struct EventBroadcaster {
    subs: Mutex<Vec<Subscriber>>,
    history: Mutex<VecDeque<Notification>>,
    history_limit: usize,
}

impl EventBroadcaster {
    pub fn new() -> Self {
        Self {
            subs: Mutex::new(Vec::new()),
            history: Mutex::new(VecDeque::new()),
            history_limit: 128,
        }
    }

    /// Register a new subscriber. Returns the receiver half of the channel
    /// — the connection thread loops on `recv()` and writes each item to
    /// the wire.
    pub fn subscribe(&self) -> mpsc::Receiver<Notification> {
        let (tx, rx) = mpsc::channel();
        {
            let history = self.history.lock().unwrap_or_else(|e| e.into_inner());
            for notif in history.iter() {
                let _ = tx.send(notif.clone());
            }
        }
        let mut subs = self.subs.lock().unwrap_or_else(|e| e.into_inner());
        subs.push(tx);
        rx
    }

    /// Publish a notification to every subscriber. Stale (closed) channels
    /// are pruned.
    pub fn publish(&self, notif: Notification) {
        {
            let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
            history.push_back(notif.clone());
            while history.len() > self.history_limit {
                history.pop_front();
            }
        }
        let mut subs = self.subs.lock().unwrap_or_else(|e| e.into_inner());
        subs.retain(|tx| tx.send(notif.clone()).is_ok());
    }

    /// Convenience: publish a `state.changed` event.
    pub fn publish_state(&self, state: &str, running: bool, core: &str) {
        self.publish(Notification::new(
            "event.stateChanged",
            json!({ "state": state, "running": running, "core": core }),
        ));
    }

    /// Convenience: publish a free-form log line.
    pub fn publish_log(&self, level: &str, message: &str) {
        self.publish(Notification::new(
            "event.log",
            json!({ "level": level, "message": message }),
        ));
    }

    /// Convenience: publish an arbitrary event.
    pub fn publish_custom(&self, method: &str, params: Value) {
        self.publish(Notification::new(method, params));
    }

    /// Number of currently registered subscribers (after pruning is best-effort).
    pub fn subscriber_count(&self) -> usize {
        self.subs.lock().map(|s| s.len()).unwrap_or(0)
    }
}

impl Default for EventBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscriber_receives_published_event() {
        let b = EventBroadcaster::new();
        let rx = b.subscribe();
        b.publish_state("connected", true, "sing-box");

        let notif = rx.recv().unwrap();
        assert_eq!(notif.method, "event.stateChanged");
        assert_eq!(notif.params["state"], "connected");
        assert_eq!(notif.params["running"], true);
        assert_eq!(notif.params["core"], "sing-box");
    }

    #[test]
    fn dropped_subscriber_is_pruned() {
        let b = EventBroadcaster::new();
        let rx = b.subscribe();
        assert_eq!(b.subscriber_count(), 1);
        drop(rx);
        b.publish_state("disconnected", false, "mock");
        assert_eq!(b.subscriber_count(), 0);
    }

    #[test]
    fn multiple_subscribers_each_receive() {
        let b = EventBroadcaster::new();
        let rx1 = b.subscribe();
        let rx2 = b.subscribe();
        b.publish_log("info", "hello");
        assert_eq!(rx1.recv().unwrap().params["message"], "hello");
        assert_eq!(rx2.recv().unwrap().params["message"], "hello");
    }

    #[test]
    fn late_subscriber_receives_recent_history() {
        let b = EventBroadcaster::new();
        b.publish_log("info", "startup");

        let rx = b.subscribe();
        let notif = rx.recv().unwrap();
        assert_eq!(notif.method, "event.log");
        assert_eq!(notif.params["level"], "info");
        assert_eq!(notif.params["message"], "startup");
    }
}
