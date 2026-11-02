//! Normalizer goldens: code never spoken, markup stripped, identifiers
//! split, whitespace collapsed.

use crate::normalize::normalize_for_speech;

#[test]
fn fenced_code_never_spoken() {
    let text = "Here is the fix:\n```rust\nlet x = 1;\n```\nDone now friend.";
    let spoken = normalize_for_speech(text);
    assert!(!spoken.contains("let x"), "{spoken}");
    assert!(spoken.contains("Here is the fix"), "{spoken}");
    assert!(spoken.contains("Done now friend"), "{spoken}");
}

#[test]
fn markdown_strips_to_plain_text() {
    let spoken = normalize_for_speech("# Title here\n\n**bold** and *italic* words.");
    assert!(!spoken.contains('#'), "{spoken}");
    assert!(!spoken.contains('*'), "{spoken}");
    assert!(spoken.contains("Title here"), "{spoken}");
    assert!(spoken.contains("bold and italic words"), "{spoken}");
}

#[test]
fn links_keep_labels_images_drop() {
    let spoken = normalize_for_speech("See [the docs](https://example.com) for it.");
    assert!(spoken.contains("the docs"), "{spoken}");
    assert!(!spoken.contains("https"), "{spoken}");
    let image = normalize_for_speech("Look ![alt text](img.png) here now.");
    assert!(!image.contains("alt text"), "{image}");
}

#[test]
fn identifiers_split_into_words() {
    assert_eq!(normalize_for_speech("camelCase"), "camel Case");
    assert_eq!(normalize_for_speech("snake_case"), "snake case");
    assert_eq!(normalize_for_speech("HTTPServer"), "HTTP Server");
    assert_eq!(normalize_for_speech("mp3Player"), "mp3 Player");
}

#[test]
fn whitespace_collapses_and_prose_survives() {
    assert_eq!(
        normalize_for_speech("  Hello   world.\n\nNext line here.  "),
        "Hello world. Next line here."
    );
}
