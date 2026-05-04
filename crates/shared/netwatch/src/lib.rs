//! Cross-platform network interface watcher for a Pingling host.
//!
//! Wraps the [`netwatcher`](https://crates.io/crates/netwatcher) crate (which
//! itself wraps platform-native APIs: `NotifyIpInterfaceChange` on Windows,
//! `SystemConfiguration` on macOS, `netlink` on Linux) and exposes:
//!
//! - A [`Watcher`] trait the daemon links against directly (no IPC, no wasm).
//! - An [`UpdateEvent`] enum that downstream subscribers receive on a channel.
//! - An optional [`NetwatchPlugin`] hook slot that sits between the raw events
//!   and downstream subscribers — for debugging and policy injection. The
//!   default is [`PassthroughPlugin`] (zero overhead, no allocation).
//!
//! ## Why a native crate, not a wasm plugin
//!
//! Network change notifications are a *platform* abstraction, not a vendor
//! concern. The reactive event channel needs zero serialization overhead per
//! interface change, and wasm guests can't access platform syscalls without
//! host-imported functions — which means writing the platform code in Rust
//! anyway. The wasm slot here is for *debugging* the interpretation layer, not
//! for the platform abstraction itself.
//!
//! ## Passthrough by default
//!
//! `Watcher::with_plugin(None)` (or simply not setting one) routes raw events
//! straight to subscribers. No wasm file on disk = no plugin ever loaded =
//! identical behavior to a build that omits this crate's plugin module.

pub mod backend;
pub mod plugin;
pub mod watcher;

pub use backend::NetwatcherBackend;
pub use plugin::{NetwatchPlugin, PassthroughPlugin};
pub use watcher::{IfaceMap, IfaceSnapshot, IpAddrInfo, UpdateEvent, Watcher};
