//! Domain traits.
//!
//! These define the contracts that outer layers must implement.
//! Domain code depends on these traits, never on concrete implementations.

pub mod plugin;
pub mod plugin_slot;
pub mod plugin_slot_payloads;
pub mod profile_storage;
pub mod settings_storage;
pub mod vpn_core;

pub use plugin::{Authenticator, Plugin};
pub use plugin_slot::{
    new_invocation_id, phase, run_slot_chain, run_slot_chain_observed, slot_names,
    NullSlotObserver, SlotChainResult, SlotContext, SlotObservation, SlotObserver, SlotOutcome,
};
pub use plugin_slot_payloads::{
    ConnectResult, CoreLifecycleResult, CoreStartPayload, CoreStopPayload, DaemonShutdownPayload,
    DaemonStartupPayload, DisconnectResult, IpcDispatchOutcome, IpcDispatchPayload, LatencyResult,
    OutboundSelectPayload, OutboundTestLatencyPayload, ProfileActivatePayload,
    ProfilePersistPayload, VpnConnectPayload, VpnDisconnectPayload, CORE_START_WIRE_VERSION,
    CORE_STOP_WIRE_VERSION, DAEMON_SHUTDOWN_WIRE_VERSION, DAEMON_STARTUP_WIRE_VERSION,
    IPC_DISPATCH_WIRE_VERSION, OUTBOUND_SELECT_WIRE_VERSION, OUTBOUND_TEST_LATENCY_WIRE_VERSION,
    PROFILE_ACTIVATE_WIRE_VERSION, PROFILE_PERSIST_WIRE_VERSION, VPN_CONNECT_WIRE_VERSION,
    VPN_DISCONNECT_WIRE_VERSION,
};
pub use profile_storage::{
    InstallIdProvider, Profile, ProfileMeta, ProfileSource, ProfileStorage, TempConfigPath,
};
pub use settings_storage::SettingsStorage;
pub use vpn_core::VpnCore;
