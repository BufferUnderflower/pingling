//! Encrypted profile storage contract.
//!
//! A [`Profile`] is a named VPN configuration that the active core
//! consumes. The daemon stores many profiles encrypted at rest, tracks
//! which one is "active" (the one the core will start), and hands the
//! decrypted bytes to the core via a temporary file when `start` is
//! called.
//!
//! # Why not just a string in settings?
//!
//! The legacy `core_config_path` setting points at a plaintext file on
//! disk — fine for dev, terrible for production. A config blob can
//! contain credentials (server addresses, shared secrets, TLS
//! fingerprints) that shouldn't be world-readable in `~/Library/...`.
//! Profiles solve this by encrypting the config body with a key that
//! only the daemon process can read (via the OS keychain).
//!
//! # Split between metadata and body
//!
//! A profile's "metadata" ([`ProfileMeta`]) is intentionally NOT
//! encrypted. Metadata is the human-readable name, core type, source
//! (where the profile came from), timestamps, and vendor-supplied
//! tags. Clients render metadata in profile cards; leaking it is
//! acceptable.
//!
//! The "body" — the config JSON — IS encrypted. The only way plaintext
//! body bytes leave the [`ProfileStorage`] is via
//! [`ProfileStorage::load_active_for_core_start`], which returns a
//! [`TempConfigPath`] that deletes itself on drop. Clients never see
//! the body via IPC. See the design spec at
//! `docs/superpowers/specs/2026-04-11-profiles-deeplink-encrypted-storage.md`.
//!
//! # Atomicity
//!
//! Implementations must treat `put` and `delete` as atomic with
//! respect to `list`. A caller iterating profiles concurrently with
//! a `put` sees either the old set or the new set, never a partial
//! write. The reference impl ([`data::profile_store::EncryptedProfileStore`])
//! achieves this by writing to `<uuid>.bin.tmp` then renaming.

use crate::errors::VpnError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Full profile record — metadata + vendor-attached context. Never
/// contains the config body (that's passed separately to [`ProfileStorage::put`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Stable UUID assigned on first `put`. Never reassigned.
    pub id: String,
    /// Human-readable name shown in clients. Free-form, user-editable.
    pub name: String,
    /// Which core consumes this config (`"sing-box"`, `"xray"`, etc.)
    pub core_type: String,
    /// Where this profile came from. Used for display and trust
    /// decisions (e.g. "show a warning if activating a profile
    /// that came from a deeplink with an unknown origin").
    pub source: ProfileSource,
    /// Opaque key/value tags the plugin attaches when creating the
    /// profile. Typical fields: `account_id`, `expires_at`, `region`,
    /// `plan`. Clients render these in the profile detail card.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    /// When the profile was first added.
    pub created_at: SystemTime,
    /// Last time the profile was activated (for sort-by-recent in
    /// client UIs). `None` until the profile is activated at least
    /// once.
    pub last_used_at: Option<SystemTime>,
}

/// Lightweight profile header for client list views. Mirrors [`Profile`]
/// minus the `metadata` detail (kept) and adds the `is_active` flag
/// derived at list time.
///
/// The separation from [`Profile`] exists so [`ProfileStorage::list`]
/// can be implemented without forcing clients to pay the decrypt cost
/// for every profile. In practice [`ProfileMeta`] and [`Profile`] have
/// the same fields + `is_active`; the distinction is conceptual.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileMeta {
    pub id: String,
    pub name: String,
    pub core_type: String,
    pub source: ProfileSource,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    pub created_at: SystemTime,
    pub last_used_at: Option<SystemTime>,
    /// Whether this profile is currently the active one.
    pub is_active: bool,
}

/// Where a profile originated. Used by clients to label profiles
/// ("Imported from deeplink", "Legacy config") and by future trust
/// heuristics (e.g. warn before auto-connecting to a deeplink-sourced
/// profile on the first click).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProfileSource {
    /// Imported via a `pingle://` deep-link. Stores the full URL
    /// for audit purposes — the URL itself is not sensitive (the
    /// token inside it may be, but it's already been consumed by
    /// the plugin by the time the profile is stored).
    Deeplink { url: String },
    /// Imported via IPC `profile.put` — typically from a client
    /// that let the user paste config JSON or drop a file.
    Imported {
        #[serde(default)]
        filename: Option<String>,
    },
    /// Migrated from the legacy `core_config_path` setting. Created
    /// once per daemon on first launch after the profiles feature
    /// lands, if a legacy path is present.
    Legacy,
    /// Created programmatically by a plugin (via the
    /// `deeplink.resolve` → `kind: "profile"` path, or a direct
    /// plugin-initiated import).
    Plugin { plugin_name: String },
}

/// RAII wrapper around a decrypted config written to a temporary file.
///
/// The only way plaintext config bytes leave [`ProfileStorage`]: when
/// [`ProfileStorage::load_active_for_core_start`] is called, it writes
/// the decrypted config to a temp file and returns this wrapper. The
/// caller (typically the connect handler) passes `path()` to
/// [`crate::VpnCore::start`] and holds the [`TempConfigPath`] for the
/// lifetime of the connection. When the handler disconnects and drops
/// the wrapper, the temp file is removed.
///
/// If the process crashes while holding a [`TempConfigPath`], the OS
/// will eventually reap it on reboot (macOS `/tmp`, Windows `%TEMP%`).
/// Implementations should use a temp dir that's outside the user's
/// documents to reduce the blast radius of a crash-time leak.
#[derive(Debug)]
pub struct TempConfigPath {
    path: PathBuf,
}

impl TempConfigPath {
    /// Construct a new wrapper. The file at `path` must already exist;
    /// the wrapper takes ownership of it.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The filesystem path to the plaintext config. Hand this to
    /// [`crate::VpnCore::start`].
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempConfigPath {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            // Don't panic in a destructor — the caller may already be
            // unwinding. Log and move on; the OS will eventually reap
            // the temp file.
            log::warn!(
                "TempConfigPath: failed to delete {}: {e}",
                self.path.display()
            );
        }
    }
}

/// CRUD + active-profile management for encrypted profiles.
///
/// All methods are `Send + Sync` and must be safe to call from any
/// thread. Implementations typically wrap an inner `Mutex` around
/// filesystem state; callers should not need to add their own
/// synchronization.
///
/// # Error handling
///
/// - Missing items (get a non-existent id, delete a non-existent id)
///   are NOT errors — `get_meta` returns `None`, `delete` is a no-op.
/// - Only operational failures (encryption, I/O, keychain access)
///   return `Err`.
/// - `set_active` on a non-existent id IS an error ([`VpnError::CoreNotFound`]
///   style), because it's a programming error to activate something
///   that doesn't exist.
pub trait ProfileStorage: Send + Sync {
    /// List all profiles' metadata, with the active one flagged.
    /// The list order is unspecified — clients should sort by
    /// `last_used_at` or `name` as they see fit.
    fn list(&self) -> Result<Vec<ProfileMeta>, VpnError>;

    /// Get one profile's metadata only. Returns `None` if the id
    /// does not exist.
    fn get_meta(&self, id: &str) -> Result<Option<ProfileMeta>, VpnError>;

    /// Insert or update a profile.
    ///
    /// `profile.id` may be an empty string — in that case the
    /// implementation generates a fresh UUID and writes it back via
    /// the returned profile. Callers that want a specific id
    /// (e.g. for reproducible tests) can set it explicitly.
    ///
    /// `config_json` is plaintext JSON. The implementation encrypts
    /// it before writing to disk. Callers should avoid keeping long-
    /// lived references to the plaintext after calling this.
    ///
    /// Returns the final profile (with id filled in if it was
    /// empty) so callers can use the id for subsequent calls.
    fn put(&self, profile: Profile, config_json: &str) -> Result<Profile, VpnError>;

    /// Delete a profile by id. Idempotent — removing a non-existent
    /// id is not an error.
    ///
    /// If the deleted profile was the active one, the active pointer
    /// is cleared (no active profile until the next `set_active`).
    fn delete(&self, id: &str) -> Result<(), VpnError>;

    /// Get the currently-active profile id, if any.
    fn active(&self) -> Result<Option<String>, VpnError>;

    /// Set the active profile by id. Fails with
    /// [`VpnError::CoreNotFound`] if the id does not exist — use
    /// `list` first to enumerate valid ids.
    ///
    /// Also updates the profile's `last_used_at` to the current time.
    fn set_active(&self, id: &str) -> Result<(), VpnError>;

    /// Clear the active profile pointer. After this call, the daemon
    /// falls back to the legacy `core_config_path` setting (if any)
    /// on the next connect.
    fn clear_active(&self) -> Result<(), VpnError>;

    /// Load the active profile's plaintext config into a temporary
    /// file and return the handle.
    ///
    /// This is the ONLY method that decrypts and materializes
    /// plaintext config bytes outside the storage layer. It should
    /// only be called by the connect handler on the path to
    /// [`crate::VpnCore::start`].
    ///
    /// # Errors
    /// - [`VpnError::NotConnected`] if no active profile is set.
    ///   (Reusing the variant for "no active profile" — the semantics
    ///   match: "nothing to connect to".)
    /// - [`VpnError::StorageError`] on decrypt, I/O, or keychain
    ///   failure.
    fn load_active_for_core_start(&self) -> Result<TempConfigPath, VpnError>;
}

/// Read-only provider for the daemon's install ID.
///
/// The install ID is a UUID generated on first launch, stored in the
/// OS keychain, and returned by this trait. It survives reinstalls on
/// the same machine (keychain entries persist across app deletion) but
/// is lost if the user explicitly wipes their keychain or switches
/// user accounts.
///
/// Plugins read the install ID via the [`crate::Plugin::handle_ipc`]
/// method `daemon.installId`, which the daemon exposes. The intended
/// use is trial-abuse detection: a vendor plugin can correlate
/// server-side trial issuance with the install ID to refuse a fresh
/// trial to a returning uninstaller.
pub trait InstallIdProvider: Send + Sync {
    /// Return the install ID, generating + persisting it on first
    /// call if necessary. Subsequent calls return the same value.
    fn install_id(&self) -> Result<String, VpnError>;
}
