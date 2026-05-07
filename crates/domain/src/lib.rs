//! Pingling domain layer — pure contracts and value types.
//!
//! This crate defines the shared language of the system: traits that VPN engine
//! adapters implement, value types that flow through all layers, error types,
//! and the typed middleware pipeline framework.
//!
//! It has **zero external dependencies** - no serde, no GUI framework, no async runtime.
//!
//! This crate is intentionally kept dependency-free so it can be reused in:
//! - Daemon binaries
//! - The headless CLI binary (`cli`)
//! - Future Flutter FFI bindings (mobile companion)
//! - Integration tests without a running daemon instance
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
    new_invocation_id, phase, plugin_slot, plugin_slot_payloads, run_slot_chain,
    run_slot_chain_observed, run_slot_phase, run_slot_phase_observed, slot_names, Authenticator,
    ConnectResult, CoreLifecycleResult, CoreStartPayload, CoreStopPayload, DaemonShutdownPayload,
    DaemonStartupPayload, DisconnectResult, InstallIdProvider, LatencyResult, NullSlotObserver,
    OutboundSelectPayload, OutboundTestLatencyPayload, Plugin, Profile, ProfileActivatePayload,
    ProfileMeta, ProfilePersistPayload, ProfileSource, ProfileStorage, SettingsStorage,
    SlotChainResult, SlotContext, SlotObservation, SlotObserver, SlotOutcome, TempConfigPath,
    VpnConnectPayload, VpnCore, VpnDisconnectPayload, CORE_START_WIRE_VERSION,
    CORE_STOP_WIRE_VERSION, DAEMON_SHUTDOWN_WIRE_VERSION, DAEMON_STARTUP_WIRE_VERSION,
    OUTBOUND_SELECT_WIRE_VERSION, OUTBOUND_TEST_LATENCY_WIRE_VERSION,
    PROFILE_ACTIVATE_WIRE_VERSION, PROFILE_PERSIST_WIRE_VERSION, VPN_CONNECT_WIRE_VERSION,
    VPN_DISCONNECT_WIRE_VERSION,
};
pub use types::{
    ConnectionInfo, ConnectionState, CoreDescriptor, CoreEvent, CoreInfo, CoreSource, Outbound,
    OutboundProtocol, OutboundTransport, PrerequisiteCheck,
};
