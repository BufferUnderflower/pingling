//! Persistent [`SettingsStorage`] backed by `tauri-plugin-store`.
//!
//! This module only compiles when the `tauri` feature is active,
//! since it depends on Tauri runtime types.

use domain::{SettingsStorage, VpnError};
use serde_json::Value;
use std::sync::Arc;
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

/// Persistent, file-backed key/value store using Tauri's plugin-store.
pub struct TauriStoreSettings {
    store: Arc<tauri_plugin_store::Store<tauri::Wry>>,
}

impl TauriStoreSettings {
    /// Creates a new store backed by the given file path.
    ///
    /// The path is relative to the app's data directory.
    pub fn new(app_handle: &AppHandle, filename: &str) -> Result<Self, VpnError> {
        let store = app_handle
            .store(filename)
            .map_err(|e| VpnError::StorageError(format!("Failed to open store: {}", e)))?;

        Ok(Self { store })
    }
}

impl SettingsStorage for TauriStoreSettings {
    fn get_string(&self, key: &str) -> Result<Option<String>, VpnError> {
        match self.store.get(key) {
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(VpnError::StorageError(format!(
                "Value for key '{}' is not a string",
                key
            ))),
            None => Ok(None),
        }
    }

    fn set_string(&mut self, key: &str, value: &str) -> Result<(), VpnError> {
        self.store
            .set(key.to_string(), Value::String(value.to_string()));

        self.store
            .save()
            .map_err(|e| VpnError::StorageError(format!("Failed to save store: {}", e)))?;
        Ok(())
    }

    fn remove(&mut self, key: &str) -> Result<(), VpnError> {
        self.store.delete(key);
        self.store.save().map_err(|e| {
            VpnError::StorageError(format!("Failed to save store after delete: {}", e))
        })?;
        Ok(())
    }

    fn keys(&self) -> Result<Vec<String>, VpnError> {
        // FIX C1: `store.entries()` returns `Vec<(String, JsonValue)>` directly (not a Result).
        // The old code incorrectly called `.map_err()?` on a Vec, which is a compile error.
        let keys: Vec<String> = self.store.entries().into_iter().map(|(k, _)| k).collect();
        Ok(keys)
    }

    fn clear(&mut self) -> Result<(), VpnError> {
        self.store.clear();
        self.store.save().map_err(|e| {
            VpnError::StorageError(format!("Failed to save store after clear: {}", e))
        })?;
        Ok(())
    }

    fn len(&self) -> Result<usize, VpnError> {
        Ok(self.store.length())
    }

    fn is_empty(&self) -> Result<bool, VpnError> {
        Ok(self.store.length() == 0)
    }
}
