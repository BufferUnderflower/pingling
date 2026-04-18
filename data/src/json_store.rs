//! Persistent [`SettingsStorage`] backed by a JSON file on disk.
//!
//! This is the daemon-friendly settings backend used by the tray app and the
//! headless IPC server. It keeps the current key/value map in memory and writes
//! it back atomically after each mutation.

use domain::{SettingsStorage, VpnError};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// File-backed key/value settings storage.
pub struct JsonFileSettingsStorage {
    path: PathBuf,
    data: BTreeMap<String, String>,
}

impl JsonFileSettingsStorage {
    /// Create a store at the platform default path.
    pub fn default_path() -> Result<Self, VpnError> {
        Self::with_path(util::paths::settings_file())
    }

    /// Create a store at an explicit path.
    pub fn with_path(path: PathBuf) -> Result<Self, VpnError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                VpnError::StorageError(format!("create settings dir {}: {e}", parent.display()))
            })?;
        }

        let data = if path.exists() {
            let text = fs::read_to_string(&path).map_err(|e| {
                VpnError::StorageError(format!("read settings {}: {e}", path.display()))
            })?;
            serde_json::from_str(&text).map_err(|e| {
                VpnError::StorageError(format!("parse settings {}: {e}", path.display()))
            })?
        } else {
            BTreeMap::new()
        };

        Ok(Self { path, data })
    }

    /// Path of the underlying JSON file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn persist(&self) -> Result<(), VpnError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                VpnError::StorageError(format!("create settings dir {}: {e}", parent.display()))
            })?;
        }

        let tmp = self.path.with_extension("json.tmp");
        let mut file = fs::File::create(&tmp).map_err(|e| {
            VpnError::StorageError(format!("create settings tmp {}: {e}", tmp.display()))
        })?;
        let json = serde_json::to_string_pretty(&self.data)
            .map_err(|e| VpnError::StorageError(format!("encode settings: {e}")))?;
        file.write_all(json.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| VpnError::StorageError(format!("write settings tmp: {e}")))?;
        drop(file);
        fs::rename(&tmp, &self.path).map_err(|e| {
            VpnError::StorageError(format!("rename settings {}: {e}", self.path.display()))
        })?;
        Ok(())
    }
}

impl SettingsStorage for JsonFileSettingsStorage {
    fn get_string(&self, key: &str) -> Result<Option<String>, VpnError> {
        Ok(self.data.get(key).cloned())
    }

    fn set_string(&mut self, key: &str, value: &str) -> Result<(), VpnError> {
        self.data.insert(key.to_string(), value.to_string());
        self.persist()
    }

    fn remove(&mut self, key: &str) -> Result<(), VpnError> {
        self.data.remove(key);
        self.persist()
    }

    fn keys(&self) -> Result<Vec<String>, VpnError> {
        Ok(self.data.keys().cloned().collect())
    }

    fn clear(&mut self) -> Result<(), VpnError> {
        self.data.clear();
        self.persist()
    }

    fn len(&self) -> Result<usize, VpnError> {
        Ok(self.data.len())
    }

    fn is_empty(&self) -> Result<bool, VpnError> {
        Ok(self.data.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    #[serial]
    fn round_trip_persists_entries() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("settings.json");
        let mut store = JsonFileSettingsStorage::with_path(path.clone()).expect("store");
        store.set_string("config_path", "/tmp/config.json").unwrap();

        let reopened = JsonFileSettingsStorage::with_path(path).expect("reopen");
        assert_eq!(
            reopened.get_string("config_path").unwrap(),
            Some("/tmp/config.json".into())
        );
    }

    #[test]
    #[serial]
    fn default_path_uses_shared_settings_location() {
        let _tempdir = TempDir::new().expect("runtime tempdir");

        let store = JsonFileSettingsStorage::default_path().expect("store");
        assert_eq!(store.path(), util::paths::settings_file().as_path());
    }
}
