//! [`Watcher`] implementation backed by the [`netwatcher`] crate from crates.io.
//!
//! `netwatcher` already abstracts over Windows `NotifyIpInterfaceChange`,
//! macOS `SystemConfiguration`, and Linux `netlink`. We just adapt its
//! types to ours and forward its callback into a [`Receiver`] channel.

use crate::watcher::{IfaceMap, IfaceSnapshot, IpAddrInfo, UpdateEvent, Watcher};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Production [`Watcher`] backed by the `netwatcher` crate.
///
/// Construct with [`NetwatcherBackend::new`]. The backend lazily starts a
/// real watcher on first `subscribe()` call and keeps it alive until the
/// backend is dropped.
pub struct NetwatcherBackend {
    /// Active watch handle from the netwatcher crate. `None` until the
    /// first `subscribe()` call. Held in a mutex so the lazy start is
    /// race-free.
    handle: Mutex<Option<netwatcher::WatchHandle>>,
    /// Broadcast list of subscriber senders. Each `subscribe()` call
    /// adds one sender; the watcher's callback fans events out to all
    /// of them. Wrapped in `Arc<Mutex<_>>` so the callback closure (which
    /// is `'static`) can hold its own clone.
    subs: Arc<Mutex<Vec<Sender<UpdateEvent>>>>,
}

impl NetwatcherBackend {
    /// Create a new backend. Does not start the underlying watcher yet.
    pub fn new() -> Self {
        Self {
            handle: Mutex::new(None),
            subs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Lazily start the underlying `netwatcher::watch_interfaces` call
    /// on the first subscribe. Subsequent calls are no-ops.
    fn ensure_started(&self) -> Result<(), String> {
        let mut guard = self.handle.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return Ok(());
        }
        let subs = Arc::clone(&self.subs);
        let handle = netwatcher::watch_interfaces(move |update| {
            let events = events_from_update(update);
            let list = subs.lock().unwrap_or_else(|e| e.into_inner());
            for ev in events {
                // Best-effort fan-out — drop senders whose receiver was
                // dropped silently. We don't prune the list here; the
                // dead sender just becomes a no-op.
                for tx in list.iter() {
                    let _ = tx.send(ev.clone());
                }
            }
        })
        .map_err(|e| format!("netwatcher: failed to start: {e}"))?;
        *guard = Some(handle);
        Ok(())
    }
}

impl Default for NetwatcherBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert one `netwatcher::Update` into zero or more [`UpdateEvent`]s.
///
/// `netwatcher` reports diffs as `{added, removed, modified}` lists; we
/// emit one [`UpdateEvent`] per affected interface.
fn events_from_update(update: netwatcher::Update) -> Vec<UpdateEvent> {
    let mut out = Vec::new();
    for added_idx in &update.diff.added {
        if let Some(iface) = update.interfaces.get(added_idx) {
            out.push(UpdateEvent::Added(iface_to_snapshot(iface)));
        }
    }
    for removed_idx in &update.diff.removed {
        // The removed interface is no longer in `update.interfaces`,
        // and `netwatcher` doesn't preserve the name across the diff.
        // We use a placeholder; subscribers should not depend on it.
        out.push(UpdateEvent::Removed {
            index: *removed_idx,
            name: format!("if{removed_idx}"),
        });
    }
    for (idx, diff) in &update.diff.modified {
        if let Some(iface) = update.interfaces.get(idx) {
            out.push(UpdateEvent::Modified {
                snapshot: iface_to_snapshot(iface),
                addrs_added: diff.addrs_added.iter().map(ip_record_to_info).collect(),
                addrs_removed: diff.addrs_removed.iter().map(ip_record_to_info).collect(),
                hw_addr_changed: diff.hw_addr_changed,
            });
        }
    }
    out
}

fn iface_to_snapshot(iface: &netwatcher::Interface) -> IfaceSnapshot {
    IfaceSnapshot {
        index: iface.index,
        name: iface.name.clone(),
        hw_addr: iface.hw_addr.clone(),
        ips: iface.ips.iter().map(ip_record_to_info).collect(),
    }
}

fn ip_record_to_info(rec: &netwatcher::IpRecord) -> IpAddrInfo {
    IpAddrInfo {
        ip: rec.ip.to_string(),
        prefix_len: rec.prefix_len,
        is_v4: rec.ip.is_ipv4(),
    }
}

impl Watcher for NetwatcherBackend {
    fn list_interfaces(&self) -> Result<IfaceMap, String> {
        let raw = netwatcher::list_interfaces()
            .map_err(|e| format!("netwatcher: list_interfaces: {e}"))?;
        Ok(raw
            .into_iter()
            .map(|(idx, iface)| (idx, iface_to_snapshot(&iface)))
            .collect())
    }

    fn subscribe(&self) -> Result<Receiver<UpdateEvent>, String> {
        self.ensure_started()?;
        let (tx, rx) = mpsc::channel();
        self.subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: list_interfaces succeeds on the test host.
    /// We do NOT assert non-empty — CI hosts may have unusual configs.
    #[test]
    fn list_interfaces_returns_ok() {
        let backend = NetwatcherBackend::new();
        let result = backend.list_interfaces();
        assert!(result.is_ok(), "list_interfaces failed: {result:?}");
    }
}
