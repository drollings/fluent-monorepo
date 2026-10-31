use super::*;

#[test]
fn test_full_yamake_load() {
    let json = include_str!("../../../data/yamake.json");
    let (reg, _caps) = load_yamake_config(json).expect("valid yamake loads");
    assert!(reg.len() > 50);
}

#[test]
fn empty_graph_loads_empty_registry() {
    let (reg, caps) = load_yamake_config(r#"{"targets": []}"#).expect("empty loads");
    assert_eq!(reg.len(), 0);
    assert_eq!(caps.count(), 0);
}

#[test]
fn malformed_json_returns_error() {
    let Err(err) = load_yamake_config("not json") else {
        panic!("malformed must refuse")
    };
    assert!(err.to_string().contains("invalid yamake.json"));
}

#[test]
fn unknown_target_type_returns_error() {
    let Err(err) =
        load_yamake_config(r#"{"targets": [{"id": 1, "name": "a", "target_type": "bogus"}]}"#)
    else {
        panic!("unknown type must refuse")
    };
    assert!(err.to_string().contains("unknown target_type"));
}

#[test]
fn duplicate_target_returns_error() {
    let Err(err) = load_yamake_config(
        r#"{"targets": [
            {"id": 1, "name": "a", "target_type": "file"},
            {"id": 2, "name": "a", "target_type": "file"}
        ]}"#,
    ) else {
        panic!("duplicate must refuse")
    };
    assert!(err.to_string().contains("already exists"));
}

#[test]
fn concrete_targets_self_provide_their_name() {
    let (reg, caps) = load_yamake_config(
        r#"{"targets": [{"id": 1, "name": "build", "target_type": "file", "provides": ["artifact"]}]}"#,
    )
    .expect("valid config loads");
    let target = reg.get("build").expect("target registered");
    let provides = caps.bitvec_to_names(&target.provides);
    assert!(
        provides.iter().any(|n| &**n == "build"),
        "file self-provides"
    );
    assert!(
        provides.iter().any(|n| &**n == "artifact"),
        "declared provide kept"
    );
}

#[test]
fn abstract_targets_do_not_self_provide() {
    let (reg, caps) = load_yamake_config(
        r#"{"targets": [{"id": 1, "name": "staff", "target_type": "abstract"}]}"#,
    )
    .expect("valid config loads");
    let target = reg.get("staff").expect("target registered");
    assert!(
        caps.bitvec_to_names(&target.provides).is_empty(),
        "abstract targets must not self-provide (cycle guard)"
    );
}

#[test]
fn phony_targets_self_provide_like_files() {
    let (reg, caps) =
        load_yamake_config(r#"{"targets": [{"id": 1, "name": "clean", "target_type": "phony"}]}"#)
            .expect("valid config loads");
    let target = reg.get("clean").expect("target registered");
    assert!(caps
        .bitvec_to_names(&target.provides)
        .iter()
        .any(|n| &**n == "clean"));
}

#[test]
fn depends_and_essential_are_wired() {
    let (reg, _caps) = load_yamake_config(
        r#"{"targets": [
            {"id": 1, "name": "src", "target_type": "file"},
            {"id": 2, "name": "bin", "target_type": "file", "depends": ["src"], "essential": true}
        ]}"#,
    )
    .expect("valid config loads");
    let bin = reg.get("bin").expect("bin registered");
    assert!(bin.essential);
    let _ = reg.get("src").expect("src registered");
}

#[test]
fn yamake_malformed_returns_error() {
    assert!(load_yamake_config("not json").is_err());
    assert!(
        load_yamake_config(r#"{"targets": [{"id": 1, "name": "a", "target_type": "bogus"}]}"#)
            .is_err()
    );
}
