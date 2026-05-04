pub mod broadcaster;
pub mod discovery;
pub mod logging;
pub mod protocol;
pub mod runtime_paths;
pub mod slot_observer;

pub use broadcaster::EventBroadcaster;
pub use runtime_paths::runtime_paths_json;
pub use slot_observer::BroadcastingSlotObserver;

/// Daemon IPC protocol version. Bump on breaking wire-format changes.
pub const PROTOCOL_VERSION: u32 = 1;
