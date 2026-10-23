//! Audit ledger: allocation shape, dependency direction, and bloat.
//!
//! Same posture as the scaffolding ledger (shape and source text, never
//! wall-time; budgets observed in `benches/chart_budget.rs`), plus the
//! terminal-specific direction pins: the compute truth is read, never
//! forked (no second indicator table, no second regime constant, no second
//! error type); `strategy`/`venue` stay a dev-only round-trip proof — never
//! a main dependency — while the server enters only as an optional
//! composition-root dependency (the example outside `src/` names it; render
//! modules never do). Growth beyond this ledger fails loudly until the
//! ledger itself is edited with a justification — that edit is the review.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Workspace root: two levels up from a member crate directory.
fn workspace_root() -> PathBuf {
    manifest_dir().join("..").join("..")
}

fn read_text(path: &Path) -> String {
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

/// Trading tokens that must never appear in a `src/` module: the trading
/// crates are read at dev time only, never linked by the library.
///
/// Shared with the gateway composition-root ledger, which enforces the same
/// direction over the same tree — one list, one scan shape, one edit point.
pub(crate) const TRADING_TOKENS: &[&str] = &[
    "protopyne_server",
    "protopyne_strategy",
    "protopyne_venue",
    "protopyne-server",
    "protopyne-strategy",
    "protopyne-venue",
];

/// Whether `text` names `token` as a whole word rather than as part of a
/// longer identifier.
///
/// Every occurrence is checked, not just the first: `paint_ribbon` occurs in
/// `tests/topology.rs` as the prefix of `standard_replay_controller_…`, and
/// stopping at that first hit would report a genuinely-wired `paint_ribbon`
/// as unused.
pub(crate) fn mentions(text: &str, token: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let needle: Vec<char> = token.chars().collect();
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    if needle.is_empty() || needle.len() > chars.len() {
        return false;
    }
    chars
        .windows(needle.len())
        .enumerate()
        .any(|(start, window)| {
            if window != needle.as_slice() {
                return false;
            }
            let before_ok = start == 0 || !is_word(chars[start - 1]);
            let after = start + needle.len();
            let after_ok = after >= chars.len() || !is_word(chars[after]);
            before_ok && after_ok
        })
}

/// Every non-comment line of `text` that names `token`.
pub(crate) fn lines_naming(text: &str, token: &str) -> Vec<String> {
    code_lines(text)
        .into_iter()
        .filter(|line| mentions(line, token))
        .map(str::to_owned)
        .collect()
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

/// The composition-root direction rule, in one place: no `src/` module names
/// a trading crate. Both the terminal ledger and the gateway composition-root
/// ledger enforce it, and they now share this body and this token list
/// instead of keeping two copies in sync by hand.
pub(crate) fn forbid_trading_tokens_in_src() -> Vec<String> {
    let mut offenders = Vec::new();
    for name in src_file_names() {
        for token in TRADING_TOKENS {
            for line in lines_naming(&read_src(&name), token) {
                offenders.push(format!("{name} names {token}: {line}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the trading crates are read at dev time only and never linked by a \
         render module — the server enters solely as an optional \
         composition-root dependency named from outside src/. Offenders: {offenders:?}"
    );
    offenders
}

#[test]
fn bloat_ledger_pins_source_files() {
    assert_eq!(
        src_file_names(),
        vec![
            "annotations.rs",
            "chart.rs",
            "delta_volume_profile.rs",
            "export.rs",
            "fast_mas.rs",
            "feed.rs",
            "fonts.rs",
            "grant_form.rs",
            "indicators.rs",
            "integration_lite.rs",
            "lib.rs",
            "oscillators.rs",
            "pine_overlay.rs",
            "prefs.rs",
            "regime_labels.rs",
            "replay.rs",
            "session.rs",
            "topology.rs",
            "views/broker.rs",
            "views/health.rs",
            "views/inbox.rs",
            "views/journal.rs",
            "views/mod.rs",
            "views/portfolio.rs",
            "views/tickets.rs",
            "widgets.rs",
            "workspace.rs",
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
            "chrono",
            "common-core",
            "eframe",
            "egui",
            "fluent-concurrency",
            "fluent-dag",
            "fluent-egui",
            "fluent-wvr",
            "protopyne-chart-model",
            "protopyne-chart-scale",
            "protopyne-kernels",
            "protopyne-server",
            "protopyne-types",
            "protopyne-view",
            "serde",
            "serde_json",
            "tokio",
        ],
        "a new default-feature dependency must justify itself by editing this ledger"
    );
    let text = read_text(&manifest_dir().join("Cargo.toml"));
    let deps_section = manifest_section(&text, "[dependencies]");
    for trading in ["protopyne-strategy", "protopyne-venue"] {
        assert!(
            !deps_section.contains(trading),
            "{trading} must not enter main dependencies (trading path stays a dev-only round-trip proof)"
        );
    }
    // The server is the one exception, and only as an optional dependency:
    // the composition-root example outside `src/` constructs `ServerGateway`
    // behind the `server-gateway` feature, while render modules keep reading
    // through the gateway abstraction (pinned by
    // `terminal_never_mutates_the_trading_path` and the gateway ledger test).
    //
    // What `optional` buys is a *default library* build with no link to the
    // trading path. It does not make the crate trading-free: `strategy`,
    // `venue`, and `server` are non-optional **dev-dependencies**, so
    // `cargo test`, `cargo bench`, and `cargo run --example` all link them.
    // The claim is narrower than "the build never links the trading path",
    // and stating it any wider would be false.
    gate_optional_dependency(
        &text,
        "protopyne-server",
        "the composition root's server stays optional so a default library \
         build never links the trading path (dev-dependencies still do, by design)",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    gate_optional_dependency(
        &text,
        "eframe",
        "the windowing shell stays optional so a default build needs no backend",
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

/// Body of one manifest section: lines after the header up to the next
/// section header. Values may contain `[` themselves
/// (`features = ["pine"]`), so only a leading `[` ends it.
fn manifest_section(manifest: &str, header: &str) -> String {
    let mut body = String::new();
    let mut inside = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == header;
            continue;
        }
        if inside {
            body.push_str(line);
            body.push('\n');
        }
    }
    body
}

/// The line in `section` that declares `dep`, or `None` when it does not.
///
/// Matching on the key rather than on the substring is what makes this check
/// able to fail: a section-wide `contains("optional = true")` is satisfied by
/// any dependency's optionality, so adding an optional dependency silently
/// de-fangs the rule it was meant to enforce.
fn dependency_line<'a>(section: &'a str, dep: &str) -> Option<&'a str> {
    section.lines().find(|line| {
        let key = line.split('=').next().unwrap_or("").trim();
        key == dep
    })
}

/// Require `dep` to be declared optional in the main `[dependencies]`
/// section. Returns the declaring line on success so callers can quote it in
/// a failure message; returns the reason on failure so the gate can be
/// exercised against a synthetic manifest.
fn gate_optional_dependency(manifest: &str, dep: &str, why: &str) -> Result<String, String> {
    let section = manifest_section(manifest, "[dependencies]");
    let line = dependency_line(&section, dep).ok_or_else(|| {
        format!("{dep} is not declared in [dependencies] at all, so {why} cannot hold")
    })?;
    if line.contains("optional = true") {
        Ok(line.to_owned())
    } else {
        Err(format!("{dep} must stay optional so {why} (line: {line})"))
    }
}

#[test]
fn optional_dependency_gate_fires_when_the_attribute_is_removed() {
    // Negative control: the gate is anchored to one dependency's line, so
    // dropping `optional = true` from that line fails even while another
    // dependency in the same section keeps the flag — the exact failure a
    // section-wide substring check cannot see.
    let guarded = "[dependencies]\n\
                   protopyne-server = { path = \"../protopyne-server\", optional = true }\n\
                   eframe = { version = \"0.36\", optional = true, default-features = false }\n";
    gate_optional_dependency(guarded, "eframe", "the default build needs no backend")
        .expect("the real shape passes");

    let unguarded = guarded.replace(
        "eframe = { version = \"0.36\", optional = true, default-features = false }",
        "eframe = { version = \"0.36\", default-features = false }",
    );
    assert_ne!(
        guarded, unguarded,
        "the synthetic edit must actually change the manifest"
    );
    assert!(
        manifest_section(&unguarded, "[dependencies]").contains("optional = true"),
        "the other dependency is still optional, which is what makes the gate non-trivial"
    );
    let refused =
        gate_optional_dependency(&unguarded, "eframe", "the default build needs no backend")
            .expect_err("removing the attribute must fail the gate");
    assert!(
        refused.contains("eframe must stay optional"),
        "reason names the dependency: {refused}"
    );

    // A dependency declared nowhere in the section is a different failure and
    // must not read as "optional".
    let absent = gate_optional_dependency(
        "[dependencies]\neframe = { version = \"0.36\", optional = true }\n",
        "protopyne-server",
        "the composition root",
    );
    assert!(
        absent.is_err(),
        "a missing declaration is never an optional declaration"
    );
}

#[test]
fn trading_path_enters_only_as_a_dev_dependency() {
    // The claim `optional` supports is narrow, so the ledger states it
    // narrowly: a default **library** build links no trading crate, and the
    // test, bench, and example builds link all three through dev-dependencies.
    // Both halves are asserted, because the tempting shorthand — "the build
    // never links the trading path" — is false and a future edit could make
    // it true by accident in the wrong direction.
    let text = read_text(&manifest_dir().join("Cargo.toml"));
    let deps = manifest_section(&text, "[dependencies]");
    let dev_deps = manifest_section(&text, "[dev-dependencies]");
    for trading in ["protopyne-strategy", "protopyne-venue"] {
        assert!(
            dependency_line(&deps, trading).is_none(),
            "{trading} must stay out of [dependencies] so a default library \
             build links no trading crate"
        );
    }
    for trading in ["protopyne-strategy", "protopyne-server"] {
        assert!(
            dependency_line(&dev_deps, trading).is_some(),
            "{trading} is a non-optional dev-dependency: `cargo test`, \
             `cargo bench`, and `cargo run --example` link it, which is the \
             round-trip proof the terminal ledger relies on"
        );
    }
    assert!(
        dependency_line(&dev_deps, "protopyne-venue").is_none(),
        "the venue crate is not linked here at all: the round-trip proof goes \
         through `protopyne-server`, which owns the session"
    );
    assert!(
        dependency_line(&deps, "protopyne-server").is_some(),
        "the server also enters [dependencies], behind the `server-gateway` \
         feature and optional so a default build does not link it"
    );
}

#[test]
fn store_enters_only_as_a_dev_dependency() {
    // The terminal reads chart data through the gateway abstraction, never
    // by linking the store into its default library build. The store is a
    // dev-dependency so the workspace tape suite can seed one behind a
    // store-backed gateway; a default build links neither.
    let text = read_text(&manifest_dir().join("Cargo.toml"));
    let deps = manifest_section(&text, "[dependencies]");
    let dev_deps = manifest_section(&text, "[dev-dependencies]");
    assert!(
        dependency_line(&deps, "protopyne-store").is_none(),
        "protopyne-store must stay out of [dependencies] so a default library \
         build links no store"
    );
    assert!(
        dependency_line(&dev_deps, "protopyne-store").is_some(),
        "the workspace tape suite seeds a store: the store is a dev-dependency"
    );
}

#[test]
fn the_workspace_plans_through_the_bounded_gateway_read() {
    // Positive pin: the workspace plans its chart window through the
    // bounded, store-aware read, so a symbol or timeframe switch replans
    // through the store when one backs the gateway — never a second memo.
    assert!(
        read_src("workspace.rs").contains("chart_bars_for_limit"),
        "the workspace must plan its window through the bounded gateway read"
    );
}

#[test]
fn terminal_never_mutates_the_trading_path() {
    for token in TRADING_TOKENS {
        forbid_in_src(
            token,
            "trading crates are read at dev time only, never a main dependency",
        );
    }
    forbid_trading_tokens_in_src();
}

/// Whole-word matching, exercised directly: a token must not match inside a
/// longer identifier, and must match across punctuation and repetition.
#[test]
fn token_matching_is_whole_word() {
    assert!(mentions("protopyne_server::Gateway", "protopyne_server"));
    assert!(mentions("(protopyne-server)", "protopyne-server"));
    assert!(mentions("use protopyne_venue::{Venue};", "protopyne_venue"));
    assert!(!mentions("protopyne_venue_extra", "protopyne_venue"));
    assert!(!mentions("my_protopyne_venue", "protopyne_venue"));
    assert!(!mentions("protopyne_ven", "protopyne_venue"));
    assert_eq!(
        lines_naming(
            "a protopyne_venue b\n// protopyne_venue\n",
            "protopyne_venue"
        ),
        vec!["a protopyne_venue b".to_owned()]
    );
}

#[test]
fn compute_truth_is_reused_never_forked() {
    // One table, one band, one error type: the terminal adapts kernel
    // outputs for display but never redefines them.
    forbid_in_src(
        "enum EguiError",
        "the error type lives in the scaffolding (single truth)",
    );
    forbid_in_src(
        "REGIME_HYSTERESIS =",
        "the band constant lives in kernels (re-export only)",
    );
    forbid_in_src(
        "struct PanePlan",
        "the plan lives in the scaffolding (topology pours in)",
    );
    forbid_in_src("struct PaneCache", "the cache lives in the scaffolding");
    forbid_in_src("struct Viewport", "the viewport lives in the scaffolding");
    forbid_in_src("struct ThemeConfig", "the theme lives in the scaffolding");
}

/// Venue-owned order and ticket types the terminal must never name: the
/// paint edge fills a neutral `TicketDraft`, and the core maps it to an intent.
/// Mirrors `fluent-egui/tests/budget.rs`, which denies `OrderIntent` to the
/// scaffolding edge for the same reason — one paint edge is not enough.
const TICKET_TYPES_DENIED: &[(&str, &str)] = &[
    (
        "OrderIntent",
        "venue — the core builds it from a TicketDraft",
    ),
    ("ManualTicket", "venue — the core journals it"),
    ("TicketTracker", "venue — the core owns the waiting set"),
    (
        "OrderSide",
        "venue — the draft's TicketSide is the neutral tag",
    ),
    ("client_id_for", "venue — the core mints ticket identity"),
];

#[test]
fn the_terminal_names_no_venue_order_or_ticket_types() {
    let mut hits = Vec::new();
    for name in src_file_names() {
        let text = read_src(&name);
        for (token, owner) in TICKET_TYPES_DENIED {
            for line in lines_naming(&text, token) {
                hits.push(format!("{name} names {token} (belongs to {owner}): {line}"));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "the intent schema stays core-side; the terminal fills a TicketDraft. Offenders: {hits:?}"
    );
}

#[test]
fn the_terminal_never_authors_money_or_quantities() {
    // The core owns every economic value a replay books: the terminal reads
    // them through the gateway and parses the core's own decimal text, so a
    // constructed `Money` or `Qty` here would be a second authority. Values
    // still *flow* (typed fields, parses); only construction is forbidden —
    // the rule the `Decimal::` fence could not see.
    for token in ["Money::", "Qty::"] {
        forbid_in_src(
            token,
            "money and quantity are read from the core, never minted at the paint edge",
        );
    }
}

#[test]
fn display_floats_never_parse_back_into_money() {
    for token in ["rust_decimal", "Decimal::", "parse::<Decimal>", "Decimal>"] {
        forbid_in_src(token, "the Decimal -> f64 boundary is one-way");
    }
}

#[test]
fn display_fence_marks_kernel_preview_frames() {
    // Positive pin: every module that builds a kernel preview frame fences
    // display legs — the fence type must be named exactly where display floats
    // enter kernel frames, so a future refactor cannot silently drop the mark.
    // `topology.rs` owns the shared full-OHLC preview frame and the
    // single-column source frames; `integration_lite` owns its close-only
    // synth frame. `fast_mas` routes through the shared builder, so it pins
    // the delegation instead of naming the fence twice.
    for module in ["topology.rs", "integration_lite.rs"] {
        assert!(
            code_lines(&read_src(module))
                .iter()
                .any(|line| line.contains("DisplayOnly")),
            "{module} must route preview frames through the DisplayOnly fence"
        );
    }
    assert!(
        code_lines(&read_src("fast_mas.rs"))
            .iter()
            .any(|line| line.contains("source_frame")),
        "fast_mas.rs must route preview frames through the shared source-frame builder"
    );
}

/// Code lines of the function whose signature line contains `marker`,
/// from the signature through its column-zero closing brace.
///
/// Top-level functions close at column zero, while `format!` braces sit
/// mid-line — so the scan ends exactly at the function's own close and a
/// brace inside a string literal can never end it early.
fn fn_body_lines(text: &str, marker: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains(marker))
        .unwrap_or_else(|| panic!("expected a function matching {marker:?}"));
    let mut end = None;
    for (offset, line) in lines[start..].iter().enumerate() {
        if offset > 0 && *line == "}" {
            end = Some(start + offset);
            break;
        }
    }
    let close = end.unwrap_or_else(|| panic!("function matching {marker:?} never closes"));
    lines[start..=close].join("\n")
}

/// Allocation-shape pin: `area_view` moves the caller's series and builds
/// no per-bar vectors, so the per-frame area handoff allocates nothing of
/// its own. A future copy reintroduces per-frame allocation — if the shape
/// ever needs one again, this ledger is the edit point, with the
/// justification recorded here.
#[test]
fn bloat_ledger_pins_area_view_allocation_shape() {
    let body = fn_body_lines(&read_src("chart.rs"), "pub fn area_view");
    let code = code_lines(&body);
    for token in ["to_vec", "vec!", "clone"] {
        let hits: Vec<&&str> = code.iter().filter(|line| mentions(line, token)).collect();
        assert!(
            hits.is_empty(),
            "area_view must not contain {token:?} (move, never copy): {hits:?}"
        );
    }
}

/// Allocation-shape pin: `PaneCache::pin` promotes keys through the shared
/// `Arc` cache, so the per-frame pin clones no series. The cache lives in
/// the scaffolding crate, so this pin reads its source across the crate
/// boundary — same ledger, same rule.
#[test]
fn bloat_ledger_pins_pane_cache_pin_allocation_shape() {
    let path = workspace_root()
        .join("src")
        .join("fluent-egui")
        .join("src")
        .join("plan.rs");
    let body = fn_body_lines(&read_text(&path), "pub fn pin");
    let code = code_lines(&body);
    let hits: Vec<&&str> = code.iter().filter(|line| mentions(line, "clone")).collect();
    assert!(
        hits.is_empty(),
        "PaneCache::pin must not clone (Arc promotion only): {hits:?}"
    );
}

/// Workspace member directories (`src/protopyne-view`), in manifest order.
///
/// Derived, never hand-listed: the fence below scans every crate in the
/// workspace, so a crate added after this ledger was written is covered the
/// day it is added. A hardcoded list silently exempts every crate that is
/// not on it — which is how the neutral read crate came to be able to name
/// the display escape at all.
fn workspace_member_dirs() -> Vec<String> {
    let text = read_text(&workspace_root().join("Cargo.toml"));
    let members = text
        .split_once("members = [")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once(']'))
        .map(|(list, _)| list)
        .expect("workspace manifest declares a members list");
    let dirs: Vec<String> = members
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.trim_matches(',').trim_matches('"').to_owned())
        .filter(|entry| !entry.is_empty())
        .collect();
    assert!(
        !dirs.is_empty(),
        "the workspace must declare at least one member"
    );
    dirs
}

/// The fence's type and module path must never be named outside the paint
/// crates and its home crate: display floats cross into kernel preview frames
/// only at the terminal, never into decisions.
///
/// The fence leaks one way, through `reveal`, and this is the only gate on it.
/// That is why the crate list is derived from the workspace manifest rather
/// than spelled out: an unlisted crate is an unenforced crate, and the crate
/// most likely to want a display float is the one added last.
fn display_escape_offenders(members: &[String], root: &Path) -> (usize, Vec<String>) {
    let files: Vec<(String, String)> = members
        .iter()
        .flat_map(|dir| {
            walk_files(&root.join(dir))
                .into_iter()
                // Read Rust sources only: member trees also carry binary
                // fixtures (e.g. trimmed Parquet histories) that are not
                // valid UTF-8 and never enter this scan.
                .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
                .map(move |path| (path.display().to_string(), read_text(&path)))
        })
        .collect();
    display_escape_offenders_in(members, &files)
}

/// The scan itself, over already-collected file texts so a synthetic control
/// can drive the same predicate the live gate uses.
fn display_escape_offenders_in(
    members: &[String],
    files: &[(String, String)],
) -> (usize, Vec<String>) {
    let home = "protopyne-chart-model";
    let paint_edges = ["fluent-egui", "protopyne-egui"];
    let decision: Vec<&str> = members
        .iter()
        .map(|dir| dir.rsplit('/').next().unwrap_or(dir))
        .filter(|name| *name != home && !paint_edges.contains(name))
        .collect();
    let escapees: Vec<String> = files
        .iter()
        .filter(|(path, _)| {
            path.ends_with(".rs")
                && members.iter().any(|dir| path.contains(&format!("{dir}/")))
                && decision
                    .iter()
                    .any(|name| path.contains(&format!("{name}/")))
        })
        .flat_map(|(path, text)| {
            code_lines(text)
                .into_iter()
                .filter(|line| {
                    ["DisplayOnly", "display_only"]
                        .iter()
                        .any(|t| line.contains(t))
                })
                .map(move |line| format!("{path}: {line}"))
                .collect::<Vec<_>>()
        })
        .collect();
    (decision.len(), escapees)
}

#[test]
fn display_escape_stays_out_of_decision_crates() {
    let (scanned, escapees) = display_escape_offenders(&workspace_member_dirs(), &workspace_root());
    assert!(
        scanned >= 5,
        "the fence scan must cover the decision crates, saw {scanned}"
    );
    assert!(
        escapees.is_empty(),
        "the display escape must stay in the paint crates and its home crate; \
         every other crate is decision code. Offenders: {escapees:?}"
    );
}

#[test]
fn display_escape_fence_fires_on_a_synthetic_workspace_member() {
    // Negative control: the fence follows the member list, so a decision crate
    // that names the escape is reported, and a paint crate or the fence's home
    // that names it is not. `protopyne-view` is the crate the hand-written
    // directory list used to miss, so it stands in for the class of crate most
    // likely to want a display float.
    let members = vec![
        "src/protopyne-types".to_owned(),
        "src/protopyne-view".to_owned(),
    ];
    let files = vec![
        (
            "src/protopyne-types/src/bar.rs".to_owned(),
            "let px: f64 = 1.0;".to_owned(),
        ),
        (
            "src/protopyne-view/src/views.rs".to_owned(),
            "let price = DisplayOnly::new(1.0).reveal();".to_owned(),
        ),
        (
            "src/protopyne-view/src/notes.txt".to_owned(),
            "DisplayOnly".to_owned(),
        ),
        (
            "src/protopyne-egui/src/integration_lite.rs".to_owned(),
            "let price = DisplayOnly::new(1.0).reveal();".to_owned(),
        ),
        (
            "src/protopyne-chart-model/src/display_only.rs".to_owned(),
            "pub struct DisplayOnly<T>(T);".to_owned(),
        ),
    ];
    let (scanned, offenders) = display_escape_offenders_in(&members, &files);
    assert_eq!(scanned, 2, "both listed members are decision crates");
    assert_eq!(
        offenders.len(),
        1,
        "only the decision crate's Rust file is an offender: {offenders:?}"
    );
    assert!(
        offenders[0].starts_with("src/protopyne-view/src/views.rs"),
        "the neutral read crate is covered: {offenders:?}"
    );
    // A live scan over the same two members is clean, which is what makes the
    // control meaningful rather than vacuous.
    let (live_scanned, live_offenders) = display_escape_offenders(&members, &workspace_root());
    assert_eq!(live_scanned, 2);
    assert!(
        live_offenders.is_empty(),
        "neither crate names the escape today"
    );
}

#[test]
fn frame_holds_no_per_widget_machinery() {
    forbid_in_src("oneshot", "no per-widget channels on the frame path");
    forbid_in_src("tokio::spawn", "tasks are Scope-owned, never raw-spawned");
    forbid_in_src("block_on", "no sync-over-async bridges in paint code");
    forbid_in_src("std::fs", "no blocking file I/O in paint code");
    forbid_in_src("std::net", "no blocking network I/O in paint code");
    forbid_in_src("tokio::time::sleep", "no sleeps in paint code");
    forbid_in_src("thread::sleep", "no sleeps in paint code");
    forbid_in_src("Middleware", "no middleware layer per frame");
    // Timing covers plan builds only: `chart.rs` (via `timed_plan_build`)
    // is the single module allowed to name the wrapper.
    for name in src_file_names() {
        if name == "chart.rs" {
            continue;
        }
        for line in code_lines(&read_src(&name)) {
            assert!(
                !line.contains("Instrumented"),
                "{name} must not time per-bar work (timing covers plan builds only)"
            );
        }
    }
    assert!(
        code_lines(&read_src("chart.rs"))
            .iter()
            .any(|line| line.contains("Instrumented")),
        "plan-build timing must stay composed (the wrapper has exactly one home)"
    );
}

#[test]
fn timing_is_observed_never_asserted() {
    for entry in std::fs::read_dir(manifest_dir().join("tests")).expect("tests list") {
        let entry = entry.expect("test entry");
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
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
        manifest_dir()
            .join("benches")
            .join("chart_budget.rs")
            .is_file(),
        "domain budgets ride the bench harness, which must exist"
    );
}

#[test]
fn theme_ships_no_embedded_assets() {
    forbid_in_src("include_str!", "no embedded text blobs in paint code");
    forbid_in_src("include_bytes!", "no embedded binary blobs in paint code");
    forbid_in_src(".svg", "no SVG assets in paint code");
    let embedded: Vec<String> = walk_files(&manifest_dir())
        .into_iter()
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("svg"))
        .map(|path| path.display().to_string())
        .collect();
    assert!(
        embedded.is_empty(),
        "no SVG assets ship with the crate: {embedded:?}"
    );
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("list {}: {e}", dir.display()));
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

/// Re-exported `pub` items that nothing in the crate names, with the reason
/// each still ships unwired.
///
/// **Axis: producer-confidence.** This measures doubt that a module is
/// wired. It says nothing about whether a pane draws correctly, whether a
/// chart type has a picture, or anything about a trading decision, and it
/// never deletes: it fails the build and a human or an agent decides. A
/// zero-consumer count is never on its own a reason to remove a named
/// requirement — an unwired capability means the wiring step is missing, not
/// that the capability is unwanted.
///
/// An entry is a claim about the tree, not a permanent pardon: it is asserted
/// against the live scan, so an entry whose item gains a consumer fails until
/// the entry is deleted.
const REACHABILITY_EXEMPTIONS: &[(&str, &str, &str)] = &[];

/// Top-level `pub` declarations in one module's code lines.
///
/// Scope is deliberately one level: an `impl`-block method is reached through
/// its type, so the type's own reachability is what this gate measures.
/// Reporting every accessor as unwired would make the signal useless.
fn pub_declarations(module: &str, text: &str) -> Vec<Decl> {
    text.lines()
        .filter_map(|raw| {
            // Column zero only: an indented method belongs to a type, not to
            // the module's own surface.
            let line = raw.trim_end();
            let rest = line.strip_prefix("pub ")?;
            let (kind, tail) = [
                "fn", "struct", "enum", "const", "static", "type", "trait", "mod",
            ]
            .into_iter()
            .find_map(|kind| {
                rest.strip_prefix(kind)
                    .and_then(|tail| tail.strip_prefix(' '))
                    .map(|tail| (kind, tail))
            })?;
            let name = tail
                .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .next()?;
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
///
/// A `pub use` is a declaration of intent, not a use: re-exporting an item
/// nothing calls is exactly the shape this gate exists to surface, so the
/// root's re-export block is never counted as a consumer.
fn crate_reexports() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
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

/// Every consumer surface, as code lines per crate-relative file: all `src/`
/// modules plus `tests/`, `benches/`, and `examples/`.
///
/// Tests, benches, and examples count because a suite that exercises an
/// emitter keeps it honest the same way a production caller does — an item
/// used only by its own suite is used. Two files are excluded, each for a
/// stated reason:
///
/// - `src/lib.rs`, because a re-export is not a use;
/// - `tests/budget.rs`, because this ledger spells out every item name it
///   asks about, so counting it would make every gate vacuous.
fn consumer_lines() -> BTreeMap<String, Vec<String>> {
    let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in src_file_names() {
        if name != "lib.rs" {
            let lines: Vec<String> = code_lines(&read_src(&name))
                .into_iter()
                .map(str::to_owned)
                .collect();
            files.insert(format!("src/{name}"), lines);
        }
    }
    for tree in ["tests", "benches", "examples"] {
        let dir = manifest_dir().join(tree);
        if !dir.is_dir() {
            continue;
        }
        for path in walk_files(&dir) {
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let relative = path
                .strip_prefix(&dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let lines: Vec<String> = code_lines(&read_text(&path))
                .into_iter()
                .map(str::to_owned)
                .collect();
            files.insert(format!("{tree}/{relative}"), lines);
        }
    }
    files.remove("tests/budget.rs");
    files
}

/// Re-exported declarations that no consumer line names.
///
/// Two exclusions are intrinsic to the rule rather than left to the caller,
/// so a synthetic control exercises the same predicate the live gate uses:
///
/// - `src/lib.rs` — a re-export is a declaration of intent, not a use;
/// - the item's own declaration line — declaring an item does not wire it.
///
/// A reference anywhere else in the declaring module does count, so a type
/// reachable through an exported function's signature is wired.
fn unwired_exports(
    decls: &[Decl],
    reexports: &BTreeSet<String>,
    consumers: &BTreeMap<String, Vec<String>>,
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
    let modules = src_file_names()
        .into_iter()
        .filter(|name| name != "lib.rs")
        .collect::<Vec<_>>();
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
        assert!(
            !reason.trim().is_empty(),
            "{module}::{name} exemption needs a reason"
        );
        assert!(
            !seen.contains(&(*module, *name)),
            "{module}::{name} is exempted twice"
        );
        seen.push((module, name));
        assert!(
            unwired
                .iter()
                .any(|decl| decl.module == *module && decl.name == *name),
            "{module}::{name} is exempted but is wired — delete the exemption"
        );
    }
}

#[test]
fn reachability_gate_fires_on_a_synthetic_unwired_item() {
    // Negative control, one case per rule the gate rests on:
    // - a sibling-module consumer counts;
    // - a reference inside the declaring module counts, so a type reachable
    //   through an exported function's signature is wired;
    // - the declaration line itself does not count, so re-exporting is not
    //   using;
    // - a crate-root re-export does not count.
    // `Private` is deliberately absent: an item the root never re-exports is
    // not this gate's business, wired or not.
    let reexports: BTreeSet<String> = ["Wired", "SignatureWired", "ReexportedOnly"]
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
        decl("a.rs", "fn", "Wired"),
        decl("b.rs", "struct", "SignatureWired"),
        decl("c.rs", "fn", "ReexportedOnly"),
        decl("d.rs", "fn", "Private"),
    ];
    let consumers = BTreeMap::from([
        (
            "src/a.rs".to_owned(),
            vec!["fn caller() { Wired(); }".to_owned()],
        ),
        (
            "src/b.rs".to_owned(),
            vec!["pub fn builder() -> SignatureWired { SignatureWired }".to_owned()],
        ),
        ("src/c.rs".to_owned(), vec!["fn helper() {}".to_owned()]),
        ("src/d.rs".to_owned(), vec!["fn helper() {}".to_owned()]),
        (
            "src/lib.rs".to_owned(),
            vec!["pub use c::ReexportedOnly;".to_owned()],
        ),
    ]);
    assert_eq!(
        unwired_exports(&decls, &reexports, &consumers)
            .iter()
            .map(|decl| decl.name.as_str())
            .collect::<Vec<_>>(),
        vec!["ReexportedOnly"],
        "only an item nothing but its own declaration and the crate root names \
         is unwired; a type reachable through a signature, a sibling caller, \
         and an item the root never re-exports are all out of scope"
    );
}

#[test]
fn pub_declarations_are_parsed_at_top_level_only() {
    // The scope of the scan, asserted: one level of `pub`, and every keyword
    // the ledger claims to read.
    let text = concat!(
        "pub fn f() {}\n",
        "pub struct S;\n",
        "pub enum E { A }\n",
        "pub const C: u8 = 0;\n",
        "pub static ST: u8 = 0;\n",
        "pub type T = u8;\n",
        "pub trait Tr {}\n",
        "impl S {\n",
        "    pub fn method(&self) {}\n",
        "}\n",
    );
    let found = pub_declarations("m.rs", text);
    assert_eq!(
        found
            .iter()
            .map(|decl| (decl.kind, decl.name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("fn", "f"),
            ("struct", "S"),
            ("enum", "E"),
            ("const", "C"),
            ("static", "ST"),
            ("type", "T"),
            ("trait", "Tr"),
        ],
        "top-level declarations only; an impl method is reached through its type"
    );
}

#[test]
fn crate_reexports_reads_multiline_use_blocks() {
    let lib = concat!(
        "pub use paint::{\n",
        "    A,\n",
        "    paint_area_fill,\n",
        "    paint_ribbon,\n",
        "};\n",
        "pub use plan::PanePlan;\n",
        "pub mod views;\n",
    );
    let mut names = BTreeSet::new();
    let mut pending = false;
    for line in code_lines(lib) {
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
    for expected in ["paint_area_fill", "PanePlan"] {
        assert!(names.contains(expected), "{expected} is re-exported");
    }
    assert!(
        !names.contains("views"),
        "a `pub mod` is not a re-exported item"
    );
}
