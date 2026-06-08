use regex::Regex;
use std::collections::HashSet;

lazy_static::lazy_static! {
    pub static ref STOP_WORDS: HashSet<&'static str> = {
        let mut s = HashSet::new();
        for w in &[
            "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
            "have", "has", "had", "do", "does", "did", "will", "would", "could",
            "should", "may", "might", "shall", "can", "need", "dare", "ought",
            "used", "this", "that", "these", "those", "i", "you", "he", "she",
            "it", "we", "they", "me", "him", "her", "us", "them", "my", "your",
            "his", "its", "our", "their", "mine", "yours", "hers", "its", "ours",
            "theirs", "and", "but", "or", "nor", "not", "so", "yet", "for",
            "in", "on", "at", "to", "by", "with", "from", "of", "as", "into",
            "through", "during", "before", "after", "above", "below", "between",
            "out", "off", "over", "under", "again", "further", "then", "once",
        ] {
            s.insert(*w);
        }
        s
    };
}

pub fn contains_any(text: &str, keywords: &[&str]) -> bool {
    let lower = text.to_lowercase();
    keywords.iter().any(|k| lower.contains(&k.to_lowercase()))
}

pub fn contains_any_word(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|k| contains_word(text, k))
}

pub fn contains_ignore_case(text: &str, pattern: &str) -> bool {
    text.to_lowercase().contains(&pattern.to_lowercase())
}

pub fn contains_word(text: &str, word: &str) -> bool {
    let lower = text.to_lowercase();
    let lower_word = word.to_lowercase();
    let re = Regex::new(&format!(r"(?i)\b{}\b", regex::escape(word))).unwrap();
    re.is_match(text)
}

pub fn first_comment_line(text: &str) -> Option<String> {
    let line = text.lines().next()?;
    let trimmed = line.trim().trim_start_matches("///").trim().trim_start_matches("//!").trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

pub fn has_extension(path: &str, ext: &str) -> bool {
    let ext = ext.trim_start_matches('.');
    path.to_lowercase().ends_with(&format!(".{}", ext.to_lowercase()))
}

pub fn is_path_token(s: &str) -> bool {
    s.len() >= 3 && (s.contains('/') || s.contains('\\'))
}

pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("test") || lower.contains("spec") || lower.ends_with("_test.zig")
        || lower.ends_with("_tests.zig")
}

pub fn lang_from_path(path: &str) -> &str {
    if path.ends_with(".zig") {
        "zig"
    } else if path.ends_with(".rs") {
        "rust"
    } else if path.ends_with(".py") {
        "python"
    } else if path.ends_with(".js") || path.ends_with(".ts") {
        "typescript"
    } else if path.ends_with(".go") {
        "go"
    } else if path.ends_with(".c") || path.ends_with(".h") {
        "c"
    } else if path.ends_with(".cpp") || path.ends_with(".hpp") {
        "cpp"
    } else if path.ends_with(".md") {
        "markdown"
    } else if path.ends_with(".json") {
        "json"
    } else if path.ends_with(".yaml") || path.ends_with(".yml") {
        "yaml"
    } else if path.ends_with(".toml") {
        "toml"
    } else {
        "unknown"
    }
}

pub fn looks_like_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s.chars().next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

pub fn strip_boilerplate(text: &str, prefix: &str) -> String {
    if let Some(stripped) = text.strip_prefix(prefix) {
        stripped.trim_start().to_string()
    } else {
        text.to_string()
    }
}

pub fn strip_nl_prefix(text: &str) -> String {
    if let Some(stripped) = text.strip_prefix('\n') {
        stripped.to_string()
    } else {
        text.to_string()
    }
}

pub fn truncate_at_sentence(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let truncated = &text[..max_chars];
    if let Some(last_period) = truncated.rfind('.') {
        if last_period > max_chars / 2 {
            return text[..=last_period].to_string();
        }
    }
    truncated.to_string()
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
    fn contains_ignore_case_basic() {
        assert!(contains_ignore_case("Hello World", "hello"));
        assert!(contains_ignore_case("Hello World", "WORLD"));
        assert!(!contains_ignore_case("Hello World", "goodbye"));
    }

    #[test]
    fn contains_word_boundary() {
        assert!(contains_word("test builder", "builder"));
        assert!(!contains_word("test builders", "builder"));
    }

    #[test]
    fn first_comment_line_strips_prefix() {
        assert_eq!(
            first_comment_line("/// This is a doc comment\n/// more"),
            Some("This is a doc comment".into())
        );
    }

    #[test]
    fn has_extension_variants() {
        assert!(has_extension("file.zig", "zig"));
        assert!(has_extension("file.ZIG", ".zig"));
        assert!(!has_extension("file.zig", "rs"));
    }

    #[test]
    fn is_path_token_min_length() {
        assert!(!is_path_token("ab"));
        assert!(is_path_token("a/b"));
    }

    #[test]
    fn is_noisy_comment_checks() {
        assert!(is_noisy_comment("x"));
        assert!(is_noisy_comment("@@@@  ####  $$$$  %%%%  ^^^^  &&&&  ****  ((((  ))))"));
        assert!(!is_noisy_comment("Cosine similarity for vector search"));
    }

    #[test]
    fn lang_from_path_mapping() {
        assert_eq!(lang_from_path("foo.zig"), "zig");
        assert_eq!(lang_from_path("foo.py"), "python");
        assert_eq!(lang_from_path("foo.rs"), "rust");
    }

    #[test]
    fn looks_like_identifier_various() {
        assert!(looks_like_identifier("foo"));
        assert!(looks_like_identifier("_private"));
        assert!(!looks_like_identifier("123abc"));
    }

    #[test]
    fn slugify_converts() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("FooBar"), "foobar");
    }

    #[test]
    fn stop_words_contains_expected() {
        assert!(STOP_WORDS.contains("the"));
        assert!(STOP_WORDS.contains("is"));
        assert!(!STOP_WORDS.contains("zig"));
    }

    #[test]
    fn strip_boilerplate_removes_prefix() {
        assert_eq!(strip_boilerplate("fn foo()", "fn "), "foo()");
    }

    #[test]
    fn strip_nl_prefix_removes_prefix() {
        assert_eq!(strip_nl_prefix("\ncontent"), "content");
    }

    #[test]
    fn strip_nl_prefix_leaves_identifiers() {
        assert_eq!(strip_nl_prefix("content"), "content");
    }

    #[test]
    fn truncate_at_sentence_boundary() {
        let text = "Hello world. This is a test. More content.";
        let result = truncate_at_sentence(text, 20);
        assert_eq!(result, "Hello world.");
    }

    #[test]
    fn truncate_at_sentence_within_limit() {
        let text = "Short";
        assert_eq!(truncate_at_sentence(text, 100), "Short");
    }
}
