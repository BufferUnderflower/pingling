use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{validate_token, HostFailure, HostResult, SlotBinding};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCapabilityKind {
    HostFunction,
    Permission,
    HttpHost,
    Need,
    RuntimeCapability,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostCapabilityDescriptor {
    pub kind: HostCapabilityKind,
    pub name: String,
}

impl HostCapabilityDescriptor {
    pub fn new(kind: HostCapabilityKind, name: impl Into<String>) -> HostResult<Self> {
        let name = name.into();
        validate_token("capability", &name)?;
        Ok(Self { kind, name })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
pub struct CompatibilityRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_host_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_host_version: Option<String>,
}

impl CompatibilityRange {
    pub fn validate(&self) -> HostResult<()> {
        if let Some(protocol) = &self.protocol {
            validate_token("protocol", protocol)?;
        }
        if let Some(version) = &self.min_host_version {
            validate_token("minimum host version", version)?;
        }
        if let Some(version) = &self.max_host_version {
            validate_token("maximum host version", version)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MethodErrorDescriptor {
    pub code: i64,
    pub message: String,
}

impl MethodErrorDescriptor {
    pub fn validate(&self) -> HostResult<()> {
        if self.message.trim().is_empty() {
            return Err(HostFailure::invalid_input(
                "method error message must not be empty",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MethodDescriptor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default = "default_open_schema")]
    pub params_schema: Value,
    #[serde(default = "default_open_schema")]
    pub result_schema: Value,
    #[serde(default)]
    pub errors: Vec<MethodErrorDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stability: Option<String>,
}

impl MethodDescriptor {
    pub fn new(name: impl Into<String>) -> HostResult<Self> {
        let name = name.into();
        validate_token("method", &name)?;
        Ok(Self {
            name,
            summary: None,
            params_schema: default_open_schema(),
            result_schema: default_open_schema(),
            errors: Vec::new(),
            stability: None,
        })
    }

    pub fn opaque(name: impl Into<String>) -> HostResult<Self> {
        Self::new(name)
    }

    pub fn validate(&self) -> HostResult<()> {
        validate_token("method", &self.name)?;
        for error in &self.errors {
            error.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventDescriptor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default = "default_open_schema")]
    pub payload_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stability: Option<String>,
}

impl EventDescriptor {
    pub fn new(name: impl Into<String>) -> HostResult<Self> {
        let name = name.into();
        validate_token("event", &name)?;
        Ok(Self {
            name,
            summary: None,
            payload_schema: default_open_schema(),
            subscription: None,
            stability: None,
        })
    }

    pub fn validate(&self) -> HostResult<()> {
        validate_token("event", &self.name)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WitFieldDescriptor {
    pub name: String,
    pub ty: String,
}

impl WitFieldDescriptor {
    pub fn validate(&self) -> HostResult<()> {
        validate_wit_identifier("WIT field", &self.name)?;
        validate_wit_type("WIT field type", &self.ty)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WitResultDescriptor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
}

impl WitResultDescriptor {
    pub fn validate(&self) -> HostResult<()> {
        if self.ok.is_none() && self.err.is_none() {
            return Err(HostFailure::invalid_input(
                "WIT result must include ok or err type",
            ));
        }
        if let Some(ok) = &self.ok {
            validate_wit_type("WIT result ok type", ok)?;
        }
        if let Some(err) = &self.err {
            validate_wit_type("WIT result err type", err)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ComponentFunctionDescriptor {
    pub name: String,
    #[serde(default)]
    pub params: Vec<WitFieldDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<WitResultDescriptor>,
}

impl ComponentFunctionDescriptor {
    pub fn validate(&self) -> HostResult<()> {
        validate_wit_identifier("WIT function", &self.name)?;
        let mut params = BTreeSet::new();
        for param in &self.params {
            param.validate()?;
            if !params.insert(param.name.as_str()) {
                return Err(HostFailure::invalid_input(format!(
                    "WIT function {} declares param {} more than once",
                    self.name, param.name
                )));
            }
        }
        if let Some(result) = &self.result {
            result.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ComponentRecordDescriptor {
    pub name: String,
    #[serde(default)]
    pub fields: Vec<WitFieldDescriptor>,
}

impl ComponentRecordDescriptor {
    pub fn validate(&self) -> HostResult<()> {
        validate_wit_identifier("WIT record", &self.name)?;
        let mut fields = BTreeSet::new();
        for field in &self.fields {
            field.validate()?;
            if !fields.insert(field.name.as_str()) {
                return Err(HostFailure::invalid_input(format!(
                    "WIT record {} declares field {} more than once",
                    self.name, field.name
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ComponentInterfaceDescriptor {
    pub name: String,
    #[serde(default)]
    pub records: Vec<ComponentRecordDescriptor>,
    #[serde(default)]
    pub functions: Vec<ComponentFunctionDescriptor>,
}

impl ComponentInterfaceDescriptor {
    pub fn validate(&self) -> HostResult<()> {
        validate_wit_identifier("WIT interface", &self.name)?;
        let mut records = BTreeSet::new();
        for record in &self.records {
            record.validate()?;
            if !records.insert(record.name.as_str()) {
                return Err(HostFailure::invalid_input(format!(
                    "WIT interface {} declares record {} more than once",
                    self.name, record.name
                )));
            }
        }
        let mut functions = BTreeSet::new();
        for function in &self.functions {
            function.validate()?;
            if !functions.insert(function.name.as_str()) {
                return Err(HostFailure::invalid_input(format!(
                    "WIT interface {} declares function {} more than once",
                    self.name, function.name
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ComponentDescriptor {
    pub wit_package: String,
    pub world: String,
    #[serde(default)]
    pub imports: Vec<ComponentInterfaceDescriptor>,
    #[serde(default)]
    pub exports: Vec<ComponentInterfaceDescriptor>,
}

impl ComponentDescriptor {
    pub fn validate(&self) -> HostResult<()> {
        validate_wit_package(&self.wit_package)?;
        validate_wit_identifier("WIT world", &self.world)?;
        validate_component_interfaces("import", &self.imports)?;
        validate_component_interfaces("export", &self.exports)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IpcPackageDescriptor {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<CompatibilityRange>,
    #[serde(default)]
    pub methods: Vec<MethodDescriptor>,
    #[serde(default)]
    pub events: Vec<EventDescriptor>,
    #[serde(default)]
    pub slots: Vec<SlotBinding>,
    #[serde(default)]
    pub required_capabilities: Vec<HostCapabilityDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentDescriptor>,
}

impl IpcPackageDescriptor {
    pub fn new(id: impl Into<String>) -> HostResult<Self> {
        let id = id.into();
        validate_token("package id", &id)?;
        Ok(Self {
            id,
            version: None,
            domain: None,
            compatibility: None,
            methods: Vec::new(),
            events: Vec::new(),
            slots: Vec::new(),
            required_capabilities: Vec::new(),
            component: None,
        })
    }

    pub fn validate(&self) -> HostResult<()> {
        validate_token("package id", &self.id)?;
        if let Some(domain) = &self.domain {
            validate_token("package domain", domain)?;
        }
        if let Some(version) = &self.version {
            validate_token("package version", version)?;
        }
        if let Some(compatibility) = &self.compatibility {
            compatibility.validate()?;
        }

        let mut methods = BTreeSet::new();
        for method in &self.methods {
            method.validate()?;
            if !methods.insert(method.name.as_str()) {
                return Err(HostFailure::invalid_input(format!(
                    "package {} declares method {} more than once",
                    self.id, method.name
                )));
            }
        }

        let mut events = BTreeSet::new();
        for event in &self.events {
            event.validate()?;
            if !events.insert(event.name.as_str()) {
                return Err(HostFailure::invalid_input(format!(
                    "package {} declares event {} more than once",
                    self.id, event.name
                )));
            }
        }

        for capability in &self.required_capabilities {
            validate_token("capability", &capability.name)?;
        }
        if let Some(component) = &self.component {
            component.validate()?;
        }

        Ok(())
    }
}

pub fn render_wit_world(component: &ComponentDescriptor) -> HostResult<String> {
    component.validate()?;

    let mut out = String::new();
    out.push_str(&format!("package {};\n\n", component.wit_package));

    for interface in component.imports.iter().chain(component.exports.iter()) {
        render_interface(&mut out, interface);
        out.push('\n');
    }

    out.push_str(&format!("world {} {{\n", component.world));
    for interface in &component.imports {
        out.push_str(&format!("  import {};\n", interface.name));
    }
    for interface in &component.exports {
        out.push_str(&format!("  export {};\n", interface.name));
    }
    out.push_str("}\n");
    Ok(out)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContractInventorySummary {
    pub methods: Vec<String>,
    pub events: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MergedContractRegistry {
    packages: BTreeMap<String, IpcPackageDescriptor>,
    method_owners: BTreeMap<String, String>,
    event_owners: BTreeMap<String, String>,
}

impl Default for MergedContractRegistry {
    fn default() -> Self {
        Self {
            packages: BTreeMap::new(),
            method_owners: BTreeMap::new(),
            event_owners: BTreeMap::new(),
        }
    }
}

impl MergedContractRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_package(&mut self, package: IpcPackageDescriptor) -> HostResult<()> {
        package.validate()?;
        if self.packages.contains_key(&package.id) {
            return Err(HostFailure::invalid_input(format!(
                "package {} is already registered",
                package.id
            )));
        }
        for method in &package.methods {
            if let Some(owner) = self.method_owners.get(&method.name) {
                return Err(HostFailure::invalid_input(format!(
                    "method {} is already owned by {}",
                    method.name, owner
                )));
            }
        }
        for event in &package.events {
            if let Some(owner) = self.event_owners.get(&event.name) {
                return Err(HostFailure::invalid_input(format!(
                    "event {} is already owned by {}",
                    event.name, owner
                )));
            }
        }

        for method in &package.methods {
            self.method_owners
                .insert(method.name.clone(), package.id.clone());
        }
        for event in &package.events {
            self.event_owners
                .insert(event.name.clone(), package.id.clone());
        }

        self.packages.insert(package.id.clone(), package);
        Ok(())
    }

    pub fn packages(&self) -> impl Iterator<Item = &IpcPackageDescriptor> {
        self.packages.values()
    }

    pub fn method_owner(&self, method: &str) -> Option<&str> {
        self.method_owners.get(method).map(String::as_str)
    }

    pub fn event_owner(&self, event: &str) -> Option<&str> {
        self.event_owners.get(event).map(String::as_str)
    }

    pub fn methods(&self) -> Vec<OwnedMethodDescriptor<'_>> {
        let mut methods = self
            .packages
            .iter()
            .flat_map(|(package_id, package)| {
                package
                    .methods
                    .iter()
                    .map(move |method| OwnedMethodDescriptor {
                        package_id: package_id.as_str(),
                        descriptor: method,
                    })
            })
            .collect::<Vec<_>>();
        methods.sort_by(|left, right| {
            left.descriptor
                .name
                .cmp(&right.descriptor.name)
                .then_with(|| left.package_id.cmp(right.package_id))
        });
        methods
    }

    pub fn events(&self) -> Vec<OwnedEventDescriptor<'_>> {
        let mut events = self
            .packages
            .iter()
            .flat_map(|(package_id, package)| {
                package
                    .events
                    .iter()
                    .map(move |event| OwnedEventDescriptor {
                        package_id: package_id.as_str(),
                        descriptor: event,
                    })
            })
            .collect::<Vec<_>>();
        events.sort_by(|left, right| {
            left.descriptor
                .name
                .cmp(&right.descriptor.name)
                .then_with(|| left.package_id.cmp(right.package_id))
        });
        events
    }

    pub fn inventory_summary(&self) -> ContractInventorySummary {
        ContractInventorySummary {
            methods: self
                .methods()
                .into_iter()
                .map(|item| item.descriptor.name.clone())
                .collect(),
            events: self
                .events()
                .into_iter()
                .map(|item| item.descriptor.name.clone())
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OwnedMethodDescriptor<'a> {
    pub package_id: &'a str,
    pub descriptor: &'a MethodDescriptor,
}

#[derive(Clone, Copy, Debug)]
pub struct OwnedEventDescriptor<'a> {
    pub package_id: &'a str,
    pub descriptor: &'a EventDescriptor,
}

fn default_open_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": true,
    })
}

fn render_interface(out: &mut String, interface: &ComponentInterfaceDescriptor) {
    out.push_str(&format!("interface {} {{\n", interface.name));
    for record in &interface.records {
        out.push_str(&format!("  record {} {{\n", record.name));
        for field in &record.fields {
            out.push_str(&format!("    {}: {},\n", field.name, field.ty));
        }
        out.push_str("  }\n\n");
    }
    for function in &interface.functions {
        let params = function
            .params
            .iter()
            .map(|param| format!("{}: {}", param.name, param.ty))
            .collect::<Vec<_>>()
            .join(", ");
        match &function.result {
            Some(result) => out.push_str(&format!(
                "  {}: func({}) -> {};\n",
                function.name,
                params,
                render_result(result)
            )),
            None => out.push_str(&format!("  {}: func({});\n", function.name, params)),
        }
    }
    out.push_str("}\n");
}

fn render_result(result: &WitResultDescriptor) -> String {
    match (&result.ok, &result.err) {
        (Some(ok), Some(err)) => format!("result<{ok}, {err}>"),
        (Some(ok), None) => ok.clone(),
        (None, Some(err)) => format!("result<_, {err}>"),
        (None, None) => "result".to_owned(),
    }
}

fn validate_wit_package(value: &str) -> HostResult<()> {
    let Some((namespace, package)) = value.split_once(':') else {
        return Err(HostFailure::invalid_input(
            "WIT package must use namespace:name form",
        ));
    };
    validate_wit_identifier("WIT package namespace", namespace)?;
    validate_wit_identifier("WIT package name", package)
}

fn validate_wit_identifier(kind: &str, value: &str) -> HostResult<()> {
    if value.is_empty() {
        return Err(HostFailure::invalid_input(format!(
            "{kind} must not be empty"
        )));
    }
    let mut chars = value.chars();
    let first = chars
        .next()
        .ok_or_else(|| HostFailure::invalid_input(format!("{kind} must not be empty")))?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(HostFailure::invalid_input(format!(
            "{kind} must start with a letter or underscore"
        )));
    }
    if chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')) {
        Ok(())
    } else {
        Err(HostFailure::invalid_input(format!(
            "{kind} contains unsupported characters"
        )))
    }
}

fn validate_wit_type(kind: &str, value: &str) -> HostResult<()> {
    if matches!(
        value,
        "bool"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "s8"
            | "s16"
            | "s32"
            | "s64"
            | "f32"
            | "f64"
            | "char"
            | "string"
            | "_"
    ) {
        return Ok(());
    }
    validate_wit_identifier(kind, value)
}

fn validate_component_interfaces(
    kind: &str,
    interfaces: &[ComponentInterfaceDescriptor],
) -> HostResult<()> {
    let mut names = BTreeSet::new();
    for interface in interfaces {
        interface.validate()?;
        if !names.insert(interface.name.as_str()) {
            return Err(HostFailure::invalid_input(format!(
                "WIT {kind} interface {} is declared more than once",
                interface.name
            )));
        }
    }
    Ok(())
}
