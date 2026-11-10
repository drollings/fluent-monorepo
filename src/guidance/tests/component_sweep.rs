//! Component-vector sweep: changing one input backdates exactly the
//! receipts that consumed it. Prompt rotation never re-lints; lint
//! config never re-embeds; toolchain moves format and lint but not
//! embeddings. Work keys reuse stored receipts on match.

use std::collections::BTreeMap;

use crate::components::{
    receipt_reusable, ComponentInputs, ComponentVector, COMP_EMBEDDER, COMP_LINT_CFG,
    COMP_PROMPT_PREFIX, COMP_TOOLCHAIN,
};
use crate::policy::MaintenancePolicy;

fn inputs_with(prompt: &str, lint: &str, toolchain: &str, embedder: &str) -> ComponentInputs {
    ComponentInputs {
        toolchain: Some(toolchain.to_string()),
        lint_cfg: Some(lint.to_string()),
        prompt_triag: Some(prompt.to_string()),
        embedder: Some(embedder.to_string()),
        ..Default::default()
    }
}

#[test]
fn prompt_change_backdates_only_model_cache_receipts() {
    let policy = MaintenancePolicy::default();
    let before = ComponentVector::current_with(&policy, &inputs_with("p1", "lint", "tc", "emb"));
    let after = ComponentVector::current_with(&policy, &inputs_with("p2", "lint", "tc", "emb"));
    assert!(after.components.contains_key("prompt:triage"));
    let mut prompt_receipt = BTreeMap::new();
    prompt_receipt.insert(
        "prompt:triage".to_string(),
        before.components["prompt:triage"].clone(),
    );
    let mut lint_receipt = BTreeMap::new();
    lint_receipt.insert(COMP_LINT_CFG.to_string(), before.components[COMP_LINT_CFG].clone());
    assert!(
        !receipt_reusable(&prompt_receipt, &after.components, &["prompt:triage"]),
        "prompt receipt must invalidate on prompt rotation"
    );
    assert!(
        receipt_reusable(&lint_receipt, &after.components, &[COMP_LINT_CFG]),
        "lint receipt must survive prompt rotation"
    );
}

#[test]
fn lint_change_backdates_only_lint_receipts() {
    let policy = MaintenancePolicy::default();
    let before = ComponentVector::current_with(&policy, &inputs_with("p", "lint-1", "tc", "emb"));
    let after = ComponentVector::current_with(&policy, &inputs_with("p", "lint-2", "tc", "emb"));
    let mut lint_receipt = BTreeMap::new();
    lint_receipt.insert(COMP_LINT_CFG.to_string(), before.components[COMP_LINT_CFG].clone());
    assert!(!receipt_reusable(&lint_receipt, &after.components, &[COMP_LINT_CFG]));
    let mut prompt_receipt = BTreeMap::new();
    prompt_receipt.insert(
        "prompt:triage".to_string(),
        before.components["prompt:triage"].clone(),
    );
    assert!(receipt_reusable(&prompt_receipt, &after.components, &["prompt:triage"]));
}

#[test]
fn toolchain_moves_toolchain_consumers_but_not_embeddings() {
    let policy = MaintenancePolicy::default();
    let before = ComponentVector::current_with(&policy, &inputs_with("p", "lint", "tc-1", "emb"));
    let after = ComponentVector::current_with(&policy, &inputs_with("p", "lint", "tc-2", "emb"));
    // A lint receipt that consumed the toolchain (as real lint receipts
    // do) invalidates; one that consumed only lint config survives.
    let mut lint_with_toolchain = BTreeMap::new();
    lint_with_toolchain.insert(COMP_TOOLCHAIN.to_string(), before.components[COMP_TOOLCHAIN].clone());
    lint_with_toolchain.insert(COMP_LINT_CFG.to_string(), before.components[COMP_LINT_CFG].clone());
    assert!(
        !receipt_reusable(
            &lint_with_toolchain,
            &after.components,
            &[COMP_TOOLCHAIN, COMP_LINT_CFG]
        ),
        "toolchain consumer must invalidate on toolchain change"
    );
    let mut lint_only = BTreeMap::new();
    lint_only.insert(COMP_LINT_CFG.to_string(), before.components[COMP_LINT_CFG].clone());
    assert!(
        receipt_reusable(&lint_only, &after.components, &[COMP_LINT_CFG]),
        "narrow lint-only receipt survives toolchain change"
    );
    let mut emb_receipt = BTreeMap::new();
    emb_receipt.insert(COMP_EMBEDDER.to_string(), before.components[COMP_EMBEDDER].clone());
    assert!(
        receipt_reusable(&emb_receipt, &after.components, &[COMP_EMBEDDER]),
        "embeddings must survive toolchain change"
    );
}

#[test]
fn work_key_match_reuses_mismatch_reprocesses() {
    use crate::item_hashes::work_key;
    let policy = MaintenancePolicy::default();
    let vector = ComponentVector::current_with(&policy, &ComponentInputs::default());
    let key = work_key("item-1", &["sig-a"], &vector.components);
    assert!(crate::item_hashes::work_key_matches(
        "item-1",
        &["sig-a"],
        &vector.components,
        Some(&key)
    ));
    assert!(!crate::item_hashes::work_key_matches(
        "item-1",
        &["sig-b"],
        &vector.components,
        Some(&key)
    ));
    assert!(!crate::item_hashes::work_key_matches(
        "item-1",
        &["sig-a"],
        &vector.components,
        None
    ));
}

#[test]
fn sweep_end_to_end_backdates_only_consumers() {
    use crate::components::sweep_sidecars;
    use fluent_types::sidecar::SidecarHeader;
    use fluent_wvr_testutil::tempdir;
    let sidecars = tempdir();
    let sources = tempdir();
    let policy = MaintenancePolicy::default();
    let before = ComponentVector::current_with(&policy, &inputs_with("p1", "lint", "tc", "emb"));
    std::fs::write(sources.path().join("prompt.rs"), "fn a() {}").expect("src");
    std::fs::write(sources.path().join("lint.rs"), "fn b() {}").expect("src");
    for (name, key) in [
        ("prompt.rs", "prompt:triage"),
        ("lint.rs", COMP_LINT_CFG),
    ] {
        let header = SidecarHeader {
            src: name.to_string(),
            body: "b".to_string(),
            src_hash: None,
            components: [(key.to_string(), before.components[key].clone())]
                .into_iter()
                .collect(),
        };
        std::fs::write(
            sidecars.path().join(format!("{name}.json")),
            header.render() + "\n{}",
        )
        .expect("sidecar");
        std::fs::File::options()
            .write(true)
            .open(sidecars.path().join(format!("{name}.json")))
            .expect("open")
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .expect("touch");
    }
    let after = ComponentVector::current_with(&policy, &inputs_with("p2", "lint", "tc", "emb"));
    let outcomes = sweep_sidecars(sidecars.path(), sources.path(), &after);
    let prompt = outcomes
        .iter()
        .find(|o| o.path.ends_with("prompt.rs.json"))
        .expect("prompt outcome");
    let lint = outcomes
        .iter()
        .find(|o| o.path.ends_with("lint.rs.json"))
        .expect("lint outcome");
    assert_eq!(
        prompt.decision,
        crate::components::SweepDecision::Backdated,
        "prompt consumer backdates on prompt rotation"
    );
    assert_eq!(
        lint.decision,
        crate::components::SweepDecision::Current,
        "lint receipt survives prompt rotation"
    );
    let _ = COMP_PROMPT_PREFIX;
    let _ = COMP_TOOLCHAIN;
}
