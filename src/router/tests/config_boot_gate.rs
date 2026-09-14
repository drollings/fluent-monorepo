use super::*;

fn boot_fixture(name: &str) -> RoutingConfig {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name);
    let text = std::fs::read_to_string(&path).expect("fixture readable");
    serde_json::from_str(&text).expect("fixture parses as RoutingConfig")
}

#[test]
fn boot_gate_accepts_valid_shape() {
    let cfg = boot_fixture("boot_valid.json");
    cfg.validate_for_boot(None)
        .expect("valid shape passes the boot gate");
}

#[test]
fn boot_gate_rejects_unknown_group() {
    let cfg = boot_fixture("boot_invalid_group.json");
    let err = cfg
        .validate_for_boot(None)
        .expect_err("unknown group must fail closed");
    assert!(
        err.contains("no-such-group") && err.contains("code"),
        "error names the group and the route, got: {err}"
    );
}

#[test]
fn boot_gate_rejects_unknown_role() {
    let cfg = boot_fixture("boot_invalid_role.json");
    let err = cfg
        .validate_for_boot(None)
        .expect_err("unknown role must fail closed");
    assert!(
        err.contains("nope") && err.contains("code"),
        "error names the role and the route, got: {err}"
    );
}

#[test]
fn boot_gate_rejects_unknown_member() {
    let cfg = boot_fixture("boot_invalid_member.json");
    let err = cfg
        .validate_for_boot(None)
        .expect_err("unknown member must fail closed");
    assert!(
        err.contains("zzz-unknown") && err.contains("dev"),
        "error names the member and the group, got: {err}"
    );
}

#[test]
fn boot_gate_validates_terminal_roles() {
    let cfg = boot_fixture("boot_valid.json");
    let tree: crate::config::ClassificationTree = serde_json::from_value(serde_json::json!({
        "root": {"type": "classifier", "description": "d", "model": "tiny", "children": [
            {"key": "code", "description": "", "node": {
                "type": "terminal", "route": "code",
                "group": "dev", "role": "quick", "description": "",
            }},
        ]}
    }))
    .expect("tree parses");
    // Materialize pools the way boot does so role profiles resolve.
    let mut cfg = cfg;
    let keys: Vec<String> = cfg.models.keys().cloned().collect();
    for key in &keys {
        let pool = crate::config::materialize_effective_pool(key, &cfg.roles);
        cfg.models.get_mut(key).expect("member").effective_profiles = Some(pool);
    }
    cfg.validate_for_boot(Some(&tree))
        .expect("served terminal role validates");
}

#[test]
fn presence_gate_rejects_missing_referenced_weights() {
    let cfg: crate::config::RouterConfig = serde_json::from_value(serde_json::json!({
        "models": {
            "m": {"weights": "/nonexistent-dir-xyz/m.gguf", "intelligence": 1}
        },
        "model_groups": {"g": ["m"]},
        "default_route": "local",
    }))
    .expect("parses");
    let err = cfg
        .validate_model_presence(std::path::Path::new("/tmp"))
        .expect_err("missing referenced weights must fail closed");
    assert!(
        err.contains('m') && err.contains("nonexistent-dir-xyz"),
        "error names the model and the path, got: {err}"
    );
}

#[test]
fn presence_gate_accepts_present_weights_and_skips_template() {
    let dir = std::env::temp_dir().join("coral-boot-gate-weights");
    std::fs::create_dir_all(&dir).expect("tempdir");
    let gguf = dir.join("m.gguf");
    std::fs::write(&gguf, b"stub").expect("stub weights");
    let cfg: crate::config::RouterConfig = serde_json::from_value(serde_json::json!({
        "models": {
            "default": {"intelligence": 1},
            "m": {"weights": gguf.to_string_lossy(), "intelligence": 1},
            "idle": {"weights": "/nonexistent-dir-xyz/idle.gguf", "intelligence": 1}
        },
        "model_groups": {"g": ["m"]},
        "default_route": "local",
    }))
    .expect("parses");
    cfg.validate_model_presence(std::path::Path::new("/tmp"))
        .expect("present referenced weights pass; template skipped");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn presence_gate_warns_over_unreferenced_missing_weights() {
    let cfg: crate::config::RouterConfig = serde_json::from_value(serde_json::json!({
        "models": {
            "m": {"endpoint": "http://a", "intelligence": 1},
            "idle": {"weights": "/nonexistent-dir-xyz/idle.gguf", "intelligence": 1}
        },
        "model_groups": {"g": ["m"]},
        "default_route": "local",
    }))
    .expect("parses");
    let ((), logs) = crate::test_support::capture_logs(|| {
        cfg.validate_model_presence(std::path::Path::new("/tmp"))
            .expect("unreferenced missing weights warn, not fail");
    });
    assert!(
        logs.join("\n").contains("idle"),
        "warning names the unreferenced model"
    );
}

#[test]
fn presence_gate_rejects_endpointless_external_model() {
    let cfg: crate::config::RouterConfig = serde_json::from_value(serde_json::json!({
        "models": {
            "m": {"intelligence": 1}
        },
        "model_groups": {"g": ["m"]},
        "default_route": "local",
    }))
    .expect("parses");
    let err = cfg
        .validate_model_presence(std::path::Path::new("/tmp"))
        .expect_err("referenced model with no weights and no endpoint must fail");
    assert!(err.contains('m'), "error names the model, got: {err}");
}

#[test]
fn config_schema_derives_from_types() {
    let schema = crate::config::config_schema();
    assert_eq!(
        schema["source"].as_str(),
        Some("derived from config types via Describable")
    );
    assert!(
        schema["sections"]["config"].is_object(),
        "top-level inventory present"
    );
}
