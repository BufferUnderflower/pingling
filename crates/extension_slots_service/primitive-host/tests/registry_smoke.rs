use pingling_host_contract::{Slot, SlotPhase};
use pingling_primitive_host::primitive_registry;

#[test]
fn primitive_registry_claims_well_known_slots_last() {
    let registry = primitive_registry().unwrap();
    let bindings =
        registry.bindings_for(&Slot::new(Slot::CONFIG_PROCESS).unwrap(), SlotPhase::Exec);

    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].plugin_id, "pingling.primitive.passthrough");
    assert_eq!(bindings[0].priority, i32::MAX);
}
