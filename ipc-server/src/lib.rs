//! Newline-delimited JSON-RPC 2.0 server exposing the pingle
//! [`VpnManager`](service::VpnManager) over three transports:
//!
//! - **Unix domain socket** (Unix only) at a per-user well-known path
//! - **TCP loopback** on `127.0.0.1` (OS-assigned port)
//! - **UDP discovery beacon** on `0.0.0.0:7878` answering broadcast probes
//!
//! See module docs for protocol details. The crate is intentionally light:
//! no async runtime, no Tauri dependency, no extra transports — pure
//! `std::thread` + `std::net` + `serde_json`. That keeps it embeddable in
//! both the Tauri daemon (`app`) and a headless test/CLI binary.

pub mod broadcaster;
pub mod discovery;
pub mod methods;
pub mod protocol;
pub mod protocol_constants;
pub mod server;

pub use broadcaster::EventBroadcaster;
pub use server::{start, ServerHandle, PROTOCOL_VERSION};
