//! Shell tests: scripts mention the entry points; completions parse.

use crate::shell::{init_script, write_completions, Shell};

#[test]
fn scripts_name_dictation_fill() {
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let script = init_script(shell);
        assert!(script.contains("orator dictate"), "{shell:?}");
    }
    assert!(init_script(Shell::Bash).contains("bind -x"));
    assert!(Shell::parse("tcsh").is_none());
    assert_eq!(Shell::parse("fish"), Some(Shell::Fish));
}

#[test]
fn completions_emit_for_supported_shells() {
    for shell in ["bash", "zsh", "fish"] {
        let mut buffer = Vec::new();
        write_completions(shell, &mut buffer).expect("supported shell");
        let text = String::from_utf8(buffer).expect("utf8 completions");
        assert!(text.contains("orator"), "{shell}");
    }
    let mut buffer = Vec::new();
    write_completions("tcsh", &mut buffer).expect_err("unsupported shell fails");
}
