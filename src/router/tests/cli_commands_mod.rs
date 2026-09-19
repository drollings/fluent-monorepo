use super::server::parse_prometheus_metrics;
use super::*;
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

fn gguf_entry(
    name: &str,
    tag: &str,
    size: u64,
    arch: &str,
    file_type: Option<u32>,
) -> crate::cli::gguf::GgufEntry {
    crate::cli::gguf::GgufEntry {
        name: name.into(),
        tag: tag.into(),
        display: format!("{name}:{tag}"),
        path: std::path::PathBuf::from(format!("/models/{name}/{tag}.gguf")),
        size,
        mtime: 0.0,
        arch: arch.into(),
        file_type,
        short_id: "0123456789ab".into(),
    }
}

#[test]
fn ls_markdown_shape_is_pinned() {
    use crate::cli::commands::filesystem::{render_config_listing, ConfigListing, RouteRow};
    let listing = ConfigListing {
        routes: vec![
            RouteRow {
                route: "code".into(),
                model: "qwen".into(),
                ctx: Some(65536),
                reasoning: "medium".into(),
            },
            RouteRow {
                route: "local".into(),
                model: "lfm".into(),
                ctx: Some(8192),
                reasoning: "none".into(),
            },
        ],
        models: vec![],
    };
    let models = vec![
        gguf_entry("big", "q8_0", 6_500_000_000, "qwen35", Some(7)),
        gguf_entry("tiny", "latest", 219_000_000, "lfm2", Some(2)),
    ];
    let text = render_config_listing(&listing, &models);
    // Same sections, order, columns, and rows as the plain-text screen:
    // models table first (no heading), then ROUTES.
    assert!(
        text.starts_with(
            "## MODELS\n| NAME | SIZE | QUANT | ARCH | MODIFIED |\n| --- | --- | --- | --- | --- |"
        ),
        "models table leads, got:\n{text}"
    );
    assert!(
        text.contains(
            "## ROUTES\n| ROUTE | MODEL | CTX | REASONING |\n| --- | --- | --- | --- |\n| code | qwen | 65536 | medium |\n| local | lfm | 8192 | none |\n"
        ),
        "routes section pinned, got:\n{text}"
    );
    // Models section: `gguf_tool ls` columns; the MODIFIED cell is
    // relative-time (volatile), so the golden pins everything else.
    assert!(
        text.contains("| NAME | SIZE | QUANT | ARCH | MODIFIED |\n| --- | --- | --- | --- | --- |"),
        "models table header:\n{text}"
    );
    assert!(
        text.contains("| big:q8_0 | 6.5 GB | q8_0 | qwen35 |"),
        "big row (sorted first):\n{text}"
    );
    assert!(
        text.contains("| tiny:latest | 219 MB | q4_0 | lfm2 |"),
        "tiny row: empty/latest tag falls back to file_type, got:\n{text}"
    );
}

#[test]
fn ls_markdown_escapes_pipes() {
    use crate::cli::commands::filesystem::{render_config_listing, ConfigListing, RouteRow};
    let listing = ConfigListing {
        routes: vec![RouteRow {
            route: "a|b".into(),
            model: "g".into(),
            ctx: Some(8),
            reasoning: "none".into(),
        }],
        models: vec![],
    };
    let text = render_config_listing(&listing, &[]);
    assert!(text.contains("a\\|b"), "pipes escaped, got:\n{text}");
    assert!(
        text.contains("## ROUTES\n| ROUTE | MODEL | CTX | REASONING |"),
        "routes header present, got:\n{text}"
    );
}

#[test]
fn ls_json_round_trips_losslessly() {
    use crate::cli::commands::filesystem::{render_config_json, ConfigListing, RouteRow};
    let listing = ConfigListing {
        routes: vec![RouteRow {
            route: "local".into(),
            model: "lfm".into(),
            ctx: Some(8192),
            reasoning: "none".into(),
        }],
        models: vec![],
    };
    let models = vec![gguf_entry("tiny", "q4_0", 219_000_000, "lfm2", Some(2))];
    let value = render_config_json(&listing, &models);
    assert_eq!(
        value["routes"]["local"],
        serde_json::json!([{"model": "lfm", "ctx": 8192, "reasoning": "none"}])
    );
    assert!(
        value.get("roles").is_none(),
        "roles section retired, got: {value}"
    );
    assert_eq!(value["models"][0]["name"], serde_json::json!("tiny:q4_0"));
    assert_eq!(
        value["models"][0]["size_bytes"],
        serde_json::json!(219_000_000u64)
    );
    assert_eq!(value["models"][0]["quant"], serde_json::json!("q4_0"));
    assert_eq!(value["models"][0]["arch"], serde_json::json!("lfm2"));
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
        TableFormat::Text,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("nonexistent-dir-xyz"),
        "error names the path, got: {err}"
    );
}

#[test]
fn ls_lists_fixture_config_sections() {
    use crate::cli::commands::filesystem::{load_listing, RouteRow};
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/ls_fixture.json");
    let listing = load_listing(&path).expect("fixture loads");
    // Both routes ride the `quick` role, whose `models` chain lists `m1`
    // then `m2`: one row per fallback level in configured order, the window
    // is the role's default `num_ctx`, and nothing enables thinking.
    let m1 = |route: &str| RouteRow {
        route: route.to_string(),
        model: "m1".to_string(),
        ctx: Some(16384),
        reasoning: "none".to_string(),
    };
    let m2 = |route: &str| RouteRow {
        route: route.to_string(),
        model: "m2".to_string(),
        ctx: Some(16384),
        reasoning: "none".to_string(),
    };
    assert_eq!(
        listing.routes,
        vec![m1("code"), m2("code"), m1("local"), m2("local"),]
    );
    assert_eq!(
        listing.models,
        vec![
            ("m1".to_string(), 1, None),
            ("m2".to_string(), 5, Some("/w/m2.gguf".to_string())),
        ]
    );
}

#[test]
fn ls_sentinel_fallback_levels_render_as_dashes() {
    use crate::cli::commands::filesystem::{listing_from_config, RouteRow};
    use crate::config::RouterConfig;
    let cfg = RouterConfig::from_json_str(
        r#"{
            "models": {
                "m1": {"endpoint": "http://a", "intelligence": 1}
            },
            "roles": {
                "r": {
                    "params": {"num_ctx": 4096},
                    "models": ["last", "m1", "any"]
                }
            },
            "classification": {
                "root": {
                    "type": "classifier",
                    "description": "d",
                    "model": "m1",
                    "children": [
                        {"key": "code", "description": "",
                         "node": {"type": "terminal", "route": "code",
                                  "description": "", "role": "r"}}
                    ]
                }
            },
            "default_route": "code"
        }"#,
    )
    .expect("sentinel fixture parses");
    assert_eq!(
        listing_from_config(&cfg).routes,
        vec![
            RouteRow {
                route: "code".to_string(),
                model: "-".to_string(),
                ctx: None,
                reasoning: "-".to_string(),
            },
            RouteRow {
                route: "code".to_string(),
                model: "m1".to_string(),
                ctx: Some(4096),
                reasoning: "none".to_string(),
            },
            RouteRow {
                route: "code".to_string(),
                model: "-".to_string(),
                ctx: None,
                reasoning: "-".to_string(),
            },
        ]
    );
}

#[test]
fn table_format_from_markdown_flag() {
    assert_eq!(TableFormat::from_markdown_flag(false), TableFormat::Text);
    assert_eq!(TableFormat::from_markdown_flag(true), TableFormat::Markdown);
    assert_eq!(TableFormat::default(), TableFormat::Text);
}
