//! Disconnect operation — gracefully stop the VPN tunnel.

use crate::pipeline::Operation;
use crate::types::ConnectionState;
use std::collections::BTreeMap;

/// Disconnect the active core.
pub struct OpDisconnect;

#[derive(Debug, Clone)]
pub struct DisconnectInput {
    pub core_type: String,
    pub state: ConnectionState,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct DisconnectOutput {
    pub metadata: BTreeMap<String, String>,
}

impl Operation for OpDisconnect {
    type Input = DisconnectInput;
    type Output = DisconnectOutput;
    fn name() -> &'static str {
        "disconnect"
    }
}
