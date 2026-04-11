//! AES-256-GCM encrypted profile storage + OS keychain key material.
//!
//! # On-disk layout
//!
//! ```text
//! <base_dir>/
//! ├── active.txt          ASCII UUID of the active profile, or empty
//! ├── <uuid>.json         plaintext metadata sidecar
//! ├── <uuid>.bin          encrypted config body
//! └── ...
//! ```
//!
//! macOS: `~/Library/Application Support/pingle/profiles/`
//! Windows: `%APPDATA%\pingle\profiles\`
//! Linux: `$XDG_CONFIG_HOME/pingle/profiles/`
//!
//! # Encrypted body format (`.bin` file)
//!
//! ```text
//! [0..18)   ASCII magic: b"pingle-profile-v1\n"
//! [18..30)  12-byte AES-GCM nonce (random, unique per write)
//! [30..)    ciphertext || 16-byte GCM tag
//! ```
//!
//! # Key management
//!
//! 32-byte AES-256 key stored in the OS keychain under
//! `one.pingle.vpn/profile-key`. Generated on first access via
//! `OsRng`, read on subsequent calls. If the keychain entry is
//! wiped, a fresh key is generated and existing `.bin` files become
//! unreadable (decrypt error); metadata sidecars remain readable so
//! the user can see which profiles were lost.
//!
//! # Install ID
//!
//! Separate keychain entry `one.pingle.vpn/install-id` stores a UUID
//! generated on first launch. Survives reinstalls on the same user
//! account. Plugins read it via the `daemon.installId` IPC method for
//! trial-abuse correlation.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use domain::{
    InstallIdProvider, Profile, ProfileMeta, ProfileStorage, TempConfigPath, VpnError,
};
use rand::RngCore;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;
use uuid::Uuid;

/// Keychain service name under which the daemon stores its secrets.
const KEYCHAIN_SERVICE: &str = "one.pingle.vpn";

/// Keychain account name for the 32-byte profile encryption key.
const PROFILE_KEY_ACCOUNT: &str = "profile-key";

/// Keychain account name for the install ID UUID.
const INSTALL_ID_ACCOUNT: &str = "install-id";

/// Magic header prefixing every encrypted body file.
const MAGIC: &[u8] = b"pingle-profile-v1\n";

/// Length of the AES-GCM nonce in bytes.
const NONCE_LEN: usize = 12;

/// Encrypted profile storage rooted at a base directory.
///
/// Construct one per daemon instance via [`EncryptedProfileStore::default_path`]
/// (production) or [`EncryptedProfileStore::with_base_dir`] (tests).
///
/// Internally holds a `Mutex` around the "active profile id" state to
/// serialize active-pointer writes; filesystem operations themselves
/// are atomic at the OS level (rename-on-write).
pub struct EncryptedProfileStore {
    base_dir: PathBuf,
    /// Serializes `set_active` / `clear_active` / `active` calls.
    active_lock: Mutex<()>,
    /// Cached cipher. Populated lazily on first use.
    cipher: Mutex<Option<Aes256Gcm>>,
}

impl EncryptedProfileStore {
    /// Construct a store backed by the OS config dir.
    ///
    /// Creates `<config>/pingle/profiles/` if it doesn't exist.
    pub fn default_path() -> Result<Self, VpnError> {
        let base = dirs::config_dir()
            .ok_or_else(|| VpnError::StorageError("cannot resolve OS config dir".to_string()))?
            .join("pingle")
            .join("profiles");
        Self::with_base_dir(base)
    }

    /// Construct a store at an explicit base directory. Used by tests
    /// and by callers that want to override the default location.
    ///
    /// Creates the directory if it doesn't exist.
    pub fn with_base_dir(base_dir: PathBuf) -> Result<Self, VpnError> {
        fs::create_dir_all(&base_dir)
            .map_err(|e| VpnError::StorageError(format!("create profiles dir: {e}")))?;
        Ok(Self {
            base_dir,
            active_lock: Mutex::new(()),
            cipher: Mutex::new(None),
        })
    }

    // -- key management ------------------------------------------------------

    fn cipher(&self) -> Result<Aes256Gcm, VpnError> {
        let mut slot = self.cipher.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = slot.as_ref() {
            return Ok(c.clone());
        }
        let key_bytes = load_or_create_profile_key()?;
        let cipher = Aes256Gcm::new_from_slice(&key_bytes)
            .map_err(|e| VpnError::StorageError(format!("init AES-256-GCM: {e}")))?;
        *slot = Some(cipher.clone());
        Ok(cipher)
    }

    // -- file paths ----------------------------------------------------------

    fn meta_path(&self, id: &str) -> PathBuf {
        self.base_dir.join(format!("{id}.json"))
    }

    fn body_path(&self, id: &str) -> PathBuf {
        self.base_dir.join(format!("{id}.bin"))
    }

    fn active_path(&self) -> PathBuf {
        self.base_dir.join("active.txt")
    }

    fn temp_dir(&self) -> PathBuf {
        std::env::temp_dir().join("pingle-active-configs")
    }

    // -- metadata I/O --------------------------------------------------------

    fn read_meta(&self, id: &str) -> Result<Option<ProfileMeta>, VpnError> {
        let path = self.meta_path(id);
        match fs::read_to_string(&path) {
            Ok(text) => {
                let mut meta: ProfileMeta = serde_json::from_str(&text).map_err(|e| {
                    VpnError::StorageError(format!("parse metadata {}: {e}", path.display()))
                })?;
                // On-disk is_active is always false; caller overlays the real flag.
                meta.is_active = false;
                Ok(Some(meta))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(VpnError::StorageError(format!(
                "read metadata {}: {e}",
                path.display()
            ))),
        }
    }

    fn write_meta(&self, meta: &ProfileMeta) -> Result<(), VpnError> {
        let path = self.meta_path(&meta.id);
        let tmp = path.with_extension("json.tmp");
        let mut f = fs::File::create(&tmp).map_err(|e| {
            VpnError::StorageError(format!("create metadata tmp {}: {e}", tmp.display()))
        })?;
        let json = serde_json::to_string_pretty(meta)
            .map_err(|e| VpnError::StorageError(format!("encode metadata: {e}")))?;
        f.write_all(json.as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|e| VpnError::StorageError(format!("write metadata tmp: {e}")))?;
        drop(f);
        fs::rename(&tmp, &path).map_err(|e| {
            VpnError::StorageError(format!("rename metadata {}: {e}", path.display()))
        })?;
        Ok(())
    }

    // -- body I/O ------------------------------------------------------------

    fn write_body(&self, id: &str, plaintext: &[u8]) -> Result<(), VpnError> {
        let cipher = self.cipher()?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| VpnError::StorageError(format!("encrypt profile body: {e}")))?;

        let path = self.body_path(id);
        let tmp = path.with_extension("bin.tmp");
        let mut f = fs::File::create(&tmp).map_err(|e| {
            VpnError::StorageError(format!("create body tmp {}: {e}", tmp.display()))
        })?;
        f.write_all(MAGIC)
            .and_then(|_| f.write_all(&nonce_bytes))
            .and_then(|_| f.write_all(&ciphertext))
            .and_then(|_| f.sync_all())
            .map_err(|e| VpnError::StorageError(format!("write body tmp: {e}")))?;
        drop(f);
        fs::rename(&tmp, &path).map_err(|e| {
            VpnError::StorageError(format!("rename body {}: {e}", path.display()))
        })?;
        Ok(())
    }

    fn read_body_plaintext(&self, id: &str) -> Result<Vec<u8>, VpnError> {
        let path = self.body_path(id);
        let bytes = fs::read(&path)
            .map_err(|e| VpnError::StorageError(format!("read body {}: {e}", path.display())))?;
        if bytes.len() < MAGIC.len() + NONCE_LEN {
            return Err(VpnError::StorageError(format!(
                "body {} too short: {} bytes",
                path.display(),
                bytes.len()
            )));
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(VpnError::StorageError(format!(
                "body {} missing magic header",
                path.display()
            )));
        }
        let nonce_bytes = &bytes[MAGIC.len()..MAGIC.len() + NONCE_LEN];
        let ciphertext = &bytes[MAGIC.len() + NONCE_LEN..];
        let cipher = self.cipher()?;
        let nonce = Nonce::from_slice(nonce_bytes);
        cipher.decrypt(nonce, ciphertext).map_err(|e| {
            VpnError::StorageError(format!(
                "decrypt body {}: {e} (key rotated or file corrupted)",
                path.display()
            ))
        })
    }

    // -- active-pointer I/O --------------------------------------------------

    fn read_active_id(&self) -> Result<Option<String>, VpnError> {
        let path = self.active_path();
        match fs::read_to_string(&path) {
            Ok(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(trimmed.to_string()))
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(VpnError::StorageError(format!(
                "read active pointer {}: {e}",
                path.display()
            ))),
        }
    }

    fn write_active_id(&self, id: Option<&str>) -> Result<(), VpnError> {
        let path = self.active_path();
        let tmp = path.with_extension("txt.tmp");
        let mut f = fs::File::create(&tmp)
            .map_err(|e| VpnError::StorageError(format!("create active tmp: {e}")))?;
        let payload = id.unwrap_or("").as_bytes();
        f.write_all(payload)
            .and_then(|_| f.sync_all())
            .map_err(|e| VpnError::StorageError(format!("write active tmp: {e}")))?;
        drop(f);
        fs::rename(&tmp, &path)
            .map_err(|e| VpnError::StorageError(format!("rename active pointer: {e}")))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Keychain helpers
// ---------------------------------------------------------------------------

/// Global serialization for keychain writes.
///
/// `keyring` on macOS uses the Security framework synchronously but
/// multiple concurrent `set_secret` calls can race — the Security
/// framework returns "item already exists" when two threads try to
/// create the same entry simultaneously. We serialize all writes
/// behind a static mutex to make get-or-create idempotent under
/// concurrent access.
static KEYCHAIN_WRITE_LOCK: Mutex<()> = Mutex::new(());

fn load_or_create_profile_key() -> Result<[u8; 32], VpnError> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, PROFILE_KEY_ACCOUNT)
        .map_err(|e| VpnError::StorageError(format!("keychain entry: {e}")))?;

    // Fast path: try to read the existing key.
    match entry.get_secret() {
        Ok(bytes) if bytes.len() == 32 => {
            let mut out = [0u8; 32];
            out.copy_from_slice(&bytes);
            return Ok(out);
        }
        Ok(bytes) => {
            return Err(VpnError::StorageError(format!(
                "keychain profile key has wrong length: {} (expected 32)",
                bytes.len()
            )));
        }
        Err(keyring::Error::NoEntry) => {}
        Err(e) => return Err(VpnError::StorageError(format!("keychain get: {e}"))),
    }

    // Slow path: create the key under a global lock, then re-read.
    // Another thread may have created it between our miss and our
    // acquire of the lock — the re-read handles that race.
    let _guard = KEYCHAIN_WRITE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Ok(bytes) = entry.get_secret() {
        if bytes.len() == 32 {
            let mut out = [0u8; 32];
            out.copy_from_slice(&bytes);
            return Ok(out);
        }
    }
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    match entry.set_secret(&key) {
        Ok(()) => {
            log::info!("profile store: generated new encryption key");
            Ok(key)
        }
        Err(_) => {
            // Someone else won the race — read their value.
            let bytes = entry
                .get_secret()
                .map_err(|e| VpnError::StorageError(format!("keychain reread: {e}")))?;
            if bytes.len() != 32 {
                return Err(VpnError::StorageError(format!(
                    "keychain profile key has wrong length after race: {}",
                    bytes.len()
                )));
            }
            let mut out = [0u8; 32];
            out.copy_from_slice(&bytes);
            Ok(out)
        }
    }
}

fn load_or_create_install_id() -> Result<String, VpnError> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, INSTALL_ID_ACCOUNT)
        .map_err(|e| VpnError::StorageError(format!("keychain entry: {e}")))?;

    // Fast path.
    match entry.get_password() {
        Ok(id) if !id.is_empty() => return Ok(id),
        Ok(_) | Err(keyring::Error::NoEntry) => {}
        Err(e) => {
            return Err(VpnError::StorageError(format!(
                "keychain get install id: {e}"
            )))
        }
    }

    // Slow path with global lock.
    let _guard = KEYCHAIN_WRITE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Ok(existing) = entry.get_password() {
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    let id = Uuid::new_v4().to_string();
    match entry.set_password(&id) {
        Ok(()) => {
            log::info!("profile store: generated new install id");
            Ok(id)
        }
        Err(_) => entry
            .get_password()
            .map_err(|e| VpnError::StorageError(format!("keychain reread install id: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// ProfileStorage trait impl
// ---------------------------------------------------------------------------

impl ProfileStorage for EncryptedProfileStore {
    fn list(&self) -> Result<Vec<ProfileMeta>, VpnError> {
        let active = self.read_active_id()?;
        let mut out = Vec::new();
        let entries = fs::read_dir(&self.base_dir).map_err(|e| {
            VpnError::StorageError(format!(
                "list profiles dir {}: {e}",
                self.base_dir.display()
            ))
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            if let Some(mut meta) = self.read_meta(stem)? {
                meta.is_active = Some(&meta.id) == active.as_ref();
                out.push(meta);
            }
        }
        Ok(out)
    }

    fn get_meta(&self, id: &str) -> Result<Option<ProfileMeta>, VpnError> {
        let active = self.read_active_id()?;
        match self.read_meta(id)? {
            Some(mut m) => {
                m.is_active = Some(&m.id) == active.as_ref();
                Ok(Some(m))
            }
            None => Ok(None),
        }
    }

    fn put(&self, mut profile: Profile, config_json: &str) -> Result<Profile, VpnError> {
        if profile.id.is_empty() {
            profile.id = Uuid::new_v4().to_string();
        }
        let meta = ProfileMeta {
            id: profile.id.clone(),
            name: profile.name.clone(),
            core_type: profile.core_type.clone(),
            source: profile.source.clone(),
            metadata: profile.metadata.clone(),
            created_at: profile.created_at,
            last_used_at: profile.last_used_at,
            is_active: false,
        };
        self.write_body(&profile.id, config_json.as_bytes())?;
        self.write_meta(&meta)?;
        Ok(profile)
    }

    fn delete(&self, id: &str) -> Result<(), VpnError> {
        let _guard = self.active_lock.lock().unwrap_or_else(|e| e.into_inner());
        let meta_path = self.meta_path(id);
        let body_path = self.body_path(id);
        if meta_path.exists() {
            fs::remove_file(&meta_path)
                .map_err(|e| VpnError::StorageError(format!("delete metadata: {e}")))?;
        }
        if body_path.exists() {
            fs::remove_file(&body_path)
                .map_err(|e| VpnError::StorageError(format!("delete body: {e}")))?;
        }
        if self.read_active_id()?.as_deref() == Some(id) {
            self.write_active_id(None)?;
        }
        Ok(())
    }

    fn active(&self) -> Result<Option<String>, VpnError> {
        let _guard = self.active_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read_active_id()
    }

    fn set_active(&self, id: &str) -> Result<(), VpnError> {
        let _guard = self.active_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut meta = self
            .read_meta(id)?
            .ok_or_else(|| VpnError::CoreNotFound(format!("profile {id} not found")))?;
        meta.last_used_at = Some(SystemTime::now());
        self.write_meta(&meta)?;
        self.write_active_id(Some(id))?;
        Ok(())
    }

    fn clear_active(&self) -> Result<(), VpnError> {
        let _guard = self.active_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.write_active_id(None)?;
        Ok(())
    }

    fn load_active_for_core_start(&self) -> Result<TempConfigPath, VpnError> {
        let active = self.read_active_id()?;
        let id = active.ok_or(VpnError::NotConnected)?;
        let plaintext = self.read_body_plaintext(&id)?;
        let temp_dir = self.temp_dir();
        fs::create_dir_all(&temp_dir).map_err(|e| {
            VpnError::StorageError(format!("create temp dir {}: {e}", temp_dir.display()))
        })?;
        let temp_path = temp_dir.join(format!("{}-{}.json", std::process::id(), id));
        let mut f = fs::File::create(&temp_path).map_err(|e| {
            VpnError::StorageError(format!("create temp config {}: {e}", temp_path.display()))
        })?;
        f.write_all(&plaintext)
            .and_then(|_| f.sync_all())
            .map_err(|e| VpnError::StorageError(format!("write temp config: {e}")))?;
        drop(f);

        // Restrict perms to 0600 on Unix so only the daemon user can read.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&temp_path)
                .map_err(|e| VpnError::StorageError(format!("stat temp config: {e}")))?
                .permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&temp_path, perms)
                .map_err(|e| VpnError::StorageError(format!("chmod temp config: {e}")))?;
        }

        Ok(TempConfigPath::new(temp_path))
    }
}

impl InstallIdProvider for EncryptedProfileStore {
    fn install_id(&self) -> Result<String, VpnError> {
        load_or_create_install_id()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// All tests use `with_base_dir(tempdir)` so they don't touch the user's
// real profiles dir. They DO touch the real OS keychain because `keyring`
// has no injectable backend — ephemeral CI keychains mean that's fine.
// If your dev machine's keychain gets polluted during iteration:
//   security delete-generic-password -s one.pingle.vpn -a profile-key
//   security delete-generic-password -s one.pingle.vpn -a install-id

#[cfg(test)]
mod tests {
    use super::*;
    use domain::ProfileSource;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, EncryptedProfileStore) {
        let dir = TempDir::new().expect("tempdir");
        let store = EncryptedProfileStore::with_base_dir(dir.path().to_path_buf())
            .expect("construct store");
        (dir, store)
    }

    fn sample_profile(name: &str) -> Profile {
        Profile {
            id: String::new(),
            name: name.to_string(),
            core_type: "sing-box".to_string(),
            source: ProfileSource::Imported { filename: None },
            metadata: BTreeMap::new(),
            created_at: SystemTime::now(),
            last_used_at: None,
        }
    }

    #[test]
    fn put_assigns_uuid_when_empty() {
        let (_dir, store) = make_store();
        let p = store.put(sample_profile("Test"), r#"{"a":1}"#).unwrap();
        assert!(!p.id.is_empty());
        assert!(Uuid::parse_str(&p.id).is_ok());
    }

    #[test]
    fn put_then_list_returns_metadata() {
        let (_dir, store) = make_store();
        let p = store
            .put(sample_profile("Home"), r#"{"dns":"1.1.1.1"}"#)
            .unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, p.id);
        assert_eq!(list[0].name, "Home");
        assert!(!list[0].is_active);
    }

    #[test]
    fn list_never_returns_config_body() {
        let (_dir, store) = make_store();
        store
            .put(sample_profile("Home"), r#"{"secret":"xyz"}"#)
            .unwrap();
        let list = store.list().unwrap();
        let json = serde_json::to_string(&list[0]).unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("xyz"));
    }

    #[test]
    fn set_active_requires_existing_profile() {
        let (_dir, store) = make_store();
        let err = store.set_active("does-not-exist").unwrap_err();
        assert!(matches!(err, VpnError::CoreNotFound(_)));
    }

    #[test]
    fn set_active_updates_is_active_flag() {
        let (_dir, store) = make_store();
        let p = store.put(sample_profile("Home"), "{}").unwrap();
        store.set_active(&p.id).unwrap();
        let list = store.list().unwrap();
        assert!(list[0].is_active);
        assert_eq!(store.active().unwrap().as_deref(), Some(p.id.as_str()));
    }

    #[test]
    fn set_active_updates_last_used_at() {
        let (_dir, store) = make_store();
        let p = store.put(sample_profile("Home"), "{}").unwrap();
        assert!(store.get_meta(&p.id).unwrap().unwrap().last_used_at.is_none());
        store.set_active(&p.id).unwrap();
        assert!(store.get_meta(&p.id).unwrap().unwrap().last_used_at.is_some());
    }

    #[test]
    fn delete_clears_active_when_deleting_active() {
        let (_dir, store) = make_store();
        let p = store.put(sample_profile("Home"), "{}").unwrap();
        store.set_active(&p.id).unwrap();
        store.delete(&p.id).unwrap();
        assert!(store.active().unwrap().is_none());
    }

    #[test]
    fn delete_nonexistent_is_noop() {
        let (_dir, store) = make_store();
        assert!(store.delete("missing-id").is_ok());
    }

    #[test]
    fn load_active_decrypts_body() {
        let (_dir, store) = make_store();
        let plaintext = r#"{"log":{"level":"debug"}}"#;
        let p = store.put(sample_profile("Home"), plaintext).unwrap();
        store.set_active(&p.id).unwrap();
        let temp = store.load_active_for_core_start().unwrap();
        let round_trip = fs::read_to_string(temp.path()).unwrap();
        assert_eq!(round_trip, plaintext);
    }

    #[test]
    fn load_active_without_active_errors() {
        let (_dir, store) = make_store();
        let err = store.load_active_for_core_start().unwrap_err();
        assert!(matches!(err, VpnError::NotConnected));
    }

    #[test]
    fn temp_config_path_drops_file_on_scope_exit() {
        let (_dir, store) = make_store();
        let p = store.put(sample_profile("Home"), "{}").unwrap();
        store.set_active(&p.id).unwrap();
        let temp_file_path;
        {
            let temp = store.load_active_for_core_start().unwrap();
            temp_file_path = temp.path().to_path_buf();
            assert!(temp_file_path.exists());
        }
        assert!(!temp_file_path.exists());
    }

    #[test]
    fn put_overwrites_same_id() {
        let (_dir, store) = make_store();
        let mut p = sample_profile("Home");
        p.id = "fixed-id".to_string();
        store.put(p.clone(), "{}").unwrap();
        p.name = "Home v2".to_string();
        store.put(p, r#"{"updated":true}"#).unwrap();
        let meta = store.get_meta("fixed-id").unwrap().unwrap();
        assert_eq!(meta.name, "Home v2");
    }

    #[test]
    fn get_meta_nonexistent_returns_none() {
        let (_dir, store) = make_store();
        assert!(store.get_meta("nope").unwrap().is_none());
    }

    #[test]
    fn clear_active_removes_pointer() {
        let (_dir, store) = make_store();
        let p = store.put(sample_profile("Home"), "{}").unwrap();
        store.set_active(&p.id).unwrap();
        store.clear_active().unwrap();
        assert!(store.active().unwrap().is_none());
    }

    #[test]
    fn install_id_is_stable_across_calls() {
        let (_dir, store) = make_store();
        let id1 = store.install_id().unwrap();
        let id2 = store.install_id().unwrap();
        assert_eq!(id1, id2);
        assert!(Uuid::parse_str(&id1).is_ok());
    }
}
