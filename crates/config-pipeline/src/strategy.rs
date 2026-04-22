//! Strategy iteration types — direct port of the dart `ConnectionStrategy`
//! + `RetryPolicy` + new `StrategyPlan` envelope.
//!
//! These types are deliberately sing-box-flavored (`StackType`,
//! `ResolverType` are sing-box config concepts). They live in
//! `core-config-processor` rather than `domain` because `domain` is
//! intentionally vendor-agnostic — only cores that drive sing-box
//! benefit from this contract.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// TUN stack type. Drives the `tun.stack` field of a sing-box config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackType {
    System,
    GVisor,
    Mixed,
}

impl StackType {
    /// String value sing-box expects in `tun.stack`.
    pub fn as_singbox_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::GVisor => "gvisor",
            Self::Mixed => "mixed",
        }
    }
}

/// DNS resolver flavor. Drives the `dns.servers[].type` choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolverType {
    Doh,
    Tcp,
    Udp,
    System,
}

impl ResolverType {
    /// String value sing-box expects in `dns.servers[].type`.
    pub fn as_singbox_str(&self) -> &'static str {
        match self {
            Self::Doh => "https",
            Self::Tcp => "tcp",
            Self::Udp => "udp",
            Self::System => "local",
        }
    }
}

/// How to retry within a single strategy.
///
/// `NoRetry` = single attempt, then advance to the next strategy.
/// `Fixed` = up to N attempts with constant delay between them.
/// `Exponential` = up to N attempts with `delay = initial * 2^(n-2)`,
///   capped at `max_delay`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetryPolicy {
    NoRetry,
    Fixed {
        max_attempts: u32,
        #[serde(with = "duration_ms")]
        delay: Duration,
    },
    Exponential {
        max_attempts: u32,
        #[serde(with = "duration_ms")]
        initial_delay: Duration,
        #[serde(with = "duration_ms")]
        max_delay: Duration,
    },
}

impl RetryPolicy {
    /// Total attempts allowed under this policy. `NoRetry` returns 1.
    /// `Fixed` and `Exponential` return their `max_attempts`, clamped
    /// to a minimum of 1.
    pub fn max_attempts(&self) -> u32 {
        match self {
            Self::NoRetry => 1,
            Self::Fixed { max_attempts, .. } | Self::Exponential { max_attempts, .. } => {
                (*max_attempts).max(1)
            }
        }
    }

    /// Delay before attempt number `next_attempt` (1-based). Returns
    /// `Duration::ZERO` for the first attempt regardless of policy.
    /// For `Exponential`: `initial * 2^(next_attempt - 2)`, capped at
    /// `max_delay`.
    pub fn delay_for(&self, next_attempt: u32) -> Duration {
        if next_attempt <= 1 {
            return Duration::ZERO;
        }
        match self {
            Self::NoRetry => Duration::ZERO,
            Self::Fixed { delay, .. } => *delay,
            Self::Exponential {
                initial_delay,
                max_delay,
                ..
            } => {
                let exponent = next_attempt - 2;
                let multiplier = 1u32.checked_shl(exponent).unwrap_or(u32::MAX);
                let proposed = initial_delay.checked_mul(multiplier).unwrap_or(*max_delay);
                proposed.min(*max_delay)
            }
        }
    }
}

/// One attempt configuration. Direct port of the dart `ConnectionStrategy`
/// with the addition of an inline `RetryPolicy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionStrategy {
    /// Human-readable id surfaced in logs and to plugins.
    pub id: String,
    /// TUN stack type for this attempt.
    pub stack: StackType,
    /// DNS resolver flavor for this attempt.
    pub resolver_type: ResolverType,
    /// Wall-clock cap for ONE attempt under this strategy.
    #[serde(with = "duration_ms")]
    pub total_timeout: Duration,
    /// How to retry within this strategy.
    pub retry: RetryPolicy,
}

/// Ordered list of strategies + a hard global timeout that caps the
/// entire iteration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyPlan {
    /// Strategies tried in order. Empty plan = no-op (middleware
    /// passes through).
    pub strategies: Vec<ConnectionStrategy>,
    /// Hard cap across the entire plan. If the global timeout fires
    /// mid-strategy, the current attempt is abandoned and the connect
    /// fails. None = no global cap (rare).
    #[serde(default, with = "duration_opt_ms")]
    pub global_timeout: Option<Duration>,
}

// ---------------------------------------------------------------------------
// Duration serde helpers — store as integer milliseconds for wire stability
// ---------------------------------------------------------------------------

mod duration_ms {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        (d.as_millis() as u64).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let ms = u64::deserialize(d)?;
        Ok(Duration::from_millis(ms))
    }
}

mod duration_opt_ms {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        d.map(|d| d.as_millis() as u64).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        let ms: Option<u64> = Option::deserialize(d)?;
        Ok(ms.map(Duration::from_millis))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_retry_max_attempts_is_one() {
        assert_eq!(RetryPolicy::NoRetry.max_attempts(), 1);
    }

    #[test]
    fn fixed_max_attempts() {
        let p = RetryPolicy::Fixed {
            max_attempts: 5,
            delay: Duration::from_secs(1),
        };
        assert_eq!(p.max_attempts(), 5);
    }

    #[test]
    fn exponential_max_attempts() {
        let p = RetryPolicy::Exponential {
            max_attempts: 4,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        };
        assert_eq!(p.max_attempts(), 4);
    }

    #[test]
    fn delay_for_first_attempt_is_zero() {
        let p = RetryPolicy::Fixed {
            max_attempts: 3,
            delay: Duration::from_secs(2),
        };
        assert_eq!(p.delay_for(1), Duration::ZERO);
    }

    #[test]
    fn fixed_delay_is_constant() {
        let p = RetryPolicy::Fixed {
            max_attempts: 3,
            delay: Duration::from_secs(2),
        };
        assert_eq!(p.delay_for(2), Duration::from_secs(2));
        assert_eq!(p.delay_for(3), Duration::from_secs(2));
    }

    #[test]
    fn exponential_delay_doubles() {
        let p = RetryPolicy::Exponential {
            max_attempts: 5,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        };
        assert_eq!(p.delay_for(2), Duration::from_secs(1));
        assert_eq!(p.delay_for(3), Duration::from_secs(2));
        assert_eq!(p.delay_for(4), Duration::from_secs(4));
        assert_eq!(p.delay_for(5), Duration::from_secs(8));
    }

    #[test]
    fn exponential_delay_caps_at_max() {
        let p = RetryPolicy::Exponential {
            max_attempts: 10,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(5),
        };
        assert_eq!(p.delay_for(2), Duration::from_secs(1));
        assert_eq!(p.delay_for(3), Duration::from_secs(2));
        assert_eq!(p.delay_for(4), Duration::from_secs(4));
        assert_eq!(p.delay_for(5), Duration::from_secs(5)); // capped
        assert_eq!(p.delay_for(10), Duration::from_secs(5)); // still capped
    }

    #[test]
    fn connection_strategy_round_trip_serde() {
        let original = ConnectionStrategy {
            id: "default-doh".into(),
            stack: StackType::System,
            resolver_type: ResolverType::Doh,
            total_timeout: Duration::from_secs(30),
            retry: RetryPolicy::Fixed {
                max_attempts: 3,
                delay: Duration::from_secs(2),
            },
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: ConnectionStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn strategy_plan_round_trip_serde() {
        let original = StrategyPlan {
            strategies: vec![
                ConnectionStrategy {
                    id: "doh".into(),
                    stack: StackType::System,
                    resolver_type: ResolverType::Doh,
                    total_timeout: Duration::from_secs(25),
                    retry: RetryPolicy::Fixed {
                        max_attempts: 3,
                        delay: Duration::from_secs(2),
                    },
                },
                ConnectionStrategy {
                    id: "tcp".into(),
                    stack: StackType::GVisor,
                    resolver_type: ResolverType::Tcp,
                    total_timeout: Duration::from_secs(25),
                    retry: RetryPolicy::NoRetry,
                },
            ],
            global_timeout: Some(Duration::from_secs(90)),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: StrategyPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn strategy_plan_no_global_timeout_round_trip() {
        let original = StrategyPlan {
            strategies: vec![],
            global_timeout: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: StrategyPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn stack_type_singbox_str() {
        assert_eq!(StackType::System.as_singbox_str(), "system");
        assert_eq!(StackType::GVisor.as_singbox_str(), "gvisor");
        assert_eq!(StackType::Mixed.as_singbox_str(), "mixed");
    }

    #[test]
    fn resolver_type_singbox_str() {
        assert_eq!(ResolverType::Doh.as_singbox_str(), "https");
        assert_eq!(ResolverType::Tcp.as_singbox_str(), "tcp");
        assert_eq!(ResolverType::Udp.as_singbox_str(), "udp");
        assert_eq!(ResolverType::System.as_singbox_str(), "local");
    }
}
