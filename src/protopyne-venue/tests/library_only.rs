//! Library-only markers: a subsystem with no production caller says so.
//!
//! The marker is documentation, so this test reads the module sources and
//! pins each unwired subsystem's `Library-only` line, with a wired module as
//! a negative control so the marker cannot quietly spread onto a module the
//! server actually drives. No code is deleted here — the marker is the
//! recorded disposition.

use std::path::PathBuf;

fn source(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn unwired_subsystems_are_marked_library_only() {
    let cases = [("depth.rs", "pub struct DepthBook")];
    for (file, symbol) in cases {
        let text = source(file);
        assert!(text.contains(symbol), "{file} holds {symbol}");
        assert!(
            text.contains("Library-only"),
            "{file} must mark {symbol} library-only"
        );
    }
}

#[test]
fn wired_modules_carry_no_library_only_marker() {
    for file in [
        "session.rs",
        "journal.rs",
        "submit.rs",
        "paper.rs",
        "command.rs",
        "ticket.rs",
        "alert.rs",
    ] {
        let text = source(file);
        assert!(
            !text.contains("Library-only"),
            "{file} is a wired module and must not carry a library-only marker"
        );
    }
}
