//! Watcher trait + the event types it emits.
//!
//! See [`Watcher`] for the entry point and [`UpdateEvent`] for the
//! event type subscribers receive.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::mpsc::Receiver;

/// One IP address bound to an interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpAddrInfo {
    /// Stringified IP — `"192.168.1.10"` or `"fe80::1"`.
    pub ip: String,
    /// CIDR prefix length.
    pub prefix_len: u8,
    /// `true` for IPv4, `false` for IPv6.
    pub is_v4: bool,
}

/// Snapshot of one network interface as the watcher saw it at a moment in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IfaceSnapshot {
    /// Platform interface index.
    pub index: u32,
    /// Platform interface name (e.g. `"en0"`, `"wlan0"`, `"Ethernet 2"`).
    pub name: String,
    /// MAC address as a colon-separated hex string. Empty if unavailable.
    pub hw_addr: String,
    /// All IP addresses currently bound to this interface.
    pub ips: Vec<IpAddrInfo>,
}

/// One change event emitted by a [`Watcher`].
///
/// All variants carry a full snapshot of the affected interface (or
/// the index, when the interface has just disappeared) so subscribers
/// don't have to maintain their own mirror of the system state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateEvent {
    /// A new interface appeared (boot, USB ethernet plug-in, VPN tunnel up).
    Added(IfaceSnapshot),
    /// An existing interface disappeared (cable unplug, VPN down).
    Removed {
        /// Platform interface index.
        index: u32,
        /// Best-effort name (may be a placeholder when the underlying
        /// crate doesn't preserve the name across the diff).
        name: String,
    },
    /// An existing interface gained / lost addresses or changed hardware id.
    Modified {
        /// Snapshot of the interface as it looks *after* the change.
        snapshot: IfaceSnapshot,
        /// IP addresses added since the previous observation.
        addrs_added: Vec<IpAddrInfo>,
        /// IP addresses removed since the previous observation.
        addrs_removed: Vec<IpAddrInfo>,
        /// Whether the MAC address changed since the previous observation.
        hw_addr_changed: bool,
    },
}

/// One-shot snapshot of all interfaces. Used by [`Watcher::list_interfaces`].
pub type IfaceMap = HashMap<u32, IfaceSnapshot>;

/// Cross-platform network interface watcher.
///
/// Implementations are typically backed by a native platform crate (the
/// canonical implementation is `crate::backend::NetwatcherBackend`).
/// The trait is small enough to mock in tests; see `crate::plugin` for
/// the in-process hook slot that sits in front of any real backend.
pub trait Watcher: Send + Sync {
    /// One-shot list of all interfaces present right now.
    ///
    /// # Errors
    ///
    /// Returns an error string if the platform API call fails.
    fn list_interfaces(&self) -> Result<IfaceMap, String>;

    /// Subscribe to push events. The returned [`Receiver`] yields
    /// [`UpdateEvent`] values whenever the OS reports a change.
    ///
    /// Each call returns a fresh receiver — implementations broadcast.
    /// Dropping the receiver unsubscribes (no resource leak).
    ///
    /// # Errors
    ///
    /// Returns an error string if the watcher cannot start (typically a
    /// permission issue or a platform API failure).
    fn subscribe(&self) -> Result<Receiver<UpdateEvent>, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_addr_info_round_trip_serde() {
        let original = IpAddrInfo {
            ip: "192.168.1.10".into(),
            prefix_len: 24,
            is_v4: true,
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: IpAddrInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn iface_snapshot_round_trip_serde() {
        let original = IfaceSnapshot {
            index: 5,
            name: "en0".into(),
            hw_addr: "aa:bb:cc:dd:ee:ff".into(),
            ips: vec![IpAddrInfo {
                ip: "10.0.0.1".into(),
                prefix_len: 8,
                is_v4: true,
            }],
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: IfaceSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn update_event_added_round_trip_serde() {
        let snap = IfaceSnapshot {
            index: 1,
            name: "lo0".into(),
            hw_addr: String::new(),
            ips: vec![],
        };
        let event = UpdateEvent::Added(snap);
        let json = serde_json::to_string(&event).unwrap();
        let parsed: UpdateEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn update_event_removed_round_trip_serde() {
        let event = UpdateEvent::Removed {
            index: 7,
            name: "tun0".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: UpdateEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn update_event_modified_round_trip_serde() {
        let event = UpdateEvent::Modified {
            snapshot: IfaceSnapshot {
                index: 3,
                name: "en1".into(),
                hw_addr: "11:22:33:44:55:66".into(),
                ips: vec![],
            },
            addrs_added: vec![IpAddrInfo {
                ip: "10.0.0.2".into(),
                prefix_len: 8,
                is_v4: true,
            }],
            addrs_removed: vec![],
            hw_addr_changed: false,
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: UpdateEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, event);
    }
}
