//! Extism-based hook adapter — bridges WASM plugins to the typed pipeline.
//!
//! Each `.wasm` file loaded via [`ExtismPlugin`] produces a set of typed
//! [`Hook<Op>`] implementations. The adapter serializes operation inputs and
//! outputs to JSON, calls exported WASM functions, and deserializes the
//! response.
//!
//! # WASM function naming convention
//!
//! Plugin authors export any combination of these functions. Unimplemented
//! functions are silently skipped — the hook becomes a no-op for that phase.
//!
//! | Phase | Operation | WASM function |
//! |-------|-----------|---------------|
//! | before | `OpConnect` | `before_connect` |
//! | after  | `OpConnect` | `after_connect` |
//! | on_error | `OpConnect` | `on_connect_error` |
//! | before | `OpValidateConfig` | `before_validate` |
//! | after  | `OpValidateConfig` | `after_validate` |
//! | on_error | `OpValidateConfig` | `on_validate_error` |
//! | before | `OpDisconnect` | `before_disconnect` |
//! | after  | `OpListOutbounds` | `filter_outbounds` |
//! | after  | `OpTestLatency` | `adjust_latency` |
//! | on_error | `OpTestLatency` | `on_latency_error` |
//!
//! # JSON protocol
//!
//! ## `before_connect`
//! Input: `{"config_path":"…","core_type":"…","metadata":{…}}`
//! Output: one of:
//! - `{"reject":true,"reason":"…"}` — short-circuit with error
//! - `{"config_path":"…"}` — rewrite config path
//! - `{}` — observe only
//!
//! ## `before_validate`
//! Input: `{"config_path":"…","core_type":"…","config_content":"…or null","metadata":{…}}`
//! Output: one of:
//! - `{"reject":true,"reason":"…"}` — reject before validation runs
//! - `{"config_path":"…"}` — rewrite path (plugin wrote transformed content to a new file)
//! - `{"config_content":"…"}` — inline-replace content (adapter writes to temp, updates path)
//! - `{}` — observe only
//!
//! ## `filter_outbounds` (after on OpListOutbounds)
//! Input: `[{"id":"…","name":"…","country_code":"…or null"}, …]`
//! Output: `["id1","id2",…]` — IDs to keep (others are removed)
//!
//! ## `adjust_latency` (after on OpTestLatency)
//! Input: `{"jp-1":30,"us-1":25}` — outbound_id → latency_ms
//! Output: `{"jp-1":30,"us-1":75}` — adjusted map (full replacement)
//!
//! ## `after_connect`
//! Input: `{"input":{…ConnectInput…},"output":{…ConnectOutput…}}`
//! Output: `{}` (passthrough) or `{"reject":true,"reason":"…"}`
//!
//! ## `on_*_error`
//! Input: `{"input":{…},"error":"…message…"}`
//! Output: ignored (return value is void)
//!
//! # Not in default workspace
//!
//! This crate depends on `extism` (heavy WASM runtime). Add it to the workspace
//! `members` in `Cargo.toml` when needed, or use it as an optional feature.

pub mod plugin_adapter;

use domain::ops::*;
use domain::pipeline::Hook;
use domain::VpnError;
use extism::{Manifest, Plugin, Wasm};
use log::{debug, info, warn};
use std::path::Path;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// ExtismPlugin — the loaded WASM module
// ---------------------------------------------------------------------------

/// A loaded WASM plugin that implements [`Hook<Op>`] for supported operations.
///
/// Create via [`ExtismPlugin::load`], then register with
/// [`Pipeline::push_hook`](domain::pipeline::Pipeline::push_hook) for each
/// operation the plugin extends.
pub struct ExtismPlugin {
    name: String,
    plugin: Mutex<Plugin>,
}

/// Options that control how a [`ExtismPlugin`] is constructed.
///
/// Default = no allowed hosts (HTTPS calls from the plugin will be
/// blocked) and no custom timeout. The hook plugins (filter_outbounds,
/// before_connect, etc.) don't need any HTTP, so this is the right
/// default for the original use case.
///
/// Plugins that talk to a remote backend override `allowed_hosts` to
/// whitelist the panel's domain — see
/// [`crate::plugin_adapter::PluginAdapter::load`].
#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// Hostnames the plugin's `extism_pdk::http::request(...)` calls
    /// are allowed to reach. Glob patterns supported by extism (e.g.
    /// `*.example.com`). Anything not in this list returns a
    /// "HTTP request to <url> is not allowed" error from the wasm
    /// runtime — by design, so a malicious plugin can't exfiltrate
    /// to attacker-controlled hosts.
    pub allowed_hosts: Vec<String>,
    /// Per-call wall-clock timeout in milliseconds. None = use the
    /// extism default (~30s as of 1.21).
    pub timeout_ms: Option<u64>,
}

impl ExtismPlugin {
    /// Load a WASM plugin from a file with no HTTP / network access.
    ///
    /// Convenience wrapper around [`Self::load_with_options`] for the
    /// hook-plugin use case where the plugin only filters / rewrites
    /// daemon-supplied data and never reaches out to the network.
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_with_options(path, LoadOptions::default())
    }

    /// Load a WASM plugin with explicit options.
    ///
    /// The file stem becomes the plugin name used in logs and
    /// diagnostics.
    pub fn load_with_options(path: &Path, opts: LoadOptions) -> Result<Self, String> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        info!("loading extism plugin: {name} from {}", path.display());
        let wasm = Wasm::file(path);
        let mut manifest = Manifest::new([wasm]);
        if !opts.allowed_hosts.is_empty() {
            manifest = manifest.with_allowed_hosts(opts.allowed_hosts.clone().into_iter());
            info!(
                "  allowed_hosts: {}",
                opts.allowed_hosts.join(", ")
            );
        }
        if let Some(ms) = opts.timeout_ms {
            manifest = manifest.with_timeout(std::time::Duration::from_millis(ms));
        }
        let plugin = Plugin::new(&manifest, [], true).map_err(|e| format!("load {name}: {e}"))?;

        Ok(Self {
            name,
            plugin: Mutex::new(plugin),
        })
    }

    /// Plugin name (file stem). Used in logs.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Call a wasm export with a serializable input and a
    /// deserializable output. Returns the deserialised value or a
    /// stringified error suitable for [`VpnError::Unknown`].
    ///
    /// This is the building block the [`crate::user_api_adapter`]
    /// module uses to dispatch every `UserApi` trait method into the
    /// wasm plugin.
    pub fn call_json<I, O>(&self, fn_name: &str, input: &I) -> Result<O, String>
    where
        I: serde::Serialize,
        O: serde::de::DeserializeOwned,
    {
        if !self.has_function(fn_name) {
            return Err(format!("plugin {} does not export {fn_name}", self.name));
        }
        let json_in = serde_json::to_string(input)
            .map_err(|e| format!("serialise input for {fn_name}: {e}"))?;
        debug!("calling {}.{fn_name} with {} bytes", self.name, json_in.len());
        let mut plugin = self.plugin.lock().unwrap_or_else(|e| e.into_inner());
        let raw = plugin
            .call::<&str, &str>(fn_name, &json_in)
            .map_err(|e| format!("plugin {} {fn_name}: {e}", self.name))?;
        serde_json::from_str(raw).map_err(|e| {
            format!(
                "plugin {} {fn_name}: invalid JSON response: {e}",
                self.name
            )
        })
    }

    /// Whether the WASM module exports a given function name.
    pub fn has_function(&self, fn_name: &str) -> bool {
        self.plugin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .function_exists(fn_name)
    }

    /// Call a WASM function with a JSON string, returning JSON output.
    ///
    /// Returns `None` if the function does not exist or if the call fails
    /// (error is logged as a warning).
    fn call_opt(&self, fn_name: &str, json: &str) -> Option<serde_json::Value> {
        if !self.has_function(fn_name) {
            return None;
        }
        debug!("calling {}.{fn_name}", self.name);
        let mut plugin = self.plugin.lock().unwrap_or_else(|e| e.into_inner());
        match plugin.call::<&str, &str>(fn_name, json) {
            Ok(raw) => match serde_json::from_str(raw) {
                Ok(v) => Some(v),
                Err(e) => {
                    warn!("plugin {} {fn_name}: invalid JSON response: {e}", self.name);
                    None
                }
            },
            Err(e) => {
                warn!("plugin {} {fn_name}: call failed: {e}", self.name);
                None
            }
        }
    }

    /// Extract a rejection from a WASM response, if present.
    ///
    /// Returns `Some(VpnError)` if `{"reject": true, "reason": "…"}`.
    fn check_reject(&self, v: &serde_json::Value, fn_name: &str) -> Option<VpnError> {
        if v.get("reject").and_then(|r| r.as_bool()) == Some(true) {
            let reason = v
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("rejected by plugin");
            Some(VpnError::Unknown(format!("{} ({}): {reason}", self.name, fn_name)))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Hook<OpConnect>
// ---------------------------------------------------------------------------

impl Hook<OpConnect> for ExtismPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    /// `before_connect` — can reject or rewrite `config_path`.
    fn before(&self, input: &mut ConnectInput) -> Result<(), VpnError> {
        let json = serde_json::json!({
            "config_path": input.config_path,
            "core_type": input.core_type,
            "metadata": input.metadata,
        })
        .to_string();

        if let Some(v) = self.call_opt("before_connect", &json) {
            if let Some(e) = self.check_reject(&v, "before_connect") {
                return Err(e);
            }
            if let Some(cp) = v.get("config_path").and_then(|v| v.as_str()) {
                input.config_path = cp.to_string();
            }
        }
        Ok(())
    }

    /// `after_connect` — can reject a successful connection output.
    fn after(&self, _input: &ConnectInput, _output: &mut ConnectOutput) -> Result<(), VpnError> {
        let json = serde_json::json!({
            "core_type": _input.core_type,
            "config_path": _input.config_path,
        })
        .to_string();

        if let Some(v) = self.call_opt("after_connect", &json) {
            if let Some(e) = self.check_reject(&v, "after_connect") {
                return Err(e);
            }
        }
        Ok(())
    }

    /// `on_connect_error` — notified when connect fails; read-only.
    fn on_error(&self, input: &ConnectInput, err: &VpnError) {
        let json = serde_json::json!({
            "core_type": input.core_type,
            "config_path": input.config_path,
            "error": err.to_string(),
        })
        .to_string();
        self.call_opt("on_connect_error", &json);
    }
}

// ---------------------------------------------------------------------------
// Hook<OpValidateConfig>
// ---------------------------------------------------------------------------

impl Hook<OpValidateConfig> for ExtismPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    /// `before_validate` — receives config content if loaded; can decrypt,
    /// patch, or rewrite `config_path`.
    ///
    /// # Config content transformation
    ///
    /// If the plugin returns `{"config_content":"…"}`, the adapter writes
    /// the new content to a temporary file and updates `input.config_path`
    /// to point to it. The terminal handler (`validate_config`) then receives
    /// the transformed config without knowing about the plugin.
    fn before(&self, input: &mut ValidateConfigInput) -> Result<(), VpnError> {
        let json = serde_json::json!({
            "config_path": input.config_path,
            "core_type": input.core_type,
            "config_content": input.config_content,
            "metadata": input.metadata,
        })
        .to_string();

        if let Some(v) = self.call_opt("before_validate", &json) {
            if let Some(e) = self.check_reject(&v, "before_validate") {
                return Err(e);
            }
            // Plugin may rewrite the path directly.
            if let Some(cp) = v.get("config_path").and_then(|v| v.as_str()) {
                input.config_path = cp.to_string();
            }
            // Plugin may return transformed content inline.
            // Write it to a temp file and update config_path.
            if let Some(new_content) = v.get("config_content").and_then(|v| v.as_str()) {
                match write_temp_config(new_content) {
                    Ok(tmp_path) => {
                        input.config_content = Some(new_content.to_string());
                        input.config_path = tmp_path;
                    }
                    Err(e) => {
                        warn!("plugin {} before_validate: could not write temp config: {e}", self.name);
                    }
                }
            }
        }
        Ok(())
    }

    /// `after_validate` — observe or reject a successful validation.
    fn after(
        &self,
        _input: &ValidateConfigInput,
        _output: &mut ValidateConfigOutput,
    ) -> Result<(), VpnError> {
        let json = serde_json::json!({
            "config_path": _input.config_path,
            "core_type": _input.core_type,
        })
        .to_string();

        if let Some(v) = self.call_opt("after_validate", &json) {
            if let Some(e) = self.check_reject(&v, "after_validate") {
                return Err(e);
            }
        }
        Ok(())
    }

    /// `on_validate_error` — notified when validation fails; read-only.
    fn on_error(&self, input: &ValidateConfigInput, err: &VpnError) {
        let json = serde_json::json!({
            "config_path": input.config_path,
            "core_type": input.core_type,
            "error": err.to_string(),
        })
        .to_string();
        self.call_opt("on_validate_error", &json);
    }
}

// ---------------------------------------------------------------------------
// Hook<OpDisconnect>
// ---------------------------------------------------------------------------

impl Hook<OpDisconnect> for ExtismPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    /// `before_disconnect` — can reject the disconnect request.
    fn before(&self, input: &mut DisconnectInput) -> Result<(), VpnError> {
        let json = serde_json::json!({
            "core_type": input.core_type,
            "metadata": input.metadata,
        })
        .to_string();

        if let Some(v) = self.call_opt("before_disconnect", &json) {
            if let Some(e) = self.check_reject(&v, "before_disconnect") {
                return Err(e);
            }
        }
        Ok(())
    }

    /// `on_disconnect_error` — notified when disconnect fails; read-only.
    fn on_error(&self, input: &DisconnectInput, err: &VpnError) {
        let json = serde_json::json!({
            "core_type": input.core_type,
            "error": err.to_string(),
        })
        .to_string();
        self.call_opt("on_disconnect_error", &json);
    }
}

// ---------------------------------------------------------------------------
// Hook<OpListOutbounds>
// ---------------------------------------------------------------------------

impl Hook<OpListOutbounds> for ExtismPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    /// `filter_outbounds` — receives the full list; returns IDs to keep.
    ///
    /// Input JSON: `[{"id":"…","name":"…","country_code":"…or null"}, …]`
    /// Output JSON: `["id1","id2",…]` — only these IDs are kept.
    ///
    /// If the plugin doesn't export `filter_outbounds`, the list passes through
    /// unchanged.
    fn after(
        &self,
        _input: &ListOutboundsInput,
        output: &mut ListOutboundsOutput,
    ) -> Result<(), VpnError> {
        // Build a rich JSON representation so plugins have full outbound metadata.
        let outbounds_json: Vec<serde_json::Value> = output
            .outbounds
            .iter()
            .map(|o| {
                serde_json::json!({
                    "id": o.id,
                    "name": o.name,
                    "protocol": o.protocol.as_str(),
                    "country_code": o.country_code,
                    "location": o.location,
                    "latency_ms": o.latency_ms,
                    "selected": o.selected,
                })
            })
            .collect();

        let json = serde_json::to_string(&outbounds_json).unwrap_or_else(|_| "[]".into());

        match self.call_opt("filter_outbounds", &json) {
            Some(v) => {
                // Plugin returns a JSON array of IDs to keep.
                if let Some(ids) = v.as_array() {
                    let keep: Vec<String> = ids
                        .iter()
                        .filter_map(|id| id.as_str().map(|s| s.to_string()))
                        .collect();
                    let before = output.outbounds.len();
                    output.outbounds.retain(|o| keep.contains(&o.id));
                    let removed = before - output.outbounds.len();
                    if removed > 0 {
                        output.metadata.insert(
                            format!("{}:filter:removed", self.name),
                            removed.to_string(),
                        );
                    }
                }
            }
            None => {
                // Plugin absent or function not exported — pass through.
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Hook<OpTestLatency>
// ---------------------------------------------------------------------------

impl Hook<OpTestLatency> for ExtismPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    /// `adjust_latency` — receives latency map; returns adjusted map.
    ///
    /// Input JSON: `{"jp-1":30,"us-1":25}` (outbound_id → latency_ms as u32)
    /// Output JSON: `{"jp-1":30,"us-1":75}` — full replacement of the map.
    ///
    /// If the plugin doesn't export `adjust_latency`, results pass through
    /// unchanged.
    fn after(
        &self,
        _input: &TestLatencyInput,
        output: &mut TestLatencyOutput,
    ) -> Result<(), VpnError> {
        let json = serde_json::to_string(&output.results).unwrap_or_else(|_| "{}".into());

        if let Some(v) = self.call_opt("adjust_latency", &json) {
            if let Some(map) = v.as_object() {
                let mut new_results = std::collections::BTreeMap::new();
                for (id, latency) in map {
                    if let Some(ms) = latency.as_u64() {
                        new_results.insert(id.clone(), ms as u32);
                    }
                }
                if !new_results.is_empty() {
                    output.results = new_results;
                }
            }
        }
        Ok(())
    }

    /// `on_latency_error` — notified when latency test fails; read-only.
    fn on_error(&self, input: &TestLatencyInput, err: &VpnError) {
        let json = serde_json::json!({
            "core_type": input.core_type,
            "outbound_ids": input.outbound_ids,
            "error": err.to_string(),
        })
        .to_string();
        self.call_opt("on_latency_error", &json);
    }
}

// ---------------------------------------------------------------------------
// Helper: write transformed config content to a temp file
// ---------------------------------------------------------------------------

/// Write plugin-transformed config content to a temp file.
///
/// Returns the temp file path. The caller is responsible for ensuring the file
/// remains accessible for the duration of the validate operation.
fn write_temp_config(content: &str) -> Result<String, std::io::Error> {
    use std::io::Write;
    let mut tmp = tempfile::Builder::new()
        .prefix("pingle-plugin-config-")
        .suffix(".json")
        .tempfile()?;
    tmp.write_all(content.as_bytes())?;
    // Keep the file (persist) so the path remains valid after this function returns.
    let (_, path) = tmp.keep().map_err(|e| e.error)?;
    path.to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "non-UTF8 path"))
}
