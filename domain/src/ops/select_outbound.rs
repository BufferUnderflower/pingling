//! Select outbound — a capability operation.

use crate::pipeline::Operation;
use std::collections::BTreeMap;

/// Select an outbound by ID, routing traffic through it.
pub struct OpSelectOutbound;

#[derive(Debug, Clone)]
pub struct SelectOutboundInput {
    pub outbound_id: String,
    pub core_type: String,
    pub config_path: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct SelectOutboundOutput {
    pub metadata: BTreeMap<String, String>,
}

impl Operation for OpSelectOutbound {
    type Input = SelectOutboundInput;
    type Output = SelectOutboundOutput;
    fn name() -> &'static str {
        "select_outbound"
    }
}
