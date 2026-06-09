pub fn is_path_token(s: &str) -> bool {
    s.len() >= 3 && (s.contains('/') || s.contains('\\'))
}

pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("test")
        || lower.contains("spec")
        || lower.ends_with("_test.zig")
        || lower.ends_with("_tests.zig")
}

pub fn strip_boilerplate(text: &str, prefix: &str) -> String {
    if let Some(stripped) = text.strip_prefix(prefix) {
        stripped.trim_start().to_string()
    } else {
        text.to_string()
    }
}

const NL_PREFIXES: &[&str] = &[
    "what is ",
    "what are ",
    "what does ",
    "what's ",
    "where is ",
    "where are ",
    "where does ",
    "where can i find ",
    "how does ",
    "how do ",
    "how can i ",
    "how to ",
    "why is ",
    "why does ",
    "why do ",
    "when is ",
    "when does ",
    "when do ",
    "who is ",
    "who are ",
    "who does ",
    "which is ",
    "which are ",
    "which does ",
    "explain ",
    "define ",
    "describe ",
    "tell me about ",
];

pub fn strip_nl_prefix(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    for &prefix in NL_PREFIXES {
        if lower.starts_with(prefix) {
            return text[prefix.len()..].to_string();
        }
    }
    text.to_string()
}

pub fn is_noisy_comment(comment: &str) -> bool {
    if comment.len() < 10 {
        return true;
    }
    let non_alpha: usize = comment
        .chars()
        .filter(|c| !c.is_alphanumeric() && !c.is_whitespace())
        .count();
    non_alpha as f64 / comment.len() as f64 > 0.5
}

pub fn skill_name_from_ref(ref_path: &str) -> String {
    let normalized = ref_path.replace('\\', "/");
    let basename = normalized.rsplit('/').next().unwrap_or(ref_path);
    if basename.eq_ignore_ascii_case("SKILL.md") {
        normalized
            .rsplit('/')
            .nth(1)
            .unwrap_or(basename)
            .to_string()
    } else {
        basename.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_path_token_min_length() {
        assert!(!is_path_token("ab"));
        assert!(is_path_token("a/b"));
    }

    #[test]
    fn is_noisy_comment_checks() {
        assert!(is_noisy_comment("x"));
        assert!(!is_noisy_comment("Cosine similarity for vector search"));
    }

    #[test]
    fn is_test_path_detection() {
        assert!(is_test_path("src/test.rs"));
        assert!(is_test_path("tests/foo.rs"));
        assert!(is_test_path("foo_test.zig"));
        assert!(!is_test_path("src/main.rs"));
    }

    #[test]
    fn strip_boilerplate_removes_prefix() {
        assert_eq!(strip_boilerplate("fn foo()", "fn "), "foo()");
    }

    #[test]
    fn strip_boilerplate_no_match() {
        assert_eq!(strip_boilerplate("hello world", "fn "), "hello world");
    }

    #[test]
    fn strip_nl_prefix_new_semantics_what_is() {
        assert_eq!(strip_nl_prefix("what is X"), "X");
    }

    #[test]
    fn strip_nl_prefix_new_semantics_how_does() {
        assert_eq!(strip_nl_prefix("how does Y work"), "Y work");
    }

    #[test]
    fn strip_nl_prefix_new_semantics_no_match() {
        assert_eq!(strip_nl_prefix("hello world"), "hello world");
    }

    #[test]
    fn strip_nl_prefix_new_semantics_explain() {
        assert_eq!(strip_nl_prefix("explain Z"), "Z");
    }

    #[test]
    fn skill_name_from_ref_skil_md() {
        assert_eq!(
            skill_name_from_ref("doc/skills/zig-current/SKILL.md"),
            "zig-current"
        );
    }

    #[test]
    fn skill_name_from_ref_fallback() {
        assert_eq!(skill_name_from_ref("doc/skills/foo.md"), "foo.md");
    }
}
