//! Domain traits.
//!
//! These define the contracts that outer layers must implement.
//! Domain code depends on these traits, never on concrete implementations.

pub mod plugin;
pub mod profile_storage;
pub mod settings_storage;
pub mod vpn_core;

pub use plugin::{Authenticator, Plugin};
pub use profile_storage::{
    InstallIdProvider, Profile, ProfileMeta, ProfileSource, ProfileStorage, TempConfigPath,
};
pub use settings_storage::SettingsStorage;
pub use vpn_core::VpnCore;
