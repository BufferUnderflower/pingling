use crate::attempt::ConfigRequest;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Wire protocol version for the config.process slot payload.
pub const CONFIG_PROCESS_WIRE_VERSION: u32 = 1;

/// Payload type exchanged over the config.process slot chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigProcessPayload {
    /// The sing-box config JSON, as a free-form value.
    pub config: Value,
    /// Contextual info about the current attempt.
    pub request: ConfigRequest,
    /// The kind of VpnCore processing this config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_type: Option<String>,
    /// The version string of the active core.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_version: Option<String>,
    /// The target OS platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_os: Option<String>,
}
