//! Control-plane audits: strategy selection branches in exactly one place,
//! and every control-plane numeric dial states which axis it gates on.
//!
//! The scans read library sources only (tests pin the seams, and the server
//! binary stays the explicit composition root). A "threshold" here is a
//! numeric field whose author opted it into the control plane with
//! `#[field(...)]` but stated no axis; bare numeric fields never opted in,
//! so the schema never advertises them as dials. Each allow-list entry names
//! a field that is provably not a trade decision gate, with the reason why;
//! the list is asserted exact in both directions, so a new untagged dial
//! fails until its author tags it or justifies it, and a stale entry fails
//! until it is deleted.

use std::path::{Path, PathBuf};

fn package_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn library_roots() -> Vec<PathBuf> {
    let base = package_dir();
    let mut roots = vec![base.join("src")];
    let entries = std::fs::read_dir(base.join("..")).expect("src/ lists");
    let mut others: Vec<PathBuf> = entries
        .filter_map(|entry| {
            let path = entry.expect("dir entry").path();
            let name = path.file_name()?.to_str()?.to_owned();
            if name.starts_with("protopyne-") && path.join("src").is_dir() {
                Some(path.join("src"))
            } else {
                None
            }
        })
        .collect();
    others.sort();
    for root in others {
        if root != base.join("src") {
            roots.push(root);
        }
    }
    roots
}

fn rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(root).expect("sources list");
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn is_library_source(file: &Path) -> bool {
    if file
        .components()
        .any(|part| part.as_os_str() == "tests" || part.as_os_str() == "examples")
    {
        return false;
    }
    if file.file_name().is_some_and(|name| name == "main.rs") {
        return false;
    }
    true
}

fn all_library_sources() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in library_roots() {
        rs_files(&root, &mut files);
    }
    files.retain(|file| is_library_source(file));
    files.sort();
    files
}

fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component::*;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn repo_relative(file: &Path) -> String {
    let root = normalize_lexically(&package_dir().join("..").join(".."));
    normalize_lexically(file).strip_prefix(&root).map_or_else(
        |_| file.display().to_string(),
        |relative| relative.to_string_lossy().replace('\\', "/"),
    )
}

#[test]
fn strategy_selection_branches_only_in_the_registry_shim() {
    // The registry shim owns the only `match` on the kind; the preview
    // route carries the kind on its request shape but never branches on it.
    // A new production mention anywhere else fails here first.
    let mut violations = Vec::new();
    for file in all_library_sources() {
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "rehearsal.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("source reads");
        for (index, line) in text.lines().enumerate() {
            if !line.contains("StrategyKind") {
                continue;
            }
            let allowed = name == "mod.rs"
                && (line.contains("use crate::rehearsal")
                    || line.contains("pub strategy: StrategyKind"));
            let trimmed = line.trim_start();
            let is_arm = trimmed.starts_with("StrategyKind::") && line.contains("=>");
            if !allowed || is_arm {
                violations.push(format!("{}:{}: {line}", file.display(), index + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "strategy-kind mentions outside the registry shim:\n{}",
        violations.join("\n")
    );
}

/// Numeric fields the control plane advertises without an axis tag, each
/// provably not a trade decision gate. Third element is the reason.
const UNTAGGED_NON_GATES: &[(&str, &str, &str)] = &[
    // Display dials: they configure the picture and never enter a decision.
    (
        "src/protopyne-egui/src/chart.rs",
        "threshold",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/chart.rs",
        "alpha_low",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/chart.rs",
        "alpha_high",
        "display dial",
    ),
    ("src/protopyne-egui/src/chart.rs", "height", "display dial"),
    (
        "src/protopyne-egui/src/chart.rs",
        "bar_spacing",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/delta_volume_profile.rs",
        "bins",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/delta_volume_profile.rs",
        "lookback",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/delta_volume_profile.rs",
        "width",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/delta_volume_profile.rs",
        "offset",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/delta_volume_profile.rs",
        "value_area_pct",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/fast_mas.rs",
        "first_ma_length",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/fast_mas.rs",
        "second_ma_length",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/fast_mas.rs",
        "signal_ma_length",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/fast_mas.rs",
        "mean_ma_length",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/fast_mas.rs",
        "trend_ma_length",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/fast_mas.rs",
        "base_ma_length",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "eit_alpha",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "fill_alpha",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "ribbon_health_k",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "ma_health_k",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "delta_unit",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "pivot_threshold",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "ribbon_alpha_low",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "ribbon_alpha_high",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "pivots_lookback",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "signal_period",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "signal_param1",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "mean_period",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "mean_param1",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "brightness",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "fast_period",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "fast_smooth_period",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "fast_k",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "slow_period",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "slow_smooth_period",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/integration_lite.rs",
        "slow_k",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/oscillator_panel.rs",
        "period",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/oscillator_panel.rs",
        "smooth_period",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/oscillator_panel.rs",
        "gain",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/oscillator_panel.rs",
        "offset",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/oscillator_panel.rs",
        "k",
        "display dial",
    ),
    (
        "src/protopyne-egui/src/oscillator_panel.rs",
        "mult",
        "display dial",
    ),
    // Execution bounds: they cap evaluated work, never gate a trade.
    (
        "src/protopyne-kernels/src/runtime.rs",
        "max_bars",
        "execution bound",
    ),
    (
        "src/protopyne-kernels/src/runtime.rs",
        "warmup_bars",
        "execution bound",
    ),
    // Operational capacity bounds for the HTTP, feed, and ingest perimeter.
    (
        "src/protopyne-server/src/boot.rs",
        "max_concurrent_requests",
        "operational capacity bound",
    ),
    (
        "src/protopyne-server/src/boot.rs",
        "feed_capacity",
        "operational capacity bound",
    ),
    (
        "src/protopyne-server/src/boot.rs",
        "feed_interval_ms",
        "operational capacity bound",
    ),
    (
        "src/protopyne-server/src/ingest.rs",
        "queue_bound",
        "operational capacity bound",
    ),
    (
        "src/protopyne-server/src/ingest.rs",
        "bootstrap_bars",
        "operational capacity bound",
    ),
    (
        "src/protopyne-server/src/ingest.rs",
        "max_lag_bars",
        "operational capacity bound",
    ),
    (
        "src/protopyne-server/src/ingest.rs",
        "max_gap_bars",
        "operational capacity bound",
    ),
    (
        "src/protopyne-server/src/ingest.rs",
        "max_repair_span_ms",
        "operational capacity bound",
    ),
    (
        "src/protopyne-server/src/ingest.rs",
        "repair_concurrency",
        "operational capacity bound",
    ),
    // Durability plumbing bounds: price scale, lag windows, repair worker.
    (
        "src/protopyne-store/src/catalog.rs",
        "digits",
        "durability plumbing bound",
    ),
    (
        "src/protopyne-store/src/coverage.rs",
        "max_lag_bars",
        "durability plumbing bound",
    ),
    (
        "src/protopyne-store/src/coverage.rs",
        "max_gap_bars",
        "durability plumbing bound",
    ),
    (
        "src/protopyne-store/src/repair.rs",
        "max_repair_span_ms",
        "durability plumbing bound",
    ),
    (
        "src/protopyne-store/src/repair.rs",
        "repair_concurrency",
        "durability plumbing bound",
    ),
    (
        "src/protopyne-store/src/repair.rs",
        "backoff_base_ms",
        "durability plumbing bound",
    ),
    (
        "src/protopyne-store/src/repair.rs",
        "backoff_max_shift",
        "durability plumbing bound",
    ),
    (
        "src/protopyne-store/src/repair.rs",
        "backoff_ceiling_ms",
        "durability plumbing bound",
    ),
    // Transport and session plumbing: auth retry, shutdown, adapter bounds.
    (
        "src/protopyne-venue/src/auth.rs",
        "refresh_early_secs",
        "transport plumbing bound",
    ),
    (
        "src/protopyne-venue/src/auth.rs",
        "max_attempts",
        "transport plumbing bound",
    ),
    (
        "src/protopyne-venue/src/auth.rs",
        "backoff_base_ms",
        "transport plumbing bound",
    ),
    (
        "src/protopyne-venue/src/auth.rs",
        "backoff_jitter_pct",
        "transport plumbing bound",
    ),
    (
        "src/protopyne-venue/src/auth.rs",
        "min_refresh_interval_secs",
        "transport plumbing bound",
    ),
    (
        "src/protopyne-venue/src/auth.rs",
        "ttl_secs",
        "transport plumbing bound",
    ),
    (
        "src/protopyne-venue/src/session.rs",
        "shutdown_timeout_secs",
        "transport plumbing bound",
    ),
    (
        "src/protopyne-venue/src/tastytrade.rs",
        "quote_max_age_secs",
        "transport plumbing bound",
    ),
    (
        "src/protopyne-venue/src/tastytrade.rs",
        "max_attempts",
        "transport plumbing bound",
    ),
    (
        "src/protopyne-venue/src/tastytrade.rs",
        "backoff_base_ms",
        "transport plumbing bound",
    ),
];

const NUMERIC_TYPES: &[&str] = &[
    "f64", "f32", "i64", "u64", "i32", "u32", "isize", "usize", "i8", "u8",
];

fn is_numeric_type(raw: &str) -> bool {
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if NUMERIC_TYPES.contains(&compact.as_str()) {
        return true;
    }
    if let Some(inner) = compact
        .strip_prefix("Option<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return NUMERIC_TYPES.contains(&inner);
    }
    false
}

/// A numeric field the control plane advertises (`#[field(...)]`) without an
/// axis tag: the file plus the field name.
fn untagged_control_plane_numerics(file: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(file).expect("source reads");
    let mut found = Vec::new();
    let mut pending: Option<String> = None;
    let mut block = String::new();
    let mut in_block = false;
    for line in text.lines() {
        if line.contains("#[field(") {
            in_block = true;
            block.clear();
            block.push_str(line);
            block.push('\n');
            if line.contains(")]") {
                in_block = false;
                pending = Some(std::mem::take(&mut block));
            }
            continue;
        }
        if in_block {
            block.push_str(line);
            block.push('\n');
            if line.contains(")]") {
                in_block = false;
                pending = Some(std::mem::take(&mut block));
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("///")
            || trimmed.starts_with("//")
            || trimmed.starts_with("#[")
        {
            continue;
        }
        if let Some((name, ty)) = split_pub_field(trimmed) {
            if pending.is_some() && is_numeric_type(ty) {
                let attr = pending.take().expect("block held");
                if !attr.contains("axis") {
                    found.push((repo_relative(file), name.to_owned()));
                }
            } else {
                pending = None;
            }
        } else {
            pending = None;
        }
    }
    found
}

/// Split a `pub name: Type` item into its name and raw type text.
fn split_pub_field(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("pub ")?.trim_start();
    // Skip `pub fn`, `pub struct`, `pub enum`, `pub use`, `pub mod`, `pub const` items.
    for keyword in [
        "fn ", "struct ", "enum ", "use ", "mod ", "const ", "static ", "type ",
    ] {
        if rest.starts_with(keyword) {
            return None;
        }
    }
    let (name, after) = rest.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let ty = after.split([',', ';', '=']).next().unwrap_or("").trim();
    if ty.is_empty() {
        return None;
    }
    Some((name, ty))
}

#[test]
fn every_control_plane_numeric_states_its_axis() {
    let mut found = Vec::new();
    for file in all_library_sources() {
        found.extend(untagged_control_plane_numerics(&file));
    }
    found.sort();
    found.dedup();
    let allowed: Vec<(String, String)> = UNTAGGED_NON_GATES
        .iter()
        .map(|(file, field, _)| ((*file).to_owned(), (*field).to_owned()))
        .collect();
    let unlisted: Vec<String> = found
        .iter()
        .filter(|entry| !allowed.contains(entry))
        .map(|(file, field)| format!("{file}:{field}"))
        .collect();
    assert!(
        unlisted.is_empty(),
        "control-plane numerics without an axis tag and without a recorded reason:\n{}",
        unlisted.join("\n")
    );
    for (file, field, reason) in UNTAGGED_NON_GATES {
        assert!(
            !reason.trim().is_empty(),
            "{file}:{field} allow-list entry needs a reason"
        );
        assert!(
            found.contains(&((*file).to_owned(), (*field).to_owned())),
            "{file}:{field} is allow-listed but no longer untagged — delete the entry"
        );
    }
}
