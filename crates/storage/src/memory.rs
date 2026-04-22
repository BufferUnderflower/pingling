//! In-memory [`SettingsStorage`] implementation.
//!
//! Intended for unit tests, integration tests, and rapid prototyping.
//! Data does not persist across restarts.

use pingling_domain::{SettingsStorage, VpnError};
use std::collections::BTreeMap;

/// Thread-safe, in-memory key/value store.
pub struct MemorySettingsStorage {
    data: BTreeMap<String, String>,
}

impl MemorySettingsStorage {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self {
            data: BTreeMap::new(),
        }
    }

    /// Creates a store pre-populated with the given entries.
    pub fn with_entries(entries: &[(&str, &str)]) -> Self {
        let mut storage = Self::new();
        for (k, v) in entries {
            storage.data.insert(k.to_string(), v.to_string());
        }
        storage
    }
}

impl Default for MemorySettingsStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsStorage for MemorySettingsStorage {
    fn get_string(&self, key: &str) -> Result<Option<String>, VpnError> {
        Ok(self.data.get(key).cloned())
    }

    fn set_string(&mut self, key: &str, value: &str) -> Result<(), VpnError> {
        self.data.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn remove(&mut self, key: &str) -> Result<(), VpnError> {
        self.data.remove(key);
        Ok(())
    }

    fn has(&self, key: &str) -> Result<bool, VpnError> {
        Ok(self.data.contains_key(key))
    }

    fn keys(&self) -> Result<Vec<String>, VpnError> {
        Ok(self.data.keys().cloned().collect())
    }

    fn clear(&mut self) -> Result<(), VpnError> {
        self.data.clear();
        Ok(())
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

    #[test]
    fn new_store_is_empty() {
        let store = MemorySettingsStorage::new();
        assert!(store.is_empty().unwrap());
        assert_eq!(store.len().unwrap(), 0);
        assert!(store.keys().unwrap().is_empty());
    }

    #[test]
    fn with_entries_prepopulates() {
        let store = MemorySettingsStorage::with_entries(&[("host", "127.0.0.1"), ("port", "8080")]);
        assert_eq!(store.len().unwrap(), 2);
        assert_eq!(store.get_string("host").unwrap(), Some("127.0.0.1".into()));
        assert_eq!(store.get_string("port").unwrap(), Some("8080".into()));
    }

    #[test]
    fn set_and_get() {
        let mut store = MemorySettingsStorage::new();
        store.set_string("key", "value").unwrap();
        assert_eq!(store.get_string("key").unwrap(), Some("value".into()));
    }

    #[test]
    fn get_missing_key_returns_none() {
        let store = MemorySettingsStorage::new();
        assert_eq!(store.get_string("missing").unwrap(), None);
    }

    #[test]
    fn set_overwrites() {
        let mut store = MemorySettingsStorage::new();
        store.set_string("key", "first").unwrap();
        store.set_string("key", "second").unwrap();
        assert_eq!(store.get_string("key").unwrap(), Some("second".into()));
        assert_eq!(store.len().unwrap(), 1);
    }

    #[test]
    fn remove_existing_key() {
        let mut store = MemorySettingsStorage::new();
        store.set_string("key", "value").unwrap();
        store.remove("key").unwrap();
        assert_eq!(store.get_string("key").unwrap(), None);
        assert!(store.is_empty().unwrap());
    }

    #[test]
    fn remove_missing_key_is_noop() {
        let mut store = MemorySettingsStorage::new();
        store.remove("nonexistent").unwrap(); // should not error
        assert!(store.is_empty().unwrap());
    }

    #[test]
    fn has_existing_key() {
        let mut store = MemorySettingsStorage::new();
        store.set_string("key", "value").unwrap();
        assert!(store.has("key").unwrap());
    }

    #[test]
    fn has_missing_key() {
        let store = MemorySettingsStorage::new();
        assert!(!store.has("key").unwrap());
    }

    #[test]
    fn keys_returns_all_keys_sorted() {
        let mut store = MemorySettingsStorage::new();
        store.set_string("charlie", "3").unwrap();
        store.set_string("alpha", "1").unwrap();
        store.set_string("bravo", "2").unwrap();
        let keys = store.keys().unwrap();
        assert_eq!(keys, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn clear_removes_all() {
        let mut store = MemorySettingsStorage::new();
        store.set_string("a", "1").unwrap();
        store.set_string("b", "2").unwrap();
        store.clear().unwrap();
        assert!(store.is_empty().unwrap());
        assert_eq!(store.keys().unwrap().len(), 0);
    }

    #[test]
    fn len_reflects_count() {
        let mut store = MemorySettingsStorage::new();
        assert_eq!(store.len().unwrap(), 0);
        store.set_string("a", "1").unwrap();
        assert_eq!(store.len().unwrap(), 1);
        store.set_string("b", "2").unwrap();
        assert_eq!(store.len().unwrap(), 2);
        store.remove("a").unwrap();
        assert_eq!(store.len().unwrap(), 1);
    }
}
