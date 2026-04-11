//! Extism-backed config processor adapter.
//!
//! Wraps an `Arc<dyn domain::Plugin>` so a loaded wasm plugin behaves
//! like a first-class [`ConfigProcessor`]. As of the slot-chain
//! migration, the adapter dispatches through the canonical
//! [`domain::slot_names::CONFIG_PROCESS`] slot — a middleware chain
//! of `before` → `exec` → `after` phases — and falls back to the
//! legacy flat method name [`CONFIG_PROCESS_METHOD`] for plugins that
//! haven't yet adopted the slot convention.
//!
//! ## New wire contract (slot chain)
//!
//! For each attempted config-process call, the host dispatches up to
//! three methods on the plugin:
//!
//! ```text
//! slot.config.process.before
//! slot.config.process.exec
//! slot.config.process.after
//! ```
//!
//! The envelope is a [`domain::SlotContext`] carrying a typed payload:
//!
//! ```json
//! {
//!   "slot": "config.process",
//!   "phase": "before" | "exec" | "after",
//!   "wire_version": 1,
//!   "invocation_id": "...",
//!   "payload": { "config": {...}, "request": {...} }
//! }
//! ```
//!
//! The plugin returns a [`domain::SlotOutcome`] tagged enum:
//!
//! ```json
//! {"kind": "unchanged"}
//! {"kind": "continue", "payload": {"config": {...}, "request": {...}}}
//! {"kind": "halt", "payload": {"config": {...}, "request": {...}}}
//! {"kind": "error", "message": "..."}
//! {"kind": "unhandled"}
//! ```
//!
//! The host folds phase outputs into subsequent phase inputs and uses
//! the final payload's `config` field as the transformed config.
//!
//! ## Legacy fallback contract
//!
//! If the plugin returns `Unhandled` (or `None` from handle_ipc) for
//! every phase in the chain, the adapter falls back to the pre-slot
//! method name:
//!
//! ```json
//! // method: "config.process"
//! // params: {"config": {...}, "request": {...}}
//! ```
//!
//! and expects the legacy [`LegacyConfigProcessResponse`] tagged enum
//! as the response. This path will be dropped once every plugin the
//! daemon talks to has migrated.
//!
//! ## Why this lives here, not in `plugin-extism`
//!
//! `core-config-processor` does NOT depend on `plugin-extism` — that
//! dependency would go the wrong way. Taking an `Arc<dyn
//! domain::Plugin>` keeps the adapter generic: any implementation of
//! the `Plugin` trait (wasm, native mock, remote subprocess) plugs in
//! the same way.

use crate::attempt::ConfigRequest;
use crate::pipeline::ConfigProcessor;
use domain::{run_slot_chain, slot_names, Plugin};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

/// Well-known legacy method name — the adapter tries the slot chain
/// first and only dispatches this if the plugin didn't claim any
/// phase of the [`slot_names::CONFIG_PROCESS`] slot. Kept as a
/// public const so pre-slot plugins can reference it without
/// hardcoding the string.
pub const CONFIG_PROCESS_METHOD: &str = "config.process";

/// Wire protocol version for the config.process slot payload. Bump
/// when the [`ConfigProcessPayload`] shape changes incompatibly.
pub const CONFIG_PROCESS_WIRE_VERSION: u32 = 1;

/// Payload type exchanged over the config.process slot chain. Carried
/// inside [`domain::SlotContext::payload`] and returned inside
/// [`domain::SlotOutcome::Continue`] / [`domain::SlotOutcome::Halt`].
///
/// Plugins receive this in each phase and can mutate `config` and/or
/// (rarely) read `request` to decide what to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigProcessPayload {
    /// The sing-box config JSON, as a free-form value. Plugins
    /// transform it by returning `SlotOutcome::Continue { payload }`
    /// with a new `config` field in the payload.
    pub config: Value,
    /// Contextual info about the current attempt — strategy,
    /// attempt number, previous error, DNS hints. Plugins use this
    /// to decide whether to intervene (e.g. swap DNS resolver on
    /// retry), but typically don't modify it.
    pub request: ConfigRequest,
}

/// Legacy plugin-side response for pre-slot `config.process` calls.
///
/// Kept for backwards compatibility — the adapter falls back to this
/// shape when the plugin's slot chain returns nothing. Plugins that
/// adopt the slot chain return [`domain::SlotOutcome`] instead; this
/// enum is deprecated and will be removed once all plugins migrate.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LegacyConfigProcessResponse {
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

    /// Legacy fallback: dispatch the flat `config.process` method
    /// name and parse the result with the pre-slot tagged enum.
    /// Called only when the slot chain returned `None` (plugin
    /// didn't claim any phase).
    fn process_via_legacy(
        &self,
        config: Value,
        request: &ConfigRequest,
    ) -> Result<Value, String> {
        let params = json!({
            "config": config,
            "request": request,
        });
        match self.plugin.handle_ipc(CONFIG_PROCESS_METHOD, &params) {
            None => {
                // Plugin doesn't claim the legacy method either —
                // config flows through unchanged. This is the
                // expected outcome for plugins that only implement
                // auth / deeplink and know nothing about config
                // processing.
                Ok(config)
            }
            Some(Err(err)) => Err(format!(
                "{}: plugin handle_ipc failed: {err}",
                self.name
            )),
            Some(Ok(raw)) => {
                let parsed: LegacyConfigProcessResponse = serde_json::from_value(raw)
                    .map_err(|e| {
                        format!("{}: malformed config.process response: {e}", self.name)
                    })?;
                match parsed {
                    LegacyConfigProcessResponse::Transformed { config: new_config } => {
                        Ok(new_config)
                    }
                    LegacyConfigProcessResponse::Unchanged => Ok(config),
                    LegacyConfigProcessResponse::Error { message } => Err(format!(
                        "{}: plugin reported error: {message}",
                        self.name
                    )),
                }
            }
        }
    }
}

impl ConfigProcessor for ExtismProcessorAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn process(&self, config: Value, request: &ConfigRequest) -> Result<Value, String> {
        // Prepare the typed payload for the slot chain.
        let payload = ConfigProcessPayload {
            config: config.clone(),
            request: request.clone(),
        };
        let invocation_id = domain::new_invocation_id();

        // First try the slot-chain convention. Walks
        // slot.config.process.{before, exec, after} through the
        // plugin's existing handle_ipc dispatcher.
        let chain_result = run_slot_chain(
            self.plugin.as_ref(),
            slot_names::CONFIG_PROCESS,
            CONFIG_PROCESS_WIRE_VERSION,
            &invocation_id,
            payload,
        );

        match chain_result {
            // Slot chain handled it — use the folded payload's config.
            Ok(Some(final_payload)) => Ok(final_payload.config),
            // Slot chain was not claimed by any phase. Fall back to
            // the legacy flat method name so pre-slot plugins still
            // work.
            Ok(None) => self.process_via_legacy(config, request),
            // Slot chain surfaced an error (plugin returned
            // SlotOutcome::Error, or a phase handle_ipc failed, or
            // an envelope serde error). Propagate upward.
            Err(err) => Err(format!("{}: {err}", self.name)),
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

    /// Test plugin that records every call and returns scripted
    /// responses from a FIFO queue, keyed by the incoming method
    /// name. Unknown methods return `None`.
    ///
    /// Keyed by method name (not just a flat vec) because the slot
    /// chain dispatches three phases plus the legacy fallback —
    /// we don't want tests to have to reason about dispatch order.
    struct ScriptedPlugin {
        responses: Mutex<std::collections::HashMap<String, Option<Result<Value, VpnError>>>>,
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl ScriptedPlugin {
        fn new(
            responses: Vec<(&'static str, Option<Result<Value, VpnError>>)>,
        ) -> Self {
            let mut map = std::collections::HashMap::new();
            for (k, v) in responses {
                map.insert(k.to_string(), v);
            }
            Self {
                responses: Mutex::new(map),
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
            self.responses
                .lock()
                .unwrap()
                .get(method)
                .cloned()
                .unwrap_or(None)
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

    /// Helper: construct a `SlotOutcome::Unchanged` value.
    fn unchanged() -> Value {
        json!({"kind": "unchanged"})
    }

    /// Helper: construct a `SlotOutcome::Continue` with a new config.
    fn cont(new_cfg: Value) -> Value {
        let req = sample_request();
        json!({
            "kind": "continue",
            "payload": {
                "config": new_cfg,
                "request": req,
            }
        })
    }

    /// Helper: construct a `SlotOutcome::Error`.
    fn error(msg: &str) -> Value {
        json!({"kind": "error", "message": msg})
    }

    // -----------------------------------------------------------------
    // Slot-chain path tests — the preferred contract going forward.
    // -----------------------------------------------------------------

    #[test]
    fn slot_chain_unclaimed_falls_through_to_legacy_passthrough() {
        // Plugin claims neither the slot chain nor the legacy method.
        // Expected: config flows through unchanged.
        let plugin = Arc::new(ScriptedPlugin::new(vec![]));
        let adapter = ExtismProcessorAdapter::new("test", plugin.clone());
        let input = json!({"log": {"level": "info"}});
        let output = adapter.process(input.clone(), &sample_request()).unwrap();
        assert_eq!(output, input);

        // Four dispatches: 3 slot phases + 1 legacy flat name.
        let calls = plugin.calls();
        let methods: Vec<&str> = calls.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(
            methods,
            vec![
                "slot.config.process.before",
                "slot.config.process.exec",
                "slot.config.process.after",
                "config.process",
            ]
        );
    }

    #[test]
    fn slot_chain_exec_continue_transforms_config() {
        let new_cfg = json!({"log": {"level": "debug"}});
        let plugin = Arc::new(ScriptedPlugin::new(vec![(
            "slot.config.process.exec",
            Some(Ok(cont(new_cfg.clone()))),
        )]));
        let adapter = ExtismProcessorAdapter::new("test", plugin.clone());
        let input = json!({"log": {"level": "info"}});
        let output = adapter.process(input, &sample_request()).unwrap();
        assert_eq!(output, new_cfg);
        // Legacy fallback must NOT have fired — chain handled it.
        let methods: Vec<_> = plugin.calls().into_iter().map(|(m, _)| m).collect();
        assert!(!methods.iter().any(|m| m == "config.process"));
    }

    #[test]
    fn slot_chain_before_observes_without_transforming() {
        // before returns `unchanged`, exec + after untouched.
        let plugin = Arc::new(ScriptedPlugin::new(vec![(
            "slot.config.process.before",
            Some(Ok(unchanged())),
        )]));
        let adapter = ExtismProcessorAdapter::new("test", plugin.clone());
        let input = json!({"marker": 42});
        let output = adapter.process(input.clone(), &sample_request()).unwrap();
        // Payload unchanged through the chain — but slot chain DID
        // claim a phase, so the legacy fallback must NOT fire.
        assert_eq!(output, input);
        let methods: Vec<_> = plugin.calls().into_iter().map(|(m, _)| m).collect();
        assert!(!methods.iter().any(|m| m == "config.process"));
    }

    #[test]
    fn slot_chain_exec_error_propagates() {
        let plugin = Arc::new(ScriptedPlugin::new(vec![(
            "slot.config.process.exec",
            Some(Ok(error("bad config shape"))),
        )]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let err = adapter
            .process(json!({}), &sample_request())
            .expect_err("plugin reported error");
        assert!(err.contains("bad config shape"));
    }

    #[test]
    fn slot_chain_plugin_trap_propagates() {
        // First slot phase returns a handle_ipc error — the chain
        // aborts immediately.
        let plugin = Arc::new(ScriptedPlugin::new(vec![(
            "slot.config.process.before",
            Some(Err(VpnError::Unknown("wasm trap".into()))),
        )]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let err = adapter.process(json!({}), &sample_request()).unwrap_err();
        assert!(err.contains("wasm trap"));
    }

    #[test]
    fn slot_chain_envelope_carries_typed_payload() {
        // Capture the actual envelope and verify its shape.
        let plugin = Arc::new(ScriptedPlugin::new(vec![(
            "slot.config.process.exec",
            Some(Ok(unchanged())),
        )]));
        let adapter = ExtismProcessorAdapter::new("test", plugin.clone());
        let input = json!({"marker": 42});
        adapter.process(input.clone(), &sample_request()).unwrap();

        // Find the slot.config.process.exec call and confirm the
        // envelope has the expected top-level fields.
        let call = plugin
            .calls()
            .into_iter()
            .find(|(m, _)| m == "slot.config.process.exec")
            .expect("exec phase was dispatched");
        let env = call.1;
        assert_eq!(env["slot"], "config.process");
        assert_eq!(env["phase"], "exec");
        assert_eq!(env["wire_version"], CONFIG_PROCESS_WIRE_VERSION);
        assert!(env["invocation_id"].is_string());
        assert_eq!(env["payload"]["config"], input);
        assert!(env["payload"]["request"].is_object());
    }

    // -----------------------------------------------------------------
    // Legacy fallback path tests — dropped once all plugins migrate.
    // -----------------------------------------------------------------

    #[test]
    fn legacy_transformed_response_replaces_config() {
        let new_config = json!({"log": {"level": "debug"}, "dns": {"servers": []}});
        let plugin = Arc::new(ScriptedPlugin::new(vec![(
            "config.process",
            Some(Ok(json!({
                "kind": "transformed",
                "config": new_config.clone()
            }))),
        )]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let input = json!({"log": {"level": "info"}});
        let output = adapter.process(input, &sample_request()).unwrap();
        assert_eq!(output, new_config);
    }

    #[test]
    fn legacy_unchanged_response_preserves_config() {
        let plugin = Arc::new(ScriptedPlugin::new(vec![(
            "config.process",
            Some(Ok(json!({"kind": "unchanged"}))),
        )]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let input = json!({"log": {"level": "info"}});
        let output = adapter.process(input.clone(), &sample_request()).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn legacy_error_response_propagates_to_pipeline() {
        let plugin = Arc::new(ScriptedPlugin::new(vec![(
            "config.process",
            Some(Ok(json!({
                "kind": "error",
                "message": "bad config shape"
            }))),
        )]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let err = adapter
            .process(json!({}), &sample_request())
            .expect_err("plugin reported error");
        assert!(err.contains("bad config shape"));
    }

    #[test]
    fn legacy_plugin_error_propagates_to_pipeline() {
        let plugin = Arc::new(ScriptedPlugin::new(vec![(
            "config.process",
            Some(Err(VpnError::Unknown("wasm trap".into()))),
        )]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let err = adapter.process(json!({}), &sample_request()).unwrap_err();
        assert!(err.contains("wasm trap"));
    }

    #[test]
    fn legacy_malformed_response_propagates_as_error() {
        let plugin = Arc::new(ScriptedPlugin::new(vec![(
            "config.process",
            Some(Ok(json!({"kind": "garbage"}))),
        )]));
        let adapter = ExtismProcessorAdapter::new("test", plugin);
        let err = adapter.process(json!({}), &sample_request()).unwrap_err();
        assert!(err.contains("malformed"));
    }
}
