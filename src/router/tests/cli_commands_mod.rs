use super::*;
use super::server::{model_weights_bytes, parse_prometheus_metrics};
use serde_json::json;
use crate::cli::CliContext;

#[test]
fn prometheus_parse_handles_labels_and_help() {
    let text = "# HELP x total\n# TYPE x counter\nllamacpp:prompt_tokens_total 42\nllamacpp:predicted_tokens_seconds{model=\"x\"} 3.5\nnot_a_number abc\n";
    let metrics = parse_prometheus_metrics(text);
    assert_eq!(metrics.get("llamacpp:prompt_tokens_total"), Some(&42.0));
    assert_eq!(metrics.get("llamacpp:predicted_tokens_seconds"), Some(&3.5));
    assert_eq!(metrics.len(), 2);
}

#[tokio::test]
async fn pull_rejects_name_without_namespace_and_tag() {
    let ctx = CliContext::new(None, true, false, false);
    let err = pull(&ctx, "nomodel", None, false).await.unwrap_err();
    assert!(err.to_string().contains("namespace/model:tag"));
}

#[test]
fn ps_weights_prefer_router_model_bytes_over_config_and_gguf() {
    let dir = tempfile::tempdir().unwrap();
    // A config weights file that exists on disk (would be the old answer).
    let model_dir = dir.path().join("swarm");
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(model_dir.join("latest.gguf"), vec![0u8; 4096]).unwrap();
    let config: RouterConfig = serde_json::from_value(json!({
        "models": {
            "swarm": {
                "endpoint": "http://127.0.0.1:1/v1/chat/completions",
                "name": "abiray/test",
                "weights": model_dir.join("latest.gguf").to_string_lossy(),
                "intelligence": 2,
                "cost_input": 1e-6, "cost_output": 6e-6, "cost_cached_read": 4e-7,
                "tok_s": 8
            }
        }
    }))
    .unwrap();

    // No instance detail: falls back to the config weights file.
    assert_eq!(
        model_weights_bytes(&[], Some(&config), dir.path(), "swarm"),
        4096
    );
    // Router-reported model_bytes wins over the config file size.
    let insts = vec![json!({ "model_bytes": 1_000_000_000u64, "vram_bytes": 100 })];
    assert_eq!(
        model_weights_bytes(&insts, Some(&config), dir.path(), "swarm"),
        1_000_000_000
    );
    // A sleeping plain model reports model_bytes = 0: its weights are NOT
    // resident, so 0 is returned - never the on-disk weights file size.
    let sleeping = vec![json!({ "model_bytes": 0u64, "vram_bytes": 0 })];
    assert_eq!(
        model_weights_bytes(&sleeping, Some(&config), dir.path(), "swarm"),
        0,
        "sleeping weights are not resident"
    );
    // No config, no instance → GGUF layout still resolves the file.
    assert_eq!(
        model_weights_bytes(&[], None, dir.path(), "swarm"),
        4096
    );
    // Nothing at all → 0 (never crashes).
    assert_eq!(model_weights_bytes(&[], None, dir.path(), "absent"), 0);
}

#[test]
fn ls_markdown_shape_is_pinned() {
    use crate::cli::commands::filesystem::{render_config_listing, ConfigListing};
    let listing = ConfigListing {
        routes: vec![
            ("code".into(), "code".into()),
            ("local".into(), "default".into()),
        ],
        groups: vec![
            ("code".into(), vec!["qwen".into()]),
            ("default".into(), vec!["lfm".into(), "tiny".into()]),
        ],
        models: vec![
            ("tiny".into(), 1, None),
            ("big".into(), 5, Some("/w/big.gguf".into())),
        ],
    };
    assert_eq!(
        render_config_listing(&listing),
        "## Routes\n\
         | code | local |\n\
         | --- | --- |\n\
         | code | default |\n\
         \n\
         ## Model groups\n\
         | group | models |\n\
         | --- | --- |\n\
         | code | qwen |\n\
         | default | lfm, tiny |\n\
         \n\
         ## Models\n\
         | model | intelligence | weights |\n\
         | --- | --- | --- |\n\
         | tiny | 1 | - |\n\
         | big | 5 | /w/big.gguf |\n"
    );
}

#[test]
fn ls_markdown_escapes_pipes() {
    use crate::cli::commands::filesystem::{render_config_listing, ConfigListing};
    let listing = ConfigListing {
        routes: vec![("a|b".into(), "g".into())],
        groups: vec![],
        models: vec![],
    };
    let text = render_config_listing(&listing);
    assert!(text.contains("a\\|b"), "pipes escaped, got:\n{text}");
    assert!(text.contains("(none)"), "empty groups section marked, got:\n{text}");
}

#[test]
fn ls_empty_config_has_headers_no_rows() {
    use crate::cli::commands::filesystem::{render_config_listing, ConfigListing};
    let listing = ConfigListing {
        routes: vec![],
        groups: vec![],
        models: vec![],
    };
    let text = render_config_listing(&listing);
    assert!(text.contains("## Routes"));
    assert!(text.contains("## Model groups"));
    assert!(text.contains("| model | intelligence | weights |"));
    assert!(!text.contains("| tiny |"), "no model rows");
}

#[test]
fn ls_json_round_trips_losslessly() {
    use crate::cli::commands::filesystem::{render_config_json, ConfigListing};
    let listing = ConfigListing {
        routes: vec![("local".into(), "default".into())],
        groups: vec![("default".into(), vec!["lfm".into(), "tiny".into()])],
        models: vec![("tiny".into(), 1, None)],
    };
    let value = render_config_json(&listing);
    assert_eq!(value["routes"]["local"], serde_json::json!("default"));
    assert_eq!(value["groups"]["default"], serde_json::json!(["lfm", "tiny"]));
    assert_eq!(value["models"][0]["key"], serde_json::json!("tiny"));
    assert!(value["models"][0]["weights"].is_null());
    // And it prints as valid JSON text.
    let text = serde_json::to_string_pretty(&value).expect("serializes");
    let back: serde_json::Value = serde_json::from_str(&text).expect("parses");
    assert_eq!(back, value);
}

#[test]
fn ls_missing_file_errors() {
    use crate::cli::commands::filesystem::list;
    let ctx = CliContext::new(None, true, false, false);
    let err = list(
        &ctx,
        std::path::Path::new("/nonexistent-dir-xyz/coral-router.json"),
        false,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("nonexistent-dir-xyz"),
        "error names the path, got: {err}"
    );
}

#[test]
fn ls_lists_fixture_config_sections() {
    use crate::cli::commands::filesystem::load_listing;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/ls_fixture.json");
    let listing = load_listing(&path).expect("fixture loads");
    assert_eq!(
        listing.routes,
        vec![
            ("code".to_string(), "dev".to_string()),
            ("local".to_string(), "dev".to_string()),
        ]
    );
    assert_eq!(
        listing.groups,
        vec![("dev".to_string(), vec!["m1".to_string(), "m2".to_string()])]
    );
    assert_eq!(
        listing.models,
        vec![
            ("m1".to_string(), 1, None),
            ("m2".to_string(), 5, Some("/w/m2.gguf".to_string())),
        ]
    );
}
