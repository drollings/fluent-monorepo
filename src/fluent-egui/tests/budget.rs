//! Audit ledger: allocation shape, dependency direction, and bloat.
//!
//! Every assert here is API shape or source text, never wall-time: frame
//! budgets are observed in `benches/frame_budget.rs` and recorded there,
//! while gates pin what the frame may *hold* (no per-widget
//! channels, no raw spawns, no blocking I/O), which way dependencies point,
//! and exactly which files and default-feature dependencies exist. Growth
//! beyond this ledger fails loudly until the ledger itself is edited with a
//! justification — that edit is the review.
//!
//! Boundary rule pinned here: this crate is domain-blind scaffolding. It may
//! reuse display vocabulary (`protopyne-chart-model` rows), pure scale math
//! (`protopyne-chart-scale`), and the shared validation bridge
//! (`protopyne-types`), but it names no compute truth (`protopyne-kernels`),
//! no trading path (`server`/`strategy`/`venue`), and no trading-domain
//! vocabulary. Domain display truth lives in the terminal crate, which
//! carries its own ledger.
//!
//! "Domain-blind" is not "vocabulary-free": the crate legitimately speaks in
//! chart *shapes* — ribbons, pivots, oscillators, delta bars, bull/bear style
//! keys. Both halves of that boundary are asserted: a deny-list of the
//! trading types this crate must never name, and an allow-list of the chart
//! words it owns, each with its reason. A one-sided gate would be a claim the
//! crate does not make.

use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_text(path: &PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Source file text for one `src/<name>.rs` module.
fn read_src(name: &str) -> String {
    read_text(&manifest_dir().join("src").join(name))
}

/// Non-comment lines: `//`, `//!`, and `///` lines document but never
/// depend, so direction scans skip them (same rule as the linkage test).
fn code_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('/'))
        .collect()
}

/// Every `*.rs` file under `src/`, recursively, as `src`-relative
/// slash-separated paths in sorted order. Recursion is deliberate: a render
/// submodule (e.g. `views/portfolio.rs`) must appear in the ledger, not slip
/// past because it lives one level down. The non-render trees (`tests/`,
/// `benches/`, `examples/`) are siblings of `src/`, so they are exempt by
/// construction and never enter this list.
fn src_file_names() -> Vec<String> {
    let mut names = Vec::new();
    collect_rs(&manifest_dir().join("src"), Path::new(""), &mut names);
    names.sort();
    names
}

fn collect_rs(dir: &Path, prefix: &Path, out: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("list {}: {e}", dir.display()));
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let rel = prefix.join(entry.file_name());
        if path.is_dir() {
            collect_rs(&path, &rel, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// Whether `text` names `token` as a whole word rather than as part of a
/// longer identifier. Every occurrence is checked, not just the first.
fn mentions(text: &str, token: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let needle: Vec<char> = token.chars().collect();
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    if needle.is_empty() || needle.len() > chars.len() {
        return false;
    }
    chars.windows(needle.len()).enumerate().any(|(start, window)| {
        if window != needle.as_slice() {
            return false;
        }
        let before_ok = start == 0 || !is_word(chars[start - 1]);
        let after = start + needle.len();
        let after_ok = after >= chars.len() || !is_word(chars[after]);
        before_ok && after_ok
    })
}

fn forbid_in_src(token: &str, why: &str) {
    for name in src_file_names() {
        for line in code_lines(&read_src(&name)) {
            assert!(
                !line.contains(token),
                "{name} must not contain {token:?}: {why} (line: {line})"
            );
        }
    }
}

#[test]
fn bloat_ledger_pins_source_files() {
    // `style.rs` earns its line by being the one per-indicator paint-override
    // component (colors, stroke width). Folding it into `config.rs` would put
    // chart-wide theme truth and per-indicator overrides in one struct, and
    // folding it into `fields.rs` would make the schema reader own the schema.
    // Its own file keeps "what the chart looks like" (theme) and "what this
    // indicator looks like" (style) separable, and the settings pane composes
    // them.
    // `wheel.rs` earns its line by isolating the region→target decision (a
    // wheel acts on the plot's time axis, a price strip's own axis, or a
    // floating list) from the interaction machine that applies it. Folding it
    // into `interaction.rs` would drag `egui` geometry into that headless
    // module; folding it into `slot.rs` would make a chart-wide decision look
    // like a legend concern. Its own file is the smaller honest boundary.
    // `context_menu.rs` earns its line by isolating the secondary-click
    // decision (only a pane's axis strip answers, everywhere else raises
    // nothing) plus the one generic popup wrapper from both the machine that
    // applies the picked action and the domain crate that fills the menu.
    // Folding detection into `hittest.rs` would make a gesture-routing
    // question look like a readout question; folding the popup into `slot.rs`
    // would tie a chart-wide menu to the legend stack.
    assert_eq!(
        src_file_names(),
        vec![
            "annotations.rs",
            "config.rs",
            "context_menu.rs",
            "error.rs",
            "feed.rs",
            "fields.rs",
            "hittest.rs",
            "interaction.rs",
            "lib.rs",
            "modal.rs",
            "paint.rs",
            "plan.rs",
            "replay.rs",
            "slot.rs",
            "style.rs",
            "view.rs",
            "viewport.rs",
            "wheel.rs",
        ],
        "a new source file must justify itself by editing this ledger"
    );
}

/// Parse the `[dependencies]` key list from the crate manifest.
///
/// Handles both TOML spellings of the same set — inline keys under
/// `[dependencies]` and per-dependency table headers `[dependencies.NAME]` —
/// and tracks inline-table nesting so a multi-line value's continuation lines
/// are never mistaken for new dependency names.
fn main_dep_keys() -> Vec<String> {
    let text = read_text(&manifest_dir().join("Cargo.toml"));
    let mut keys = Vec::new();
    let mut in_deps = false;
    let mut depth = 0i32;
    for line in text.lines().map(str::trim) {
        if depth == 0 && line.starts_with('[') {
            if let Some(rest) = line.strip_prefix("[dependencies") {
                if let Some(name) = rest.strip_prefix('.').and_then(|n| n.strip_suffix(']')) {
                    if !name.is_empty() {
                        keys.push(name.trim().to_owned());
                    }
                    in_deps = false;
                } else {
                    in_deps = line == "[dependencies]";
                }
            } else {
                in_deps = false;
            }
            continue;
        }
        if !in_deps {
            continue;
        }
        if depth == 0 && !line.is_empty() && !line.starts_with('#') {
            let key = line.split('=').next().unwrap_or("").trim();
            if !key.is_empty() {
                keys.push(key.to_owned());
            }
        }
        depth = (depth + bracket_delta(line)).max(0);
    }
    keys.sort();
    keys.dedup();
    keys
}

/// Net inline-table depth on one line, ignoring quoted strings and trailing
/// `#` comments, so nested manifests parse without a TOML dependency.
fn bracket_delta(line: &str) -> i32 {
    let mut delta = 0i32;
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '#' => break,
                '{' | '[' => delta += 1,
                '}' | ']' => delta -= 1,
                _ => {}
            },
        }
    }
    delta
}

#[test]
fn bloat_ledger_pins_default_feature_deps() {
    assert_eq!(
        main_dep_keys(),
        vec![
            "bon",
            "common-core",
            "eframe",
            "egui",
            "fluent-concurrency",
            "fluent-dag",
            "fluent-wvr",
            "protopyne-chart-model",
            "protopyne-chart-scale",
            "protopyne-types",
            "serde",
            "serde_json",
            "thiserror",
            "tokio",
        ],
        "a new default-feature dependency must justify itself by editing this ledger"
    );
    let text = read_text(&manifest_dir().join("Cargo.toml"));
    // Main `[dependencies]` section body: lines after the header up to the
    // next section header (a line starting with `[`). Values may contain
    // `[` themselves (`features = ["pine"]`), so only a leading `[` ends it.
    let mut in_deps = false;
    let mut body = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = trimmed == "[dependencies]";
            continue;
        }
        if in_deps {
            body.push_str(line);
            body.push('\n');
        }
    }
    let deps_section = body;
    for trading in [
        "protopyne-kernels",
        "protopyne-server",
        "protopyne-strategy",
        "protopyne-venue",
    ] {
        assert!(
            !deps_section.contains(trading),
            "{trading} must not enter main dependencies (scaffolding stays off compute truth and the trading path)"
        );
    }
    assert!(
        deps_section.contains("optional = true"),
        "the windowing shell stays optional so the default build needs no backend"
    );
}

#[test]
fn scaffolding_names_no_compute_truth_or_trading_path() {
    for token in [
        "protopyne_kernels",
        "protopyne-kernels",
        "protopyne_server",
        "protopyne_strategy",
        "protopyne_venue",
        "protopyne-server",
        "protopyne-strategy",
        "protopyne-venue",
    ] {
        forbid_in_src(token, "compute truth and trading crates are never a scaffolding dependency");
    }
}

/// Trading-domain types and modules the scaffolding must never name, with the
/// crate each one lives in.
///
/// This is a boundary, not a sample. The earlier list held six kernel
/// identifiers that already appeared nowhere in the crate, so it could not
/// fail; these are the type and module names that *would* let a trading
/// concept into domain-blind scaffolding — a grant, a proposal, an order, a
/// position, a Pine value or plot. Each entry names the crate that owns the
/// concept, so the failure message says where the name belongs.
const TRADING_VOCABULARY_DENIED: &[(&str, &str)] = &[
    // `protopyne-strategy`
    ("Grant", "strategy"),
    ("GrantPatch", "strategy"),
    ("GrantBudgets", "strategy"),
    ("SignalThresholds", "strategy"),
    ("TradeThresholds", "strategy"),
    ("PortfolioThresholds", "strategy"),
    ("evaluate_grant", "strategy"),
    ("Proposal", "strategy"),
    ("ProposalState", "strategy"),
    ("RiskEngine", "strategy"),
    ("RiskLimits", "strategy"),
    ("Direction", "strategy"),
    ("StructureKind", "strategy"),
    // `protopyne-venue`
    ("TradingState", "venue"),
    ("Venue", "venue"),
    ("VenueRegistry", "venue"),
    ("OrderIntent", "venue"),
    ("OrderRecord", "venue"),
    ("SpreadPosition", "venue"),
    ("ReconcileGate", "venue"),
    ("safe_submit", "venue"),
    ("Journal", "venue"),
    // `protopyne-kernels` — Pine and the indicator registry
    ("PineEngine", "kernels"),
    ("PineRuntime", "kernels"),
    ("PineValue", "kernels"),
    ("RunOutcome", "kernels"),
    ("PlotPrimitive", "kernels"),
    ("parse_program", "kernels"),
    ("IndicatorRegistry", "kernels"),
    ("IndicatorResult", "kernels"),
    ("ParamSet", "kernels"),
    // `protopyne-types` — the exact money and quantity denominations
    ("Money", "types"),
    ("Qty", "types"),
];

/// Generic chart vocabulary this crate legitimately owns, with why.
///
/// A deny-list alone is a lie of omission: the crate really does speak in
/// ribbons, pivots, oscillators, delta bars, and bull/bear styling, because
/// those are properties of a *chart*, not of a *trade*. Naming them here
/// keeps the boundary honest — the gate says what is owned and why, instead
/// of implying the crate has no vocabulary at all. Each entry must also be
/// used, so a stale allow-list entry fails.
/// Each entry is the concrete symbol the crate ships, so the match is
/// whole-word and cannot be satisfied by an unrelated word in a comment.
const SCAFFOLDING_VOCABULARY_OWNED: &[(&str, &str)] = &[
    ("ribbon", "a bounded band between two smoothed series — a shape, not a signal"),
    ("pivot_marks", "detected local extremes — geometry, not a trade decision"),
    ("oscillator_points", "a normalized bounded series mapped to pixels — a scale, not a strategy"),
    ("delta_bar_geoms", "change-over-window bars — arithmetic, not a position"),
    ("\"bull\"", "a style key naming an up close, resolved to a color at paint time"),
    ("\"bear\"", "a style key naming a down close, resolved to a color at paint time"),
];

/// Code lines in every `src/` module, with the file they came from.
fn src_code() -> Vec<(String, String)> {
    src_file_names()
        .into_iter()
        .flat_map(|name| {
            code_lines(&read_src(&name))
                .into_iter()
                .map(str::to_owned)
                .map(move |line| (name.clone(), line))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn trading_vocabulary_hits(lines: &[(String, String)]) -> Vec<String> {
    let mut hits = Vec::new();
    for (file, line) in lines {
        for (token, owner) in TRADING_VOCABULARY_DENIED {
            if mentions(line, token) {
                hits.push(format!("{file} names {token} (belongs to {owner}): {line}"));
            }
        }
    }
    hits
}

#[test]
fn scaffolding_names_no_trading_domain_vocabulary() {
    let lines = src_code();
    let hits = trading_vocabulary_hits(&lines);
    assert!(
        hits.is_empty(),
        "trading-domain vocabulary lives in the crate that owns the concept; \
         the scaffolding is domain-blind and speaks only in chart shapes. \
         Offenders: {hits:?}"
    );
}

#[test]
fn owned_chart_vocabulary_is_actually_used() {
    // The allow-list is not a permission slip: each owned word must appear in
    // the crate's own code, so the list cannot quietly grow to cover
    // something that was never there.
    let lines = src_code();
    let mut unused = Vec::new();
    for (token, why) in SCAFFOLDING_VOCABULARY_OWNED {
        let used = lines.iter().any(|(_, line)| mentions(line, token));
        if !used {
            unused.push(*token);
        }
        assert!(!why.trim().is_empty(), "owned vocabulary {token} needs a justification");
    }
    assert!(unused.is_empty(), "owned chart vocabulary no longer appears in the crate: {unused:?}");
}

#[test]
fn trading_vocabulary_gate_fires_on_a_synthetic_module() {
    // Negative control: the deny-list reaches code that really does import a
    // trading concept, and leaves code that only speaks chart shapes alone.
    // Fed through `code_lines` exactly as the live gate does, so the comment
    // is stripped before the vocabulary check rather than by luck.
    let lines: Vec<(String, String)> = [
        ("paint.rs", "use protopyne_strategy::grant::Grant;"),
        ("plan.rs", "let grant: GrantPatch = load();"),
        ("view.rs", "let band = ribbon_bands(&view, &viewport);"),
        ("paint.rs", "// Grant is discussed here, and this line documents it"),
        ("paint.rs", "let GrantFree = 1;"),
    ]
    .into_iter()
    .flat_map(|(file, text)| {
        code_lines(text).into_iter().map(str::to_owned).map(move |line| (file.to_owned(), line))
    })
    .collect();
    let hits = trading_vocabulary_hits(&lines);
    assert_eq!(hits.len(), 2, "both grant references are caught, the comment is not: {hits:?}");
    assert!(hits.iter().all(|hit| hit.contains("strategy")), "each names the owning crate");
    assert!(
        !hits.iter().any(|hit| hit.contains("ribbon")),
        "chart vocabulary the crate owns is not a violation"
    );
}

#[test]
fn display_floats_never_parse_back_into_money() {
    for token in ["rust_decimal", "Decimal::", "parse::<Decimal>", "Decimal>"] {
        forbid_in_src(token, "the Decimal -> f64 boundary is one-way");
    }
}

#[test]
fn frame_holds_no_per_widget_machinery() {
    // Channels belong to pools, never to widgets: `ResultPool` owns its
    // oneshot-per-submit internally; paint code holds no channel at all.
    forbid_in_src("oneshot", "no per-widget channels on the frame path");
    forbid_in_src("tokio::spawn", "tasks are Scope-owned, never raw-spawned");
    forbid_in_src("block_on", "no sync-over-async bridges in paint code");
    forbid_in_src("std::fs", "no blocking file I/O in paint code");
    forbid_in_src("std::net", "no blocking network I/O in paint code");
    forbid_in_src("tokio::time::sleep", "no sleeps in paint code");
    forbid_in_src("thread::sleep", "no sleeps in paint code");
    forbid_in_src("Middleware", "no middleware layer per frame");
    // Timing lives in the terminal crate (plan-build timing over the domain
    // topology): no scaffolding module may name the wrapper.
    forbid_in_src("Instrumented", "timing covers domain plan builds in the terminal crate only");
    forbid_in_src("LatencyHistogram", "timing covers domain plan builds in the terminal crate only");
}

#[test]
fn timing_is_observed_never_asserted() {
    // Gates pin shape; the bench harness owns wall-time. No gate may sprout
    // a stopwatch: tests assert structure, benches record numbers.
    for entry in std::fs::read_dir(manifest_dir().join("tests")).expect("tests list") {
        let entry = entry.expect("test entry");
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        // This ledger names the forbidden tokens in its own literals, so it
        // exempts itself: the rule binds gate code, not the ledger text.
        if file_name == "budget.rs" {
            continue;
        }
        let text = read_text(&entry.path());
        for line in code_lines(&text) {
            assert!(
                !line.contains("Instant::now"),
                "{file_name} must not time itself (budgets are observed in benches/, never asserted)"
            );
            assert!(
                !line.contains("criterion"),
                "{file_name} must not bench itself (the harness lives in benches/)"
            );
        }
    }
    assert!(
        manifest_dir().join("benches").join("frame_budget.rs").is_file(),
        "frame budgets ride the bench harness, which must exist"
    );
}

#[test]
fn theme_ships_no_embedded_assets() {
    // No icon catalog, no SVG embedding, no baked-in blobs: the theme is
    // six role-named hex strings resolved at paint time, lazily by
    // construction (nothing to load until a frame asks for a color).
    forbid_in_src("include_str!", "no embedded text blobs in paint code");
    forbid_in_src("include_bytes!", "no embedded binary blobs in paint code");
    forbid_in_src(".svg", "no SVG assets in paint code");
    let embedded: Vec<String> = walk_files(&manifest_dir())
        .into_iter()
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("svg"))
        .map(|path| path.display().to_string())
        .collect();
    assert!(embedded.is_empty(), "no SVG assets ship with the crate: {embedded:?}");
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("list {}: {e}", dir.display()));
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Reachability
// ---------------------------------------------------------------------------

/// Workspace crates whose sources count as consumers of this crate's
/// re-exported items, beyond its own tree.
///
/// The terminal is the scaffolding's only production consumer and it lives in
/// a different crate: the emitters it calls every frame — candles, ribbon
/// fill, area fill, oscillators, pivots — would all read as unwired here while
/// being called there. "Every crate that depends on this one" is not derivable
/// from inside this crate, so the surface is named, and a second consumer has
/// to be added here deliberately.
const DOWNSTREAM_CONSUMERS: &[&str] = &["src/protopyne-egui"];

/// Re-exported `pub` items that nothing names, with the reason each still
/// ships unwired.
///
/// **Axis: producer-confidence.** This measures doubt that a module is
/// wired. It says nothing about whether a pane draws correctly or anything
/// about a trading decision, and it never deletes: it fails the build and a
/// human or an agent decides. An unwired capability means the wiring step is
/// missing, not that the capability is unwanted — these are complete,
/// exported emitters whose paint call site has not been built.
///
/// An entry is a claim about the tree, not a permanent pardon: it is asserted
/// against the live scan, so an entry whose item gains a consumer fails until
/// the entry is deleted.
///
/// The table is currently empty: every emitter is named by at least one
/// test, and the terminal crate (a downstream consumer) calls the frame
/// path every run. Test coverage pins behavior, not production use — an
/// emitter called only by tests still has its production-wiring question
/// open, and that decision lives with the pane work, not with this ledger.
const REACHABILITY_EXEMPTIONS: &[(&str, &str, &str)] = &[];

/// One top-level `pub` declaration in a `src/` module.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Decl {
    /// `src`-relative module path that declares it.
    module: String,
    /// Crate-relative file that declares it.
    file: String,
    /// Declaration keyword (`fn`, `struct`, `const`, …).
    kind: &'static str,
    /// Declared name.
    name: String,
    /// The declaration line itself, excluded from the consumer search so an
    /// item is never wired by the act of declaring it.
    line: String,
}

/// Top-level `pub` declarations in one module's code lines.
///
/// Scope is deliberately one level: an `impl`-block method is reached through
/// its type, so the type's own reachability is what this gate measures.
/// Reporting every accessor as unwired would make the signal useless.
fn pub_declarations(module: &str, text: &str) -> Vec<Decl> {
    text.lines()
        .filter_map(|raw| {
            let line = raw.trim_end();
            let rest = line.strip_prefix("pub ")?;
            let (kind, tail) = ["fn", "struct", "enum", "const", "static", "type", "trait", "mod"]
                .into_iter()
                .find_map(|kind| {
                    rest.strip_prefix(kind)
                        .and_then(|tail| tail.strip_prefix(' '))
                        .map(|tail| (kind, tail))
                })?;
            let name = tail.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')).next()?;
            (!name.is_empty()).then(|| Decl {
                module: module.to_owned(),
                file: format!("src/{module}"),
                kind,
                name: name.to_owned(),
                line: line.to_owned(),
            })
        })
        .collect()
}

/// Every identifier the crate root re-exports through a `pub use`.
fn crate_reexports() -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    let mut pending = false;
    for line in code_lines(&read_src("lib.rs")) {
        if line.starts_with("pub use") {
            pending = true;
        }
        if pending {
            for ident in line.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')) {
                if ident.starts_with(char::is_alphabetic) {
                    names.insert(ident.to_owned());
                }
            }
        }
        if pending && line.trim_end().ends_with(';') {
            pending = false;
        }
    }
    names
}

/// Every consumer surface, as code lines per file key: this crate's `src/`
/// modules, `tests/`, `benches/`, and `examples/`, plus the sources of every
/// [`DOWNSTREAM_CONSUMERS`] crate.
///
/// Two files are excluded, each for a stated reason:
///
/// - `src/lib.rs`, because a re-export is a declaration of intent, not a use
///   (the predicate skips it too, so the exclusion holds for any caller);
/// - `tests/budget.rs`, because this ledger spells out every item name it
///   asks about, so counting it would make every gate vacuous.
fn consumer_lines() -> std::collections::BTreeMap<String, Vec<String>> {
    let mut files: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let absorb = |files: &mut std::collections::BTreeMap<String, Vec<String>>, key: String, text: String| {
        let lines: Vec<String> = code_lines(&text).into_iter().map(str::to_owned).collect();
        files.insert(key, lines);
    };
    for name in src_file_names() {
        if name != "lib.rs" {
            absorb(&mut files, format!("src/{name}"), read_src(&name));
        }
    }
    for dir in ["tests", "benches", "examples"] {
        let base = manifest_dir().join(dir);
        if !base.is_dir() {
            continue;
        }
        for path in walk_files(&base) {
            if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                let relative = path
                    .strip_prefix(&base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                absorb(&mut files, format!("{dir}/{relative}"), read_text(&path));
            }
        }
    }
    let root = manifest_dir().join("..").join("..");
    for downstream in DOWNSTREAM_CONSUMERS {
        for path in walk_files(&root.join(downstream)) {
            if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                let relative = path
                    .strip_prefix(root.join(downstream))
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                absorb(&mut files, format!("{downstream}/{relative}"), read_text(&path));
            }
        }
    }
    files.remove("tests/budget.rs");
    files
}

/// Re-exported declarations that no consumer line names.
///
/// Two exclusions are intrinsic to the rule rather than left to the caller, so
/// a synthetic control exercises the same predicate the live gate uses:
/// `src/lib.rs` (a re-export is not a use) and the item's own declaration line
/// (declaring an item does not wire it). A reference anywhere else in the
/// declaring module does count, so a type reachable through an exported
/// function's signature is wired.
fn unwired_exports(
    decls: &[Decl],
    reexports: &std::collections::BTreeSet<String>,
    consumers: &std::collections::BTreeMap<String, Vec<String>>,
) -> Vec<Decl> {
    decls
        .iter()
        .filter(|decl| reexports.contains(&decl.name))
        .filter(|decl| {
            !consumers.iter().any(|(file, lines)| {
                if *file == "src/lib.rs" {
                    return false;
                }
                lines.iter().any(|line| {
                    mentions(line, &decl.name) && !(*file == decl.file && *line == decl.line)
                })
            })
        })
        .cloned()
        .collect()
}

fn live_unwired_exports() -> Vec<Decl> {
    let modules =
        src_file_names().into_iter().filter(|name| name != "lib.rs").collect::<Vec<_>>();
    let decls: Vec<Decl> = modules
        .iter()
        .flat_map(|module| pub_declarations(module, &read_src(module)))
        .collect();
    unwired_exports(&decls, &crate_reexports(), &consumer_lines())
}

#[test]
fn src_module_must_have_a_declared_consumer() {
    let unwired = live_unwired_exports();
    let undeclared: Vec<&Decl> = unwired
        .iter()
        .filter(|decl| {
            !REACHABILITY_EXEMPTIONS
                .iter()
                .any(|(module, name, _)| *module == decl.module && *name == decl.name)
        })
        .collect();
    assert!(
        undeclared.is_empty(),
        "every `pub` item the crate root re-exports must be named by a \
         consumer, or carry an exemption entry saying why it ships unwired. \
         Undeclared: {undeclared:?}"
    );
}

#[test]
fn every_reachability_exemption_names_a_live_item_and_a_reason() {
    let unwired = live_unwired_exports();
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for (module, name, reason) in REACHABILITY_EXEMPTIONS {
        assert!(!reason.trim().is_empty(), "{module}::{name} exemption needs a reason");
        assert!(!seen.contains(&(*module, *name)), "{module}::{name} is exempted twice");
        seen.push((module, name));
        assert!(
            unwired.iter().any(|decl| decl.module == *module && decl.name == *name),
            "{module}::{name} is exempted but is wired — delete the exemption"
        );
    }
}

#[test]
fn downstream_consumer_surface_is_actually_scanned() {
    // The cross-crate surface is a claim; this is the proof it is read. If
    // the path is wrong the emitters the terminal calls would read as unwired
    // and the reachability gate would fail loudly — so assert it directly.
    let consumers = consumer_lines();
    assert!(
        consumers.keys().any(|key| key.starts_with("src/protopyne-egui/")),
        "the terminal crate must be in the consumer surface"
    );
    // The terminal's candle blit lives in the library workspace module, so
    // the pin scans the whole downstream crate surface (src and examples)
    // rather than a single example shell.
    assert!(
        consumers.iter().any(|(key, lines)| {
            key.starts_with("src/protopyne-egui/")
                && lines.iter().any(|line| mentions(line, "paint_candles_tinted"))
        }),
        "the terminal's candle blit is read somewhere in the downstream surface"
    );
    assert!(
        !consumers.contains_key("tests/budget.rs"),
        "the ledger's own text never counts as a consumer"
    );
}

#[test]
fn reachability_gate_fires_on_a_synthetic_unwired_item() {
    // Negative control, one case per rule the gate rests on:
    // - a consumer in another crate's file counts;
    // - a reference inside the declaring module counts, so a type reachable
    //   through an exported function's signature is wired;
    // - the declaration line itself does not count;
    // - a crate-root re-export does not count;
    // - an item the root never re-exports is out of scope entirely.
    let reexports: std::collections::BTreeSet<String> =
        ["DownstreamWired", "SignatureWired", "ReexportedOnly"]
            .into_iter()
            .map(str::to_owned)
            .collect();
    let decl = |module: &str, kind: &'static str, name: &str| Decl {
        module: module.to_owned(),
        file: format!("src/{module}"),
        kind,
        name: name.to_owned(),
        line: format!("pub {kind} {name};"),
    };
    let decls = vec![
        decl("a.rs", "fn", "DownstreamWired"),
        decl("b.rs", "struct", "SignatureWired"),
        decl("c.rs", "fn", "ReexportedOnly"),
        decl("d.rs", "fn", "Private"),
    ];
    let consumers = std::collections::BTreeMap::from([
        ("src/a.rs".to_owned(), vec!["fn helper() {}".to_owned()]),
        (
            "src/b.rs".to_owned(),
            vec!["pub fn builder() -> SignatureWired { SignatureWired }".to_owned()],
        ),
        ("src/c.rs".to_owned(), vec!["fn helper() {}".to_owned()]),
        ("src/d.rs".to_owned(), vec!["fn helper() {}".to_owned()]),
        ("src/lib.rs".to_owned(), vec!["pub use c::ReexportedOnly;".to_owned()]),
        (
            "src/protopyne-egui/examples/demo.rs".to_owned(),
            vec!["paint(DownstreamWired());".to_owned()],
        ),
    ]);
    assert_eq!(
        unwired_exports(&decls, &reexports, &consumers)
            .iter()
            .map(|decl| decl.name.as_str())
            .collect::<Vec<_>>(),
        vec!["ReexportedOnly"],
        "only an item nothing but its own declaration and the crate root names \
         is unwired"
    );
}
