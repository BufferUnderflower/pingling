//! Newline-delimited JSON-RPC 2.0 server exposing the pingle
//! [`VpnManager`](service::VpnManager) over three transports:
//!
//! - **Unix domain socket** (Unix only) at a per-user well-known path
//! - **TCP loopback** on `127.0.0.1` (OS-assigned port)
//! - **UDP discovery beacon** on `0.0.0.0:7878` answering broadcast probes
//!
//! See module docs for protocol details. The crate is intentionally light:
//! no async runtime, no Tauri dependency, no extra transports — pure
//! `std::thread` + `std::net` + `serde_json`. That lets it host the
//! headless daemon (`ipc-server-headless`) with zero GUI deps, and
//! leaves the door open for a separate per-user tray process that
//! talks to the daemon over the same JSON-RPC channel. See
//! [`docs/plugin-slots.md`] for the slot-chain observer that fires
//! `event.slot.*` notifications through the broadcaster.

pub mod broadcaster;
pub mod deeplink;
pub mod discovery;
pub mod logging;
pub mod methods;
pub mod protocol;
pub mod protocol_constants;
pub mod runtime_monitor;
pub mod runtime_paths;
pub mod server;
pub mod slot_observer;

pub use broadcaster::EventBroadcaster;
pub use runtime_monitor::spawn_runtime_monitor;
pub use runtime_paths::runtime_paths_json;
pub use server::{start, start_with_broadcaster, ServerHandle, PROTOCOL_VERSION};
pub use slot_observer::BroadcastingSlotObserver;
