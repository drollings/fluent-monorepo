//! Chord codec tests: parse, pretty, reserved.

use crate::hotkey::{parse_chord, Chord};

#[test]
fn parses_any_order_case_insensitive() {
    let chord = parse_chord("alt+CTRL-d").expect("parses");
    assert_eq!(chord.key, "d");
    assert_eq!(chord.pretty(), "Ctrl+Alt+d");
}

#[test]
fn dash_and_plus_separators_agree() {
    assert_eq!(parse_chord("Ctrl-Shift-x"), parse_chord("ctrl+shift+x"));
}

#[test]
fn rejects_lone_modifiers_and_empties() {
    assert!(parse_chord("ctrl").is_none());
    assert!(parse_chord("").is_none());
    assert!(parse_chord("ctrl++d").is_none());
    assert!(parse_chord("ctrl-d-e").is_none());
}

#[test]
fn bare_keys_are_reserved() {
    let bare = Chord {
        modifiers: Default::default(),
        key: "d".to_string(),
    };
    assert!(bare.is_reserved());
    let chorded = parse_chord("ctrl-d").expect("parses");
    assert!(!chorded.is_reserved());
}

#[test]
fn fallback_bindings_quote_the_cli_verbs() {
    // Quoted literals: renaming a CLI verb without mirroring here
    // fails the build (no drift between the compositor fallback and
    // the command surface).
    let bindings = crate::hotkey::fallback_bindings();
    let commands: Vec<&str> = bindings.iter().map(|binding| binding.0).collect();
    assert!(commands.contains(&"orator ptt start"));
    assert!(commands.contains(&"orator ptt stop"));
    assert!(commands.contains(&"orator toggle"));
    for command in &commands {
        assert!(
            command.starts_with("orator "),
            "fallback shells out to the CLI surface: {command}"
        );
    }
}
