//! Filesystem admin commands (`list`, `scan`, `rm`, `show`, `pull`) operating
//! on the GGUF model layout.
//!
//! These are **operator tooling**: they run in a standalone CLI process
//! before the server has established a `CURRENT_CAPS` scope, and they
//! deliberately manipulate the model store (`rm -rf`, downloads, preset
//! writes). They are **capability-exempt** by design and are NOT gated with the
//! `FsCapability` token that the serving path enforces — an operator running
//! `coral-router` against the filesystem is exercising explicit, authorized
//! authority, not an ambient effect inside the serving request path.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use reqwest::Client;
use serde_json::{json, Value};

use super::ShowFlags;
use super::{cli_err, sync_preset, TableFormat};
use crate::cli::gguf::{
    cached_entries, format_relative_time, format_size, quant_name, read_gguf_metadata,
    resolve_model, scan_gguf_models, GgufEntry,
};
use crate::cli::preset::{render_aichat_config, render_litellm_yaml, write_models_preset};
use crate::cli::tables::{content_widths, render_text_table, terminal_width};
use crate::cli::{CliContext, CliResult};
use crate::config::RouterConfig;
use common_core::format::render_markdown_table;

/// One resolved route row: the route name, one fallback level of the
/// serving role's `models` chain (route → role → member, in configured
/// order), the effective serving window (`CTX`), and the effective
/// reasoning level (`REASONING`). Availability-sentinel levels (`last` /
/// `any`) carry no static target, so they render as `-` across
/// `MODEL`/`CTX`/`REASONING`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRow {
    pub route: String,
    pub model: String,
    pub ctx: Option<u64>,
    pub reasoning: String,
}

/// One config section listing: resolved routes (route → one row per
/// fallback level + `CTX` + `REASONING`) and models (key, intelligence,
/// weights path) — routes sorted by key for deterministic output (serde
/// maps are sorted; file order is not preserved). Built from the resolved
/// config file, never synthesized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigListing {
    pub routes: Vec<RouteRow>,
    pub models: Vec<(String, u8, Option<String>)>,
}

/// Render the full listing as Markdown — the same sections, order, columns,
/// and rows as the plain-text screen (`MODELS`, `ROUTES`),
/// differing only in format and separators (pipe tables, full values, never
/// truncated; every section titled, unlike the title-less text MODELS grid).
pub fn render_config_listing(listing: &ConfigListing, models: &[GgufEntry]) -> String {
    let route_rows: Vec<Vec<String>> = listing
        .routes
        .iter()
        .map(|row| {
            vec![
                row.route.clone(),
                row.model.clone(),
                row.ctx.map_or_else(|| "-".to_string(), |c| c.to_string()),
                row.reasoning.clone(),
            ]
        })
        .collect();
    let mut out = String::from("## MODELS\n");
    out.push_str(&render_markdown_table(
        &["NAME", "SIZE", "QUANT", "ARCH", "MODIFIED"],
        &sorted_model_rows(models, false),
    ));
    out.push_str("\n## ROUTES\n");
    out.push_str(&render_markdown_table(
        &["ROUTE", "MODEL", "CTX", "REASONING"],
        &route_rows,
    ));
    out
}

/// One scanned-models row (`NAME SIZE QUANT ARCH MODIFIED`), matching
/// `gguf_tool ls`: the file-stem tag lowercased, falling back to the GGUF
/// `file_type` mapping when the tag is empty/`latest`, `-` with neither.
/// `clip_arch` truncates `ARCH` to 10 chars for the fixed-width text screen
/// (Markdown keeps full values).
fn gguf_model_cells(entry: &GgufEntry, clip_arch: bool) -> Vec<String> {
    let tag = entry.tag.to_lowercase();
    let quant = if tag.is_empty() || tag == "latest" {
        entry.file_type.map_or_else(|| "-".to_string(), quant_name)
    } else {
        tag
    };
    let arch = if clip_arch {
        entry.arch.chars().take(10).collect()
    } else {
        entry.arch.clone()
    };
    vec![
        entry.display.clone(),
        format_size(entry.size),
        quant,
        arch,
        format_relative_time(entry.mtime),
    ]
}

/// Scanned-models rows sorted by display name (disk-walk order is not
/// deterministic, so the screen sorts — the single ordering home).
fn sorted_model_rows(models: &[GgufEntry], clip_arch: bool) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = models
        .iter()
        .map(|e| gguf_model_cells(e, clip_arch))
        .collect();
    rows.sort();
    rows
}

/// `gguf_tool ls` field widths (`NAME` capped at 90 there too).
const MODELS_MIN_WIDTHS: [usize; 5] = [4, 8, 12, 10, 14];
const MODELS_HEADERS: [&str; 5] = ["NAME", "SIZE", "QUANT", "ARCH", "MODIFIED"];

/// Render one plain-text section (header, zero or more rows) in the shared
/// borderless `ps`-style format.
fn render_text_section(
    headers: &[&str],
    rows: &[Vec<String>],
    right: &[bool],
    width: u16,
) -> String {
    let widths = content_widths(headers, rows);
    let mut out = String::new();
    out.push_str(&render_text_table(headers, rows, right, 0, &widths, width));
    out
}

/// Render the full listing as aligned plain-text tables (the default `ls`
/// screen): MODELS (scanned GGUF entries, `gguf_tool ls` columns, no title —
/// that screen's shape), then the titled ROUTES section (route → model +
/// `CTX` + `REASONING`) — the same borderless, terminal-width-aware format
/// as the `ps` screen. The first column absorbs slack; on narrow terminals
/// the `ARCH` column is clipped to the `gguf_tool` 10-char width (full
/// values are one `--markdown` away).
pub fn render_config_text(listing: &ConfigListing, models: &[GgufEntry], width: u16) -> String {
    let route_rows: Vec<Vec<String>> = listing
        .routes
        .iter()
        .map(|row| {
            vec![
                row.route.clone(),
                row.model.clone(),
                row.ctx.map_or_else(|| "-".to_string(), |c| c.to_string()),
                row.reasoning.clone(),
            ]
        })
        .collect();
    let mut out = render_text_table(
        &MODELS_HEADERS,
        &sorted_model_rows(models, true),
        &[false, true, false, false, false],
        0,
        &MODELS_MIN_WIDTHS,
        width,
    );
    out.push('\n');
    out.push_str(&render_text_section(
        &["ROUTE", "MODEL", "CTX", "REASONING"],
        &route_rows,
        &[false, false, true, false],
        width,
    ));
    out
}

/// Render the listing losslessly for scripts: resolved routes from the
/// config (one object per fallback level per route, grouped into an array
/// carrying the serving `model`, the effective `ctx` window, and the
/// `reasoning` level), and one entry per scanned GGUF model carrying the
/// raw fields behind the `ls` table
/// (`name`/`size_bytes`/`quant`/`arch`/`mtime` — no formatting).
pub fn render_config_json(listing: &ConfigListing, models: &[GgufEntry]) -> Value {
    let mut routes: serde_json::Map<String, Value> = serde_json::Map::new();
    for row in &listing.routes {
        let level = json!({
            "model": row.model,
            "ctx": row.ctx.map_or(Value::Null, Value::from),
            "reasoning": row.reasoning,
        });
        routes
            .entry(row.route.clone())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("route entry is an array")
            .push(level);
    }
    let mut models_json: Vec<Value> = models
        .iter()
        .map(|e| {
            json!({
                "name": e.display,
                "size_bytes": e.size,
                "quant": gguf_model_cells(e, false)[2],
                "arch": e.arch,
                "mtime": e.mtime,
            })
        })
        .collect();
    models_json.sort_by(|a, b| {
        a.get("name")
            .and_then(Value::as_str)
            .cmp(&b.get("name").and_then(Value::as_str))
    });
    json!({ "routes": routes, "models": models_json })
}

/// Read the `reasoning_effort` string from a params object, if declared.
fn reasoning_effort_in(params: Option<&serde_json::Value>) -> Option<String> {
    params
        .and_then(|p| p.as_object())
        .and_then(|obj| obj.get("reasoning_effort"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Effective serving window for a dispatch through `role` on `entry`: the
/// role run block's `num_ctx` clamped to the `max_ctx` cap (model-side
/// override, then the entry, then the role block) — the same window the
/// boot-materialized pool profile carries (`materialize_effective_pool`).
fn effective_ctx(
    entry: &crate::config::ModelEntry,
    role_entry: &crate::config::routing::RoleEntry,
    over: Option<&crate::config::ModelRoleOverride>,
) -> u64 {
    let window = role_entry.params.num_ctx;
    let cap = over
        .and_then(|o| o.max_ctx)
        .or(entry.max_ctx)
        .or(role_entry.params.max_ctx);
    cap.map_or(window, |c| window.min(c))
}

/// Effective reasoning level for a dispatch through `role` on `entry`:
/// `none` when thinking is off, else the composed `reasoning_effort`
/// (model-side override, then the role run block, then the entry params),
/// falling back to `on` when thinking is enabled with no effort string.
/// Enablement composes override → role → entry (first declaration wins);
/// the effort lookup mirrors `ModelEntry::resolve_thinking` without
/// collapsing the effort string to `on`/`off`.
fn effective_reasoning(
    entry: &crate::config::ModelEntry,
    role_entry: &crate::config::routing::RoleEntry,
    over: Option<&crate::config::ModelRoleOverride>,
) -> String {
    let enabled = over
        .and_then(|o| o.enable_thinking)
        .or(role_entry.enable_thinking)
        .or(entry.enable_thinking);
    if enabled == Some(false) {
        return "none".to_string();
    }
    let effort = over
        .and_then(|o| reasoning_effort_in(o.params.as_ref()))
        .or_else(|| reasoning_effort_in(role_entry.params.sampling_value().as_ref()))
        .or_else(|| reasoning_effort_in(entry.params.as_ref()));
    match (effort, enabled) {
        (Some(level), _) => level,
        (None, Some(true)) => "on".to_string(),
        (None, _) => "none".to_string(),
    }
}

/// Build the listing from a parsed config: tree-derived routes resolved
/// through their role to the serving models — one row per fallback level
/// of the role's `models` chain in configured order (the expanded dispatch
/// members, so nested role refs fan out exactly as dispatch sees them),
/// with the composed `CTX` window and `REASONING` level per level; plus
/// models with intelligence and weights as declared. Sorted by route/key
/// throughout (levels keep configured order). Availability-sentinel levels
/// (`last` / `any`) and unresolvable routes render as `-` rather than
/// failing the listing.
pub fn listing_from_config(cfg: &RouterConfig) -> ConfigListing {
    let routing = cfg.routing_config();
    let mut views: Vec<(String, String)> = cfg
        .routes_view()
        .into_iter()
        .map(|(route, rref)| (route, rref.role))
        .collect();
    views.sort();
    let mut routes: Vec<RouteRow> = Vec::new();
    for (route, role) in views {
        let members = routing.role_dispatch_members(&role);
        if members.is_empty() {
            routes.push(RouteRow {
                route,
                model: "[last]".to_string(),
                ctx: None,
                reasoning: "-".to_string(),
            });
            continue;
        }
        let Some(role_entry) = cfg.roles.get(&role) else {
            routes.push(RouteRow {
                route,
                model: "-".to_string(),
                ctx: None,
                reasoning: "-".to_string(),
            });
            continue;
        };
        for member in members {
            if member == "last" || member == "any" {
                routes.push(RouteRow {
                    route: route.clone(),
                    model: "-".to_string(),
                    ctx: None,
                    reasoning: "-".to_string(),
                });
                continue;
            }
            let (base, _) = fluent_types::model_key::split_model_key(&member);
            if routing.onnx_keys.contains(base) {
                routes.push(RouteRow {
                    route: route.clone(),
                    model: member,
                    ctx: None,
                    reasoning: "none".to_string(),
                });
                continue;
            }
            let Some(entry) = cfg.models.get(base) else {
                routes.push(RouteRow {
                    route: route.clone(),
                    model: base.to_string(),
                    ctx: None,
                    reasoning: "-".to_string(),
                });
                continue;
            };
            let over = entry.role_params.as_ref().and_then(|o| o.get(&role));
            routes.push(RouteRow {
                route: route.clone(),
                model: base.to_string(),
                ctx: Some(effective_ctx(entry, role_entry, over)),
                reasoning: effective_reasoning(entry, role_entry, over),
            });
        }
    }
    let mut models: Vec<(String, u8, Option<String>)> = cfg
        .models
        .iter()
        .map(|(key, entry)| (key.clone(), entry.intelligence, entry.weights.clone()))
        .collect();
    models.sort();
    ConfigListing { routes, models }
}

/// Load the listing from the resolved config file (parse errors surface the
/// loader message, including migration pointers — same shapes boot accepts).
/// Parses through the boot-equivalent `from_json_str` leg so sparse
/// inheritance (`models.default`, `roles.default.params`) composes exactly
/// as dispatch sees it.
pub fn load_listing(config_path: &Path) -> Result<ConfigListing, super::CliError> {
    let text = std::fs::read_to_string(config_path)
        .map_err(|e| cli_err(format!("cannot read {}: {e}", config_path.display())))?;
    let cfg: RouterConfig = RouterConfig::from_json_str(&text)
        .map_err(|e| cli_err(format!("cannot parse {}: {e}", config_path.display())))?;
    Ok(listing_from_config(&cfg))
}

pub fn list(ctx: &CliContext, config_path: &Path, json: bool, format: TableFormat) -> CliResult {
    let listing = load_listing(config_path)?;
    // The MODELS table serves the GGUF layout under the config's `gguf_dir`
    // (the `CliContext` path, resolved from `--gguf-dir` else the config
    // value by the binary) from the `models.json` cache — a full header
    // re-parse of every weights file costs seconds (see `list_gguf_entries`)
    // and the layout is unchanged when the directory mtime is stable, so a
    // fresh `scan` is only paid when something actually changed. A
    // missing/unreadable dir degrades to an empty table — routes still
    // render.
    let models = cached_entries(&ctx.gguf_dir);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&render_config_json(&listing, &models))
                .expect("listing serializes")
        );
    } else {
        match format {
            TableFormat::Text => {
                print!(
                    "{}",
                    render_config_text(&listing, &models, terminal_width())
                );
            }
            TableFormat::Markdown => {
                print!("{}", render_config_listing(&listing, &models));
            }
        }
    }
    Ok(())
}

// ── scan ────────────────────────────────────────────────────────────────────

/// `coral-router scan` — scan the GGUF directory and regenerate serving
/// configs (llama.cpp preset, optionally LiteLLM/aichat/JSON).
pub fn scan(
    ctx: &CliContext,
    write_litellm: Option<&PathBuf>,
    write_aichat: Option<&PathBuf>,
    path_prefix: Option<&str>,
    json: bool,
) -> CliResult {
    let gguf_dir = &ctx.gguf_dir;
    if !gguf_dir.is_dir() {
        return Err(cli_err(format!(
            "GGUF directory not found: {}",
            gguf_dir.display()
        )));
    }
    let models = scan_gguf_models(gguf_dir);

    if let Some(path) = write_litellm {
        std::fs::write(path, render_litellm_yaml(&models))
            .map_err(|e| cli_err(format!("cannot write {}: {e}", path.display())))?;
        println!("Wrote LiteLLM config: {}", path.display());
    }
    if let Some(path) = write_aichat {
        std::fs::write(path, render_aichat_config(&models))
            .map_err(|e| cli_err(format!("cannot write {}: {e}", path.display())))?;
        println!("Wrote aichat config: {}", path.display());
    }

    if ctx.dry_run {
        ctx.log_debug("[DRY-RUN] would write models-preset.ini");
    } else {
        let preset = write_models_preset(gguf_dir, path_prefix)
            .map_err(|e| cli_err(format!("preset write failed: {e}")))?;
        println!("Wrote llama.cpp preset to {}", preset.display());
    }

    if json {
        let value: Vec<Value> = models
            .iter()
            .map(|m| {
                let dir = m.model_dir();
                json!({
                    "name": m.display,
                    "model_file": m.path.to_string_lossy(),
                    "params_file": opt_path(&dir.join("params.json")),
                    "template_file": opt_path(&dir.join("template.txt")),
                    "config_file": opt_path(&dir.join("config.json")),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&value)?);
    }

    if write_litellm.is_none() && write_aichat.is_none() && !json {
        let mut names: Vec<&str> = models.iter().map(|m| m.display.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            let model = models
                .iter()
                .find(|m| m.display == name)
                .expect("name from list");
            let file = model.path.file_name().map_or_else(
                || "(no gguf)".to_string(),
                |s| s.to_string_lossy().into_owned(),
            );
            println!("  {name:<40} {file}");
        }
    }
    Ok(())
}

fn opt_path(path: &Path) -> Value {
    if path.exists() {
        Value::String(path.to_string_lossy().into_owned())
    } else {
        Value::Null
    }
}

// ── rm ──────────────────────────────────────────────────────────────────────

/// `coral-router rm <model>` — remove a model directory and resync the preset.
pub fn rm(ctx: &CliContext, model: &str) -> CliResult {
    let resolved = resolve_model(&ctx.gguf_dir, model).ok_or_else(|| {
        cli_err(format!(
            "model '{model}' not found in {}",
            ctx.gguf_dir.display()
        ))
    })?;
    let model_dir = resolved.model_dir().to_path_buf();
    if ctx.dry_run {
        ctx.log_debug(&format!("[DRY-RUN] rm -rf {}", model_dir.display()));
        return Ok(());
    }
    std::fs::remove_dir_all(&model_dir)
        .map_err(|e| cli_err(format!("cannot remove {}: {e}", model_dir.display())))?;
    println!("Removed {}", model_dir.display());
    sync_preset(&ctx.gguf_dir);
    Ok(())
}

// ── show ────────────────────────────────────────────────────────────────────

/// `coral-router show <model>` — print a model summary, Modelfile, or one of
/// the specific sections (license/parameters/system/template).
pub fn show(ctx: &CliContext, model: &str, flags: &ShowFlags) -> CliResult {
    let resolved = resolve_model(&ctx.gguf_dir, model).ok_or_else(|| {
        cli_err(format!(
            "model '{model}' not found in {}",
            ctx.gguf_dir.display()
        ))
    })?;
    sync_preset(&ctx.gguf_dir);

    let parent = resolved.model_dir();
    let config = read_json(&parent.join("config.json"));
    let params = read_json(&parent.join("params.json"));
    let license_text = read_text(&parent.join("LICENSE.txt"));
    let template_text = read_text(&parent.join("template.txt"));
    let system_text = read_text(&parent.join("system.txt"));
    let gguf_meta = read_gguf_metadata(&resolved.path);

    if flags.modelfile {
        print_modelfile(
            &resolved,
            &ctx.gguf_dir,
            &params,
            template_text.as_deref(),
            license_text.as_deref(),
            system_text.as_deref(),
        );
        return Ok(());
    }
    if flags.license {
        println!("{}", license_text.as_deref().unwrap_or("(no license)"));
        return Ok(());
    }
    if flags.parameters {
        if let Some(map) = params.as_object().filter(|o| !o.is_empty()) {
            for (k, v) in map {
                println!("  {k:<24} {v}");
            }
        } else {
            println!("(no parameters)");
        }
        return Ok(());
    }
    if flags.system {
        println!(
            "{}",
            system_text.as_deref().unwrap_or("(no system message)")
        );
        return Ok(());
    }
    if flags.template {
        println!("{}", template_text.as_deref().unwrap_or("(no template)"));
        return Ok(());
    }

    print_model_summary(
        &resolved,
        &gguf_meta,
        &config,
        &params,
        license_text.as_deref(),
    );
    Ok(())
}

fn read_json(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}))
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn print_model_summary(
    entry: &GgufEntry,
    gguf_meta: &HashMap<String, Value>,
    config: &Value,
    params: &Value,
    license_text: Option<&str>,
) {
    let arch = gguf_meta
        .get("general.architecture")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            config
                .get("model_family")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "?".into());
    let param_count = config
        .get("model_type")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let ctx_len = params
        .get("num_ctx")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            gguf_meta
                .get(&format!("{arch}.context_length"))
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "?".into());
    let embed_len = gguf_meta
        .get(&format!("{arch}.embedding_length"))
        .map_or_else(|| "?".to_string(), ToString::to_string);
    let file_type = config
        .get("file_type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| gguf_meta.get("general.file_type").map(ToString::to_string))
        .unwrap_or_else(|| "?".into());

    println!("  Model");
    println!("    {:24} {}", "name", entry.display);
    println!("    {:24} {arch}", "architecture");
    println!("    {:24} {param_count}", "parameters");
    println!("    {:24} {ctx_len}", "context length");
    println!("    {:24} {embed_len}", "embedding length");
    println!("    {:24} {file_type}", "quantization");

    let mut caps = vec!["completion".to_string()];
    if !arch.to_lowercase().contains("embed") {
        caps.push("tools".into());
    }
    if entry.model_dir().join("projector.gguf").exists() {
        caps.push("vision".into());
    }
    println!("\n  Capabilities");
    for c in caps {
        println!("    {c}");
    }

    if let Some(map) = params.as_object().filter(|o| !o.is_empty()) {
        println!("\n  Parameters");
        for (k, v) in map {
            println!("    {k:<24} {v}");
        }
    }

    if let Some(license) = license_text {
        let lines: Vec<&str> = license.trim().lines().take(5).collect();
        if !lines.is_empty() {
            println!("\n  License");
            for line in &lines {
                println!("    {line}");
            }
            if license.trim().lines().count() > 5 {
                println!("    ...");
            }
        }
    }
}

fn print_modelfile(
    entry: &GgufEntry,
    gguf_dir: &Path,
    params: &Value,
    template_text: Option<&str>,
    license_text: Option<&str>,
    system_text: Option<&str>,
) {
    println!("# Modelfile generated by \"coral-router show\"");
    println!("# To build a new Modelfile based on this, replace FROM with:");
    println!("# FROM {}", entry.display);
    println!();
    let rel = entry.path.strip_prefix(gguf_dir).unwrap_or(&entry.path);
    println!("FROM {}/{}", gguf_dir.display(), rel.display());
    if let Some(t) = template_text {
        println!("TEMPLATE \"\"\"{t}\"\"\"");
    }
    if let Value::Object(map) = params {
        for (k, v) in map {
            println!("PARAMETER {k} {v}");
        }
    }
    if let Some(s) = system_text {
        println!("SYSTEM \"\"\"{s}\"\"\"");
    }
    if let Some(l) = license_text {
        println!("LICENSE \"\"\"{l}\"\"\"");
    }
}

// ── pull ────────────────────────────────────────────────────────────────────

/// `coral-router pull <namespace/model:tag>` — download a GGUF model from
/// HuggingFace (or copy a local file) and fetch its companion metadata.
pub async fn pull(ctx: &CliContext, model: &str, input: Option<PathBuf>, force: bool) -> CliResult {
    if !model.contains('/') || !model.contains(':') {
        return Err(cli_err(
            "Usage: coral-router pull <namespace/model:tag>\n  e.g. coral-router pull unsloth/Hy-MT2-7B-GGUF:ud-q5_k_xl",
        ));
    }
    let (base, tag) = model.rsplit_once(':').unwrap_or((model, "latest"));
    let base = base.to_lowercase();
    let tag = tag.to_lowercase();
    let clean_base = hf_repo_from_base(&base);
    let target_dir = ctx.gguf_dir.join(clean_base);
    let gguf_target = target_dir.join(format!("{tag}.gguf"));

    let client = Client::builder().timeout(Duration::from_secs(60)).build()?;

    if let Some(input_path) = input {
        let src = input_path
            .canonicalize()
            .map_err(|e| cli_err(format!("input file not found: {e}")))?;
        if !src.is_file() {
            return Err(cli_err(format!("input file not found: {}", src.display())));
        }
        if gguf_target.exists() && !force {
            return Err(cli_err(format!(
                "Destination exists: {} (use --force to overwrite)",
                gguf_target.display()
            )));
        }
        if ctx.dry_run {
            ctx.log_debug(&format!(
                "[DRY-RUN] cp {} -> {}",
                src.display(),
                gguf_target.display()
            ));
        } else {
            std::fs::create_dir_all(&target_dir)
                .map_err(|e| cli_err(format!("cannot create {}: {e}", target_dir.display())))?;
            let size = std::fs::metadata(&src).map_or(0, |m| m.len());
            std::fs::copy(&src, &gguf_target).map_err(|e| cli_err(format!("copy failed: {e}")))?;
            println!(
                "Copied {} from {} to {}",
                format_size(size),
                src.display(),
                gguf_target.display()
            );
        }
    } else {
        if gguf_target.exists() {
            println!("Already exists: {}", gguf_target.display());
            return Ok(());
        }
        if ctx.dry_run {
            ctx.log_debug("[DRY-RUN] download skipped");
            return Ok(());
        }
        let hf_url = resolve_gguf_url(&client, clean_base, &tag).await;
        println!("Downloading {model} ...");
        println!("  from: {hf_url}");
        std::fs::create_dir_all(&target_dir)
            .map_err(|e| cli_err(format!("cannot create {}: {e}", target_dir.display())))?;
        download_to(&client, &hf_url, &gguf_target).await?;
    }

    if !ctx.dry_run {
        setup_model_from_hf(&client, clean_base, &target_dir).await;
        sync_preset(&ctx.gguf_dir);
    }
    Ok(())
}

/// Strip an `hf.co/` / `huggingface.co/` prefix from a repo id.
fn hf_repo_from_base(base: &str) -> &str {
    for prefix in ["hf.co/", "huggingface.co/"] {
        if let Some(rest) = base.strip_prefix(prefix) {
            return rest;
        }
    }
    base
}

async fn resolve_gguf_url(client: &Client, clean_base: &str, tag: &str) -> String {
    let exact = format!("https://huggingface.co/{clean_base}/resolve/main/{tag}.gguf");
    if let Ok(resp) = client.head(&exact).send().await {
        if resp.status().is_success() {
            return exact;
        }
    }
    let Some(files) = fetch_hf_json(
        client,
        &format!("https://huggingface.co/api/models/{clean_base}/tree/main"),
    )
    .await
    else {
        return exact;
    };
    let Some(files) = files.as_array() else {
        return exact;
    };
    let norm_tag = tag.to_lowercase().replace(['-', '.'], "_");
    let normalise = |s: &str| s.to_lowercase().replace(['-', '.'], "_");

    let mut best: Option<(usize, String)> = None;
    for entry in files {
        let Some(path) = entry.get("path").and_then(Value::as_str) else {
            continue;
        };
        if !path.ends_with(".gguf") {
            continue;
        }
        let stem = path.trim_end_matches(".gguf");
        if normalise(stem).ends_with(&norm_tag)
            && best.as_ref().is_none_or(|(len, _)| stem.len() < *len)
        {
            best = Some((stem.len(), path.to_string()));
        }
    }
    if let Some((_, path)) = best {
        return format!("https://huggingface.co/{clean_base}/resolve/main/{path}");
    }
    if tag == "latest" {
        if let Some(largest) = files
            .iter()
            .filter(|e| {
                e.get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|p| p.ends_with(".gguf"))
            })
            .max_by_key(|e| e.get("size").and_then(Value::as_u64).unwrap_or(0))
        {
            if let Some(path) = largest.get("path").and_then(Value::as_str) {
                return format!("https://huggingface.co/{clean_base}/resolve/main/{path}");
            }
        }
    }
    exact
}

async fn fetch_hf_json(client: &Client, url: &str) -> Option<Value> {
    let resp = client.get(url).send().await.ok()?;
    resp.error_for_status().ok()?.json().await.ok()
}

async fn fetch_hf_raw(client: &Client, path: &str) -> Option<String> {
    let url = format!("https://huggingface.co/{path}");
    let resp = client.get(&url).send().await.ok()?;
    if resp.status().is_success() {
        resp.text().await.ok()
    } else {
        None
    }
}

/// Resolve a single base-model repo string from the HF API response.
fn resolve_base_model(data: &Value) -> Option<String> {
    let card = data
        .get("cardData")
        .and_then(Value::as_object)
        .and_then(|o| o.get("base_model"));
    let raw = card.or_else(|| data.get("base_model"))?;
    match raw {
        Value::String(s) if s.contains('/') => Some(s.clone()),
        Value::Array(arr) => arr
            .iter()
            .find_map(|v| v.as_str().filter(|s| s.contains('/')).map(str::to_string)),
        _ => None,
    }
}

async fn download_to(client: &Client, url: &str, dest: &Path) -> CliResult {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| cli_err(format!("download failed: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(cli_err(format!("download failed: HTTP {status}")));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut file = std::fs::File::create(dest)
        .map_err(|e| cli_err(format!("cannot create {}: {e}", dest.display())))?;
    let mut last_log = Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| cli_err(format!("download interrupted: {e}")))?;
        file.write_all(&chunk)
            .map_err(|e| cli_err(format!("write failed: {e}")))?;
        downloaded += chunk.len() as u64;
        if total > 0 && last_log.elapsed() >= Duration::from_secs(1) && downloaded < total {
            let pct = downloaded as f64 / total as f64 * 100.0;
            eprintln!(
                "  {pct:.0}% ({} / {})",
                format_size(downloaded),
                format_size(total)
            );
            last_log = Instant::now();
        }
    }
    println!(
        "Downloaded {} to {}",
        format_size(downloaded),
        dest.display()
    );
    Ok(())
}

/// Scrape the HF GGUF repo (and its safetensors base, when declared) for
/// companion files: chat template, config.json, and LICENSE.
async fn setup_model_from_hf(client: &Client, clean_base: &str, target_dir: &Path) {
    println!("Setting up model metadata from {clean_base} ...");
    let mut sources: Vec<String> = Vec::new();
    if let Some(data) = fetch_hf_json(
        client,
        &format!("https://huggingface.co/api/models/{clean_base}"),
    )
    .await
    {
        if let Some(base_model) = resolve_base_model(&data) {
            println!("Found base model: {base_model}");
            sources.push(base_model);
        } else {
            println!("No base model found for {clean_base}; falling back to GGUF repo");
        }
    }
    sources.push(clean_base.to_string());

    let params_file = target_dir.join("params.json");
    if !params_file.exists() {
        let params = json!({ "num_ctx": 8192, "temperature": 0.7, "top_k": 40, "top_p": 0.9 });
        if let Ok(text) = serde_json::to_string_pretty(&params) {
            let _ = std::fs::write(&params_file, text);
            println!("Wrote default params to {}", params_file.display());
        }
    }

    for source in &sources {
        let source = source.as_str();
        if let Some(tok_text) =
            fetch_hf_raw(client, &format!("{source}/raw/main/tokenizer_config.json")).await
        {
            if let Ok(tok_config) = serde_json::from_str::<Value>(&tok_text) {
                if let Some(template) = tok_config.get("chat_template").and_then(Value::as_str) {
                    let tmpl_file = target_dir.join("template.txt");
                    if !tmpl_file.exists() {
                        let _ = std::fs::write(&tmpl_file, template);
                        println!("Wrote template to {}", tmpl_file.display());
                    }
                }
            }
        }
        if let Some(cfg_text) =
            fetch_hf_raw(client, &format!("{source}/raw/main/config.json")).await
        {
            if let Ok(cfg) = serde_json::from_str::<Value>(&cfg_text) {
                let cfg_file = target_dir.join("config.json");
                if !cfg_file.exists() {
                    if let Ok(text) = serde_json::to_string_pretty(&cfg) {
                        let _ = std::fs::write(&cfg_file, text);
                        println!("Wrote config to {}", cfg_file.display());
                    }
                }
            }
        }
        let lic_file = target_dir.join("LICENSE.txt");
        if !lic_file.exists() {
            for lic_name in ["LICENSE", "LICENSE.txt"] {
                if let Some(text) =
                    fetch_hf_raw(client, &format!("{source}/raw/main/{lic_name}")).await
                {
                    let _ = std::fs::write(&lic_file, text);
                    println!("Wrote LICENSE.txt from {source}");
                    break;
                }
            }
        }
        if ["template.txt", "config.json", "LICENSE.txt"]
            .iter()
            .any(|n| target_dir.join(n).exists())
        {
            break;
        }
    }
}
