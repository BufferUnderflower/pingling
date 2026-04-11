//! Pingle data layer — [`SettingsStorage`](domain::SettingsStorage) implementations.
//!
//! Provides concrete implementations injected at the application boundary:
//! - [`MemorySettingsStorage`] — in-process hash map. Used in all unit tests
//!   and in the `cli` binary (no persistence needed per invocation).
//! - [`TauriStoreSettings`] — backed by `tauri-plugin-store` (atomic JSON file).
//!   Used by the Tauri daemon so settings (config path, last core, etc.) survive
//!   restarts. Settings are also exposed to the Flutter UI via JSON-RPC
//!   (`settings.get` / `settings.set`) so the Flutter app can read and write them.

pub mod memory;
pub mod profile_store;

#[cfg(feature = "tauri-persist")]
pub mod tauri_store;

pub use memory::MemorySettingsStorage;
pub use profile_store::EncryptedProfileStore;

#[cfg(feature = "tauri-persist")]
pub use tauri_store::TauriStoreSettings;
