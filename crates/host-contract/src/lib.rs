use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

pub const HOST_PROTOCOL_VERSION: &str = "pingling.host.v1";

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Slot(String);

impl Slot {
    pub const CONFIG_TRANSFORM: &'static str = "config.transform";
    pub const RUNTIME_COMMAND: &'static str = "runtime.command";
    pub const RUNTIME_OBSERVE: &'static str = "runtime.observe";
    pub const STORAGE_LOOKUP: &'static str = "storage.lookup";
    pub const TELEMETRY_OBSERVE: &'static str = "telemetry.observe";

    pub fn new(value: impl Into<String>) -> HostResult<Self> {
        let value = value.into();
        validate_token("slot", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostInvocation {
    pub protocol: String,
    pub slot: Slot,
    pub operation: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl HostInvocation {
    pub fn new(slot: Slot, operation: impl Into<String>, payload: serde_json::Value) -> HostResult<Self> {
        let operation = operation.into();
        validate_token("operation", &operation)?;
        Ok(Self {
            protocol: HOST_PROTOCOL_VERSION.to_owned(),
            slot,
            operation,
            metadata: BTreeMap::new(),
            payload,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostOutcome {
    #[serde(default)]
    pub diagnostics: Vec<HostDiagnostic>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl HostOutcome {
    pub fn passthrough(payload: serde_json::Value) -> Self {
        Self {
            diagnostics: Vec::new(),
            payload,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostDiagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostFailure {
    pub code: String,
    pub message: String,
}

impl HostFailure {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_input".to_owned(),
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            code: "unsupported".to_owned(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HostFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for HostFailure {}

pub type HostResult<T> = Result<T, HostFailure>;

pub trait ExtensionHost {
    fn invoke(&self, invocation: HostInvocation) -> HostResult<HostOutcome>;
}

fn validate_token(kind: &str, value: &str) -> HostResult<()> {
    if value.is_empty() {
        return Err(HostFailure::invalid_input(format!("{kind} must not be empty")));
    }

    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | ':'))
    {
        Ok(())
    } else {
        Err(HostFailure::invalid_input(format!(
            "{kind} contains unsupported characters"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_slot() {
        let error = Slot::new("").unwrap_err();
        assert_eq!(error.code, "invalid_input");
    }

    #[test]
    fn builds_invocation_with_protocol() {
        let invocation = HostInvocation::new(
            Slot::new(Slot::CONFIG_TRANSFORM).unwrap(),
            "apply",
            serde_json::json!({"hello": "world"}),
        )
        .unwrap();

        assert_eq!(invocation.protocol, HOST_PROTOCOL_VERSION);
        assert_eq!(invocation.slot.as_str(), Slot::CONFIG_TRANSFORM);
    }
}
