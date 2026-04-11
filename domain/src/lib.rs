//! Pingle domain layer — pure contracts and value types.
//!
//! This crate defines the shared language of the system: traits that VPN engine
//! adapters implement, value types that flow through all layers, error types,
//! and the typed middleware pipeline framework.
//!
//! It has **zero external dependencies** — no serde, no Tauri, no async runtime.
//!
//! This crate is intentionally kept dependency-free so it can be reused in:
//! - The Tauri headless daemon (`app`)
//! - The headless CLI binary (`cli`)
//! - Future Flutter FFI bindings (mobile companion)
//! - Integration tests without a running Tauri instance
//!
//! # Modules
//! - [`errors`] — [`VpnError`] — unified error type for all layers.
//! - [`types`] — [`ConnectionState`], [`CoreEvent`], [`Outbound`], [`ConnectionInfo`], etc.
//! - [`traits`] — [`VpnCore`] (lifecycle) and [`SettingsStorage`] contracts.
//! - [`pipeline`] — [`Operation`](pipeline::Operation), [`Handler`](pipeline::Handler),
//!   [`Hook`](pipeline::Hook), [`WrapHook`](pipeline::WrapHook),
//!   [`Pipeline`](pipeline::Pipeline) — the typed lifecycle hook framework.
//! - [`ops`] — Typed operations: [`OpConnect`](ops::OpConnect),
//!   [`OpListOutbounds`](ops::OpListOutbounds), etc.

pub mod errors;
pub mod hooks; // kept empty for backward compat — will be removed
pub mod ops;
pub mod pipeline;
pub mod traits;
pub mod types;

// Convenience re-exports.
pub use errors::VpnError;
pub use ops::*;
pub use pipeline::{FnHook, FnWrapHook, Handler, Hook, Operation, Pipeline, WrapHook};
pub use traits::{
    Authenticator, InstallIdProvider, Plugin, Profile, ProfileMeta, ProfileSource, ProfileStorage,
    SettingsStorage, TempConfigPath, VpnCore,
};
pub use types::{
    ConnectionInfo, ConnectionState, CoreDescriptor, CoreEvent, CoreInfo, CoreSource, Outbound,
    OutboundProtocol, OutboundTransport, PrerequisiteCheck,
};
