use super::*;
use serde_json::json;

fn llama_row() -> Value {
    json!({
        "id": "base:ledger",
        "n_ctx": 128000u64,
        "parallel": 1,
        "pinned": true,
        "resume": false,
        "state": "loaded",
        "vram_bytes": 1_700_000_000u64,
        "model_bytes": 1_800_000_000u64,
        "quant": "q4_0",
        "last_used": -1,
    })
}

fn listed(id: &str) -> Vec<Value> {
    vec![json!({ "id": id })]
}

#[test]
fn header_carries_vram_sysram_ctx_quant_kv_live_columns() {
    let dir = tempfile::tempdir().unwrap();
    let (rows, totals) = collect_ps_rows(
        None,
        dir.path(),
        &listed("base:ledger"),
        &[llama_row()],
        &[],
    );
    let text = render_ps_table_sized(&rows, &totals, 100);
    for col in [
        "MODEL", "INSTANCE", "VRAM", "SYS RAM", "CTX", "QUANT", "KV", "LIVE",
    ] {
        assert!(text.contains(col), "missing column {col}:\n{text}");
    }
}

#[test]
fn row_shows_ctx_window_weights_quant_and_sleep() {
    let dir = tempfile::tempdir().unwrap();
    let (rows, totals) = collect_ps_rows(
        None,
        dir.path(),
        &listed("base:ledger"),
        &[llama_row()],
        &[],
    );
    // Weights row, instance row, TOTAL row.
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].model, "base");
    assert_eq!(rows[0].live, "loaded");
    assert_eq!(rows[0].vram, "1.8 GB");
    assert_eq!(rows[0].sys_ram, "-");
    let row = &rows[1];
    assert_eq!(row.ctx, "128000");
    assert_eq!(row.quant, "q4_0");
    assert_eq!(row.live, "Forever");
    assert_eq!(row.instance, "ledger");
    assert_eq!(row.vram, "1.7 GB");
    let total = &rows[2];
    assert_eq!(total.instance, "TOTAL");
    assert_eq!(total.vram, "3.5 GB");
    let text = render_ps_table_sized(&rows, &totals, 100);
    assert!(text.contains("128000"), "ctx window shown:\n{text}");
    assert!(text.contains("q4_0"), "weights quant shown:\n{text}");
    assert!(text.contains("Forever"), "sleep shown:\n{text}");
}

#[test]
fn unknown_weights_row_renders_dash_never_disk_size() {
    // A grammar-less (plain) server reports no byte counts: its envelope
    // carries zeros, and the weights row must render `-` — the on-disk
    // weights size must never appear as VRAM.
    let dir = tempfile::tempdir().unwrap();
    let mut plain = llama_row();
    plain["model_bytes"] = json!(0u64);
    plain["vram_bytes"] = json!(0u64);
    plain["total_bytes"] = json!(0u64);
    let (rows, totals) = collect_ps_rows(None, dir.path(), &listed("base:ledger"), &[plain], &[]);
    assert_eq!(
        rows[0].vram, "-",
        "unknown weights render as dash: {rows:?}"
    );
    assert_eq!(rows[1].vram, "-", "unknown instance VRAM renders as dash");
    assert_eq!(
        totals.resident_bytes, 0,
        "unknown contributes nothing resident"
    );
}

#[test]
fn kv_cache_quant_defaults_to_q8_0_without_config() {
    let dir = tempfile::tempdir().unwrap();
    let (rows, _) = collect_ps_rows(
        None,
        dir.path(),
        &listed("base:ledger"),
        &[llama_row()],
        &[],
    );
    assert_eq!(rows[1].kv, "q8_0");
    let text = render_ps_table_sized(
        &rows,
        &PsTotals {
            resident_bytes: 0,
            models: 1,
            instances: 1,
        },
        100,
    );
    assert!(text.contains("q8_0"), "KV-cache quant shown:\n{text}");
}

#[test]
fn kv_cache_quant_follows_model_spawn_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let config: RouterConfig = serde_json::from_value(json!({
        "models": {
            "base": {
                "endpoint": "http://127.0.0.1:1/v1/chat/completions",
                "params": { "cache_type_k": "q4_0", "cache_type_v": "q4_0" }
            }
        }
    }))
    .unwrap();
    let (rows, _) = collect_ps_rows(
        Some(&config),
        dir.path(),
        &listed("base:ledger"),
        &[llama_row()],
        &[],
    );
    assert_eq!(rows[1].kv, "q4_0");
}

#[test]
fn kv_cache_quant_split_renders_k_slash_v() {
    let dir = tempfile::tempdir().unwrap();
    let config: RouterConfig = serde_json::from_value(json!({
        "models": {
            "base": {
                "endpoint": "http://127.0.0.1:1/v1/chat/completions",
                "params": { "cache_type_k": "q4_0", "cache_type_v": "q8_0" }
            }
        }
    }))
    .unwrap();
    let (rows, _) = collect_ps_rows(
        Some(&config),
        dir.path(),
        &listed("base:ledger"),
        &[llama_row()],
        &[],
    );
    assert_eq!(rows[1].kv, "q4_0/q8_0");
}

#[test]
fn idle_countdown_shows_time_left_before_sleep() {
    let dir = tempfile::tempdir().unwrap();
    let now = common_core::now_secs();
    let row = json!({
        "id": "base:scratch",
        "n_ctx": 16384u64,
        "pinned": false,
        "sleep_idle_seconds": 300,
        "state": "loaded",
        "vram_bytes": 100u64,
        "total_bytes": 100u64,
        "model_bytes": 1000u64,
        "last_used": now - 170,  // 130s of the 300s window left
    });
    let (rows, _) = collect_ps_rows(None, dir.path(), &listed("base:scratch"), &[row], &[]);
    // Weights row, instance row, TOTAL row. ~130s of the 300s window left;
    // the keep-alive format renders that as `2m` (seconds dropped for
    // minute-scale values), robust to the collect-time clock ticking.
    assert_eq!(rows[1].live, "2m");
    assert_eq!(rows[2].instance, "TOTAL");
}

#[test]
fn onnx_row_uses_ram_and_runtime_marker() {
    let dir = tempfile::tempdir().unwrap();
    // Real onnx envelope shape: `model_bytes: 0`, `total_bytes` is the
    // CPU-RAM working set alone (no weights counted, no VRAM).
    let row = json!({
        "id": "onnx/llm:default",
        "n_ctx": 16384u64,
        "pinned": true,
        "state": "loaded",
        "runtime": "onnx",
        "total_bytes": 4_000_000_000u64,
        "vram_bytes": 0,
        "model_bytes": 0u64,
    });
    let (rows, totals) =
        collect_ps_rows(None, dir.path(), &listed("onnx/llm:default"), &[row], &[]);
    assert_eq!(rows.len(), 3);
    assert!(rows[0].model.contains("(onnx)"), "got {}", rows[0].model);
    assert_eq!(rows[1].sys_ram, "4 GB", "onnx working set is CPU RAM");
    assert_eq!(rows[1].vram, "-");
    let text = render_ps_table_sized(&rows, &totals, 100);
    assert!(text.contains("(onnx)"), "marker shown:\n{text}");
    assert!(text.contains("default"), "context row shown:\n{text}");
}

#[test]
fn model_without_instances_gets_unloaded_row() {
    let dir = tempfile::tempdir().unwrap();
    let (rows, totals) = collect_ps_rows(None, dir.path(), &listed("swarm"), &[], &[]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].live, "unloaded");
    assert_eq!(rows[0].instance, "-");
    assert_eq!(rows[0].vram, "-");
    let text = render_ps_table_sized(&rows, &totals, 100);
    assert!(text.contains("unloaded"), "state stated outright:\n{text}");
    assert!(text.contains("swarm"), "model listed:\n{text}");
}

#[test]
fn unloaded_model_with_two_windows_renders_one_row() {
    // The live shape: a down server synthesizes one `/v1/models` entry per
    // configured profile (`qwen3.5-9b:ledger` + `qwen3.5-9b:reasoning`) with
    // no instance detail. `ps` groups by model key, so the model renders
    // (and counts) once — never once per window.
    let dir = tempfile::tempdir().unwrap();
    let listed = vec![
        json!({ "id": "qwen3.5-9b:ledger" }),
        json!({ "id": "qwen3.5-9b:reasoning" }),
    ];
    let (rows, totals) = collect_ps_rows(None, dir.path(), &listed, &[], &[]);
    assert_eq!(rows.len(), 1, "one unloaded row per model: {rows:?}");
    assert_eq!(rows[0].live, "unloaded");
    assert_eq!(totals.models, 1, "model counted once");
    assert_eq!(totals.instances, 0);
    let text = render_ps_table_sized(&rows, &totals, 100);
    assert_eq!(
        text.matches("qwen3.5-9b").count(),
        1,
        "no duplicate:\n{text}"
    );
}

#[test]
fn offloaded_model_with_snapshot_renders_distinctly() {
    // Alive-but-empty after eviction: no `/v1/models` entries, no instance
    // detail — only the retained fork-side snapshot. The model must still
    // appear (it would otherwise vanish), as a distinct snapshot row, never
    // as residency: zero bytes, zero instance count.
    let dir = tempfile::tempdir().unwrap();
    let snapshots = vec![json!({
        "name": "ledger-resume",
        "instance": "ledger",
        "model": "qwen3.5-9b",
    })];
    let (rows, totals) = collect_ps_rows(None, dir.path(), &[], &[], &snapshots);
    assert_eq!(rows.len(), 1, "one snapshot row: {rows:?}");
    assert_eq!(rows[0].instance, "snapshot:ledger-resume");
    assert_eq!(rows[0].live, "snapshot");
    assert_eq!(rows[0].vram, "-");
    assert_eq!(totals.models, 1, "snapshot-only model counted");
    assert_eq!(totals.instances, 0, "snapshots are not instances");
    assert_eq!(totals.resident_bytes, 0, "snapshots are not residency");
    let text = render_ps_table_sized(&rows, &totals, 100);
    assert!(text.contains("snapshot:ledger-resume"), "named:\n{text}");
    assert!(text.contains("1 model"), "footer:\n{text}");
}

#[test]
fn unloaded_listing_with_snapshot_shows_both_rows() {
    // Down server (synthesized `unloaded` listing) whose snapshots outlived
    // it: the bare state row plus the distinct snapshot row, model counted
    // once.
    let dir = tempfile::tempdir().unwrap();
    let listed = vec![
        json!({ "id": "qwen3.5-9b:ledger" }),
        json!({ "id": "qwen3.5-9b:reasoning" }),
    ];
    let snapshots = vec![json!({
        "name": "ledger-resume",
        "instance": "ledger",
        "model": "qwen3.5-9b",
    })];
    let (rows, totals) = collect_ps_rows(None, dir.path(), &listed, &[], &snapshots);
    assert_eq!(rows.len(), 2, "unloaded + snapshot: {rows:?}");
    assert_eq!(rows[0].live, "unloaded");
    assert_eq!(rows[1].instance, "snapshot:ledger-resume");
    assert_eq!(rows[1].live, "snapshot");
    assert_eq!(totals.models, 1);
    assert_eq!(totals.instances, 0);
}

#[test]
fn loaded_group_with_snapshot_appends_without_counting() {
    // A resident model that kept a snapshot: the group keeps its
    // weights/instance/TOTAL shape with the snapshot row after it, and the
    // instance count covers live windows only.
    let dir = tempfile::tempdir().unwrap();
    let snapshots = vec![json!({
        "name": "ledger-resume",
        "instance": "ledger",
        "model": "base",
    })];
    let (rows, totals) = collect_ps_rows(
        None,
        dir.path(),
        &listed("base:ledger"),
        &[llama_row()],
        &snapshots,
    );
    assert_eq!(rows.len(), 4, "weights/instance/TOTAL + snapshot: {rows:?}");
    assert_eq!(rows[2].instance, "TOTAL");
    assert_eq!(rows[3].instance, "snapshot:ledger-resume");
    assert_eq!(rows[3].live, "snapshot");
    assert_eq!(totals.models, 1);
    assert_eq!(totals.instances, 1, "snapshot excluded from the count");
}

#[test]
fn latest_symlink_name_carries_no_quant() {
    // A GGUF layout whose only file is `latest.gguf`: the stem is a
    // symlink convention, not a quantization tag, so the column shows `-`.
    let dir = tempfile::tempdir().unwrap();
    let model_dir = dir.path().join("swarm");
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(model_dir.join("latest.gguf"), vec![0u8; 64]).unwrap();
    let (rows, _) = collect_ps_rows(None, dir.path(), &listed("swarm"), &[], &[]);
    assert_eq!(rows[0].quant, "-");
}

#[test]
fn every_line_fits_the_requested_width() {
    let dir = tempfile::tempdir().unwrap();
    let onnx = json!({
        "id": "onnx/llm:default",
        "n_ctx": 16384u64,
        "pinned": true,
        "state": "loaded",
        "runtime": "onnx",
        "total_bytes": 100u64,
        "vram_bytes": 0,
        "model_bytes": 1000u64,
    });
    let listed = vec![
        json!({ "id": "base:ledger" }),
        json!({ "id": "onnx/llm:default" }),
        json!({ "id": "swarm" }),
    ];
    for width in [80u16, 100, 140] {
        let (rows, totals) =
            collect_ps_rows(None, dir.path(), &listed, &[llama_row(), onnx.clone()], &[]);
        let text = render_ps_table_sized(&rows, &totals, width);
        for line in text.lines() {
            assert!(
                line.chars().count() <= usize::from(width),
                "line exceeds {width}: {line:?}"
            );
        }
    }
}

#[test]
fn totals_footer_counts_models_instances_and_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let listed = vec![json!({ "id": "base:ledger" }), json!({ "id": "swarm" })];
    let (rows, totals) = collect_ps_rows(None, dir.path(), &listed, &[llama_row()], &[]);
    assert_eq!(totals.models, 2);
    assert_eq!(totals.instances, 1);
    assert_eq!(totals.resident_bytes, 1_800_000_000 + 1_700_000_000);
    let text = render_ps_table_sized(&rows, &totals, 100);
    assert!(text.contains("2 models"), "model count:\n{text}");
    assert!(text.contains("1 instance"), "instance count:\n{text}");
    assert!(text.contains("resident"), "resident footer:\n{text}");
}

#[test]
fn no_boundary_characters_in_text_output() {
    let dir = tempfile::tempdir().unwrap();
    let (rows, totals) = collect_ps_rows(
        None,
        dir.path(),
        &listed("base:ledger"),
        &[llama_row()],
        &[],
    );
    let text = render_ps_table_sized(&rows, &totals, 100);
    for boundary in ['┌', '┐', '└', '┘', '─', '│', '├', '┤', '┬', '┴', '┼'] {
        assert!(
            !text.contains(boundary),
            "no boundary {boundary}, got:\n{text}"
        );
    }
    // The layout work is kept: header, aligned row, totals footer.
    assert!(text.contains("MODEL"), "header kept:\n{text}");
    assert!(text.contains("ledger"), "row kept:\n{text}");
    assert!(text.contains("resident"), "footer kept:\n{text}");
}

#[test]
fn markdown_twin_renders_pipe_tables_with_full_values() {
    let dir = tempfile::tempdir().unwrap();
    let long_id = "a-very-long-model-name-that-no-terminal-column-could-hold:ledger";
    let listed = vec![json!({ "id": long_id })];
    let (rows, totals) = collect_ps_rows(None, dir.path(), &listed, &[llama_row()], &[]);
    let text = render_ps_markdown(&rows, &totals);
    assert!(
        text.contains("| MODEL | INSTANCE | VRAM | SYS RAM | CTX | QUANT | KV | LIVE |"),
        "header row:\n{text}"
    );
    assert!(text.contains("| --- |"), "separator row:\n{text}");
    assert!(
        text.contains("a-very-long-model-name-that-no-terminal-column-could-hold"),
        "values never truncated:\n{text}"
    );
    assert!(text.contains("resident"), "footer kept:\n{text}");
}
