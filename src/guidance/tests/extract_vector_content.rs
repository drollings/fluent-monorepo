//! P2 `vector_content` tests: metadata prefix lines, 25% budget,
//! truncation markers, char-boundary safety.

use super::*;
use crate::extractor::FragmentMetadata;
use crate::search_types::CodeSymbolType;

fn code_meta() -> FragmentMetadata {
    FragmentMetadata::Code {
        symbol_type: CodeSymbolType::Function,
        symbol_name: Some("add".to_string()),
        scope: None,
        node_type: "function_declaration".to_string(),
        signature: Some("async function add(value: number): Promise<number>".to_string()),
        doc: Some("Adds one.".to_string()),
        modifiers: vec!["async".to_string()],
    }
}

#[test]
fn metadata_prefix_joins_code_lines() {
    let text = vector_content_for_fragment(&Some(code_meta()), "body", None);
    assert!(text.starts_with("symbol: function add\n"), "{text:?}");
    assert!(text.contains("signature: async function add(value: number): Promise<number>\n"), "{text:?}");
    assert!(text.contains("modifiers: async\n"), "{text:?}");
    assert!(text.contains("doc: Adds one.\n"), "{text:?}");
    assert!(text.ends_with("\nbody"), "{text:?}");
}

#[test]
fn metadata_budget_caps_prefix_at_quarter_chunk() {
    let (max, overlap) = chunk_options_for_metadata(100, 10, &Some(code_meta()));
    assert!(max < 100, "{max}");
    assert!(overlap <= 10, "{overlap}");
    let text = vector_content_for_fragment(&Some(code_meta()), "body", Some(100));
    let prefix = text.strip_suffix("\nbody").expect("suffix");
    assert!(crate::extractor::utf16_len(prefix) <= 25, "{prefix:?}");
}

#[test]
fn no_metadata_returns_content_unchanged() {
    assert_eq!(vector_content_for_fragment(&None, "body", Some(100)), "body");
}

#[test]
fn fit_text_to_chars_marks_truncation() {
    assert_eq!(fit_text_to_chars("hello world", 8), "hello...");
    assert_eq!(fit_text_to_chars("hi", 10), "hi");
    assert_eq!(fit_text_to_chars("hello", 3), "...");
    assert_eq!(fit_text_to_chars("hello", 2), "..");
}

#[test]
fn truncation_never_splits_characters() {
    let emoji = "😀".repeat(80);
    let cut = fit_text_to_chars(&emoji, 100);
    assert!(cut.ends_with("..."), "{cut:?}");
    assert!(cut.is_char_boundary(cut.len() - 3), "char boundary");
    // The payload is whole emoji snapped back under budget + marker.
    assert_eq!(cut.chars().count(), 48 + 3);
}

#[test]
fn markdown_metadata_uses_heading_lines() {
    let meta = Some(FragmentMetadata::Markdown {
        heading: Some("Guide".to_string()),
        level: Some(2),
        scope: Some("Top".to_string()),
    });
    let text = vector_content_for_fragment(&meta, "body", None);
    assert!(text.starts_with("heading: Guide\nheading_level: 2\nscope: Top\n"), "{text:?}");
}
