//! Byte-parity oracle for the config pre-typing normalizer legs.
//!
//! Each case runs the raw-JSON legs in constructor order
//! (`normalize_model_defaults` → `normalize_fleet_role_params`) and pins the
//! result: normalized documents as `to_string_pretty` snapshots under
//! `tests/data/config_normalize/`. The snapshots were hand-reviewed against
//! the leg semantics when recorded; any behavior change fails here first.
//!
//! To re-record snapshots after an intentional change, run with
//! `UPDATE_SNAPSHOTS=1`, review the `git diff` of the snapshot directory
//! by hand, then run again normally. Never commit unreviewed snapshots.
use super::*;

use std::path::PathBuf;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/config_normalize")
}

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../env")
        .join(name)
}

/// Run the pre-typing legs in constructor order over a cloned document.
fn normalize(value: serde_json::Value) -> serde_json::Value {
    let mut value = value;
    normalize_model_defaults(&mut value);
    normalize_fleet_role_params(&mut value);
    value
}

/// Pin the normalized pretty document to a checked-in snapshot file.
fn expect_snapshot(name: &str, input: serde_json::Value) {
    let normalized = normalize(input);
    let pretty = serde_json::to_string_pretty(&normalized).expect("normalized value serializes");
    let path = snapshot_dir().join(format!("{name}.snap"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(snapshot_dir()).expect("snapshot dir writable");
        std::fs::write(&path, format!("{pretty}\n")).expect("snapshot writable");
        return;
    }
    let expected =
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing snapshot {name}.snap"));
    assert_eq!(
        format!("{pretty}\n"),
        expected,
        "normalized document drift for {name}"
    );
}

fn env_case(name: &str, file: &str) {
    let text = std::fs::read_to_string(env_path(file))
        .unwrap_or_else(|_| panic!("env corpus file {file} readable"));
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("{file} parses as JSON"));
    expect_snapshot(name, value);
}

// -- Shipped corpus -----------------------------------------------------------

#[test]
fn parity_shipped_coral_router_json() {
    env_case("shipped_coral_router_json", "coral-router.json");
}

#[test]
fn parity_shipped_coral_router_example() {
    env_case("shipped_coral_router_example", "coral-router.json.example");
}

#[test]
fn parity_shipped_coral_router_tree() {
    env_case("shipped_coral_router_tree", "coral-router.tree.json");
}

// -- One case per normalizer branch -------------------------------------------

#[test]
fn parity_fleet_run_block_sparse_merge() {
    expect_snapshot(
        "fleet_run_block_sparse_merge",
        serde_json::json!({
            "roles": {
                "default": {
                    "params": {
                        "num_ctx": 8192, "batch_size": 512,
                        "temperature": 0.1, "top_k": 40
                    }
                },
                "spare": {
                    "params": {"batch_size": 1024, "top_k": 10}
                },
                "bare": {}
            }
        }),
    );
}

#[test]
fn parity_models_default_template_fill() {
    expect_snapshot(
        "models_default_template_fill",
        serde_json::json!({
            "models": {
                "default": {
                    "intelligence": 1,
                    "cost_input": 0.000001,
                    "tok_s": 20,
                    "total_timeout_ms": 50000,
                    "embedding": "/models/base-embed.gguf",
                    "params": {"num_ctx": 8192, "temperature": 0.1}
                },
                "bare": {
                    "endpoint": "http://x/v1/chat/completions"
                },
                "tuned": {
                    "endpoint": "http://y/v1/chat/completions",
                    "intelligence": 5,
                    "tok_s": 42,
                    "params": {"temperature": 0.9}
                }
            }
        }),
    );
}

#[test]
fn parity_models_default_feeds_fleet() {
    expect_snapshot(
        "models_default_feeds_fleet",
        serde_json::json!({
            "models": {
                "default": {"params": {"num_ctx": 8192, "batch_size": 2048, "temperature": 0.1}}
            },
            "roles": {"default": {}}
        }),
    );
}

#[test]
fn parity_template_key_inheritance() {
    expect_snapshot(
        "template_key_inheritance",
        serde_json::json!({
            "models": {
                "default": {"template": "/fleet/template.txt"},
                "a": {"endpoint": "http://x/v1/chat/completions"},
                "b": {"endpoint": "http://x/v1/chat/completions", "template": "/own/t.txt"},
            },
        }),
    );
}

#[test]
fn parity_no_models_default_leaves_document_untouched() {
    expect_snapshot(
        "no_models_default_untouched",
        serde_json::json!({
            "models": {"m": {"endpoint": "http://x/v1/chat/completions"}},
            "roles": {"spare": {"params": {"batch_size": 1024}}},
        }),
    );
}

#[test]
fn parity_non_object_model_entry_skipped() {
    expect_snapshot(
        "non_object_model_entry_skipped",
        serde_json::json!({
            "models": {
                "default": {"intelligence": 1},
                "weird": "not-an-entry",
                "m": {"endpoint": "http://x/v1/chat/completions"},
            },
        }),
    );
}

#[test]
fn parity_sibling_declaring_every_key_unchanged() {
    expect_snapshot(
        "sibling_declaring_every_key_unchanged",
        serde_json::json!({
            "models": {
                "default": {"intelligence": 1, "params": {"temperature": 0.1}},
                "full": {
                    "endpoint": "http://x/v1/chat/completions",
                    "intelligence": 5,
                    "params": {"temperature": 0.9, "top_k": 10},
                },
            },
        }),
    );
}

#[test]
fn parity_no_default_role_leaves_siblings_untouched() {
    expect_snapshot(
        "no_default_role_untouched",
        serde_json::json!({
            "models": {"default": {"params": {"temperature": 0.1}}},
            "roles": {"spare": {"params": {"top_k": 10}}},
        }),
    );
}

#[test]
fn parity_template_becomes_fleet_when_default_has_no_params() {
    // `roles.default` declares no run block, so the models template's run
    // block IS the fleet block, then inherited whole by siblings.
    expect_snapshot(
        "template_becomes_fleet_without_default_params",
        serde_json::json!({
            "models": {"default": {"params": {"temperature": 0.1}}},
            "roles": {"default": {}, "spare": {}},
        }),
    );
}

#[test]
fn parity_explicit_null_params_never_filled() {
    // An explicit-`null` sibling `params` is present (never filled from the
    // template); only an absent `params` inherits, and only object pairs
    // shallow-merge.
    expect_snapshot(
        "explicit_null_params_never_filled",
        serde_json::json!({
            "models": {
                "default": {"params": {"temperature": 0.1}},
                "nulled": {"endpoint": "http://x/v1/chat/completions", "params": null},
                "absent": {"endpoint": "http://x/v1/chat/completions"},
            },
        }),
    );
}

#[test]
fn parity_non_object_models_default_leaves_document_untouched() {
    expect_snapshot(
        "non_object_models_default_untouched",
        serde_json::json!({
            "models": {
                "default": "not-an-entry",
                "m": {"endpoint": "http://x/v1/chat/completions"},
            },
        }),
    );
}
