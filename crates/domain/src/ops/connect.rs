//! Connect operation — start a VPN tunnel.

use crate::pipeline::Operation;
use crate::types::{ConnectionInfo, ConnectionState};
use std::collections::BTreeMap;

/// Connect the active core using a config file.
pub struct OpConnect;

/// Input for [`OpConnect`].
#[derive(Debug, Clone)]
pub struct ConnectInput {
    /// Path to the VPN core config file. Middleware may rewrite this
    /// (e.g. a plugin that generates configs on the fly).
    pub config_path: String,
    /// Active core type (e.g. `"sing-box"`).
    pub core_type: String,
    /// Connection state at the time of the request.
    pub state: ConnectionState,
    /// Extensible key-value pairs. Middleware can read and write metadata
    /// to communicate with each other without schema changes.
    pub metadata: BTreeMap<String, String>,
}

/// Output from [`OpConnect`].
#[derive(Debug, Clone)]
pub struct ConnectOutput {
    /// Structured info about the established connection.
    /// `None` if the core doesn't provide it.
    pub connection_info: Option<ConnectionInfo>,
    /// Metadata enriched by middleware (e.g. timing, selected server).
    pub metadata: BTreeMap<String, String>,
}

impl Operation for OpConnect {
    type Input = ConnectInput;
    type Output = ConnectOutput;
    fn name() -> &'static str {
        "connect"
    }
}
