//! `coral-router ps` table: one compact grouped table over the whole fleet.
//!
//! The legacy layout printed one multi-line block per model (`weights` /
//! `instances` / `total` with inline labels), which wastes most of an
//! 80-column terminal on repeated labels. This module renders one grouped
//! table (`MODEL INSTANCE VRAM SYS RAM CTX QUANT KV LIVE`): a weights row
//! per model (its own VRAM/RAM footprint), one row per instance, and a
//! `TOTAL` row folding the model's footprints together; models with no
//! instance detail render a single `unloaded` row. Rendered through the
//! shared borderless text renderer (`cli::tables`, terminal-width aware) by
//! default, or as Markdown pipe tables (`--markdown`) via the shared
//! `common_core::format::render_markdown_table` — same columns, same rows,
//! same footer; only the format and separators differ.
//!
//! Informative columns, per row:
//!
//! - `VRAM` / `SYS RAM` — the memory split: `vram_bytes` vs the CPU-RAM
//!   remainder (`total_bytes - vram_bytes - model_bytes` — the all-memory
//!   total repeats the shared weights, so they are removed; what stays is
//!   the partial-offload portion). Every byte cell is a fork-reported
//!   measurement: servers that report no byte counts (grammar-less servers,
//!   unloaded models) render `-`, never an on-disk size — disk footprint is
//!   not VRAM footprint.
//! - `CTX` — the instance's context window (`n_ctx` tokens).
//! - `QUANT` — the weights quantization (`quant` overlaid on the
//!   `/instances` envelope, GGUF fallback).
//! - `KV` — the KV-cache quantization (`cache_type_k/v` spawn defaults for
//!   the model, `q8_0` unless configured otherwise; `k/v` when split).
//! - `LIVE` — for instances: the idle time remaining before sleep
//!   (`1h28m`, `Forever` for pinned/no-sleep, `loaded` when no finite
//!   window is known); for the weights row: `loaded`/`unloaded`.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use bon::Builder;
use common_core::{format::render_markdown_table, now_secs};
use serde_json::Value;

use super::tables::{render_text_table, terminal_width};
use crate::cli::gguf::{format_keep_alive, format_size, ollama_display};
use crate::config::{InstanceProfile, RouterConfig};

/// One `ps` table row: a weights row, an instance row, a `TOTAL` row, or a
/// model's single `unloaded` row.
#[derive(Debug, Clone, Builder)]
#[builder(start_fn = new)]
/// One `ps` table row: a weights row, an instance row, a `TOTAL` row, or a
/// model's single `unloaded` row.
pub struct PsRow {
    /// Display model name — set on the group's first row only (` (onnx)`
    /// suffixed for onnx rows), empty on the rows below it.
    pub model: String,
    /// `-` for a weights/unloaded row, the instance name, or `TOTAL`.
    pub instance: String,
    /// VRAM-resident bytes, human-sized (`-` when none).
    pub vram: String,
    /// CPU-RAM bytes (offload/working set), human-sized (`-` when none).
    pub sys_ram: String,
    /// Context window in tokens (`-` when unknown).
    pub ctx: String,
    /// Weights quantization (`-` when unknown).
    pub quant: String,
    /// KV-cache quantization (`cache_type_k`, or `k/v` when split).
    pub kv: String,
    /// Idle countdown for instances (`1h28m`/`Forever`/`loaded`), or the
    /// weights/unloaded state.
    pub live: String,
}

/// The idle-sleep policy for one instance, resolved from the envelope row's
/// own fields when present, else the configured profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The idle-sleep policy for one instance, resolved from the envelope row's
/// own fields when present, else the configured profile.
enum SleepPolicy {
    /// Pinned / `no_sleep` / a zero window: never sleeps.
    Forever,
    /// No policy declared anywhere.
    Inherit,
    /// A finite idle window in seconds.
    Window(i64),
}

/// Fleet totals folded under the table.
#[derive(Debug, Clone, Default)]
/// Fleet totals folded under the table.
pub struct PsTotals {
    /// Resident bytes fleet-wide (weights + contexts).
    pub resident_bytes: u64,
    /// Distinct models shown.
    pub models: usize,
    /// Live instance rows (excluding weights/unloaded/total rows).
    pub instances: usize,
}

/// Collect every `ps` row plus the fleet totals. Pure over the
/// already-fetched `/v1/models` listing (`listed`) and the `/instances`
/// envelope (`instance_list` plus its `snapshots` array); the config only
/// enriches labels (quant/KV cache type/sleep fallback) and never filters.
///
/// Snapshots are restorable state, not residency: an offloaded model whose
/// server kept snapshots (alive-but-empty after eviction) renders its
/// snapshot rows distinctly (`snapshot:<name>`, `LIVE = snapshot`, every
/// measure `-`), including when the model has no instance detail at all
/// (it would otherwise vanish from the table entirely).
pub fn collect_ps_rows(
    config: Option<&RouterConfig>,
    gguf_dir: &Path,
    listed: &[Value],
    instance_list: &[Value],
    snapshots: &[Value],
) -> (Vec<PsRow>, PsTotals) {
    let mut by_model: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for inst in instance_list {
        let id = inst.get("id").and_then(Value::as_str).unwrap_or_default();
        let key = id.split(':').next().unwrap_or(id).to_string();
        by_model.entry(key).or_default().push(inst.clone());
    }
    // The aggregate tags each snapshot with its owning `model` key.
    let mut snaps_by_model: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for snap in snapshots {
        let key = snap
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !key.is_empty() {
            snaps_by_model.entry(key).or_default().push(snap);
        }
    }

    let mut rows = Vec::new();
    let mut resident: u64 = 0;
    for (key, insts) in &by_model {
        let weights_bytes = weights_model_bytes(insts);
        let ctx_mem: u64 = insts.iter().map(inst_mem_bytes).sum();
        resident = resident.saturating_add(weights_bytes.saturating_add(ctx_mem));
        rows.extend(model_group(config, gguf_dir, key, insts));
        if let Some(snaps) = snaps_by_model.get(key) {
            rows.extend(snapshot_rows(key, snaps));
        }
    }
    let mut models = by_model.len();
    // `/v1/models` lists one entry per *window* — a down server synthesizes
    // one per configured profile (`qwen3.5-9b:ledger` plus
    // `qwen3.5-9b:reasoning`) — while `ps` groups by model key. Dedupe so
    // the same unloaded model renders (and counts) once, not once per
    // window.
    let mut seen_unloaded = HashSet::new();
    for m in listed {
        let id = m.get("id").and_then(Value::as_str).unwrap_or_default();
        let key = id.split(':').next().unwrap_or(id).to_string();
        if !by_model.contains_key(&key) && seen_unloaded.insert(key.clone()) {
            // An unloaded model contributes nothing resident — its weights
            // sit on disk, not in VRAM or SYS RAM (the on-disk size is a
            // display-only fallback elsewhere, never a residency figure).
            rows.push(unloaded_row(&key));
            if let Some(snaps) = snaps_by_model.get(&key) {
                rows.extend(snapshot_rows(&key, snaps));
            }
            models += 1;
        }
    }
    // Alive-but-empty servers (evicted to zero instances, snapshots kept
    // fork-side) appear in neither listing: surface them from their
    // snapshots alone, in model-key order.
    for (key, snaps) in &snaps_by_model {
        if !by_model.contains_key(key) && seen_unloaded.insert(key.clone()) {
            rows.extend(snapshot_rows(key, snaps));
            models += 1;
        }
    }
    let instances = rows
        .iter()
        .filter(|r| {
            !r.instance.is_empty()
                && r.instance != "-"
                && r.instance != "TOTAL"
                && !r.instance.starts_with(SNAPSHOT_PREFIX)
        })
        .count();
    (
        rows,
        PsTotals {
            resident_bytes: resident,
            models,
            instances,
        },
    )
}

/// The row group for one model key: a weights row, one row per instance,
/// and a `TOTAL` row — or a single `unloaded` row when the model has no
/// instance detail (lazy server down).
fn model_group(
    config: Option<&RouterConfig>,
    gguf_dir: &Path,
    model_key: &str,
    insts: &[Value],
) -> Vec<PsRow> {
    if insts.is_empty() {
        return vec![unloaded_row(model_key)];
    }
    let is_onnx = insts
        .iter()
        .any(|i| i.get("runtime").and_then(Value::as_str) == Some("onnx"));
    let quant = model_quant(insts, config, gguf_dir, model_key);
    let kv = cache_quant_label(config, model_key);
    let base_model = ollama_display(model_key, "");
    let display = if is_onnx {
        format!("{base_model} (onnx)")
    } else {
        base_model
    };
    let now = now_secs();

    // Weights row: the shared weights footprint (`model_bytes`, reported on
    // the first loaded instance). VRAM for llama rows, CPU RAM for onnx.
    let weights_vram = if is_onnx {
        0
    } else {
        weights_model_bytes(insts)
    };
    let weights_ram = if is_onnx {
        weights_model_bytes(insts)
    } else {
        0
    };
    let weights_live =
        if insts.iter().any(|i| inst_state(i) == "loaded") || weights_vram > 0 || weights_ram > 0 {
            "loaded".to_string()
        } else {
            insts
                .first()
                .map_or_else(|| "unloaded".to_string(), inst_state)
        };
    let mut rows = vec![PsRow {
        model: display,
        instance: "-".to_string(),
        vram: bytes_or_dash(weights_vram),
        sys_ram: bytes_or_dash(weights_ram),
        ctx: "-".to_string(),
        quant: "-".to_string(),
        kv: "-".to_string(),
        live: weights_live,
    }];

    let mut total_vram = weights_vram;
    let mut total_ram = weights_ram;
    for inst in insts {
        let raw_id = inst.get("id").and_then(Value::as_str).unwrap_or_default();
        let name = raw_id
            .strip_prefix(&format!("{model_key}:"))
            .unwrap_or(raw_id)
            .to_string();
        let vram = inst.get("vram_bytes").and_then(Value::as_u64).unwrap_or(0);
        let total = inst.get("total_bytes").and_then(Value::as_u64).unwrap_or(0);
        // The fork's `total_bytes` is all-memory and repeats the shared
        // weights (`model_bytes`) that `vram_bytes` excludes; the onnx rows
        // carry `model_bytes: 0` and a working-set-only `total_bytes`. The
        // CPU-RAM remainder is what stays after removing both the VRAM
        // resident figure and the double-counted weights.
        let weights = weights_model_bytes(insts);
        let sys_ram = total.saturating_sub(vram).saturating_sub(weights);
        total_vram = total_vram.saturating_add(vram);
        total_ram = total_ram.saturating_add(sys_ram);
        let policy = sleep_policy(config, model_key, &name, inst);
        let last_used = inst.get("last_used").and_then(Value::as_i64).unwrap_or(-1);
        rows.push(PsRow {
            model: String::new(),
            instance: name,
            vram: bytes_or_dash(vram),
            sys_ram: bytes_or_dash(sys_ram),
            ctx: inst
                .get("n_ctx")
                .and_then(Value::as_u64)
                .map_or_else(|| "-".to_string(), |v| v.to_string()),
            quant: quant.clone(),
            kv: kv.clone(),
            live: live_label(&inst_state(inst), policy, last_used, now),
        });
    }

    rows.push(PsRow {
        model: String::new(),
        instance: "TOTAL".to_string(),
        vram: bytes_or_dash(total_vram),
        sys_ram: bytes_or_dash(total_ram),
        ctx: "-".to_string(),
        quant: "-".to_string(),
        kv: "-".to_string(),
        live: "-".to_string(),
    });
    rows
}

/// Instance-cell prefix marking a snapshot row. A `:` can never start a real
/// instance name (ids carry at most one, as the model/name separator), so
/// the prefix is unambiguous — and the instance counter skips it.
const SNAPSHOT_PREFIX: &str = "snapshot:";

/// One distinct row per retained snapshot under its model: the snapshot
/// name is the restorable window (`<name>-resume` reloads that context),
/// every measure `-` (a snapshot is restorable state, not residency), and
/// `LIVE = snapshot`. The model display sits on the first row only, like
/// every other group.
fn snapshot_rows(model_key: &str, snaps: &[&Value]) -> Vec<PsRow> {
    let base_model = ollama_display(model_key, "");
    snaps
        .iter()
        .enumerate()
        .map(|(i, snap)| {
            let name = snap.get("name").and_then(Value::as_str).unwrap_or("?");
            PsRow {
                model: if i == 0 {
                    base_model.clone()
                } else {
                    String::new()
                },
                instance: format!("{SNAPSHOT_PREFIX}{name}"),
                vram: "-".to_string(),
                sys_ram: "-".to_string(),
                ctx: "-".to_string(),
                quant: "-".to_string(),
                kv: "-".to_string(),
                live: "snapshot".to_string(),
            }
        })
        .collect()
}

/// A model with no instance detail (lazy server down): one bare `unloaded`
/// row, display only — it contributes nothing resident to the fleet totals
/// (disk footprint is never counted as VRAM).
fn unloaded_row(model_key: &str) -> PsRow {
    PsRow {
        model: ollama_display(model_key, ""),
        instance: "-".to_string(),
        vram: "-".to_string(),
        sys_ram: "-".to_string(),
        ctx: "-".to_string(),
        quant: "-".to_string(),
        kv: "-".to_string(),
        live: "unloaded".to_string(),
    }
}

/// `0` renders as `-` (nothing resident).
fn bytes_or_dash(bytes: u64) -> String {
    if bytes == 0 {
        "-".to_string()
    } else {
        format_size(bytes)
    }
}

fn inst_state(inst: &Value) -> String {
    inst.get("state")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string()
}

/// The shared-weights bytes carried by the instance rows (`model_bytes`,
/// reported on the first loaded instance).
fn weights_model_bytes(insts: &[Value]) -> u64 {
    insts
        .iter()
        .filter_map(|i| i.get("model_bytes").and_then(Value::as_u64))
        .max()
        .unwrap_or(0)
}

/// Resident context memory for one instance row, for the fleet totals:
/// RAM (`total_bytes`) for onnx rows, VRAM (`vram_bytes`) for llama rows.
fn inst_mem_bytes(inst: &Value) -> u64 {
    let key = if inst.get("runtime").and_then(Value::as_str) == Some("onnx") {
        "total_bytes"
    } else {
        "vram_bytes"
    };
    inst.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Weights quantization for a model: the router-overlaid `quant` when
/// instance detail carries it, else the GGUF file-stem tag, else `-`.
fn model_quant(
    insts: &[Value],
    config: Option<&RouterConfig>,
    gguf_dir: &Path,
    model_key: &str,
) -> String {
    if let Some(inst) = insts.iter().find(|i| {
        i.get("quant")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    }) {
        return inst
            .get("quant")
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string();
    }
    let gguf_file = config_weights_path(config, gguf_dir, model_key)
        .or_else(|| gguf_weights_path(gguf_dir, model_key));
    quant_from_gguf_path(gguf_file.as_deref())
}

/// Quantization tag from a weights path's file stem (`-` when unknown or
/// the `latest` symlink name, which carries no quant information).
fn quant_from_gguf_path(path: Option<&Path>) -> String {
    match path
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().to_lowercase())
    {
        Some(tag) if !tag.is_empty() && tag != "latest" => tag,
        _ => "-".to_string(),
    }
}

/// KV-cache quantization for a model from its spawn defaults
/// (`cache_type_k/v`; the fleet default is `q8_0`). Split types render as
/// `k/v`.
fn cache_quant_label(config: Option<&RouterConfig>, model_key: &str) -> String {
    let (k, v) = match config {
        Some(cfg) => {
            let defaults = cfg.spawn_defaults_for(model_key);
            (defaults.cache_type_k.clone(), defaults.cache_type_v.clone())
        }
        None => ("q8_0".to_string(), "q8_0".to_string()),
    };
    if k == v {
        k
    } else {
        format!("{k}/{v}")
    }
}

/// The idle-sleep policy for one instance: the envelope row's own fields
/// when present (`pinned`/`no_sleep`/`sleep_idle_seconds`), else the
/// configured profile's, else [`SleepPolicy::Inherit`].
fn sleep_policy(
    config: Option<&RouterConfig>,
    model_key: &str,
    inst_name: &str,
    inst: &Value,
) -> SleepPolicy {
    let has_keys = inst.get("pinned").is_some()
        || inst.get("no_sleep").is_some()
        || inst.get("sleep_idle_seconds").is_some();
    if has_keys {
        return inst_sleep_policy(inst);
    }
    if let Some(profile) = config
        .and_then(|cfg| cfg.models.get(model_key))
        .and_then(|entry| {
            entry.effective_pool().iter().find(|p| {
                p.name.as_deref() == Some(inst_name) || p.group.as_deref() == Some(inst_name)
            })
        })
    {
        return profile_sleep_policy(profile);
    }
    inst_sleep_policy(inst)
}

fn profile_sleep_policy(profile: &InstanceProfile) -> SleepPolicy {
    if profile.pinned || profile.no_sleep {
        return SleepPolicy::Forever;
    }
    match profile.sleep_idle_seconds {
        Some(v) if v <= 0 => SleepPolicy::Forever,
        Some(v) => SleepPolicy::Window(i64::from(v)),
        None => SleepPolicy::Inherit,
    }
}

fn inst_sleep_policy(inst: &Value) -> SleepPolicy {
    let pinned = inst.get("pinned").and_then(Value::as_bool).unwrap_or(false);
    let no_sleep = inst
        .get("no_sleep")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if pinned || no_sleep {
        return SleepPolicy::Forever;
    }
    match inst.get("sleep_idle_seconds").and_then(Value::as_i64) {
        Some(v) if v <= 0 => SleepPolicy::Forever,
        Some(v) => SleepPolicy::Window(v),
        None => SleepPolicy::Inherit,
    }
}

/// The `LIVE` cell for one instance: the idle time remaining before sleep
/// (`1h28m`, `Forever`), `loaded` when no finite window is known or the
/// idle clock has not started (`last_used == -1`), or the raw state for
/// non-loaded rows (`unloaded`, `sleeping`, …).
fn live_label(state: &str, policy: SleepPolicy, last_used: i64, now: u64) -> String {
    if state != "loaded" {
        return state.to_string();
    }
    match policy {
        SleepPolicy::Forever => "Forever".to_string(),
        SleepPolicy::Inherit | SleepPolicy::Window(_) if last_used < 0 => "loaded".to_string(),
        SleepPolicy::Inherit => "loaded".to_string(),
        SleepPolicy::Window(w) => {
            let elapsed = now.saturating_sub(u64::try_from(last_used).unwrap_or(0));
            let remaining = (w as u64).saturating_sub(elapsed);
            format_keep_alive(remaining as i64)
        }
    }
}

/// Render the fleet table plus totals footer. Width tracks the live terminal
/// (`COLUMNS` fallback, then 100), clamped to a readable band.
pub fn render_ps_table(rows: &[PsRow], totals: &PsTotals) -> String {
    render_ps_table_sized(rows, totals, terminal_width())
}

/// Render at an explicit width (the test seam; production passes the live
/// terminal width).
pub fn render_ps_table_sized(rows: &[PsRow], totals: &PsTotals, width: u16) -> String {
    let mut out = render_text_table(
        &PS_HEADERS,
        &ps_cells(rows),
        &PS_RIGHT,
        0,
        &ps_min_widths(rows),
        width,
    );
    out.push('\n');
    out.push_str(&totals_footer(totals));
    out
}

/// Markdown twin of [`render_ps_table`]: the same columns as pipe tables
/// with full (never truncated) values, plus the same totals footer.
pub fn render_ps_markdown(rows: &[PsRow], totals: &PsTotals) -> String {
    let mut out = render_markdown_table(&PS_HEADERS, &ps_cells(rows));
    out.push_str(&totals_footer(totals));
    out
}

/// Fleet column headers, shared by the text and Markdown renders (Markdown
/// differs only in format and separators).
const PS_HEADERS: [&str; 8] = [
    "MODEL", "INSTANCE", "VRAM", "SYS RAM", "CTX", "QUANT", "KV", "LIVE",
];

/// Right-aligned columns (indexes into [`PS_HEADERS`]).
const PS_RIGHT: [bool; 8] = [false, false, true, true, true, false, false, false];

/// Rows as a plain cell grid — the one shared shape both renders consume.
fn ps_cells(rows: &[PsRow]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|r| {
            vec![
                r.model.clone(),
                r.instance.clone(),
                r.vram.clone(),
                r.sys_ram.clone(),
                r.ctx.clone(),
                r.quant.clone(),
                r.kv.clone(),
                r.live.clone(),
            ]
        })
        .collect()
}

/// Fixed column minima, except `INSTANCE` and `KV` which size to their
/// content (clamped): a `snapshot:<name>` cell is wider than any live
/// window name, and fixed minima would clip it even on wide terminals.
/// The `MODEL` column absorbs the remaining terminal width.
fn ps_min_widths(rows: &[PsRow]) -> Vec<usize> {
    const KV_MAX: usize = 11;
    const INSTANCE_MAX: usize = 24;
    let kv_w = rows
        .iter()
        .map(|r| r.kv.len())
        .max()
        .unwrap_or(2)
        .clamp(2, KV_MAX);
    let inst_w = rows
        .iter()
        .map(|r| r.instance.len())
        .max()
        .unwrap_or(10)
        .clamp(10, INSTANCE_MAX);
    vec![12, inst_w, 8, 8, 7, 6, kv_w, 9]
}

fn totals_footer(totals: &PsTotals) -> String {
    let instances = if totals.instances == 1 {
        "1 instance".to_string()
    } else {
        format!("{} instances", totals.instances)
    };
    format!(
        "total {} resident · {} model{} · {instances}\n",
        format_size(totals.resident_bytes),
        totals.models,
        if totals.models == 1 { "" } else { "s" },
    )
}

// ── weights helpers (moved from `commands::server`; single home) ────────────

// ── weights helpers (moved from `commands::server`; single home) ────────────

/// The configured `weights` path for a model key, if the entry declares
/// one. Relative declarations resolve under `gguf_dir` (absolute ones pass
/// through), matching the daemon's setup rule.
fn config_weights_path(
    config: Option<&RouterConfig>,
    gguf_dir: &Path,
    model_key: &str,
) -> Option<PathBuf> {
    config.and_then(|cfg| {
        cfg.models.get(model_key).and_then(|entry| {
            entry
                .weights
                .as_deref()
                .map(|w| RouterConfig::join_config_dir(gguf_dir, w))
        })
    })
}

/// Resolve a model key's GGUF file from the GGUF layout:
/// `<gguf_dir>/<key>/latest.gguf` or the smallest GGUF under the directory.
fn gguf_weights_path(gguf_dir: &Path, model_key: &str) -> Option<PathBuf> {
    let dir = gguf_dir.join(model_key);
    let read = std::fs::read_dir(&dir).ok()?;
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "gguf") {
            files.push(path);
        }
    }
    files
        .iter()
        .find(|p| p.file_stem().is_some_and(|s| s == "latest"))
        .or_else(|| {
            files
                .iter()
                .min_by_key(|p| std::fs::metadata(p).map_or(u64::MAX, |m| m.len()))
        })
        .cloned()
}

#[cfg(test)]
#[path = "../../tests/cli_ps_table.rs"]
mod tests;