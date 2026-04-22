//! Daemon-side classification of [`VpnError`] into the small stable
//! taxonomy that the strategy retry loop and the plugin protocol both
//! consume.
//!
//! # Why a separate taxonomy
//!
//! `VpnError::recoverable()` exists for the Flutter / TUI client to
//! decide whether to render a retry button. That's a different concern
//! from "what should the strategy loop do next?" — for example,
//! `PrerequisiteMissing` is `recoverable=false` but the loop should
//! bail immediately, while `dns_failure` is `recoverable=true` and the
//! loop should retry within the strategy. This module owns the latter
//! classification.

use pingling_domain::VpnError;
use serde::{Deserialize, Serialize};

/// Stable, small, daemon-classified taxonomy of error causes that the
/// strategy retry loop branches on.
///
/// **Non-exhaustive on purpose.** New variants can be added without
/// breaking existing plugins — they treat unknown variants as
/// equivalent to [`ErrorKind::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorKind {
    /// DNS resolution failed (`"no such host"`, resolver-port refused).
    DnsFailure,
    /// TCP connect timed out.
    TcpTimeout,
    /// TCP connect refused / RST.
    TcpRefused,
    /// TLS handshake error.
    TlsHandshake,
    /// HTTP non-2xx from a control-plane call.
    HttpError,
    /// 401/403, sing-box auth-rejected.
    AuthFailure,
    /// `validation_failure` — sing-box rejected the config schema.
    Validation,
    /// Adapter create / route install failed.
    TunDevice,
    /// `VpnError::PermissionDenied` or text match.
    PermissionDenied,
    /// `VpnError::PrerequisiteMissing` (libbox.dll, WinTun…).
    PrerequisiteMissing,
    /// Per-attempt total_timeout exceeded.
    Timeout,
    /// Catch-all when nothing else matches.
    Unknown,
}

/// Structured error envelope handed to plugins via `previous_error`.
///
/// `kind` lets plugins switch on a small stable taxonomy.
/// `message` carries the raw `VpnError::to_string()` for fine-grained
/// parsing. `core_error_kind` is the literal `VpnError` variant name
/// (e.g. `"ProcessStartFailed"`) — debug info, not parsed by daemon
/// logic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousError {
    pub kind: ErrorKind,
    pub message: String,
    pub core_error_kind: String,
}

/// Best-effort classification of a [`VpnError`] into [`ErrorKind`].
///
/// Walks both the variant and the message text. Returns
/// [`ErrorKind::Unknown`] for cases that don't match any known pattern.
pub fn classify_error(err: &VpnError) -> PreviousError {
    let core_error_kind = match err {
        VpnError::InvalidConfiguration(_) => "InvalidConfiguration",
        VpnError::ProcessStartFailed(_) => "ProcessStartFailed",
        VpnError::ProcessStopFailed(_) => "ProcessStopFailed",
        VpnError::ProcessKillFailed(_) => "ProcessKillFailed",
        VpnError::AlreadyConnected => "AlreadyConnected",
        VpnError::NotConnected => "NotConnected",
        VpnError::PrerequisiteMissing(_) => "PrerequisiteMissing",
        VpnError::StorageError(_) => "StorageError",
        VpnError::ValidationError(_) => "ValidationError",
        VpnError::CoreNotFound(_) => "CoreNotFound",
        VpnError::PermissionDenied(_) => "PermissionDenied",
        VpnError::OutboundNotFound(_) => "OutboundNotFound",
        VpnError::Cancelled => "Cancelled",
        VpnError::Unknown(_) => "Unknown",
    };
    let message = err.to_string();
    let lower = message.to_lowercase();

    let kind = match err {
        VpnError::PrerequisiteMissing(_) => ErrorKind::PrerequisiteMissing,
        VpnError::PermissionDenied(_) => ErrorKind::PermissionDenied,
        VpnError::ValidationError(_) | VpnError::InvalidConfiguration(_) => ErrorKind::Validation,
        _ if contains_any(&lower, &["no such host", "name resolution", "dns lookup"]) => {
            ErrorKind::DnsFailure
        }
        _ if contains_any(&lower, &["connection refused", "connection reset"]) => {
            ErrorKind::TcpRefused
        }
        _ if contains_any(&lower, &["timed out", "timeout"]) => ErrorKind::TcpTimeout,
        _ if contains_any(&lower, &["tls", "handshake", "certificate"]) => ErrorKind::TlsHandshake,
        _ if contains_any(&lower, &["401", "unauthorized", "403", "forbidden"]) => {
            ErrorKind::AuthFailure
        }
        _ if contains_any(&lower, &["http"]) => ErrorKind::HttpError,
        _ if contains_any(
            &lower,
            &[
                "tun ",
                "/dev/net/tun",
                "wintun",
                "tap adapter",
                "adapter create",
            ],
        ) =>
        {
            ErrorKind::TunDevice
        }
        _ => ErrorKind::Unknown,
    };

    PreviousError {
        kind,
        message,
        core_error_kind: core_error_kind.into(),
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_error_round_trip_serde() {
        let original = PreviousError {
            kind: ErrorKind::DnsFailure,
            message: "lookup foo.example: no such host".into(),
            core_error_kind: "ProcessStartFailed".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: PreviousError = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn classifies_prerequisite_missing() {
        let err = VpnError::PrerequisiteMissing("libbox.dll not found".into());
        let p = classify_error(&err);
        assert_eq!(p.kind, ErrorKind::PrerequisiteMissing);
        assert_eq!(p.core_error_kind, "PrerequisiteMissing");
    }

    #[test]
    fn classifies_permission_denied() {
        let err = VpnError::PermissionDenied("no TUN permission".into());
        assert_eq!(classify_error(&err).kind, ErrorKind::PermissionDenied);
    }

    #[test]
    fn classifies_validation_from_validation_error_variant() {
        let err = VpnError::ValidationError("schema mismatch".into());
        assert_eq!(classify_error(&err).kind, ErrorKind::Validation);
    }

    #[test]
    fn classifies_validation_from_invalid_configuration() {
        let err = VpnError::InvalidConfiguration("missing port".into());
        assert_eq!(classify_error(&err).kind, ErrorKind::Validation);
    }

    #[test]
    fn classifies_dns_failure_from_text() {
        let err = VpnError::ProcessStartFailed("lookup example.com: no such host".into());
        assert_eq!(classify_error(&err).kind, ErrorKind::DnsFailure);
    }

    #[test]
    fn classifies_tcp_refused() {
        let err = VpnError::Unknown("dial tcp: connection refused".into());
        assert_eq!(classify_error(&err).kind, ErrorKind::TcpRefused);
    }

    #[test]
    fn classifies_tcp_timeout() {
        let err = VpnError::Unknown("dial tcp: i/o timeout".into());
        assert_eq!(classify_error(&err).kind, ErrorKind::TcpTimeout);
    }

    #[test]
    fn classifies_tls_handshake() {
        let err = VpnError::Unknown("tls handshake failure: bad certificate".into());
        assert_eq!(classify_error(&err).kind, ErrorKind::TlsHandshake);
    }

    #[test]
    fn classifies_auth_failure() {
        let err = VpnError::Unknown("http 401 unauthorized".into());
        assert_eq!(classify_error(&err).kind, ErrorKind::AuthFailure);
    }

    #[test]
    fn classifies_tun_device() {
        let err = VpnError::ProcessStartFailed("open /dev/net/tun: device busy".into());
        assert_eq!(classify_error(&err).kind, ErrorKind::TunDevice);
    }

    #[test]
    fn falls_through_to_unknown() {
        let err = VpnError::Unknown("something weird happened".into());
        assert_eq!(classify_error(&err).kind, ErrorKind::Unknown);
    }

    #[test]
    fn classify_preserves_message() {
        let err = VpnError::ProcessStartFailed("timeout reading config".into());
        let p = classify_error(&err);
        assert!(p.message.contains("timeout reading config"));
    }
}
