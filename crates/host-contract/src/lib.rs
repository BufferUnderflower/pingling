use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

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
    pub const IPC_DISPATCH: &'static str = "ipc.dispatch";
    pub const PLUGIN_LOAD: &'static str = "plugin.load";

    pub const CONFIG_TRANSFORM: &'static str = "config.transform";
    pub const RUNTIME_COMMAND: &'static str = "runtime.command";
    pub const RUNTIME_OBSERVE: &'static str = "runtime.observe";
    pub const STORAGE_LOOKUP: &'static str = "storage.lookup";
    pub const TELEMETRY_OBSERVE: &'static str = "telemetry.observe";

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
        Ok(())
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
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
            .flat_map(|manifest| {
                manifest.methods.iter().filter_map(move |pattern| {
                    if method_matches(pattern, method) {
                        Some(MethodBinding {
                            plugin_id: manifest.id.clone(),
                            priority: manifest.priority,
                            pattern: pattern.clone(),
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
                .then_with(|| left.pattern.cmp(&right.pattern))
        });
        bindings
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
        Ok(())
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

fn method_matches(pattern: &str, method: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(".*") {
        method == prefix || method.starts_with(&format!("{prefix}."))
    } else {
        pattern == method
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
        }
    }

    #[test]
    fn rejects_empty_slot() {
        let error = Slot::new("").unwrap_err();
        assert_eq!(error.code, "invalid_input");
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
        };

        let encoded = serde_json::to_string(&manifest).unwrap();
        let decoded: PluginManifest = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, manifest);
        decoded.validate().unwrap();
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
    fn method_index_supports_exact_and_prefix_patterns() {
        let mut registry = PluginRegistry::new();
        let mut first = PluginManifest::new("example.all_auth").unwrap();
        first.priority = 200;
        first.methods = vec!["auth.*".to_owned()];
        registry.register(first).unwrap();

        let mut second = PluginManifest::new("example.login").unwrap();
        second.priority = 100;
        second.methods = vec!["auth.login".to_owned()];
        registry.register(second).unwrap();

        let ids: Vec<_> = registry
            .method_bindings("auth.login")
            .into_iter()
            .map(|binding| binding.plugin_id)
            .collect();

        assert_eq!(ids, ["example.login", "example.all_auth"]);
        assert!(registry.method_bindings("profile.get").is_empty());
    }
}
