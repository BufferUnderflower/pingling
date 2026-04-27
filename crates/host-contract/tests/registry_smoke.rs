use pingling_host_contract::{
    PluginManifest, PluginRegistry, Slot, SlotBinding, SlotPhase, SlotPolicy,
};

fn manifest(id: &str, priority: i32, slot: &str, policy: SlotPolicy) -> PluginManifest {
    PluginManifest {
        id: id.to_owned(),
        priority,
        methods: vec![format!("{id}.*")],
        slots: vec![SlotBinding::new(
            Slot::new(slot).expect("valid slot"),
            vec![SlotPhase::Before, SlotPhase::Exec, SlotPhase::After],
            policy,
        )
        .expect("valid slot binding")],
        needs: Vec::new(),
        allowed_hosts: Vec::new(),
    }
}

#[test]
fn plugin_registry_orders_by_priority_then_id() {
    let mut registry = PluginRegistry::new();
    registry
        .register(manifest(
            "example.z",
            200,
            Slot::CONFIG_PROCESS,
            SlotPolicy::Pipeline,
        ))
        .unwrap();
    registry
        .register(manifest(
            "example.a",
            100,
            Slot::CONFIG_PROCESS,
            SlotPolicy::Pipeline,
        ))
        .unwrap();
    registry
        .register(manifest(
            "example.b",
            100,
            Slot::CONFIG_PROCESS,
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
fn plugin_registry_rejects_competing_single_owner_bindings() {
    let mut registry = PluginRegistry::new();
    registry
        .register(manifest(
            "example.identity",
            0,
            Slot::AUTH_SESSION,
            SlotPolicy::SingleOwner,
        ))
        .unwrap();

    let error = registry
        .register(manifest(
            "example.identity_observer",
            10,
            Slot::AUTH_SESSION,
            SlotPolicy::Pipeline,
        ))
        .unwrap_err();

    assert_eq!(error.code, "invalid_input");
    assert!(error.message.contains("already has owner"));
}
