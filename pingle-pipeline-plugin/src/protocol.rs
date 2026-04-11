//! Wire types — JSON shapes the daemon and the wasm guest agree on.
//!
//! Match the spec under "Pipeline plugin protocol". Every type here
//! round-trips through `serde_json` and is the single source of truth
//! for both ends of the wire.

use core_config_processor::AttemptInfo;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Current wire-format version. Bumped only on breaking changes; the
/// daemon refuses plugins whose `wire_version` doesn't match.
pub const WIRE_VERSION: u32 = 1;

/// One stage of the pipeline at which the plugin can be invoked.
///
/// **Open enum.** New stages can be added without bumping
/// [`WIRE_VERSION`]. Plugins that don't recognize a stage receive it
/// as [`PipelineStage::Other`] and pass through their input unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStage {
    /// Raw config from user, before any native processor.
    PrePipeline,
    /// After dns processor finished.
    PostDns,
    /// After ruleset download + URL→local-path rewrite finished.
    PostRuleset,
    /// After routing-exclusions processor finished.
    PostRoutingExcl,
    /// After stack processor finished.
    PostStack,
    /// After every native processor finished. The default stage.
    PostPipeline,
    /// Inside `StrategyRetryWrap`, immediately before the inner handler
    /// is invoked. Last-mile tweaks. Note: runs *before* sing-box
    /// validates because validate lives downstream of the wrap.
    PreStart,
    /// Forward-compat: any stage value the deserializer doesn't
    /// recognize lands here. Plugins should pass through their input
    /// unchanged when they receive this variant.
    #[serde(untagged)]
    Other(String),
}

impl PipelineStage {
    /// Stable string identifier used in logs and as a JSON key.
    pub fn as_str(&self) -> &str {
        match self {
            Self::PrePipeline => "pre_pipeline",
            Self::PostDns => "post_dns",
            Self::PostRuleset => "post_ruleset",
            Self::PostRoutingExcl => "post_routing_excl",
            Self::PostStack => "post_stack",
            Self::PostPipeline => "post_pipeline",
            Self::PreStart => "pre_start",
            Self::Other(s) => s,
        }
    }
}

/// Canonical order in which the strategy retry wrap walks the stages
/// inside one attempt. Stages the plugin doesn't claim are skipped.
pub const CANONICAL_STAGE_ORDER: &[PipelineStage] = &[
    PipelineStage::PrePipeline,
    PipelineStage::PostDns,
    PipelineStage::PostRuleset,
    PipelineStage::PostRoutingExcl,
    PipelineStage::PostStack,
    PipelineStage::PostPipeline,
    PipelineStage::PreStart,
];

/// Identifies which VPN core is driving the request. Lets a plugin
/// branch on core kind / version / platform without daemon changes
/// when a new core is added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreInfo {
    /// Free-form core kind: `"libbox-macos"`, `"libbox-windows"`,
    /// `"singbox-standalone"`, `"mock"`, future cores.
    pub kind: String,
    /// Underlying engine version (e.g. sing-box version), not the
    /// pingle daemon version.
    pub version: String,
    /// Host platform: `"macos"`, `"windows"`, `"linux"`.
    pub platform: String,
}

/// Plugin's static self-description, returned by the optional
/// `pipeline_capabilities` wasm export.
///
/// If the plugin doesn't export `pipeline_capabilities`, the daemon
/// uses [`PipelineCapabilities::default()`] which claims only
/// `post_pipeline`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineCapabilities {
    /// Wire version the plugin understands. Daemon refuses to load
    /// when this doesn't equal [`WIRE_VERSION`].
    pub wire_version: u32,
    /// Plugin name. Used in logs.
    #[serde(default)]
    pub name: String,
    /// Plugin description. Used in `daemon.info`.
    #[serde(default)]
    pub description: String,
    /// Stages the plugin wants `process_config` invoked at. Stages
    /// not in this list cost the daemon zero per attempt — no call,
    /// no serialization.
    pub stages: Vec<PipelineStage>,
}

impl Default for PipelineCapabilities {
    fn default() -> Self {
        Self {
            wire_version: WIRE_VERSION,
            name: "unnamed".into(),
            description: String::new(),
            stages: vec![PipelineStage::PostPipeline],
        }
    }
}

/// Input handed to the wasm guest's `process_config` export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessConfigInput {
    /// Wire version the daemon is using. Plugin should fail fast on
    /// mismatch.
    pub wire_version: u32,
    /// Which core is running this attempt.
    pub core: CoreInfo,
    /// Stage the plugin is being called at, plus the per-attempt info.
    pub attempt: ProcessConfigAttempt,
    /// The full sing-box config JSON snapshot at this stage.
    pub config: Value,
}

/// `attempt` block of [`ProcessConfigInput`] — adds the `stage` field
/// to the existing per-attempt info.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessConfigAttempt {
    /// Stage being processed.
    pub stage: PipelineStage,
    /// Active strategy.
    pub strategy: core_config_processor::ConnectionStrategy,
    /// 1-based attempt counter inside the current strategy.
    pub attempt_number: u32,
    /// `None` on the first attempt of any strategy.
    pub previous_error: Option<core_config_processor::PreviousError>,
}

impl ProcessConfigAttempt {
    /// Build from a stage + the existing [`AttemptInfo`].
    pub fn from_attempt(stage: PipelineStage, info: &AttemptInfo) -> Self {
        Self {
            stage,
            strategy: info.strategy.clone(),
            attempt_number: info.attempt_number,
            previous_error: info.previous_error.clone(),
        }
    }
}

/// Output returned by the wasm guest's `process_config` export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessConfigOutput {
    /// (Possibly modified) config. Plugin returns its input unchanged
    /// for passthrough.
    pub config: Value,
    /// Optional debug strings the daemon will log at info level.
    #[serde(default)]
    pub diagnostics: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_config_processor::{
        ConnectionStrategy, ErrorKind, PreviousError, ResolverType, RetryPolicy, StackType,
    };
    use serde_json::json;
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
    fn pipeline_stage_round_trip_post_pipeline() {
        let stage = PipelineStage::PostPipeline;
        let json = serde_json::to_string(&stage).unwrap();
        assert_eq!(json, "\"post_pipeline\"");
        let parsed: PipelineStage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, stage);
    }

    #[test]
    fn pipeline_stage_round_trip_pre_start() {
        let stage = PipelineStage::PreStart;
        let json = serde_json::to_string(&stage).unwrap();
        assert_eq!(json, "\"pre_start\"");
        let parsed: PipelineStage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, stage);
    }

    #[test]
    fn pipeline_stage_unknown_value_lands_in_other() {
        let parsed: PipelineStage = serde_json::from_str("\"future_stage_xyz\"").unwrap();
        assert_eq!(parsed, PipelineStage::Other("future_stage_xyz".into()));
        // Round-trip preserves the original value.
        let back = serde_json::to_string(&parsed).unwrap();
        assert_eq!(back, "\"future_stage_xyz\"");
    }

    #[test]
    fn pipeline_stage_as_str() {
        assert_eq!(PipelineStage::PrePipeline.as_str(), "pre_pipeline");
        assert_eq!(PipelineStage::PostDns.as_str(), "post_dns");
        assert_eq!(PipelineStage::PostPipeline.as_str(), "post_pipeline");
        assert_eq!(PipelineStage::PreStart.as_str(), "pre_start");
        assert_eq!(PipelineStage::Other("xx".into()).as_str(), "xx");
    }

    #[test]
    fn canonical_stage_order_has_seven_stages() {
        assert_eq!(CANONICAL_STAGE_ORDER.len(), 7);
        assert_eq!(CANONICAL_STAGE_ORDER[0], PipelineStage::PrePipeline);
        assert_eq!(CANONICAL_STAGE_ORDER[6], PipelineStage::PreStart);
    }

    #[test]
    fn pipeline_capabilities_round_trip() {
        let original = PipelineCapabilities {
            wire_version: WIRE_VERSION,
            name: "tracer".into(),
            description: "logs everything".into(),
            stages: vec![PipelineStage::PostPipeline, PipelineStage::PostDns],
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: PipelineCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn pipeline_capabilities_default_claims_only_post_pipeline() {
        let cap = PipelineCapabilities::default();
        assert_eq!(cap.wire_version, WIRE_VERSION);
        assert_eq!(cap.stages, vec![PipelineStage::PostPipeline]);
    }

    #[test]
    fn core_info_round_trip() {
        let original = CoreInfo {
            kind: "libbox-macos".into(),
            version: "1.10.7".into(),
            platform: "macos".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: CoreInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn process_config_input_round_trip() {
        let original = ProcessConfigInput {
            wire_version: WIRE_VERSION,
            core: CoreInfo {
                kind: "libbox-macos".into(),
                version: "1.10.7".into(),
                platform: "macos".into(),
            },
            attempt: ProcessConfigAttempt {
                stage: PipelineStage::PostDns,
                strategy: sample_strategy(),
                attempt_number: 2,
                previous_error: Some(PreviousError {
                    kind: ErrorKind::DnsFailure,
                    message: "no such host".into(),
                    core_error_kind: "ProcessStartFailed".into(),
                }),
            },
            config: json!({"outbounds": [], "dns": {}}),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: ProcessConfigInput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn process_config_output_round_trip() {
        let original = ProcessConfigOutput {
            config: json!({"outbounds": []}),
            diagnostics: vec!["did a thing".into()],
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: ProcessConfigOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn process_config_output_empty_diagnostics_default() {
        let json = "{\"config\":{}}";
        let parsed: ProcessConfigOutput = serde_json::from_str(json).unwrap();
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn process_config_attempt_from_attempt() {
        let info = AttemptInfo {
            strategy: sample_strategy(),
            attempt_number: 3,
            previous_error: None,
        };
        let attempt = ProcessConfigAttempt::from_attempt(PipelineStage::PostStack, &info);
        assert_eq!(attempt.stage, PipelineStage::PostStack);
        assert_eq!(attempt.attempt_number, 3);
        assert_eq!(attempt.strategy, sample_strategy());
    }
}
