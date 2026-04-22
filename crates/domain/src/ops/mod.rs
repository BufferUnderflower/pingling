//! Typed VPN operations — the vocabulary of the pipeline system.
//!
//! Each operation is a zero-sized struct implementing [`Operation`](crate::pipeline::Operation)
//! with explicit `Input` and `Output` types. This gives compile-time guarantees
//! that middleware and handlers agree on data shapes.
//!
//! # Lifecycle operations (every core supports these)
//!
//! | Operation | Input | Output |
//! |-----------|-------|--------|
//! | [`OpConnect`] | config path, core type | optional [`ConnectionInfo`](crate::ConnectionInfo) |
//! | [`OpDisconnect`] | core type | — |
//! | [`OpRestart`] | config path, core type | optional [`ConnectionInfo`](crate::ConnectionInfo) |
//! | [`OpValidateConfig`] | config path, core type | — |
//! | [`OpGetStatus`] | — | [`ConnectionState`](crate::ConnectionState) |
//!
//! # Capability operations (only cores with registered pipelines)
//!
//! | Operation | Input | Output |
//! |-----------|-------|--------|
//! | [`OpListOutbounds`] | — | `Vec<`[`Outbound`](crate::Outbound)`>` |
//! | [`OpSelectOutbound`] | outbound ID | — |
//! | [`OpTestLatency`] | outbound IDs | latency map |
//!
//! The presence or absence of a pipeline for a capability operation IS
//! the capability declaration. No stringly-typed capability sets.

mod connect;
mod disconnect;
mod list_outbounds;
mod restart;
mod select_outbound;
mod status;
mod test_latency;
mod validate;

pub use connect::*;
pub use disconnect::*;
pub use list_outbounds::*;
pub use restart::*;
pub use select_outbound::*;
pub use status::*;
pub use test_latency::*;
pub use validate::*;
