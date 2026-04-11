//! Extism-backed config processor adapter.
//!
//! Wraps an `Arc<dyn domain::Plugin>` so the loaded wasm plugin
//! behaves like a first-class [`ConfigProcessor`]. The adapter calls
//! the plugin's `config.process` method via the generic
//! [`Plugin::handle_ipc`] dispatch and treats the returned JSON as
//! the transformed config.
//!
//! ## Wire contract
//!
//! The plugin must export `plugin_handle_ipc` and claim the
//! `config.process` method name. The daemon invokes it with:
//!
//! ```json
//! {
//!   "config": <the current config JSON>,
//!   "request": <the ConfigRequest envelope>
//! }
//! ```
//!
//! Expected response (tagged enum, serde with `rename_all = "snake_case"`):
//!
//! ```json
//! {"kind": "transformed", "config": <new config JSON>}
//! {"kind": "unchanged"}
//! {"kind": "error", "message": "..."}
//! ```
//!
//! If the plugin returns `None` from `handle_ipc` (i.e., it doesn't
//! claim the method), the adapter treats that as `unchanged` — the
//! input config flows through unmodified. This keeps the pipeline
//! resilient when a loaded plugin only cares about auth and doesn't
//! implement any config transforms.
//!
//! ## Why this lives here, not in `plugin-extism`
//!
//! `core-config-processor` does NOT depend on `plugin-extism` — that
//! dependency would go the wrong way (plugin-extism already depends
//! on domain, and domain's `Plugin` trait is the only primitive we
//! need here). Taking an `Arc<dyn domain::Plugin>` keeps the adapter
//! generic: any implementation of the `Plugin` trait (wasm, native
//! mock, remote subprocess) plugs into the pipeline the same way.

use crate::attempt::ConfigRequest;
use crate::pipeline::ConfigProcessor;
use domain::Plugin;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// Well-known method name the plugin must claim to act as a
/// config processor.
///
/// Kept as a constant so both the daemon (which constructs adapters)
/// and plugin authors (who document which method they export) can
/// reference it without hardcoding the string.
pub const CONFIG_PROCESS_METHOD: &str = "config.process";

/// Plugin-side response for `config.process`.
///
/// Plugins return one of these three shapes. The adapter unpacks
/// the response and either replaces the config with the transformed
/// version, passes the original through, or surfaces the error to
/// the pipeline runner.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ConfigProcessResponse {
    /// Plugin transformed the config. Use the new one.
    Transformed { config: Value },
    /// Plugin examined the config but made no changes. Pass through.
    Unchanged,
    /// Plugin recognised the input but couldn't process it.
    Error { message: String },
}

/// Adapter that makes an `Arc<dyn Plugin>` behave like a
/// [`ConfigProcessor`].
///
/// Construct one per plugin instance. The pipeline can hold multiple
/// adapters if the daemon has multiple plugins loaded — they run in
/// registration order.
pub struct ExtismProcessorAdapter {
    /// Display name for logs and error messages. Typically the plugin's
    /// `name()` plus the method suffix, e.g. `"pingle-hub/config.process"`.
    name: String,
    /// The underlying plugin. An `Arc` so the adapter can be cloned
    /// cheaply and the plugin's internal state (extism instance, cached
    /// tokens) is shared across pipeline invocations.
    plugin: Arc<dyn Plugin>,
}

impl ExtismProcessorAdapter {
    /// Construct an adapter wrapping the given plugin.
    ///
    /// The `name` is a free-form display string used in logs. Pass
    /// something like `format!("{}/config.process", plugin.name())`
    /// so log output clearly identifies which plugin handled which
    /// processing step.
    pub fn new(name: impl Into<String>, plugin: Arc<dyn Plugin>) -> Self {
        Self {
            name: name.into(),
            plugin,
        }
    }
}

impl ConfigProcessor for ExtismProcessorAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn process(&self, config: Value, request: &ConfigRequest) -> Result<Value, String> {
        // Build the wire envelope the plugin expects.
        let params = json!({
            "config": config,
            "request": request,
        });

        // Dispatch through the generic Plugin trait. `None` means the
        // plugin doesn't claim this method — treat it as "unchanged",
        // not an error, so the pipeline stays resilient to plugins
        // that only implement auth (or any other non-processor role).
        match self.plugin.handle_ipc(CONFIG_PROCESS_METHOD, &params) {
            None => Ok(config),
            Some(Err(err)) => Err(format!(
                "{}: plugin handle_ipc failed: {err}",
                self.name
            )),
            Some(Ok(raw)) => {
                let parsed: ConfigProcessResponse =
                    serde_json::from_value(raw).map_err(|e| {
                        format!("{}: malformed config.process response: {e}", self.name)
                    })?;
                match parsed {
                    ConfigProcessResponse::Transformed { config: new_config } => {
                        Ok(new_config)
                    }
                    ConfigProcessResponse::Unchanged => Ok(config),
                    ConfigProcessResponse::Error { message } => Err(format!(
                        "{}: plugin reported error: {message}",
                        self.name
                    )),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempt::AttemptInfo;
    use crate::strategy::{ConnectionStrategy, ResolverType, RetryPolicy, StackType};
    use domain::{Authenticator, VpnError};
    use std::sync::Mutex;
    use std::time::Duration;

    /// Test plugin that records every call and returns a scripted response.
    struct ScriptedPlugin {
        script: Mutex<Vec<Option<Result<Value, VpnError>>>>,
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl ScriptedPlugin {
        fn new(responses: Vec<Option<Result<Value, VpnError>>>) -> Self {
            Self {
                script: Mutex::new(responses),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<(String, Value)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Plugin for ScriptedPlugin {
        fn name(&self) -> &str {
            "scripted"
        }

        fn authenticator(&self) -> Option<&dyn Authenticator> {
            None
        }

        fn handle_ipc(
            &self,
            method: &str,
            params: &Value,
        ) -> Option<Result<Value, VpnError>> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_string(), params.clone()));
            // Pop the next scripted response. If the script is
            // exhausted, default to None (unclaimed).
            self.script.lock().unwrap().pop().unwrap_or(None)
        }
    }

    fn sample_request() -> ConfigRequest {
        ConfigRequest {
            with_host_dns: false,
            default_dns_server: None,
            attempt: AttemptInfo {
                strategy: ConnectionStrategy {
                    id: "test".into(),
                    stack: StackType::System,
                    resolver_type: ResolverType::Doh,
                    total_timeout: Duration::from_secs(10),
                    retry: RetryPolicy::NoRetry,
                },
                attempt_number: 1,
                previous_error: None,
            },
        }
    }

    #[test]
    fn unclaimed_method_is_passthrough() {
        // Plugin returns None (doesn't claim the method) → config
        // flows through unchanged.
        let plugin = Arc::new(ScriptedPlugin::new(vec![None]));
        let adapter = ExtismProcessorAdapter::new("test", plugin.clone());
        let input = json!({"log": {"level": "info"}});
        let output = adapter.process(input.clone(), &sample_request()).unwrap();
        assert_eq!(output, input);
        assert_eq!(plugin.calls().len(), 1);
        assert_eq!(plugin.calls()[0].0, CONFIG_PROCESS_METHOD);
    }

    #[test]
    fn transformed_response_replaces_config() {
        let new_config = json!({"log": {"level": "debug"}, "dns": {"servers": []}});
        let plugin = Arc::new(ScriptedPlugin::new(vec![Some(Ok(json!({
            "kind": "transformed",
            "config": new_config.clone()
        })))]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let input = json!({"log": {"level": "info"}});
        let output = adapter.process(input, &sample_request()).unwrap();
        assert_eq!(output, new_config);
    }

    #[test]
    fn unchanged_response_preserves_config() {
        let plugin = Arc::new(ScriptedPlugin::new(vec![Some(Ok(json!({
            "kind": "unchanged"
        })))]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let input = json!({"log": {"level": "info"}});
        let output = adapter.process(input.clone(), &sample_request()).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn error_response_propagates_to_pipeline() {
        let plugin = Arc::new(ScriptedPlugin::new(vec![Some(Ok(json!({
            "kind": "error",
            "message": "bad config shape"
        })))]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let input = json!({"log": {"level": "info"}});
        let err = adapter
            .process(input, &sample_request())
            .expect_err("plugin reported error");
        assert!(err.contains("bad config shape"));
    }

    #[test]
    fn plugin_error_propagates_to_pipeline() {
        let plugin = Arc::new(ScriptedPlugin::new(vec![Some(Err(VpnError::Unknown(
            "wasm trap".into(),
        )))]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let input = json!({});
        let err = adapter.process(input, &sample_request()).unwrap_err();
        assert!(err.contains("wasm trap"));
    }

    #[test]
    fn malformed_response_propagates_as_error() {
        // Plugin returns something that doesn't match the response enum.
        let plugin = Arc::new(ScriptedPlugin::new(vec![Some(Ok(json!({
            "kind": "garbage"
        })))]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let input = json!({});
        let err = adapter.process(input, &sample_request()).unwrap_err();
        assert!(err.contains("malformed"));
    }

    #[test]
    fn adapter_sends_method_name_and_envelope() {
        let plugin = Arc::new(ScriptedPlugin::new(vec![Some(Ok(json!({
            "kind": "unchanged"
        })))]));
        let adapter = ExtismProcessorAdapter::new("test", plugin.clone());
        let input = json!({"marker": 42});
        adapter.process(input.clone(), &sample_request()).unwrap();
        let calls = plugin.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, CONFIG_PROCESS_METHOD);
        assert_eq!(calls[0].1["config"], input);
        // request field is present and serializable
        assert!(calls[0].1["request"].is_object());
    }
}
