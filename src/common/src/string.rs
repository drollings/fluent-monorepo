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

pub fn trim_right<'a>(slice: &'a [u8], pattern: &[u8]) -> &'a [u8] {
    let mut end = slice.len();
    while end > 0 && pattern.contains(&slice[end - 1]) {
        end -= 1;
    }
    &slice[..end]
}

pub fn trim_left<'a>(slice: &'a [u8], pattern: &[u8]) -> &'a [u8] {
    let mut start = 0;
    while start < slice.len() && pattern.contains(&slice[start]) {
        start += 1;
    }
    &slice[start..]
}

fn contains_word_with_boundary(text: &str, word: &str, is_boundary: fn(u8) -> bool) -> bool {
    let lower = text.to_lowercase();
    let lower_word = word.to_lowercase();
    let bytes = lower.as_bytes();
    let wb = lower_word.as_bytes();
    if wb.is_empty() || wb.len() > bytes.len() {
        return false;
    }
    let mut i = 0;
    while i + wb.len() <= bytes.len() {
        if &bytes[i..i + wb.len()] == wb {
            let left_boundary = i == 0 || is_boundary(bytes[i - 1]);
            let right_boundary = i + wb.len() == bytes.len() || is_boundary(bytes[i + wb.len()]);
            if left_boundary && right_boundary {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_ident_boundary(c: u8) -> bool {
    !c.is_ascii_alphanumeric()
}

pub fn contains_ident_word(haystack: &str, needle: &str) -> bool {
    contains_word_with_boundary(haystack, needle, is_ident_boundary)
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
    contains_word_with_boundary(text, word, |c| !c.is_ascii_alphanumeric())
}

pub fn first_comment_line(text: &str) -> Option<String> {
    let line = text.lines().next()?;
    let trimmed = line
        .trim()
        .trim_start_matches("///")
        .trim()
        .trim_start_matches("//!")
        .trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn has_extension(path: &str, ext: &str) -> bool {
    let ext = ext.trim_start_matches('.');
    path.to_lowercase()
        .ends_with(&format!(".{}", ext.to_lowercase()))
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

pub fn lower_into<'a>(dst: &'a mut [u8], src: &[u8]) -> &'a [u8] {
    let len = src.len().min(dst.len());
    for i in 0..len {
        dst[i] = src[i].to_ascii_lowercase();
    }
    &dst[..len]
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
    fn contains_ident_word_no_false_positive_on_substring() {
        assert!(!contains_ident_word("mystructfield", "struct"));
        assert!(!contains_ident_word("unstructured", "struct"));
        assert!(contains_ident_word("test_struct", "struct"));
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

    #[test]
    fn trim_right_basic() {
        assert_eq!(trim_right(b"hello   ", b" "), b"hello");
    }

    #[test]
    fn trim_right_noop() {
        assert_eq!(trim_right(b"hello", b" "), b"hello");
    }

    #[test]
    fn trim_right_all_matching() {
        assert_eq!(trim_right(b"   ", b" "), b"");
    }

    #[test]
    fn trim_right_pattern_subset() {
        assert_eq!(trim_right(b"hello!?!", b"!?"), b"hello");
    }

    #[test]
    fn trim_left_basic() {
        assert_eq!(trim_left(b"   hello", b" "), b"hello");
    }

    #[test]
    fn trim_left_noop() {
        assert_eq!(trim_left(b"hello", b" "), b"hello");
    }

    #[test]
    fn contains_ident_word_basic() {
        assert!(contains_ident_word("my_struct_field", "struct"));
        assert!(!contains_ident_word("mystructfield", "struct"));
    }

    #[test]
    fn contains_ident_word_underscore_boundary() {
        assert!(contains_ident_word("test_foo_bar", "foo"));
        assert!(!contains_ident_word("testfoobar", "foo"));
    }

    #[test]
    fn lower_into_short_src() {
        let mut buf = [0u8; 16];
        let result = lower_into(&mut buf, b"HELLO");
        assert_eq!(result, b"hello");
    }

    #[test]
    fn lower_into_long_src_truncated() {
        let mut buf = [0u8; 4];
        let result = lower_into(&mut buf, b"HELLO WORLD");
        assert_eq!(result, b"hell");
    }

    #[test]
    fn lower_into_empty_src() {
        let mut buf = [0u8; 4];
        let result = lower_into(&mut buf, b"");
        assert_eq!(result, b"");
    }

    #[test]
    fn contains_any_basic() {
        assert!(contains_any("hello world", &["hello"]));
        assert!(contains_any("hello world", &["world", "foo"]));
        assert!(!contains_any("hello world", &["foo"]));
    }

    #[test]
    fn contains_any_word_basic() {
        assert!(contains_any_word("test builder", &["test", "builder"]));
        assert!(contains_any_word("test builder", &["builder"]));
        assert!(!contains_any_word("test builders", &["builder"]));
    }

    #[test]
    fn truncate_at_sentence_no_period() {
        let text = "This is a long string with no period at all in the first half";
        let result = truncate_at_sentence(text, 20);
        assert_eq!(result.len(), 20);
    }

    #[test]
    fn truncate_at_sentence_period_too_early() {
        let text = "A. very long string that continues past the limit";
        let result = truncate_at_sentence(text, 20);
        assert_eq!(result.len(), 20);
    }

    #[test]
    fn contains_ident_word_empty_needle() {
        assert!(!contains_ident_word("test", ""));
    }

    #[test]
    fn contains_ident_word_needle_longer_than_haystack() {
        assert!(!contains_ident_word("abc", "abcdef"));
    }

    #[test]
    fn looks_like_identifier_empty() {
        assert!(!looks_like_identifier(""));
    }

    #[test]
    fn looks_like_identifier_starts_with_digit() {
        assert!(!looks_like_identifier("123abc"));
    }

    #[test]
    fn slugify_trims_dashes() {
        assert_eq!(slugify("-hello-"), "hello");
    }

    #[test]
    fn first_comment_line_with_notice_prefix() {
        assert_eq!(
            first_comment_line("//! Module level doc\n/// member"),
            Some("Module level doc".into())
        );
    }

    #[test]
    fn first_comment_line_empty_after_strip() {
        assert_eq!(first_comment_line("///"), None);
    }

    #[test]
    fn has_extension_case_sensitivity() {
        assert!(has_extension("file.ZIG", ".zig"));
        assert!(!has_extension("file.rs", ".zig"));
    }

    #[test]
    fn contains_ident_word_boundary_special_chars() {
        assert!(contains_ident_word("foo->bar", "bar"));
        assert!(!contains_ident_word("foobar", "bar"));
    }
}
