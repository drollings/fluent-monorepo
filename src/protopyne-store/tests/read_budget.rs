//! Read-path budget: served reads never fetch the whole tape.
//!
//! The whole-tape bar fetch is named [`fetch_all_bars`](crate::sqlite_tape::fetch_all_bars)
//! so this rule is greppable, and this suite is its CI gate: the two served
//! read paths (`SqliteTape::read`, `StoreHandle::read`) must not name it,
//! while the two validated coverage paths (the audits repair and health
//! read through) must keep naming it. Timings are never the gate — this
//! source rule is.

fn manifest_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_src(name: &str) -> String {
    std::fs::read_to_string(manifest_dir().join("src").join(name))
        .unwrap_or_else(|e| panic!("read src/{name}: {e}"))
}

/// Code lines of the method whose signature line contains `marker`, from
/// the signature through its four-space-indented closing brace.
///
/// Methods inside `impl` blocks close at four-space indent, while their
/// bodies indent deeper — so the scan ends exactly at the method's own
/// close.
fn method_body_lines(text: &str, marker: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains(marker))
        .unwrap_or_else(|| panic!("expected a method matching {marker:?}"));
    let mut end = None;
    for (offset, line) in lines[start..].iter().enumerate() {
        if offset > 0 && *line == "    }" {
            end = Some(start + offset);
            break;
        }
    }
    let close = end.unwrap_or_else(|| panic!("method matching {marker:?} never closes"));
    lines[start..=close].join("\n")
}

#[test]
fn whole_tape_fetch_has_one_home() {
    let tape = read_src("sqlite_tape.rs");
    assert_eq!(
        tape.matches("fn fetch_all_bars").count(),
        1,
        "the whole-tape fetch is defined once, in sqlite_tape.rs"
    );
}

#[test]
fn served_reads_never_name_the_whole_tape_fetch() {
    for (file, marker) in [
        ("sqlite_tape.rs", "fn read(&self, req: &ReadRequest)"),
        ("handle.rs", "pub async fn read("),
    ] {
        let body = method_body_lines(&read_src(file), marker);
        assert!(
            !body.contains("fetch_all_bars"),
            "served read {marker:?} in {file} must plan from stamps, never fetch the whole tape"
        );
        assert!(
            body.contains("fetch_stamps") || body.contains("windowed_coverage"),
            "served read {marker:?} in {file} plans from stamps or the shared windowed coverage"
        );
    }
}

#[test]
fn validated_coverage_keeps_the_whole_tape_fetch() {
    for (file, marker) in [
        ("sqlite_tape.rs", "fn coverage(&self, key: TapeKey)"),
        ("handle.rs", "pub async fn coverage("),
    ] {
        let body = method_body_lines(&read_src(file), marker);
        assert!(
            body.contains("fetch_all_bars"),
            "validated coverage {marker:?} in {file} keeps the whole-tape validated read"
        );
    }
}
