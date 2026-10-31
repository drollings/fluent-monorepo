use super::*;
use bitvec::prelude::*;

#[test]
fn register_and_retrieve() {
    let mut reg = TargetRegistry::new();
    let target = Target::new()
        .id(1)
        .name("build".into())
        .target_type(TargetType::File)
        .executor(ExecutorKind::Native)
        .depends(bitvec::bitvec![0, 1])
        .provides(bitvec::bitvec![1, 0])
        .command("cargo build".into())
        .essential(true)
        .build();
    reg.register(target).unwrap();
    assert_eq!(reg.len(), 1);
    let t = reg.get("build").unwrap();
    assert_eq!(t.id, 1);
    assert!(t.essential);
}

#[test]
fn duplicate_target_errors() {
    let mut reg = TargetRegistry::new();
    let t1 = Target::new()
        .id(1)
        .name("dup".into())
        .target_type(TargetType::File)
        .executor(ExecutorKind::Native)
        .depends(BitVec::new())
        .provides(BitVec::new())
        .build();
    let t2 = Target::new()
        .id(2)
        .name("dup".into())
        .target_type(TargetType::File)
        .executor(ExecutorKind::Native)
        .depends(BitVec::new())
        .provides(BitVec::new())
        .build();
    reg.register(t1).unwrap();
    let err = reg.register(t2).unwrap_err();
    assert!(matches!(err, RegistryError::DuplicateTarget { .. }));
}

#[test]
fn get_by_bit_index() {
    let mut reg = TargetRegistry::new();
    let t = Target::new()
        .id(42)
        .name("test".into())
        .target_type(TargetType::File)
        .executor(ExecutorKind::Wasm)
        .depends(BitVec::new())
        .provides(BitVec::new())
        .build();
    reg.register(t).unwrap();
    let found = reg.get_by_bit_index(42).unwrap();
    assert_eq!(&*found.name, "test");
}

#[test]
fn essential_and_abstract_targets() {
    let mut reg = TargetRegistry::new();
    for i in 0..3 {
        let ttype = if i == 0 {
            TargetType::Abstract
        } else {
            TargetType::File
        };
        let t = Target::new()
            .id(i)
            .name(ArcIntern::from(format!("t{i}")))
            .target_type(ttype)
            .executor(ExecutorKind::Native)
            .depends(BitVec::new())
            .provides(BitVec::new())
            .essential(i == 1)
            .build();
        reg.register(t).unwrap();
    }
    assert_eq!(reg.abstract_targets().len(), 1);
    assert_eq!(reg.essential_targets().len(), 1);
}

#[test]
fn target_builder_with_defaults() {
    let t = Target::new()
        .id(1)
        .name("defaults".into())
        .target_type(TargetType::Phony)
        .executor(ExecutorKind::Native)
        .depends(BitVec::new())
        .provides(BitVec::new())
        .build();
    assert!(!t.essential);
    assert!(t.command.is_empty());
}

#[test]
fn providers_multi_provider_lookup() {
    use common_core::interner::CapabilityRegistry;
    let caps = CapabilityRegistry::new();
    let mut reg = TargetRegistry::new();
    // Non-sequential ids prove provider lookup resolves by capability, not
    // by conflating target-id and capability-id spaces.
    let provides = caps.to_bitvec(&["artifact"]);
    for (id, name) in [(101i64, "a"), (202, "b")] {
        reg.register(
            Target::new()
                .id(id)
                .name((*name).into())
                .target_type(TargetType::File)
                .executor(ExecutorKind::Native)
                .depends(BitVec::new())
                .provides(provides.clone())
                .essential(false)
                .build(),
        )
        .unwrap();
    }
    let cap_idx = caps.to_bitvec(&["artifact"]).iter_ones().next().unwrap();
    let mut names: Vec<String> = reg
        .get_providers(cap_idx)
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    let found = reg.find_providers(&caps.to_bitvec(&["artifact"]));
    assert_eq!(found.len(), 2);
}
