use std::collections::BTreeMap;
use std::sync::Arc;

use pingling_host_contract::{
    HostFailure, HostResult, MethodBinding, PluginManifest, PluginRegistry, Slot, SlotContext,
    SlotOutcome, SlotPhase, SlotPolicy,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

pub const REQUIRED_HANDLE_IPC_EXPORT: &str = "plugin_handle_ipc";
pub const OPTIONAL_MANIFEST_EXPORT: &str = "plugin_manifest";

#[derive(Debug, Serialize)]
pub struct HandleIpcInput<'a> {
    pub method: &'a str,
    pub params: &'a Value,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct HandleIpcOutput {
    pub handled: bool,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn handle_ipc(&self, method: &str, params: &Value) -> Option<HostResult<Value>>;
}

pub type SlotChainResult<P> = HostResult<Option<P>>;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SlotEvent {
    Enter,
    Skipped,
    Unhandled,
    Unchanged,
    Continue,
    Halt,
    Error,
    SuppressedError,
}

impl SlotEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Skipped => "skipped",
            Self::Unhandled => "unhandled",
            Self::Unchanged => "unchanged",
            Self::Continue => "continue",
            Self::Halt => "halt",
            Self::Error => "error",
            Self::SuppressedError => "suppressed_error",
        }
    }
}

pub struct SlotObservation<'a> {
    pub plugin_id: &'a str,
    pub slot: &'a Slot,
    pub phase: SlotPhase,
    pub policy: SlotPolicy,
    pub wire_version: u32,
    pub invocation_id: &'a str,
    pub event: SlotEvent,
    pub payload_json: &'a Value,
    pub error_message: Option<&'a str>,
}

pub trait SlotObserver: Send + Sync {
    fn observe(&self, observation: SlotObservation<'_>);
}

#[derive(Debug, Default)]
pub struct NullSlotObserver;

impl SlotObserver for NullSlotObserver {
    fn observe(&self, _observation: SlotObservation<'_>) {}
}

#[derive(Clone)]
pub struct LoadedPlugin {
    manifest: PluginManifest,
    plugin: Arc<dyn Plugin>,
}

impl LoadedPlugin {
    pub fn new(manifest: PluginManifest, plugin: Arc<dyn Plugin>) -> Self {
        Self { manifest, plugin }
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn plugin(&self) -> &Arc<dyn Plugin> {
        &self.plugin
    }
}

#[derive(Default)]
pub struct LoadedPluginRegistry {
    registry: PluginRegistry,
    plugins: BTreeMap<String, Arc<dyn Plugin>>,
}

impl LoadedPluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        manifest: PluginManifest,
        plugin: Arc<dyn Plugin>,
    ) -> HostResult<()> {
        let id = manifest.id.clone();
        self.registry.register(manifest)?;
        self.plugins.insert(id, plugin);
        Ok(())
    }

    pub fn registry(&self) -> &PluginRegistry {
        &self.registry
    }

    pub fn get(&self, plugin_id: &str) -> Option<&Arc<dyn Plugin>> {
        self.plugins.get(plugin_id)
    }

    pub fn manifests(&self) -> impl Iterator<Item = &PluginManifest> {
        self.registry.manifests()
    }

    pub fn dispatch_method(&self, method: &str, params: &Value) -> Option<HostResult<Value>> {
        self.dispatch_method_bindings(self.registry.method_bindings(method), method, params)
    }

    fn dispatch_method_bindings(
        &self,
        bindings: Vec<MethodBinding>,
        method: &str,
        params: &Value,
    ) -> Option<HostResult<Value>> {
        for binding in bindings {
            let Some(plugin) = self.plugins.get(&binding.plugin_id) else {
                return Some(Err(HostFailure::plugin_error(format!(
                    "plugin {} is indexed but not loaded",
                    binding.plugin_id
                ))));
            };
            if let Some(result) = plugin.handle_ipc(method, params) {
                return Some(result);
            }
        }
        None
    }

    pub fn run_slot_chain<P>(
        &self,
        slot: &Slot,
        wire_version: u32,
        invocation_id: &str,
        initial_payload: P,
    ) -> SlotChainResult<P>
    where
        P: Serialize + DeserializeOwned + Clone,
    {
        self.run_slot_chain_observed(
            slot,
            wire_version,
            invocation_id,
            initial_payload,
            &NullSlotObserver,
        )
    }

    pub fn run_slot_chain_observed<P>(
        &self,
        slot: &Slot,
        wire_version: u32,
        invocation_id: &str,
        initial_payload: P,
        observer: &dyn SlotObserver,
    ) -> SlotChainResult<P>
    where
        P: Serialize + DeserializeOwned + Clone,
    {
        let mut payload = initial_payload;
        let mut any_handled = false;
        for phase in SlotPhase::ORDER {
            match self.run_slot_phase_observed(
                slot,
                phase,
                wire_version,
                invocation_id,
                payload,
                observer,
            )? {
                PhaseResult::Unhandled(next) => {
                    payload = next;
                }
                PhaseResult::Handled(next) => {
                    any_handled = true;
                    payload = next;
                }
                PhaseResult::Halted(final_payload) => {
                    return Ok(Some(final_payload));
                }
            }
        }
        Ok(any_handled.then_some(payload))
    }

    pub fn run_slot_phase<P>(
        &self,
        slot: &Slot,
        phase: SlotPhase,
        wire_version: u32,
        invocation_id: &str,
        payload: P,
    ) -> SlotChainResult<P>
    where
        P: Serialize + DeserializeOwned + Clone,
    {
        match self.run_slot_phase_observed(
            slot,
            phase,
            wire_version,
            invocation_id,
            payload,
            &NullSlotObserver,
        )? {
            PhaseResult::Unhandled(_) => Ok(None),
            PhaseResult::Handled(payload) | PhaseResult::Halted(payload) => Ok(Some(payload)),
        }
    }

    pub fn run_slot_phase_observed<P>(
        &self,
        slot: &Slot,
        phase: SlotPhase,
        wire_version: u32,
        invocation_id: &str,
        mut payload: P,
        observer: &dyn SlotObserver,
    ) -> HostResult<PhaseResult<P>>
    where
        P: Serialize + DeserializeOwned + Clone,
    {
        let mut any_handled = false;
        for binding in self.registry.bindings_for(slot, phase) {
            let Some(plugin) = self.plugins.get(&binding.plugin_id) else {
                return Err(HostFailure::plugin_error(format!(
                    "plugin {} is indexed but not loaded",
                    binding.plugin_id
                )));
            };
            match run_plugin_phase(
                binding.plugin_id.as_str(),
                plugin.as_ref(),
                slot,
                phase,
                binding.policy,
                wire_version,
                invocation_id,
                payload,
                observer,
            )? {
                PluginPhaseResult::Unhandled(next) => {
                    payload = next;
                }
                PluginPhaseResult::Observed(next) => {
                    any_handled = true;
                    payload = next;
                }
                PluginPhaseResult::Handled(next) => {
                    any_handled = true;
                    payload = next;
                    if binding.policy == SlotPolicy::FirstSuccess {
                        break;
                    }
                }
                PluginPhaseResult::Halted(final_payload) => {
                    return Ok(PhaseResult::Halted(final_payload));
                }
            }
        }
        if any_handled {
            Ok(PhaseResult::Handled(payload))
        } else {
            Ok(PhaseResult::Unhandled(payload))
        }
    }
}

pub enum PhaseResult<P> {
    Unhandled(P),
    Handled(P),
    Halted(P),
}

enum PluginPhaseResult<P> {
    Unhandled(P),
    Observed(P),
    Handled(P),
    Halted(P),
}

#[allow(clippy::too_many_arguments)]
pub fn run_single_plugin_slot_chain_observed<P>(
    plugin_id: &str,
    plugin: &dyn Plugin,
    slot: &Slot,
    wire_version: u32,
    invocation_id: &str,
    initial_payload: P,
    observer: &dyn SlotObserver,
) -> SlotChainResult<P>
where
    P: Serialize + DeserializeOwned + Clone,
{
    let mut payload = initial_payload;
    let mut any_handled = false;
    for phase in SlotPhase::ORDER {
        match run_plugin_phase(
            plugin_id,
            plugin,
            slot,
            phase,
            SlotPolicy::Pipeline,
            wire_version,
            invocation_id,
            payload,
            observer,
        )? {
            PluginPhaseResult::Unhandled(next) => {
                payload = next;
            }
            PluginPhaseResult::Observed(next) => {
                any_handled = true;
                payload = next;
            }
            PluginPhaseResult::Handled(next) => {
                any_handled = true;
                payload = next;
            }
            PluginPhaseResult::Halted(final_payload) => return Ok(Some(final_payload)),
        }
    }
    Ok(any_handled.then_some(payload))
}

pub fn run_single_plugin_slot_chain<P>(
    plugin_id: &str,
    plugin: &dyn Plugin,
    slot: &Slot,
    wire_version: u32,
    invocation_id: &str,
    initial_payload: P,
) -> SlotChainResult<P>
where
    P: Serialize + DeserializeOwned + Clone,
{
    run_single_plugin_slot_chain_observed(
        plugin_id,
        plugin,
        slot,
        wire_version,
        invocation_id,
        initial_payload,
        &NullSlotObserver,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_single_plugin_slot_phase_observed<P>(
    plugin_id: &str,
    plugin: &dyn Plugin,
    slot: &Slot,
    phase: SlotPhase,
    wire_version: u32,
    invocation_id: &str,
    payload: P,
    observer: &dyn SlotObserver,
) -> SlotChainResult<P>
where
    P: Serialize + DeserializeOwned + Clone,
{
    match run_plugin_phase(
        plugin_id,
        plugin,
        slot,
        phase,
        SlotPolicy::Pipeline,
        wire_version,
        invocation_id,
        payload,
        observer,
    )? {
        PluginPhaseResult::Unhandled(_) => Ok(None),
        PluginPhaseResult::Observed(payload) => Ok(Some(payload)),
        PluginPhaseResult::Handled(payload) | PluginPhaseResult::Halted(payload) => {
            Ok(Some(payload))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_single_plugin_slot_phase<P>(
    plugin_id: &str,
    plugin: &dyn Plugin,
    slot: &Slot,
    phase: SlotPhase,
    wire_version: u32,
    invocation_id: &str,
    payload: P,
) -> SlotChainResult<P>
where
    P: Serialize + DeserializeOwned + Clone,
{
    run_single_plugin_slot_phase_observed(
        plugin_id,
        plugin,
        slot,
        phase,
        wire_version,
        invocation_id,
        payload,
        &NullSlotObserver,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_plugin_phase<P>(
    plugin_id: &str,
    plugin: &dyn Plugin,
    slot: &Slot,
    phase: SlotPhase,
    policy: SlotPolicy,
    wire_version: u32,
    invocation_id: &str,
    payload: P,
    observer: &dyn SlotObserver,
) -> HostResult<PluginPhaseResult<P>>
where
    P: Serialize + DeserializeOwned + Clone,
{
    let method = format!("slot.{slot}.{phase}");
    let ctx = SlotContext::new(slot.clone(), phase, invocation_id, payload.clone());
    let ctx = SlotContext {
        wire_version,
        ..ctx
    };
    let ctx_value = serde_json::to_value(&ctx).map_err(|error| {
        HostFailure::invalid_input(format!("slot {slot}.{phase}: serialize context: {error}"))
    })?;
    observe(
        observer,
        plugin_id,
        slot,
        phase,
        policy,
        wire_version,
        invocation_id,
        SlotEvent::Enter,
        &ctx_value["payload"],
        None,
    );

    match plugin.handle_ipc(&method, &ctx_value) {
        None => {
            observe(
                observer,
                plugin_id,
                slot,
                phase,
                policy,
                wire_version,
                invocation_id,
                SlotEvent::Skipped,
                &ctx_value["payload"],
                None,
            );
            Ok(PluginPhaseResult::Unhandled(payload))
        }
        Some(Err(error)) => {
            let message = error.to_string();
            let event = if policy == SlotPolicy::BestEffort {
                SlotEvent::SuppressedError
            } else {
                SlotEvent::Error
            };
            observe(
                observer,
                plugin_id,
                slot,
                phase,
                policy,
                wire_version,
                invocation_id,
                event,
                &ctx_value["payload"],
                Some(&message),
            );
            if policy == SlotPolicy::BestEffort {
                Ok(PluginPhaseResult::Unhandled(payload))
            } else {
                Err(error)
            }
        }
        Some(Ok(raw)) => {
            let outcome: SlotOutcome<P> = match serde_json::from_value(raw) {
                Ok(outcome) => outcome,
                Err(error) if policy == SlotPolicy::BestEffort => {
                    let message = format!("slot {slot}.{phase}: parse outcome: {error}");
                    observe(
                        observer,
                        plugin_id,
                        slot,
                        phase,
                        policy,
                        wire_version,
                        invocation_id,
                        SlotEvent::SuppressedError,
                        &ctx_value["payload"],
                        Some(&message),
                    );
                    return Ok(PluginPhaseResult::Unhandled(payload));
                }
                Err(error) => {
                    return Err(HostFailure::plugin_error(format!(
                        "slot {slot}.{phase}: parse outcome: {error}"
                    )));
                }
            };
            apply_outcome(
                plugin_id,
                slot,
                phase,
                policy,
                wire_version,
                invocation_id,
                payload,
                outcome,
                observer,
                &ctx_value["payload"],
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_outcome<P>(
    plugin_id: &str,
    slot: &Slot,
    phase: SlotPhase,
    policy: SlotPolicy,
    wire_version: u32,
    invocation_id: &str,
    payload: P,
    outcome: SlotOutcome<P>,
    observer: &dyn SlotObserver,
    original_payload_json: &Value,
) -> HostResult<PluginPhaseResult<P>>
where
    P: Serialize + DeserializeOwned + Clone,
{
    match outcome {
        SlotOutcome::Unhandled => {
            observe(
                observer,
                plugin_id,
                slot,
                phase,
                policy,
                wire_version,
                invocation_id,
                SlotEvent::Unhandled,
                original_payload_json,
                None,
            );
            Ok(PluginPhaseResult::Unhandled(payload))
        }
        SlotOutcome::Unchanged => {
            observe(
                observer,
                plugin_id,
                slot,
                phase,
                policy,
                wire_version,
                invocation_id,
                SlotEvent::Unchanged,
                original_payload_json,
                None,
            );
            Ok(PluginPhaseResult::Observed(payload))
        }
        SlotOutcome::Continue {
            payload: new_payload,
        } => {
            let json_payload = serde_json::to_value(&new_payload).unwrap_or(Value::Null);
            observe(
                observer,
                plugin_id,
                slot,
                phase,
                policy,
                wire_version,
                invocation_id,
                SlotEvent::Continue,
                &json_payload,
                None,
            );
            if matches!(policy, SlotPolicy::Broadcast | SlotPolicy::BestEffort) {
                Ok(PluginPhaseResult::Observed(payload))
            } else {
                Ok(PluginPhaseResult::Handled(new_payload))
            }
        }
        SlotOutcome::Halt {
            payload: final_payload,
        } => {
            let json_payload = serde_json::to_value(&final_payload).unwrap_or(Value::Null);
            observe(
                observer,
                plugin_id,
                slot,
                phase,
                policy,
                wire_version,
                invocation_id,
                SlotEvent::Halt,
                &json_payload,
                None,
            );
            if matches!(policy, SlotPolicy::Broadcast | SlotPolicy::BestEffort) {
                Ok(PluginPhaseResult::Observed(payload))
            } else {
                Ok(PluginPhaseResult::Halted(final_payload))
            }
        }
        SlotOutcome::Error { message } => {
            let event = if policy == SlotPolicy::BestEffort {
                SlotEvent::SuppressedError
            } else {
                SlotEvent::Error
            };
            observe(
                observer,
                plugin_id,
                slot,
                phase,
                policy,
                wire_version,
                invocation_id,
                event,
                original_payload_json,
                Some(&message),
            );
            if policy == SlotPolicy::BestEffort {
                Ok(PluginPhaseResult::Unhandled(payload))
            } else {
                Err(HostFailure::plugin_error(format!(
                    "slot {slot}.{phase}: {message}"
                )))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn observe(
    observer: &dyn SlotObserver,
    plugin_id: &str,
    slot: &Slot,
    phase: SlotPhase,
    policy: SlotPolicy,
    wire_version: u32,
    invocation_id: &str,
    event: SlotEvent,
    payload_json: &Value,
    error_message: Option<&str>,
) {
    observer.observe(SlotObservation {
        plugin_id,
        slot,
        phase,
        policy,
        wire_version,
        invocation_id,
        event,
        payload_json,
        error_message,
    });
}

pub fn new_invocation_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    format!("{:x}-{:x}", seed.wrapping_add(n), n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pingling_host_contract::{PluginManifest, SlotBinding};
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::sync::Mutex;

    struct RecordingPlugin {
        name: &'static str,
        responses: Mutex<Vec<Option<HostResult<Value>>>>,
        calls: Mutex<Vec<String>>,
    }

    impl RecordingPlugin {
        fn new(name: &'static str, responses: Vec<Option<HostResult<Value>>>) -> Self {
            Self {
                name,
                responses: Mutex::new(responses),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Plugin for RecordingPlugin {
        fn name(&self) -> &str {
            self.name
        }

        fn handle_ipc(&self, method: &str, _params: &Value) -> Option<HostResult<Value>> {
            self.calls.lock().unwrap().push(method.to_owned());
            self.responses.lock().unwrap().remove(0)
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct Pay {
        n: u32,
    }

    fn manifest(id: &str, priority: i32, policy: SlotPolicy) -> PluginManifest {
        PluginManifest {
            id: id.to_owned(),
            priority,
            methods: Vec::new(),
            slots: vec![SlotBinding::new(
                Slot::new(Slot::CONFIG_PROCESS).unwrap(),
                vec![SlotPhase::Exec],
                policy,
            )
            .unwrap()],
            needs: Vec::new(),
            allowed_hosts: Vec::new(),
        }
    }

    fn outcome_continue(n: u32) -> Value {
        json!({"kind": "continue", "payload": {"n": n}})
    }

    #[test]
    fn single_plugin_slot_chain_matches_legacy_shape() {
        let plugin = RecordingPlugin::new(
            "single",
            vec![
                None,
                Some(Ok(outcome_continue(10))),
                Some(Ok(json!({"kind": "unchanged"}))),
            ],
        );
        let slot = Slot::new("test.slot").unwrap();

        let result =
            run_single_plugin_slot_chain("single", &plugin, &slot, 1, "inv", Pay { n: 1 }).unwrap();

        assert_eq!(result, Some(Pay { n: 10 }));
        assert_eq!(
            plugin.calls(),
            vec![
                "slot.test.slot.before",
                "slot.test.slot.exec",
                "slot.test.slot.after"
            ]
        );
    }

    #[test]
    fn single_plugin_slot_phase_calls_only_requested_phase() {
        let plugin = RecordingPlugin::new("single", vec![Some(Ok(outcome_continue(10)))]);
        let slot = Slot::new("test.slot").unwrap();

        let result = run_single_plugin_slot_phase(
            "single",
            &plugin,
            &slot,
            SlotPhase::After,
            1,
            "inv",
            Pay { n: 1 },
        )
        .unwrap();

        assert_eq!(result, Some(Pay { n: 10 }));
        assert_eq!(plugin.calls(), vec!["slot.test.slot.after"]);
    }

    #[test]
    fn registry_pipeline_orders_plugins_by_priority() {
        let first = Arc::new(RecordingPlugin::new(
            "first",
            vec![Some(Ok(outcome_continue(2)))],
        ));
        let second = Arc::new(RecordingPlugin::new(
            "second",
            vec![Some(Ok(outcome_continue(3)))],
        ));
        let mut registry = LoadedPluginRegistry::new();
        registry
            .register(
                manifest("second", 200, SlotPolicy::Pipeline),
                second.clone(),
            )
            .unwrap();
        registry
            .register(manifest("first", 100, SlotPolicy::Pipeline), first.clone())
            .unwrap();

        let out = registry
            .run_slot_phase(
                &Slot::new(Slot::CONFIG_PROCESS).unwrap(),
                SlotPhase::Exec,
                1,
                "inv",
                Pay { n: 1 },
            )
            .unwrap();

        assert_eq!(out, Some(Pay { n: 3 }));
        assert_eq!(first.calls(), vec!["slot.config.process.exec"]);
        assert_eq!(second.calls(), vec!["slot.config.process.exec"]);
    }

    #[test]
    fn first_success_stops_after_concrete_result() {
        let first = Arc::new(RecordingPlugin::new(
            "first",
            vec![Some(Ok(outcome_continue(2)))],
        ));
        let second = Arc::new(RecordingPlugin::new(
            "second",
            vec![Some(Ok(outcome_continue(3)))],
        ));
        let mut registry = LoadedPluginRegistry::new();
        registry
            .register(
                manifest("first", 100, SlotPolicy::FirstSuccess),
                first.clone(),
            )
            .unwrap();
        registry
            .register(
                manifest("second", 200, SlotPolicy::FirstSuccess),
                second.clone(),
            )
            .unwrap();

        let out = registry
            .run_slot_phase(
                &Slot::new(Slot::CONFIG_PROCESS).unwrap(),
                SlotPhase::Exec,
                1,
                "inv",
                Pay { n: 1 },
            )
            .unwrap();

        assert_eq!(out, Some(Pay { n: 2 }));
        assert_eq!(first.calls().len(), 1);
        assert_eq!(second.calls().len(), 0);
    }

    #[test]
    fn first_success_continues_past_unchanged_observers() {
        let first = Arc::new(RecordingPlugin::new(
            "observer",
            vec![Some(Ok(json!({"kind": "unchanged"})))],
        ));
        let second = Arc::new(RecordingPlugin::new(
            "resolver",
            vec![Some(Ok(outcome_continue(3)))],
        ));
        let mut registry = LoadedPluginRegistry::new();
        registry
            .register(
                manifest("observer", 100, SlotPolicy::FirstSuccess),
                first.clone(),
            )
            .unwrap();
        registry
            .register(
                manifest("resolver", 200, SlotPolicy::FirstSuccess),
                second.clone(),
            )
            .unwrap();

        let out = registry
            .run_slot_phase(
                &Slot::new(Slot::CONFIG_PROCESS).unwrap(),
                SlotPhase::Exec,
                1,
                "inv",
                Pay { n: 1 },
            )
            .unwrap();

        assert_eq!(out, Some(Pay { n: 3 }));
        assert_eq!(first.calls().len(), 1);
        assert_eq!(second.calls().len(), 1);
    }

    #[test]
    fn best_effort_suppresses_errors() {
        let plugin = Arc::new(RecordingPlugin::new(
            "debug",
            vec![Some(Err(HostFailure::plugin_error("boom")))],
        ));
        let mut registry = LoadedPluginRegistry::new();
        registry
            .register(manifest("debug", 100, SlotPolicy::BestEffort), plugin)
            .unwrap();

        let out = registry
            .run_slot_phase(
                &Slot::new(Slot::CONFIG_PROCESS).unwrap(),
                SlotPhase::Exec,
                1,
                "inv",
                Pay { n: 1 },
            )
            .unwrap();

        assert_eq!(out, None);
    }

    #[test]
    fn method_dispatch_uses_manifest_order() {
        let plugin = Arc::new(RecordingPlugin::new(
            "auth",
            vec![Some(Ok(json!({"ok": true})))],
        ));
        let mut manifest = PluginManifest::new("auth").unwrap();
        manifest.methods = vec!["auth.*".to_owned()];
        let mut registry = LoadedPluginRegistry::new();
        registry.register(manifest, plugin).unwrap();

        let out = registry
            .dispatch_method("auth.login", &json!({"token": "t"}))
            .unwrap()
            .unwrap();

        assert_eq!(out, json!({"ok": true}));
    }
}
