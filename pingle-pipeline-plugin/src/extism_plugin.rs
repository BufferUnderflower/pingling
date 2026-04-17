//! `ExtismPipelinePlugin` — wasm-backed implementation of [`PipelinePlugin`].
//!
//! Loads a `.wasm` file via `extism::Plugin`, calls
//! `pipeline_capabilities` once at load time (with a safe default if
//! the export is missing), then dispatches each `process_config` call
//! by serializing the input to JSON and deserializing the output.
//!
//! Mirrors the existing `plugin-extism` adapter pattern, but with
//! pipeline-specific contracts.

use crate::protocol::{
    PipelineCapabilities, PipelineStage, ProcessConfigInput, ProcessConfigOutput, WIRE_VERSION,
};
use crate::trait_def::{PipelinePlugin, PluginError};
use extism::{Manifest, Plugin, PluginBuilder, Wasm};
use std::path::Path;
use std::sync::Mutex;

/// Wasm-backed pipeline plugin.
///
/// Construct via [`ExtismPipelinePlugin::load`]. The loaded plugin is
/// kept alive for the lifetime of this struct; drop the struct to
/// release the wasm runtime.
pub struct ExtismPipelinePlugin {
    name: String,
    plugin: Mutex<Plugin>,
    capabilities: PipelineCapabilities,
}

impl ExtismPipelinePlugin {
    /// Load a wasm pipeline plugin from a file. Returns an error if:
    /// - The wasm fails to load
    /// - `pipeline_capabilities` returns a wire version mismatch
    /// - `process_config` is not exported
    ///
    /// If `pipeline_capabilities` is not exported, the plugin is
    /// loaded with default capabilities (claims only `post_pipeline`).
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        log::info!("loading pipeline plugin: {name} from {}", path.display());
        let wasm = Wasm::file(path);
        let manifest = Manifest::new([wasm]);
        let plugin = build_plugin(manifest, &name)?;

        let mut adapter = Self {
            name,
            plugin: Mutex::new(plugin),
            capabilities: PipelineCapabilities::default(),
        };
        adapter.probe_capabilities()?;

        // Sanity check: process_config must exist.
        if !adapter.has_function("process_config") {
            return Err(PluginError::MissingExport {
                fn_name: "process_config".into(),
            });
        }

        Ok(adapter)
    }

    /// Call `pipeline_capabilities` if exported and cache the result.
    /// If not exported, leave the default in place.
    fn probe_capabilities(&mut self) -> Result<(), PluginError> {
        if !self.has_function("pipeline_capabilities") {
            log::debug!(
                "pipeline plugin {}: no pipeline_capabilities export, using defaults",
                self.name
            );
            return Ok(());
        }
        let raw = {
            let mut plugin = self.plugin.lock().unwrap_or_else(|e| e.into_inner());
            plugin
                .call::<&str, &str>("pipeline_capabilities", "")
                .map_err(|e| PluginError::Wasm(format!("pipeline_capabilities: {e}")))?
                .to_string()
        };
        let parsed: PipelineCapabilities = serde_json::from_str(&raw)
            .map_err(|e| PluginError::InvalidJson(format!("pipeline_capabilities: {e}")))?;
        if parsed.wire_version != WIRE_VERSION {
            return Err(PluginError::WireVersionMismatch {
                plugin_says: parsed.wire_version,
                daemon_uses: WIRE_VERSION,
            });
        }
        self.capabilities = parsed;
        log::info!(
            "pipeline plugin {}: claims stages {:?}",
            self.name,
            self.capabilities
                .stages
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    fn has_function(&self, fn_name: &str) -> bool {
        self.plugin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .function_exists(fn_name)
    }
}

fn build_plugin(manifest: Manifest, name: &str) -> Result<Plugin, PluginError> {
    let mut builder = PluginBuilder::new(manifest).with_wasi(true);
    if let Some(target) = configured_wasmtime_target() {
        let mut config = wasmtime::Config::new();
        config.target(&target).map_err(|e| {
            PluginError::Wasm(format!(
                "load {name}: invalid Wasmtime target `{target}`: {e:#}"
            ))
        })?;
        log::info!("loading pipeline plugin {name} with Wasmtime target {target}");
        builder = builder.with_wasmtime_config(config);
    }
    builder
        .build()
        .map_err(|e| PluginError::Wasm(format!("load {name}: {e:#}")))
}

fn configured_wasmtime_target() -> Option<String> {
    match std::env::var("PINGLE_WASMTIME_TARGET")
        .ok()
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some("native") => None,
        Some(target) => Some(target.to_string()),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::configured_wasmtime_target;

    #[test]
    fn default_runtime_stays_native_without_override() {
        std::env::remove_var("PINGLE_WASMTIME_TARGET");
        assert_eq!(configured_wasmtime_target(), None);
    }

    #[test]
    fn explicit_pulley_override_still_works() {
        std::env::set_var("PINGLE_WASMTIME_TARGET", "pulley64");
        assert_eq!(configured_wasmtime_target(), Some("pulley64".into()));
        std::env::remove_var("PINGLE_WASMTIME_TARGET");
    }
}

impl PipelinePlugin for ExtismPipelinePlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> &PipelineCapabilities {
        &self.capabilities
    }

    fn process_config(
        &self,
        _stage: PipelineStage,
        input: ProcessConfigInput,
    ) -> Result<ProcessConfigOutput, PluginError> {
        let json_in = serde_json::to_string(&input)
            .map_err(|e| PluginError::InvalidJson(format!("serialize input: {e}")))?;
        let raw = {
            let mut plugin = self.plugin.lock().unwrap_or_else(|e| e.into_inner());
            plugin
                .call::<&str, &str>("process_config", &json_in)
                .map_err(|e| PluginError::Wasm(format!("process_config: {e}")))?
                .to_string()
        };
        let parsed: ProcessConfigOutput = serde_json::from_str(&raw)
            .map_err(|e| PluginError::InvalidJson(format!("process_config: {e}")))?;
        Ok(parsed)
    }
}
