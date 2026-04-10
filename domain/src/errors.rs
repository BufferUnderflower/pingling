//! Domain error types.
//!
//! All fallible operations in the domain layer return [`VpnError`].
//! This enum is intentionally framework-agnostic — no serde, no tauri.

use std::fmt;

/// Errors that can occur within the VPN domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VpnError {
    /// The provided configuration is invalid or missing.
    InvalidConfiguration(String),

    /// The core binary could not be started.
    ProcessStartFailed(String),

    /// The core process could not be stopped gracefully.
    ProcessStopFailed(String),

    /// The core process could not be killed.
    ProcessKillFailed(String),

    /// Tried to connect but already connected.
    AlreadyConnected,

    /// Tried to disconnect but not connected.
    NotConnected,

    /// The core binary or a required dependency is missing.
    PrerequisiteMissing(String),

    /// Storage backend failure.
    StorageError(String),

    /// Core validation command returned an error.
    ValidationError(String),

    /// Requested core type is not registered or not found.
    CoreNotFound(String),

    /// Insufficient OS permissions (e.g. TUN device, VPN entitlement).
    PermissionDenied(String),

    /// Requested outbound/proxy ID does not exist in the current config.
    OutboundNotFound(String),

    /// Operation was cancelled — typically by user disconnect during a
    /// connect attempt, or by the strategy retry wrap aborting an
    /// in-flight retry. Recoverable: the user can re-trigger.
    Cancelled,

    /// Any other error.
    Unknown(String),
}

impl fmt::Display for VpnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(msg) => write!(f, "Invalid configuration: {msg}"),
            Self::ProcessStartFailed(msg) => write!(f, "Process start failed: {msg}"),
            Self::ProcessStopFailed(msg) => write!(f, "Process stop failed: {msg}"),
            Self::ProcessKillFailed(msg) => write!(f, "Process kill failed: {msg}"),
            Self::AlreadyConnected => write!(f, "Already connected"),
            Self::NotConnected => write!(f, "Not connected"),
            Self::PrerequisiteMissing(msg) => write!(f, "Prerequisite missing: {msg}"),
            Self::StorageError(msg) => write!(f, "Storage error: {msg}"),
            Self::ValidationError(msg) => write!(f, "Validation error: {msg}"),
            Self::CoreNotFound(msg) => write!(f, "Core not found: {msg}"),
            Self::PermissionDenied(msg) => write!(f, "Permission denied: {msg}"),
            Self::OutboundNotFound(msg) => write!(f, "Outbound not found: {msg}"),
            Self::Cancelled => write!(f, "Cancelled"),
            Self::Unknown(msg) => write!(f, "Unknown error: {msg}"),
        }
    }
}

impl VpnError {
    /// Stable machine-readable error code for JSON-RPC serialization.
    ///
    /// These codes form a public contract between the Rust daemon and the
    /// Flutter client. Add new variants here; never rename existing codes.
    ///
    /// ```
    /// # use domain::VpnError;
    /// assert_eq!(VpnError::AlreadyConnected.code(), "already_connected");
    /// assert_eq!(VpnError::NotConnected.code(), "not_connected");
    /// assert_eq!(
    ///     VpnError::InvalidConfiguration("bad".into()).code(),
    ///     "invalid_configuration"
    /// );
    /// ```
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfiguration(_) => "invalid_configuration",
            Self::ProcessStartFailed(_) => "process_start_failed",
            Self::ProcessStopFailed(_) => "process_stop_failed",
            Self::ProcessKillFailed(_) => "process_kill_failed",
            Self::AlreadyConnected => "already_connected",
            Self::NotConnected => "not_connected",
            Self::PrerequisiteMissing(_) => "prerequisite_missing",
            Self::StorageError(_) => "storage_error",
            Self::ValidationError(_) => "validation_error",
            Self::CoreNotFound(_) => "core_not_found",
            Self::PermissionDenied(_) => "permission_denied",
            Self::OutboundNotFound(_) => "outbound_not_found",
            Self::Cancelled => "cancelled",
            Self::Unknown(_) => "unknown",
        }
    }

    /// Whether the Flutter client should offer a retry action.
    ///
    /// `true` means the error is transient or user-fixable (retry, pick
    /// a different server, grant permission). `false` means the error
    /// requires config changes or reinstallation.
    ///
    /// ```
    /// # use domain::VpnError;
    /// assert!(VpnError::ProcessStartFailed("timeout".into()).recoverable());
    /// assert!(!VpnError::InvalidConfiguration("bad".into()).recoverable());
    /// ```
    pub fn recoverable(&self) -> bool {
        match self {
            Self::InvalidConfiguration(_) => false,
            Self::ProcessStartFailed(_) => true,
            Self::ProcessStopFailed(_) => true,
            Self::ProcessKillFailed(_) => true,
            Self::AlreadyConnected => false,
            Self::NotConnected => false,
            Self::PrerequisiteMissing(_) => false,
            Self::StorageError(_) => true,
            Self::ValidationError(_) => false,
            Self::CoreNotFound(_) => false,
            Self::PermissionDenied(_) => true,
            Self::OutboundNotFound(_) => true,
            Self::Cancelled => true,
            Self::Unknown(_) => true,
        }
    }
}

impl std::error::Error for VpnError {}

impl From<String> for VpnError {
    fn from(s: String) -> Self {
        Self::Unknown(s)
    }
}

impl From<&str> for VpnError {
    fn from(s: &str) -> Self {
        Self::Unknown(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_invalid_configuration() {
        let err = VpnError::InvalidConfiguration("missing port".into());
        assert_eq!(err.to_string(), "Invalid configuration: missing port");
    }

    #[test]
    fn display_process_start_failed() {
        let err = VpnError::ProcessStartFailed("binary not found".into());
        assert_eq!(err.to_string(), "Process start failed: binary not found");
    }

    #[test]
    fn display_already_connected() {
        assert_eq!(VpnError::AlreadyConnected.to_string(), "Already connected");
    }

    #[test]
    fn display_not_connected() {
        assert_eq!(VpnError::NotConnected.to_string(), "Not connected");
    }

    #[test]
    fn display_prerequisite_missing() {
        let err = VpnError::PrerequisiteMissing("sing-box binary".into());
        assert_eq!(err.to_string(), "Prerequisite missing: sing-box binary");
    }

    #[test]
    fn from_string() {
        let err: VpnError = "oops".to_string().into();
        assert_eq!(err, VpnError::Unknown("oops".to_string()));
    }

    #[test]
    fn from_str() {
        let err: VpnError = "oops".into();
        assert_eq!(err, VpnError::Unknown("oops".to_string()));
    }

    #[test]
    fn display_core_not_found() {
        let err = VpnError::CoreNotFound("xray".into());
        assert_eq!(err.to_string(), "Core not found: xray");
    }

    #[test]
    fn is_std_error() {
        let err = VpnError::StorageError("disk full".into());
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(
            VpnError::InvalidConfiguration("x".into()).code(),
            "invalid_configuration"
        );
        assert_eq!(
            VpnError::ProcessStartFailed("x".into()).code(),
            "process_start_failed"
        );
        assert_eq!(VpnError::AlreadyConnected.code(), "already_connected");
        assert_eq!(VpnError::NotConnected.code(), "not_connected");
        assert_eq!(VpnError::CoreNotFound("x".into()).code(), "core_not_found");
        assert_eq!(
            VpnError::PermissionDenied("x".into()).code(),
            "permission_denied"
        );
        assert_eq!(
            VpnError::OutboundNotFound("x".into()).code(),
            "outbound_not_found"
        );
        assert_eq!(VpnError::Unknown("x".into()).code(), "unknown");
    }

    #[test]
    fn cancelled_display_code_and_recoverable() {
        let err = VpnError::Cancelled;
        assert_eq!(err.to_string(), "Cancelled");
        assert_eq!(err.code(), "cancelled");
        assert!(err.recoverable());
    }

    #[test]
    fn recoverable_classification() {
        // Transient / user-fixable → recoverable
        assert!(VpnError::ProcessStartFailed("timeout".into()).recoverable());
        assert!(VpnError::StorageError("disk full".into()).recoverable());
        assert!(VpnError::PermissionDenied("no tun".into()).recoverable());
        assert!(VpnError::OutboundNotFound("jp-1".into()).recoverable());
        assert!(VpnError::Unknown("oops".into()).recoverable());

        // Config / structural → not recoverable
        assert!(!VpnError::InvalidConfiguration("bad".into()).recoverable());
        assert!(!VpnError::AlreadyConnected.recoverable());
        assert!(!VpnError::NotConnected.recoverable());
        assert!(!VpnError::PrerequisiteMissing("binary".into()).recoverable());
        assert!(!VpnError::CoreNotFound("xray".into()).recoverable());
        assert!(!VpnError::ValidationError("syntax".into()).recoverable());
    }
}
