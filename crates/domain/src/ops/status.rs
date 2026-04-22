//! Status query operation.

use crate::pipeline::Operation;
use crate::types::{ConnectionInfo, ConnectionState};

/// Query the current connection state and optional connection info.
pub struct OpGetStatus;

#[derive(Debug, Clone)]
pub struct GetStatusInput {
    pub core_type: String,
}

#[derive(Debug, Clone)]
pub struct GetStatusOutput {
    pub state: ConnectionState,
    pub connection_info: Option<ConnectionInfo>,
    pub running: bool,
}

impl Operation for OpGetStatus {
    type Input = GetStatusInput;
    type Output = GetStatusOutput;
    fn name() -> &'static str {
        "get_status"
    }
}
