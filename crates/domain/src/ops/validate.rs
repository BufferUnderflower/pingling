//! Config validation operation.
//!
//! Validates a VPN core configuration file without starting the tunnel.
//!
//! # Config content abstraction for plugins
//!
//! `ValidateConfigInput` carries an optional `config_content` field. When the
//! [`ConfigContentLoader`] middleware is registered, it reads the config file
//! and populates this field **before** any other hook runs. This lets plugins
//! access and transform the raw config text, not just the file path.
//!
//! Plugin use cases enabled by `config_content`:
//! - Decrypt an encrypted config and expose the plaintext for validation
//! - Patch or extend JSON/YAML fields (e.g. inject custom DNS, routing rules)
//! - Write the modified content to a temp file and update `config_path`
//!
//! The pipeline flow with content loading:
//!
//! ```text
//! ValidateConfigInput { config_path: "/cfg.json", config_content: None }
//!     ↓ ConfigContentLoader.before()   ← reads file, populates config_content
//! ValidateConfigInput { .., config_content: Some("{ ... }") }
//!     ↓ Plugin.before()                ← may decrypt / patch, rewrites config_path
//! ValidateConfigHandler                ← calls core.validate_config(config_path)
//! ```

use crate::pipeline::Operation;
use std::collections::BTreeMap;

/// Validate a VPN core configuration file without starting the tunnel.
pub struct OpValidateConfig;

/// Input for [`OpValidateConfig`].
#[derive(Debug, Clone)]
pub struct ValidateConfigInput {
    /// Path to the config file to validate.
    ///
    /// A `before` hook — e.g. a plugin that decrypts or transforms the config —
    /// may rewrite this to a temporary file path, as long as the new file
    /// conforms to the core's expected schema.
    pub config_path: String,

    /// Active core type (e.g. `"sing-box"`).
    pub core_type: String,

    /// Raw text content of the config file.
    ///
    /// Populated by [`ConfigContentLoader`] middleware when registered.
    /// `None` if that middleware is absent or the file could not be read.
    ///
    /// Plugins that need to inspect or modify the config should:
    /// 1. Read this field for the raw text.
    /// 2. Transform it as needed (decrypt, patch, etc.).
    /// 3. Write the result to a temp file.
    /// 4. Update `config_path` to point to the temp file.
    ///
    /// The core's `validate_config` always receives `config_path`, never this
    /// field directly — so updating the path is the contract.
    pub config_content: Option<String>,

    /// Extensible key-value metadata. Hooks communicate without schema changes.
    pub metadata: BTreeMap<String, String>,
}

/// Output from [`OpValidateConfig`].
#[derive(Debug, Clone)]
pub struct ValidateConfigOutput {
    /// Metadata enriched by hooks (e.g. validation timing, detected warnings).
    pub metadata: BTreeMap<String, String>,
}

impl Operation for OpValidateConfig {
    type Input = ValidateConfigInput;
    type Output = ValidateConfigOutput;
    fn name() -> &'static str {
        "validate_config"
    }
}
