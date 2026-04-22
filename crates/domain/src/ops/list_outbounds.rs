//! List outbounds — a capability operation.
//!
//! Only cores that have a pipeline registered for [`OpListOutbounds`] support
//! this. The terminal handler is core-specific (e.g. parses a sing-box config
//! file or queries the Clash API). Middleware can filter, reorder, or enrich
//! the outbound list before it reaches the caller.

use crate::pipeline::Operation;
use crate::types::Outbound;
use std::collections::BTreeMap;

/// List available outbounds (proxy servers) from the active core's config.
pub struct OpListOutbounds;

#[derive(Debug, Clone)]
pub struct ListOutboundsInput {
    pub core_type: String,
    /// Optional config path — some handlers parse the config file directly.
    pub config_path: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ListOutboundsOutput {
    pub outbounds: Vec<Outbound>,
    pub metadata: BTreeMap<String, String>,
}

impl Operation for OpListOutbounds {
    type Input = ListOutboundsInput;
    type Output = ListOutboundsOutput;
    fn name() -> &'static str {
        "list_outbounds"
    }
}
