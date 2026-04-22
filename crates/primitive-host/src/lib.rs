use pingling_host_contract::{
    DiagnosticLevel, ExtensionHost, HostDiagnostic, HostInvocation, HostOutcome, HostResult,
    PluginManifest, PluginRegistry, Slot, SlotBinding, SlotPhase, SlotPolicy,
};

#[derive(Clone, Debug, Default)]
pub struct PassthroughHost;

impl ExtensionHost for PassthroughHost {
    fn invoke(&self, invocation: HostInvocation) -> HostResult<HostOutcome> {
        Ok(HostOutcome {
            diagnostics: vec![HostDiagnostic {
                level: DiagnosticLevel::Debug,
                message: "no extension installed; payload returned unchanged".to_owned(),
            }],
            payload: invocation.payload,
        })
    }
}

impl PassthroughHost {
    pub fn manifest() -> PluginManifest {
        PluginManifest {
            id: "pingling.primitive.passthrough".to_owned(),
            priority: i32::MAX,
            methods: Vec::new(),
            slots: well_known_slots()
                .into_iter()
                .map(|slot| {
                    SlotBinding::new(
                        Slot::new(slot).expect("well-known slot is valid"),
                        SlotPhase::ORDER.to_vec(),
                        SlotPolicy::BestEffort,
                    )
                    .expect("well-known binding is valid")
                })
                .collect(),
            needs: Vec::new(),
            allowed_hosts: Vec::new(),
        }
    }

    pub fn registry() -> HostResult<PluginRegistry> {
        let mut registry = PluginRegistry::new();
        registry.register(Self::manifest())?;
        Ok(registry)
    }
}

pub fn passthrough(invocation: HostInvocation) -> HostResult<HostOutcome> {
    PassthroughHost.invoke(invocation)
}

pub fn primitive_registry() -> HostResult<PluginRegistry> {
    PassthroughHost::registry()
}

fn well_known_slots() -> [&'static str; 7] {
    [
        Slot::CONFIG_PROCESS,
        Slot::DEEPLINK_RESOLVE,
        Slot::AUTH_SESSION,
        Slot::VPN_CONNECT,
        Slot::VPN_DISCONNECT,
        Slot::IPC_DISPATCH,
        Slot::PLUGIN_LOAD,
    ]
}

#[cfg(test)]
mod tests {
    use pingling_host_contract::{ExtensionHost, HostInvocation, Slot, SlotPhase};

    use super::*;

    #[test]
    fn returns_payload_unchanged() {
        let payload = serde_json::json!({"value": 42});
        let invocation = HostInvocation::new(
            Slot::new(Slot::CONFIG_TRANSFORM).unwrap(),
            "apply",
            payload.clone(),
        )
        .unwrap();

        let outcome = PassthroughHost.invoke(invocation).unwrap();

        assert_eq!(outcome.payload, payload);
        assert_eq!(outcome.diagnostics.len(), 1);
    }

    #[test]
    fn primitive_registry_claims_well_known_slots_last() {
        let registry = primitive_registry().unwrap();
        let bindings =
            registry.bindings_for(&Slot::new(Slot::CONFIG_PROCESS).unwrap(), SlotPhase::Exec);

        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].plugin_id, "pingling.primitive.passthrough");
        assert_eq!(bindings[0].priority, i32::MAX);
    }
}
