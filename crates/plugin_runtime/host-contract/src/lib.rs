use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

mod contract;
pub mod payloads;

pub use contract::{
    render_wit_world, CompatibilityRange, ComponentDescriptor, ComponentFunctionDescriptor,
    ComponentInterfaceDescriptor, ComponentRecordDescriptor, ContractInventorySummary,
    EventDescriptor, HostCapabilityDescriptor, HostCapabilityKind, IpcPackageDescriptor,
    MergedContractRegistry, MethodDescriptor, MethodErrorDescriptor, OwnedEventDescriptor,
    OwnedMethodDescriptor, WitFieldDescriptor, WitResultDescriptor,
};

pub const HOST_PROTOCOL_VERSION: &str = "pingling.host.v1";
pub const DEFAULT_WIRE_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Slot(String);

impl Slot {
    pub const CONFIG_PROCESS: &'static str = "config.process";
    pub const DEEPLINK_RESOLVE: &'static str = "deeplink.resolve";
    pub const AUTH_SESSION: &'static str = "auth.session";
    pub const VPN_CONNECT: &'static str = "vpn.connect";
    pub const VPN_DISCONNECT: &'static str = "vpn.disconnect";
    pub const PLUGIN_LOAD: &'static str = "plugin.load";
    pub const CORE_START: &'static str = "core.start";
    pub const CORE_STOP: &'static str = "core.stop";
    pub const PROFILE_ACTIVATE: &'static str = "profile.activate";
    pub const PROFILE_PERSIST: &'static str = "profile.persist";
    pub const DAEMON_STARTUP: &'static str = "daemon.startup";
    pub const DAEMON_SHUTDOWN: &'static str = "daemon.shutdown";
    pub const OUTBOUND_SELECT: &'static str = "outbound.select";
    pub const OUTBOUND_TEST_LATENCY: &'static str = "outbound.test_latency";
    pub const NETWATCH_EVENT: &'static str = "netwatch.event";
    pub const LOG_EMIT: &'static str = "log.emit";
    pub const UPDATE_CHECK: &'static str = "update.check";
    pub const CONFIG_VALIDATE: &'static str = "config.validate";

    pub const CONFIG_TRANSFORM: &'static str = "config.transform";
    pub const RUNTIME_COMMAND: &'static str = "runtime.command";
    pub const RUNTIME_OBSERVE: &'static str = "runtime.observe";
    pub const STORAGE_LOOKUP: &'static str = "storage.lookup";
    pub const TELEMETRY_OBSERVE: &'static str = "telemetry.observe";

    pub const WELL_KNOWN: &[&str] = &[
        Self::CONFIG_PROCESS,
        Self::DEEPLINK_RESOLVE,
        Self::AUTH_SESSION,
        Self::VPN_CONNECT,
        Self::VPN_DISCONNECT,
        Self::PLUGIN_LOAD,
        Self::CORE_START,
        Self::CORE_STOP,
        Self::PROFILE_ACTIVATE,
        Self::PROFILE_PERSIST,
        Self::DAEMON_STARTUP,
        Self::DAEMON_SHUTDOWN,
        Self::OUTBOUND_SELECT,
        Self::OUTBOUND_TEST_LATENCY,
        Self::NETWATCH_EVENT,
        Self::LOG_EMIT,
        Self::UPDATE_CHECK,
        Self::CONFIG_VALIDATE,
        Self::CONFIG_TRANSFORM,
        Self::RUNTIME_COMMAND,
        Self::RUNTIME_OBSERVE,
        Self::STORAGE_LOOKUP,
        Self::TELEMETRY_OBSERVE,
    ];

    pub fn new(value: impl Into<String>) -> HostResult<Self> {
        let value = value.into();
        validate_token("slot", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for Slot {
    type Error = HostFailure;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotPhase {
    Before,
    Exec,
    After,
}

impl SlotPhase {
    pub const ORDER: [Self; 3] = [Self::Before, Self::Exec, Self::After];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::Exec => "exec",
            Self::After => "after",
        }
    }
}

impl fmt::Display for SlotPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotPolicy {
    Pipeline,
    FirstSuccess,
    SingleOwner,
    Broadcast,
    BestEffort,
}

impl Default for SlotPolicy {
    fn default() -> Self {
        Self::Pipeline
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SlotContext<P = serde_json::Value> {
    pub slot: Slot,
    pub phase: SlotPhase,
    pub wire_version: u32,
    pub invocation_id: String,
    #[serde(default)]
    pub payload: P,
}

impl<P> SlotContext<P> {
    pub fn new(slot: Slot, phase: SlotPhase, invocation_id: impl Into<String>, payload: P) -> Self {
        Self {
            slot,
            phase,
            wire_version: DEFAULT_WIRE_VERSION,
            invocation_id: invocation_id.into(),
            payload,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SlotOutcome<P = serde_json::Value> {
    Unchanged,
    Continue { payload: P },
    Halt { payload: P },
    Error { message: String },
    Unhandled,
}

impl<P> SlotOutcome<P> {
    pub fn map_payload<Q>(self, f: impl FnOnce(P) -> Q) -> SlotOutcome<Q> {
        match self {
            Self::Unchanged => SlotOutcome::Unchanged,
            Self::Continue { payload } => SlotOutcome::Continue {
                payload: f(payload),
            },
            Self::Halt { payload } => SlotOutcome::Halt {
                payload: f(payload),
            },
            Self::Error { message } => SlotOutcome::Error { message },
            Self::Unhandled => SlotOutcome::Unhandled,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SlotBinding {
    pub name: Slot,
    #[serde(default = "default_phases")]
    pub phases: Vec<SlotPhase>,
    #[serde(default)]
    pub policy: SlotPolicy,
}

impl SlotBinding {
    pub fn new(name: Slot, phases: Vec<SlotPhase>, policy: SlotPolicy) -> HostResult<Self> {
        if phases.is_empty() {
            return Err(HostFailure::invalid_input(
                "slot binding must include at least one phase",
            ));
        }
        Ok(Self {
            name,
            phases,
            policy,
        })
    }

    fn contains_phase(&self, phase: SlotPhase) -> bool {
        self.phases.contains(&phase)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub slots: Vec<SlotBinding>,
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<IpcPackageDescriptor>,
}

impl PluginManifest {
    pub fn new(id: impl Into<String>) -> HostResult<Self> {
        let id = id.into();
        validate_token("plugin id", &id)?;
        Ok(Self {
            id,
            priority: 0,
            methods: Vec::new(),
            slots: Vec::new(),
            needs: Vec::new(),
            allowed_hosts: Vec::new(),
            package: None,
        })
    }

    pub fn validate(&self) -> HostResult<()> {
        validate_token("plugin id", &self.id)?;
        for method in &self.methods {
            validate_method_pattern(method)?;
        }
        for need in &self.needs {
            validate_token("capability", need)?;
        }
        for host in &self.allowed_hosts {
            validate_host_pattern(host)?;
        }
        for slot in &self.slots {
            if slot.phases.is_empty() {
                return Err(HostFailure::invalid_input(format!(
                    "slot {} has no phases",
                    slot.name
                )));
            }
        }
        if let Some(package) = &self.package {
            package.validate()?;
            if package.id != self.id {
                return Err(HostFailure::invalid_input(format!(
                    "manifest id {} does not match package id {}",
                    self.id, package.id
                )));
            }
        }
        Ok(())
    }

    pub fn dispatch_methods(&self) -> Vec<String> {
        let mut methods = BTreeSet::new();
        methods.extend(self.methods.iter().cloned());
        if let Some(package) = self.normalized_package() {
            methods.extend(package.methods.into_iter().map(|method| method.name));
        }
        methods.into_iter().collect()
    }

    pub fn normalized_package(&self) -> Option<IpcPackageDescriptor> {
        let package_is_explicit = self.package.is_some();
        let mut package = match &self.package {
            Some(package) => package.clone(),
            None => IpcPackageDescriptor::new(self.id.clone()).ok()?,
        };

        if package.slots.is_empty() {
            package.slots = self.slots.clone();
        } else {
            for slot in &self.slots {
                if !package.slots.contains(slot) {
                    package.slots.push(slot.clone());
                }
            }
        }

        if !package_is_explicit {
            for method in self.methods.iter().filter(|method| !method.ends_with(".*")) {
                if !package
                    .methods
                    .iter()
                    .any(|descriptor| descriptor.name == *method)
                {
                    package
                        .methods
                        .push(MethodDescriptor::opaque(method.clone()).ok()?);
                }
            }
        }

        let mut capabilities = package.required_capabilities.clone();
        merge_manifest_capabilities(&mut capabilities, &self.needs, &self.allowed_hosts);
        package.required_capabilities = capabilities;

        Some(package)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginBinding {
    pub plugin_id: String,
    pub priority: i32,
    pub slot: Slot,
    pub phase: SlotPhase,
    pub policy: SlotPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MethodBinding {
    pub plugin_id: String,
    pub priority: i32,
    pub pattern: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginRegistry {
    plugins: BTreeMap<String, PluginManifest>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, manifest: PluginManifest) -> HostResult<()> {
        manifest.validate()?;
        if self.plugins.contains_key(&manifest.id) {
            return Err(HostFailure::invalid_input(format!(
                "plugin {} is already registered",
                manifest.id
            )));
        }
        self.validate_single_owner_conflicts(&manifest)?;
        self.plugins.insert(manifest.id.clone(), manifest);
        Ok(())
    }

    pub fn manifests(&self) -> impl Iterator<Item = &PluginManifest> {
        self.plugins.values()
    }

    pub fn get(&self, plugin_id: &str) -> Option<&PluginManifest> {
        self.plugins.get(plugin_id)
    }

    pub fn bindings_for(&self, slot: &Slot, phase: SlotPhase) -> Vec<PluginBinding> {
        let mut bindings: Vec<_> = self
            .plugins
            .values()
            .flat_map(|manifest| {
                manifest.slots.iter().filter_map(move |binding| {
                    if &binding.name == slot && binding.contains_phase(phase) {
                        Some(PluginBinding {
                            plugin_id: manifest.id.clone(),
                            priority: manifest.priority,
                            slot: binding.name.clone(),
                            phase,
                            policy: binding.policy,
                        })
                    } else {
                        None
                    }
                })
            })
            .collect();
        bindings.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.plugin_id.cmp(&right.plugin_id))
        });
        bindings
    }

    pub fn method_bindings(&self, method: &str) -> Vec<MethodBinding> {
        let mut bindings: Vec<_> = self
            .plugins
            .values()
            .filter_map(|manifest| {
                manifest
                    .dispatch_methods()
                    .into_iter()
                    .filter(|pattern| method_matches(pattern, method))
                    .max_by(|left, right| {
                        method_pattern_specificity(left)
                            .cmp(&method_pattern_specificity(right))
                            .then_with(|| left.cmp(right))
                    })
                    .map(|pattern| MethodBinding {
                        plugin_id: manifest.id.clone(),
                        priority: manifest.priority,
                        pattern,
                    })
            })
            .collect();
        bindings.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.plugin_id.cmp(&right.plugin_id))
                .then_with(|| left.pattern.cmp(&right.pattern))
        });
        bindings
    }

    pub fn merged_contract(&self) -> HostResult<MergedContractRegistry> {
        let mut merged = MergedContractRegistry::new();
        for manifest in self.plugins.values() {
            if let Some(package) = manifest.normalized_package() {
                merged.register_package(package)?;
            }
        }
        Ok(merged)
    }

    fn validate_single_owner_conflicts(&self, manifest: &PluginManifest) -> HostResult<()> {
        for candidate in &manifest.slots {
            for phase in &candidate.phases {
                for existing in self.bindings_for(&candidate.name, *phase) {
                    if candidate.policy == SlotPolicy::SingleOwner
                        || existing.policy == SlotPolicy::SingleOwner
                    {
                        return Err(HostFailure::invalid_input(format!(
                            "slot {} phase {} already has owner {}",
                            candidate.name, phase, existing.plugin_id
                        )));
                    }
                }
            }
        }
        if let Some(package) = manifest.normalized_package() {
            for method in package.methods {
                if let Some(existing) = self.method_owner(&method.name) {
                    return Err(HostFailure::invalid_input(format!(
                        "method {} is already owned by {}",
                        method.name, existing
                    )));
                }
            }
            for event in package.events {
                if let Some(existing) = self.event_owner(&event.name) {
                    return Err(HostFailure::invalid_input(format!(
                        "event {} is already owned by {}",
                        event.name, existing
                    )));
                }
            }
        }
        Ok(())
    }

    fn method_owner(&self, method: &str) -> Option<&str> {
        self.plugins
            .values()
            .filter_map(|manifest| {
                manifest.normalized_package().and_then(|package| {
                    package
                        .methods
                        .into_iter()
                        .any(|descriptor| descriptor.name == method)
                        .then_some(manifest.id.as_str())
                })
            })
            .next()
    }

    fn event_owner(&self, event: &str) -> Option<&str> {
        self.plugins
            .values()
            .filter_map(|manifest| {
                manifest.normalized_package().and_then(|package| {
                    package
                        .events
                        .into_iter()
                        .any(|descriptor| descriptor.name == event)
                        .then_some(manifest.id.as_str())
                })
            })
            .next()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostInvocation {
    pub protocol: String,
    pub slot: Slot,
    pub operation: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl HostInvocation {
    pub fn new(
        slot: Slot,
        operation: impl Into<String>,
        payload: serde_json::Value,
    ) -> HostResult<Self> {
        let operation = operation.into();
        validate_token("operation", &operation)?;
        Ok(Self {
            protocol: HOST_PROTOCOL_VERSION.to_owned(),
            slot,
            operation,
            metadata: BTreeMap::new(),
            payload,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostOutcome {
    #[serde(default)]
    pub diagnostics: Vec<HostDiagnostic>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl HostOutcome {
    pub fn passthrough(payload: serde_json::Value) -> Self {
        Self {
            diagnostics: Vec::new(),
            payload,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostDiagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostFailure {
    pub code: String,
    pub message: String,
}

impl HostFailure {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_input".to_owned(),
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            code: "unsupported".to_owned(),
            message: message.into(),
        }
    }

    pub fn plugin_error(message: impl Into<String>) -> Self {
        Self {
            code: "plugin_error".to_owned(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HostFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for HostFailure {}

pub type HostResult<T> = Result<T, HostFailure>;

pub trait ExtensionHost {
    fn invoke(&self, invocation: HostInvocation) -> HostResult<HostOutcome>;
}

fn default_phases() -> Vec<SlotPhase> {
    SlotPhase::ORDER.to_vec()
}

fn validate_token(kind: &str, value: &str) -> HostResult<()> {
    if value.is_empty() {
        return Err(HostFailure::invalid_input(format!(
            "{kind} must not be empty"
        )));
    }

    if value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | ':')
    }) {
        Ok(())
    } else {
        Err(HostFailure::invalid_input(format!(
            "{kind} contains unsupported characters"
        )))
    }
}

fn validate_method_pattern(value: &str) -> HostResult<()> {
    if let Some(prefix) = value.strip_suffix(".*") {
        validate_token("method prefix", prefix)
    } else {
        validate_token("method", value)
    }
}

fn validate_host_pattern(value: &str) -> HostResult<()> {
    if value.is_empty() {
        return Err(HostFailure::invalid_input("host must not be empty"));
    }

    if value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '*' | ':')
    }) {
        Ok(())
    } else {
        Err(HostFailure::invalid_input(
            "host contains unsupported characters",
        ))
    }
}

fn merge_manifest_capabilities(
    capabilities: &mut Vec<HostCapabilityDescriptor>,
    needs: &[String],
    allowed_hosts: &[String],
) {
    let mut indexed = capabilities
        .iter()
        .map(|capability| (capability.kind.clone(), capability.name.clone()))
        .collect::<BTreeSet<_>>();

    for need in needs {
        let capability = HostCapabilityDescriptor {
            kind: HostCapabilityKind::Need,
            name: need.clone(),
        };
        if indexed.insert((capability.kind.clone(), capability.name.clone())) {
            capabilities.push(capability);
        }
    }

    for host in allowed_hosts {
        let capability = HostCapabilityDescriptor {
            kind: HostCapabilityKind::HttpHost,
            name: host.clone(),
        };
        if indexed.insert((capability.kind.clone(), capability.name.clone())) {
            capabilities.push(capability);
        }
    }
}

fn method_matches(pattern: &str, method: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(".*") {
        method == prefix || method.starts_with(&format!("{prefix}."))
    } else {
        pattern == method
    }
}

fn method_pattern_specificity(pattern: &str) -> (u8, usize) {
    if let Some(prefix) = pattern.strip_suffix(".*") {
        (0, prefix.len())
    } else {
        (1, pattern.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(
        id: &str,
        priority: i32,
        slot: &str,
        phase: SlotPhase,
        policy: SlotPolicy,
    ) -> PluginManifest {
        PluginManifest {
            id: id.to_owned(),
            priority,
            methods: Vec::new(),
            slots: vec![SlotBinding::new(Slot::new(slot).unwrap(), vec![phase], policy).unwrap()],
            needs: Vec::new(),
            allowed_hosts: Vec::new(),
            package: None,
        }
    }

    #[test]
    fn rejects_empty_slot() {
        let error = Slot::new("").unwrap_err();
        assert_eq!(error.code, "invalid_input");
    }

    #[test]
    fn well_known_slot_index_contains_every_public_slot_constant() {
        assert!(Slot::WELL_KNOWN.contains(&Slot::CONFIG_PROCESS));
        assert!(Slot::WELL_KNOWN.contains(&Slot::TELEMETRY_OBSERVE));
        assert_eq!(Slot::WELL_KNOWN.len(), 24);
    }

    #[test]
    fn builds_invocation_with_protocol() {
        let invocation = HostInvocation::new(
            Slot::new(Slot::CONFIG_PROCESS).unwrap(),
            "apply",
            serde_json::json!({"hello": "world"}),
        )
        .unwrap();

        assert_eq!(invocation.protocol, HOST_PROTOCOL_VERSION);
        assert_eq!(invocation.slot.as_str(), Slot::CONFIG_PROCESS);
    }

    #[test]
    fn manifest_roundtrips_json() {
        let manifest = PluginManifest {
            id: "example.config".to_owned(),
            priority: 100,
            methods: vec!["config.*".to_owned()],
            slots: vec![SlotBinding::new(
                Slot::new(Slot::CONFIG_PROCESS).unwrap(),
                vec![SlotPhase::Before, SlotPhase::Exec, SlotPhase::After],
                SlotPolicy::Pipeline,
            )
            .unwrap()],
            needs: vec!["host.http.v1".to_owned()],
            allowed_hosts: vec!["*.example.test".to_owned()],
            package: None,
        };

        let encoded = serde_json::to_string(&manifest).unwrap();
        let decoded: PluginManifest = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, manifest);
        decoded.validate().unwrap();
    }

    #[test]
    fn manifest_snapshot_is_stable() {
        let manifest = PluginManifest {
            id: "example.config".to_owned(),
            priority: 100,
            methods: vec!["config.*".to_owned()],
            slots: vec![SlotBinding::new(
                Slot::new(Slot::CONFIG_PROCESS).unwrap(),
                vec![SlotPhase::Before, SlotPhase::Exec, SlotPhase::After],
                SlotPolicy::Pipeline,
            )
            .unwrap()],
            needs: vec!["host.http.v1".to_owned()],
            allowed_hosts: vec!["*.example.test".to_owned()],
            package: None,
        };

        let encoded = serde_json::to_string_pretty(&manifest).unwrap();
        assert_eq!(
            encoded,
            r#"{
  "id": "example.config",
  "priority": 100,
  "methods": [
    "config.*"
  ],
  "slots": [
    {
      "name": "config.process",
      "phases": [
        "before",
        "exec",
        "after"
      ],
      "policy": "pipeline"
    }
  ],
  "needs": [
    "host.http.v1"
  ],
  "allowed_hosts": [
    "*.example.test"
  ]
}"#
        );
    }

    #[test]
    fn slot_binding_snapshot_is_stable() {
        let binding = SlotBinding::new(
            Slot::new(Slot::VPN_CONNECT).unwrap(),
            vec![SlotPhase::Before, SlotPhase::Exec],
            SlotPolicy::FirstSuccess,
        )
        .unwrap();

        let encoded = serde_json::to_string_pretty(&binding).unwrap();
        assert_eq!(
            encoded,
            r#"{
  "name": "vpn.connect",
  "phases": [
    "before",
    "exec"
  ],
  "policy": "first_success"
}"#
        );
    }

    #[test]
    fn registry_orders_bindings_by_priority_then_id() {
        let mut registry = PluginRegistry::new();
        registry
            .register(manifest(
                "example.z",
                200,
                Slot::CONFIG_PROCESS,
                SlotPhase::Exec,
                SlotPolicy::Pipeline,
            ))
            .unwrap();
        registry
            .register(manifest(
                "example.a",
                100,
                Slot::CONFIG_PROCESS,
                SlotPhase::Exec,
                SlotPolicy::Pipeline,
            ))
            .unwrap();
        registry
            .register(manifest(
                "example.b",
                100,
                Slot::CONFIG_PROCESS,
                SlotPhase::Exec,
                SlotPolicy::Pipeline,
            ))
            .unwrap();

        let ids: Vec<_> = registry
            .bindings_for(&Slot::new(Slot::CONFIG_PROCESS).unwrap(), SlotPhase::Exec)
            .into_iter()
            .map(|binding| binding.plugin_id)
            .collect();

        assert_eq!(ids, ["example.a", "example.b", "example.z"]);
    }

    #[test]
    fn registry_plans_bindings_and_methods_deterministically() {
        let mut registry = PluginRegistry::new();
        registry
            .register(manifest(
                "example.alpha",
                50,
                Slot::CONFIG_PROCESS,
                SlotPhase::Exec,
                SlotPolicy::Pipeline,
            ))
            .unwrap();
        registry
            .register(manifest(
                "example.beta",
                10,
                Slot::CONFIG_PROCESS,
                SlotPhase::Exec,
                SlotPolicy::FirstSuccess,
            ))
            .unwrap();
        let mut method_plugin = PluginManifest::new("example.methods").unwrap();
        method_plugin.priority = 10;
        method_plugin.methods = vec!["config.*".to_owned(), "config.process".to_owned()];
        registry.register(method_plugin).unwrap();

        let binding_plan: Vec<_> = registry
            .bindings_for(&Slot::new(Slot::CONFIG_PROCESS).unwrap(), SlotPhase::Exec)
            .into_iter()
            .map(|binding| {
                serde_json::json!({
                    "plugin_id": binding.plugin_id,
                    "priority": binding.priority,
                    "slot": binding.slot.as_str(),
                    "phase": binding.phase,
                    "policy": binding.policy,
                })
            })
            .collect();

        assert_eq!(
            binding_plan,
            vec![
                serde_json::json!({
                    "phase": "exec",
                    "policy": "first_success",
                    "plugin_id": "example.beta",
                    "priority": 10,
                    "slot": "config.process"
                }),
                serde_json::json!({
                    "phase": "exec",
                    "policy": "pipeline",
                    "plugin_id": "example.alpha",
                    "priority": 50,
                    "slot": "config.process"
                }),
            ]
        );

        let method_plan: Vec<_> = registry
            .method_bindings("config.process")
            .into_iter()
            .map(|binding| {
                serde_json::json!({
                    "plugin_id": binding.plugin_id,
                    "priority": binding.priority,
                    "pattern": binding.pattern,
                })
            })
            .collect();

        assert_eq!(
            serde_json::to_string_pretty(&method_plan).unwrap(),
            r#"[
  {
    "pattern": "config.process",
    "plugin_id": "example.methods",
    "priority": 10
  }
]"#
        );
    }

    #[test]
    fn registry_blocks_single_owner_conflicts() {
        let mut registry = PluginRegistry::new();
        registry
            .register(manifest(
                "example.identity",
                0,
                Slot::AUTH_SESSION,
                SlotPhase::Exec,
                SlotPolicy::SingleOwner,
            ))
            .unwrap();

        let error = registry
            .register(manifest(
                "example.other",
                10,
                Slot::AUTH_SESSION,
                SlotPhase::Exec,
                SlotPolicy::Pipeline,
            ))
            .unwrap_err();

        assert_eq!(error.code, "invalid_input");
        assert!(error.message.contains("already has owner"));
    }

    #[test]
    fn registry_rejects_second_single_owner_claim() {
        let mut registry = PluginRegistry::new();
        registry
            .register(manifest(
                "example.identity",
                0,
                Slot::AUTH_SESSION,
                SlotPhase::Exec,
                SlotPolicy::SingleOwner,
            ))
            .unwrap();

        let error = registry
            .register(manifest(
                "example.identity_observer",
                1,
                Slot::AUTH_SESSION,
                SlotPhase::Exec,
                SlotPolicy::SingleOwner,
            ))
            .unwrap_err();

        assert_eq!(error.code, "invalid_input");
        assert!(error.message.contains("already has owner"));
    }

    #[test]
    fn method_index_supports_exact_and_prefix_patterns() {
        let mut registry = PluginRegistry::new();
        let mut first = PluginManifest::new("example.all_identity").unwrap();
        first.priority = 200;
        first.methods = vec!["identity.*".to_owned()];
        registry.register(first).unwrap();

        let mut second = PluginManifest::new("example.login").unwrap();
        second.priority = 100;
        second.methods = vec!["identity.login".to_owned()];
        registry.register(second).unwrap();

        let ids: Vec<_> = registry
            .method_bindings("identity.login")
            .into_iter()
            .map(|binding| binding.plugin_id)
            .collect();

        assert_eq!(ids, ["example.login", "example.all_identity"]);
        assert!(registry.method_bindings("profile.get").is_empty());
    }

    #[test]
    fn manifest_package_extends_dispatch_and_capabilities() {
        let mut manifest = PluginManifest::new("example.identity").unwrap();
        manifest.methods = vec!["identity.*".to_owned()];
        manifest.allowed_hosts = vec!["hub.example.test".to_owned()];
        manifest.needs = vec!["host.http.v1".to_owned()];
        manifest.package = Some(IpcPackageDescriptor {
            id: "example.identity".to_owned(),
            version: Some("1.0.0".to_owned()),
            domain: Some("auth".to_owned()),
            compatibility: None,
            methods: vec![MethodDescriptor::opaque("identity.login").unwrap()],
            events: vec![],
            slots: vec![],
            required_capabilities: vec![HostCapabilityDescriptor::new(
                HostCapabilityKind::Permission,
                "session.write",
            )
            .unwrap()],
            component: None,
        });

        let package = manifest.normalized_package().unwrap();
        let method_names: Vec<_> = package
            .methods
            .into_iter()
            .map(|method| method.name)
            .collect();
        assert_eq!(method_names, ["identity.login"]);
        assert!(package.required_capabilities.iter().any(|capability| {
            capability.kind == HostCapabilityKind::Need && capability.name == "host.http.v1"
        }));
        assert!(package.required_capabilities.iter().any(|capability| {
            capability.kind == HostCapabilityKind::HttpHost && capability.name == "hub.example.test"
        }));

        let dispatch_methods = manifest.dispatch_methods();
        assert!(dispatch_methods.iter().any(|method| method == "identity.*"));
        assert!(dispatch_methods
            .iter()
            .any(|method| method == "identity.login"));
    }

    #[test]
    fn registry_rejects_duplicate_exact_package_methods() {
        let mut registry = PluginRegistry::new();
        let mut first = PluginManifest::new("example.first").unwrap();
        first.package = Some(IpcPackageDescriptor {
            id: "example.first".to_owned(),
            version: None,
            domain: None,
            compatibility: None,
            methods: vec![MethodDescriptor::opaque("identity.login").unwrap()],
            events: vec![],
            slots: vec![],
            required_capabilities: vec![],
            component: None,
        });
        registry.register(first).unwrap();

        let mut second = PluginManifest::new("example.second").unwrap();
        second.package = Some(IpcPackageDescriptor {
            id: "example.second".to_owned(),
            version: None,
            domain: None,
            compatibility: None,
            methods: vec![MethodDescriptor::opaque("identity.login").unwrap()],
            events: vec![],
            slots: vec![],
            required_capabilities: vec![],
            component: None,
        });
        let error = registry.register(second).unwrap_err();

        assert_eq!(error.code, "invalid_input");
        assert!(error.message.contains("identity.login"));
    }

    #[test]
    fn merged_contract_indexes_exact_methods_and_events() {
        let mut registry = PluginRegistry::new();
        let mut manifest = PluginManifest::new("example.package").unwrap();
        manifest.package = Some(IpcPackageDescriptor {
            id: "example.package".to_owned(),
            version: Some("1.2.3".to_owned()),
            domain: Some("billing".to_owned()),
            compatibility: None,
            methods: vec![MethodDescriptor::opaque("commerce.catalog").unwrap()],
            events: vec![EventDescriptor::new("event.commerceChanged").unwrap()],
            slots: vec![],
            required_capabilities: vec![],
            component: None,
        });
        registry.register(manifest).unwrap();

        let merged = registry.merged_contract().unwrap();
        let inventory = merged.inventory_summary();
        assert_eq!(inventory.methods, ["commerce.catalog"]);
        assert_eq!(inventory.events, ["event.commerceChanged"]);
        assert_eq!(
            merged.method_owner("commerce.catalog"),
            Some("example.package")
        );
        assert_eq!(
            merged.event_owner("event.commerceChanged"),
            Some("example.package")
        );
    }
}
