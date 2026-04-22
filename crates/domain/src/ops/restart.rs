//! Restart operation — stop then start with the same (or new) config.

use crate::pipeline::Operation;
use crate::types::{ConnectionInfo, ConnectionState};
use std::collections::BTreeMap;

/// Restart the active core.
pub struct OpRestart;

#[derive(Debug, Clone)]
pub struct RestartInput {
    pub config_path: String,
    pub core_type: String,
    pub state: ConnectionState,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct RestartOutput {
    pub connection_info: Option<ConnectionInfo>,
    pub metadata: BTreeMap<String, String>,
}

impl Operation for OpRestart {
    type Input = RestartInput;
    type Output = RestartOutput;
    fn name() -> &'static str {
        "restart"
    }
}
