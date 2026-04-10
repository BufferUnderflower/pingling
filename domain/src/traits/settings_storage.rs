//! Key/Value settings storage contract.
//!
//! Implementations can be in-memory (for tests) or persistent (tauri-plugin-store).

use crate::errors::VpnError;

/// Contract for a key/value settings store.
///
/// All values are strings. Implementations must be `Send + Sync` so they can
/// be shared across threads via `Arc<Mutex<_>>`.
pub trait SettingsStorage: Send + Sync {
    /// Retrieve a string value by key.
    ///
    /// Returns `Ok(None)` if the key does not exist.
    fn get_string(&self, key: &str) -> Result<Option<String>, VpnError>;

    /// Store a string value, overwriting any previous value for this key.
    fn set_string(&mut self, key: &str, value: &str) -> Result<(), VpnError>;

    /// Remove a key from the store.
    ///
    /// No-op (not an error) if the key does not exist.
    fn remove(&mut self, key: &str) -> Result<(), VpnError>;

    /// Check whether a key exists in the store.
    fn has(&self, key: &str) -> Result<bool, VpnError> {
        Ok(self.get_string(key)?.is_some())
    }

    /// Return all keys currently in the store.
    fn keys(&self) -> Result<Vec<String>, VpnError>;

    /// Remove all entries from the store.
    fn clear(&mut self) -> Result<(), VpnError>;

    /// Return the number of entries in the store.
    fn len(&self) -> Result<usize, VpnError> {
        Ok(self.keys()?.len())
    }

    /// Returns `true` if the store contains no entries.
    fn is_empty(&self) -> Result<bool, VpnError> {
        Ok(self.len()? == 0)
    }
}
