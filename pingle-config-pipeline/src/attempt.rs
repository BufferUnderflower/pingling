//! Per-attempt envelope passed through the pipeline and surfaced to
//! plugins via the wire protocol.

use crate::error::PreviousError;
use crate::strategy::ConnectionStrategy;
use serde::{Deserialize, Serialize};

/// What's been tried so far. Threaded through both the native pipeline
/// and the plugin protocol — single source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptInfo {
    /// The active strategy for this attempt.
    pub strategy: ConnectionStrategy,
    /// 1-based attempt counter inside the current strategy. Resets to
    /// 1 when the strategy advances.
    pub attempt_number: u32,
    /// `None` on the first attempt of any strategy. `Some(_)` on retries.
    pub previous_error: Option<PreviousError>,
}

/// Envelope handed to the native pipeline AND surfaced inside the
/// plugin's `process_config` input. Carries everything a processor
/// (native or plugin) might need to make a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigRequest {
    /// Use the host's configured DNS instead of `dns-local`. Maps to
    /// `dns.servers[tag=dns-local]` rewriting in `DnsProcessor`.
    pub with_host_dns: bool,
    /// Fallback DNS server when `with_host_dns=false` and the existing
    /// `dns-local` server has no `server` field. Defaults to `8.8.8.8`
    /// inside `DnsProcessor` if `None`.
    pub default_dns_server: Option<String>,
    /// Per-attempt info — strategy, attempt counter, previous error.
    pub attempt: AttemptInfo,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::strategy::{ResolverType, RetryPolicy, StackType};
    use std::time::Duration;

    fn sample_strategy() -> ConnectionStrategy {
        ConnectionStrategy {
            id: "doh".into(),
            stack: StackType::System,
            resolver_type: ResolverType::Doh,
            total_timeout: Duration::from_secs(30),
            retry: RetryPolicy::Fixed {
                max_attempts: 3,
                delay: Duration::from_secs(2),
            },
        }
    }

    #[test]
    fn attempt_info_round_trip_serde() {
        let original = AttemptInfo {
            strategy: sample_strategy(),
            attempt_number: 2,
            previous_error: Some(PreviousError {
                kind: ErrorKind::DnsFailure,
                message: "no such host".into(),
                core_error_kind: "ProcessStartFailed".into(),
            }),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: AttemptInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn attempt_info_first_attempt_no_previous_error() {
        let original = AttemptInfo {
            strategy: sample_strategy(),
            attempt_number: 1,
            previous_error: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: AttemptInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
        assert!(parsed.previous_error.is_none());
    }

    #[test]
    fn config_request_round_trip_serde() {
        let original = ConfigRequest {
            with_host_dns: false,
            default_dns_server: Some("1.1.1.1".into()),
            attempt: AttemptInfo {
                strategy: sample_strategy(),
                attempt_number: 1,
                previous_error: None,
            },
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: ConfigRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }
}
