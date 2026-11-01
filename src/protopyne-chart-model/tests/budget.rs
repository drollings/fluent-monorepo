//! Workspace wiring ledger: paint-edge confinement and test-forwarder
//! coverage.
//!
//! **One gate, one home.** This crate is the ledger's home because it is the
//! workspace member that sits below every other member and depends on none of
//! them but `protopyne-types` — a rule enforced here cannot be sidestepped by
//! a crate that would sit above it, and no second copy of the manifest walk
//! can drift from this one.
//!
//! Two rules, both about wiring rather than values:
//!
//! 1. **Paint-edge confinement.** `egui` and `eframe` may be declared only by
//!    `fluent-egui` and `protopyne-egui`, in *any* dependency section — a
//!    dev-dependency links a test, bench, and example binary exactly as hard
//!    as a library dependency links the crate, so section-blind scanning is
//!    the only honest scan. The reverse is pinned too: no other crate
//!    declares a paint-edge crate at all, which keeps `egui` from re-entering
//!    through a back door.
//! 2. **Forwarder coverage.** With `autotests = false` and no `[[test]]`
//!    entries, a `tests/**/*.rs` file that no `#[path]` forwarder names never
//!    compiles and never fails — it silently rots instead of breaking the
//!    build. Every test file must be named by a forwarder in `src/`, or listed
//!    in the exemption table below with the reason it is reached another way.
//!
//! Neither rule reads a value, a price, or a trading decision. Both are
//! producer-confidence bookkeeping about whether the tree is wired, and both
//! fail the build rather than deleting anything.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Crates allowed to name the paint dependencies. Everything else is
/// headless and must stay that way.
const PAINT_EDGES: &[&str] = &["fluent-egui", "protopyne-egui"];

/// Workspace crates where this rule is not yet enforced, each with the exact
/// set of orphan files that makes it a gap and what closing it needs.
///
/// Enforcement is fail-closed: every other workspace crate that sets
/// `autotests = false` is governed automatically, so a new member crate
/// cannot enter the workspace and silently escape the gate. A deferral is a
/// frozen list, not a blanket exemption — a crate here whose orphan set grows
/// by even one file fails, so a deferral can never quietly absorb new rot.
const DEFERRED_CRATES: &[(&str, &[&str], &str)] = &[
    (
        "src/router",
        &[
            "tests/ledger_annotations.rs",
            "tests/overlay_canonical.rs",
            "tests/vector_index.rs",
        ],
        "three suites nothing names. Two are concern-separation ratchets whose \
         subject moved to `search-vector`; closing this needs a decision on \
         whether they still assert anything, not a forwarder",
    ),
    (
        "src/ontology",
        &["tests/taxonomy_equivalence.rs", "tests/yago_interlingua.rs"],
        "two suites left behind when the crate stopped forwarding its own \
         tree; each needs to be either forwarded or deleted, and deleting is \
         a decision the suite's subject has to make first",
    ),
    (
        "src/fluent-onnx",
        &["tests/live.rs", "tests/session.rs"],
        "two suites superseded by the `live-*` suites but never deleted, so \
         neither compiles nor runs",
    ),
];

/// Test files under an `autotests = false` crate that are reached without a
/// `#[path]` forwarder, each with the reason it is still live.
///
/// An entry that names no live file, or a file with no entry, is a ledger
/// bug in either direction and both are asserted against.
const FORWARDER_EXEMPTIONS: &[(&str, &str, &str)] = &[(
    "src/fluent-egui",
    "tests/ui/no_generic_undo.rs",
    "trybuild compile-fail case: driven by name from `tests/history.rs` \
         (`cases.compile_fail(..)`), never by a forwarder — it must NOT compile",
)];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workspace_root() -> PathBuf {
    manifest_dir().join("..").join("..")
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Manifest parsing
// ---------------------------------------------------------------------------

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

/// The dependency name a section header pins, when the header is a
/// per-dependency table (`[dependencies.foo]`). `None` for section headers.
fn pinned_dependency(header: &str) -> Option<String> {
    for section in ["dev-dependencies", "build-dependencies", "dependencies"] {
        if let Some(name) = header
            .strip_prefix(section)
            .and_then(|rest| rest.strip_prefix('.'))
        {
            return Some(name.to_owned());
        }
    }
    None
}

/// Whether a section header opens a dependency table, in either TOML
/// spelling. `target.'cfg(unix)'.dev-dependencies` counts: a platform-gated
/// dev-dependency links the same test binary on the platforms it covers.
fn opens_dependencies(header: &str) -> bool {
    let tail = header.rsplit('.').next().unwrap_or("");
    matches!(
        tail,
        "dependencies" | "dev-dependencies" | "build-dependencies"
    )
}

/// Every dependency key a manifest declares, across every section.
fn declared_dependencies(manifest: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let mut in_deps = false;
    let mut depth = 0i32;
    for line in manifest.lines().map(str::trim) {
        if depth == 0 && line.starts_with('[') {
            let header = line.trim_start_matches('[').trim_end_matches(']');
            if let Some(name) = pinned_dependency(header) {
                keys.insert(name);
            }
            in_deps = opens_dependencies(header);
            continue;
        }
        if !in_deps {
            continue;
        }
        if depth == 0 && !line.is_empty() && !line.starts_with('#') {
            let key = line.split('=').next().unwrap_or("").trim();
            if !key.is_empty() && key != "[" {
                keys.insert(key.to_owned());
            }
        }
        depth = (depth + bracket_delta(line)).max(0);
    }
    keys
}

/// Workspace member directories (`src/protopyne-view`), in manifest order.
fn workspace_member_dirs() -> Vec<String> {
    let text = read_text(&workspace_root().join("Cargo.toml"));
    let Some((_, after_members)) = text.split_once("members = [") else {
        panic!("workspace manifest declares a members list");
    };
    let Some((members, _)) = after_members.split_once(']') else {
        panic!("the workspace members list is closed");
    };
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

/// Package name declared by a crate manifest, or the directory name when the
/// manifest pins no `[package] name`. Scoped to the `[package]` section so a
/// dependency key named `name` further down cannot be mistaken for it.
fn package_name(manifest: &str, dir_name: &str) -> String {
    let Some((_, package)) = manifest.split_once("[package]") else {
        return dir_name.to_owned();
    };
    let end = package.find("\n[").unwrap_or(package.len());
    let declared = package[..end]
        .lines()
        .map(str::trim)
        .find_map(|line| {
            line.strip_prefix("name")?
                .split_once('=')
                .map(|(_, rest)| rest)
        })
        .map(|value| value.trim().trim_matches('"').to_owned());
    declared.unwrap_or_else(|| dir_name.to_owned())
}

// ---------------------------------------------------------------------------
// Rule 1 — paint-edge confinement
// ---------------------------------------------------------------------------

/// Crates declaring `egui` or `eframe`, mapped to the offending dependency.
/// Empty is the only acceptable result.
fn paint_dependency_owners(manifests: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut offenders = Vec::new();
    for (crate_name, manifest) in manifests {
        if PAINT_EDGES.contains(&crate_name.as_str()) {
            continue;
        }
        for dep in declared_dependencies(manifest) {
            if dep == "egui" || dep == "eframe" {
                offenders.push((crate_name.clone(), dep));
            }
        }
    }
    offenders
}

/// Crates declaring a paint-edge crate, mapped to the offending dependency.
/// Empty is the only acceptable result.
fn paint_edge_dependents(manifests: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut offenders = Vec::new();
    for (crate_name, manifest) in manifests {
        if PAINT_EDGES.contains(&crate_name.as_str()) {
            continue;
        }
        for dep in declared_dependencies(manifest) {
            if PAINT_EDGES.contains(&dep.as_str()) {
                offenders.push((crate_name.clone(), dep));
            }
        }
    }
    offenders
}

fn workspace_manifests() -> BTreeMap<String, String> {
    let root = workspace_root();
    workspace_member_dirs()
        .into_iter()
        .map(|dir| {
            let path = root.join(&dir).join("Cargo.toml");
            let manifest = read_text(&path);
            let name = package_name(&manifest, dir.rsplit('/').next().unwrap_or(&dir));
            (name, manifest)
        })
        .collect()
}

#[test]
fn paint_dependencies_stay_confined_to_the_two_paint_edges() {
    let manifests = workspace_manifests();
    let offenders = paint_dependency_owners(&manifests);
    assert!(
        offenders.is_empty(),
        "only {PAINT_EDGES:?} may declare egui/eframe; a dev-dependency links \
         the test, bench, and example binaries just as hard as a library \
         dependency links the crate — offenders: {offenders:?}"
    );
}

#[test]
fn no_headless_crate_depends_on_a_paint_edge() {
    let manifests = workspace_manifests();
    let offenders = paint_edge_dependents(&manifests);
    assert!(
        offenders.is_empty(),
        "the reverse direction is the layer fence: a headless crate that \
         depends on {PAINT_EDGES:?} re-imports the paint boundary into a crate \
         that must stay egui-free — offenders: {offenders:?}"
    );
}

#[test]
fn every_deferred_crate_is_frozen_at_its_recorded_finding() {
    // Enforcement is fail-closed, so the only way a crate escapes this rule
    // is to be listed here — and a listing is a frozen file list, asserted
    // against the tree. A deferral can shrink (good) but never grow.
    let root = workspace_root();
    let mut seen: Vec<&str> = Vec::new();
    for (dir, recorded, reason) in DEFERRED_CRATES {
        assert!(!seen.contains(dir), "{dir} is deferred twice");
        seen.push(dir);
        assert!(!reason.trim().is_empty(), "{dir} deferral needs a reason");
        let crate_root = root.join(dir);
        assert!(
            crate_root.join("Cargo.toml").is_file(),
            "{dir} is deferred but is not a workspace member — drop the entry"
        );
        let computed = orphan_test_files(&crate_root, &[]);
        assert_eq!(
            computed.iter().map(String::as_str).collect::<Vec<_>>(),
            *recorded,
            "{dir}'s orphan set moved: fix the wiring, or re-record the finding \
             with what changed"
        );
    }
}

#[test]
fn a_new_autotests_false_crate_is_governed_without_being_listed() {
    // Fail-closed control: governance is "every autotests = false member
    // that is not deferred", not "every crate someone remembered".
    assert!(
        governed_crates()
            .iter()
            .any(|dir| dir == "src/protopyne-egui")
            && !governed_crates().iter().any(|dir| dir == "src/router"),
        "membership is derived from the manifests, not from a hand-kept list"
    );
}

#[test]
fn paint_dependency_owners_gate_fires_on_a_synthetic_headless_crate() {
    // Negative control: a headless crate that gains an `egui` dev-dependency
    // is caught, and a paint crate that declares one is not.
    let mut manifests = BTreeMap::new();
    manifests.insert(
        "synthetic-headless".to_owned(),
        "[package]\nname = \"synthetic-headless\"\n[dev-dependencies]\negui = \"0.36\"\n"
            .to_owned(),
    );
    manifests.insert(
        "fluent-egui".to_owned(),
        "[package]\nname = \"fluent-egui\"\n[dependencies]\negui = \"0.36\"\n".to_owned(),
    );
    assert_eq!(
        paint_dependency_owners(&manifests),
        vec![("synthetic-headless".to_owned(), "egui".to_owned())],
        "a dev-dependency is still a link, and the exempt paint crate is still exempt"
    );
    // A paint-edge crate is not an offender for the reverse direction either.
    manifests.insert(
        "synthetic-consumer".to_owned(),
        "[package]\nname = \"synthetic-consumer\"\n[dependencies]\nfluent-egui = { path = \"..\" }\n"
            .to_owned(),
    );
    assert_eq!(
        paint_edge_dependents(&manifests),
        vec![("synthetic-consumer".to_owned(), "fluent-egui".to_owned())]
    );
}

// ---------------------------------------------------------------------------
// Rule 2 — forwarder coverage
// ---------------------------------------------------------------------------

/// Every `*.rs` file under `root`, recursively, as `prefix`-rooted
/// slash-separated paths in sorted order.
fn rust_files_under(root: &Path) -> Vec<String> {
    rust_files_below(root, "")
}

fn rust_files_below(root: &Path, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    collect_rs(root, Path::new(""), &mut out);
    out.sort();
    out.into_iter()
        .map(|name| format!("{prefix}{name}"))
        .collect()
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

/// Every `tests/` file the compiler will actually build for one crate.
///
/// `autotests = false` removes cargo's automatic discovery, so a test file
/// is live only if something names it. Three forms name one, and a gate that
/// checks fewer than all three reports live tests as orphans:
///
/// 1. a `[[test]] path = "tests/…"` entry in the manifest;
/// 2. a `#[path = "…"]` attribute, resolved relative to the directory of
///    the file that declares it — the rule the compiler itself uses, so a
///    forwarder two directories deep (`src/pine/mod.rs` naming
///    `../../tests/x.rs`) resolves to `tests/x.rs`;
/// 3. a plain `mod NAME;` declaration inside another test file, which the
///    compiler resolves to `NAME.rs` or `NAME/mod.rs` beside it — this is how
///    nested suites (`tests/live.rs` owning `tests/live/smoke_live.rs`) and
///    shared fixtures (`tests/common/mod.rs`) are built.
fn compiled_test_files(crate_root: &Path) -> BTreeSet<String> {
    let mut reached = BTreeSet::new();
    let keep = |reached: &mut BTreeSet<String>, value: String| {
        if value.starts_with("tests/") {
            reached.insert(value);
        }
    };

    let manifest = read_text(&crate_root.join("Cargo.toml"));
    for line in manifest.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "path" {
            keep(
                &mut reached,
                normalize(Path::new(value.trim().trim_matches('"'))),
            );
        }
    }

    for tree in ["src", "tests"] {
        let base = crate_root.join(tree);
        if !base.is_dir() {
            continue;
        }
        for name in rust_files_under(&base) {
            let file = base.join(&name);
            let dir = file
                .parent()
                .expect("every file has a parent directory")
                .to_path_buf();
            for line in read_text(&file).lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("#[path") {
                    if let Some(quoted) = rest.split('"').nth(1) {
                        keep(&mut reached, resolve_under(&dir, quoted, crate_root));
                    }
                    continue;
                }
                let declaration = trimmed.strip_prefix("pub ").unwrap_or(trimmed);
                if let Some(module) = declaration.strip_prefix("mod ") {
                    let ident = module.trim().trim_end_matches(';').trim();
                    let is_ident = !ident.is_empty()
                        && ident.chars().all(|ch| ch.is_alphanumeric() || ch == '_');
                    if is_ident {
                        for candidate in [format!("{ident}.rs"), format!("{ident}/mod.rs")] {
                            keep(&mut reached, resolve_under(&dir, &candidate, crate_root));
                        }
                    }
                }
            }
        }
    }
    reached
}

/// Resolve a forwarder target against its declaring file's directory and
/// re-root it at the crate root. A target that escapes the crate root is
/// dropped: that is a bug, but not this gate's business.
fn resolve_under(dir: &Path, target: &str, crate_root: &Path) -> String {
    dir.join(target)
        .strip_prefix(crate_root)
        .map(normalize)
        .unwrap_or_default()
}

/// Lexically normalize a path: drop `.` segments and resolve `..`.
fn normalize(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::CurDir => {}
            other => parts.push(other.as_os_str().to_string_lossy().into_owned()),
        }
    }
    parts.join("/")
}

/// Test files no manifest entry, forwarder, or sibling `mod` names, and that
/// no exemption entry covers.
fn unforwarded_test_files(
    test_files: &[String],
    compiled: &BTreeSet<String>,
    exemptions: &[(String, String)],
) -> Vec<String> {
    test_files
        .iter()
        .filter(|file| !compiled.contains(*file))
        .filter(|file| !exemptions.iter().any(|(_, exempt)| exempt == *file))
        .cloned()
        .collect()
}

/// Every `autotests = false` workspace member that is not explicitly
/// deferred. Derived, never hand-listed, so a new crate is covered by
/// default.
fn governed_crates() -> Vec<String> {
    let root = workspace_root();
    workspace_member_dirs()
        .into_iter()
        .filter(|dir| {
            !DEFERRED_CRATES
                .iter()
                .any(|(deferred, _, _)| deferred == dir)
                && read_text(&root.join(dir).join("Cargo.toml"))
                    .lines()
                    .any(|line| line.trim() == "autotests = false")
        })
        .collect()
}

/// Test files in one crate that nothing names. `exemptions` are
/// `(owner, file)` pairs whose owner is this crate.
fn orphan_test_files(crate_root: &Path, exemptions: &[(String, String)]) -> Vec<String> {
    let tests_dir = crate_root.join("tests");
    if !tests_dir.is_dir() {
        return Vec::new();
    }
    let test_files = rust_files_below(&tests_dir, "tests/");
    let compiled = compiled_test_files(crate_root);
    unforwarded_test_files(&test_files, &compiled, exemptions)
}

#[test]
fn every_test_file_is_named_by_a_path_forwarder() {
    let root = workspace_root();
    let mut orphans: Vec<(String, String)> = Vec::new();
    for dir in governed_crates() {
        let crate_root = root.join(&dir);
        let tests_dir = crate_root.join("tests");
        if !tests_dir.is_dir() {
            continue;
        }
        let test_files = rust_files_below(&tests_dir, "tests/");
        let compiled = compiled_test_files(&crate_root);
        let exemptions: Vec<(String, String)> = FORWARDER_EXEMPTIONS
            .iter()
            .filter(|(owner, _, _)| *owner == dir)
            .map(|(_, file, _)| (dir.clone(), (*file).to_owned()))
            .collect();
        orphans.extend(
            unforwarded_test_files(&test_files, &compiled, &exemptions)
                .into_iter()
                .map(|file| (dir.clone(), file)),
        );
    }
    assert!(
        !governed_crates().is_empty(),
        "the workspace must contain autotests = false crates to scan"
    );
    assert!(
        orphans.is_empty(),
        "with autotests = false a test file that no [[test]] entry, forwarder, \
         or sibling `mod` names never compiles and never fails — add one of the \
         three, or an exemption entry naming why it is reached another way. \
         Offenders: {orphans:?}"
    );
}

#[test]
fn every_forwarder_exemption_names_a_live_file_and_a_reason() {
    // Both directions: an entry with no live file is stale, and an entry with
    // no reason is an unenforced escape hatch.
    let root = workspace_root();
    for (owner, file, reason) in FORWARDER_EXEMPTIONS {
        assert!(
            !reason.trim().is_empty(),
            "{owner}: {file} exemption needs a reason"
        );
        assert!(
            root.join(owner).join(file).is_file(),
            "{owner}: {file} exemption names a file that does not exist — drop the entry"
        );
    }
}

#[test]
fn forwarder_coverage_gate_fires_on_a_synthetic_orphan() {
    // Negative control: an unforwarded test file is reported; the same file
    // with a forwarder, or with an exemption naming it, is not.
    let compiled: BTreeSet<String> = ["tests/forwarded.rs".to_owned()].into_iter().collect();
    let tests = vec![
        "tests/forwarded.rs".to_owned(),
        "tests/orphan.rs".to_owned(),
        "tests/nested/orphan.rs".to_owned(),
    ];
    assert_eq!(
        unforwarded_test_files(&tests, &compiled, &[]),
        vec![
            "tests/orphan.rs".to_owned(),
            "tests/nested/orphan.rs".to_owned()
        ],
        "both orphans are reported, the forwarded file is not"
    );
    assert_eq!(
        unforwarded_test_files(
            &tests,
            &compiled,
            &[("c".to_owned(), "tests/orphan.rs".to_owned())]
        ),
        vec!["tests/nested/orphan.rs".to_owned()],
        "one exemption covers its own file only"
    );
}

#[test]
fn forwarders_resolve_relative_to_the_declaring_file() {
    // The resolution rule itself: a forwarder two directories deep resolves
    // to the crate's `tests/` root, and a `..` segment normalizes.
    assert_eq!(normalize(Path::new("src/../tests/x.rs")), "tests/x.rs");
    assert_eq!(
        normalize(Path::new("src/pine/../../tests/y.rs")),
        "tests/y.rs"
    );
    assert_eq!(normalize(Path::new("./tests/./z.rs")), "tests/z.rs");
}
