//! File-tape read-path budget: served reads never walk the whole tape.
//!
//! The whole-tape coverage helper is named [`whole_tape_coverage`](crate::tape::whole_tape_coverage)
//! so this rule is greppable, and this suite is its CI gate: the served
//! `MarketTape::read` must not name it, while the validated
//! `MarketTape::coverage` (the audit repair and health read through) must
//! keep naming it. Timings are never the gate — this source rule is.

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
fn whole_tape_coverage_has_one_home() {
    let tape = read_src("tape.rs");
    assert_eq!(
        tape.matches("fn whole_tape_coverage").count(),
        1,
        "the whole-tape coverage helper is defined once, in tape.rs"
    );
}

#[test]
fn served_read_never_names_the_whole_tape_helper() {
    let body = method_body_lines(&read_src("tape.rs"), "fn read(&self, req: &ReadRequest)");
    assert!(
        !body.contains("whole_tape_coverage"),
        "served read must plan from windowed stamps, never the whole-tape helper"
    );
    assert!(
        body.contains("stamp"),
        "served read plans from stamps or the shared windowed coverage"
    );
}

#[test]
fn validated_coverage_keeps_the_whole_tape_helper() {
    let body = method_body_lines(&read_src("tape.rs"), "fn coverage(&self, key: TapeKey)");
    assert!(
        body.contains("whole_tape_coverage"),
        "validated coverage keeps the whole-tape validated read"
    );
}
