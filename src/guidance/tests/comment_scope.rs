//! Shared comment fixtures for scope tests.
#![allow(dead_code)]
pub const RUST_MULTILINE_DOC: &str =
    "/// First line.\n/// Second line.\n/// Third line.\npub fn multi() {}\n";

/// Doc comment split by derived attributes belongs to the item below.
pub const RUST_ATTRIBUTE_DOC: &str = "/// Leading note.\n#[derive(Debug, Clone)]\n/// Trailing note.\npub struct Attr {\n    pub value: i32,\n}\n";

/// Nested method docs live inside the impl scope.
pub const RUST_NESTED_METHOD: &str = "pub struct Widget {\n    pub value: i32,\n}\n\nimpl Widget {\n    /// Create one.\n    pub fn create(value: i32) -> Self {\n        Widget { value }\n    }\n\n    /// Read the value.\n    pub fn get(&self) -> i32 {\n        self.value\n    }\n}\n";

/// Block comments attach like line docs.
pub const RUST_BLOCK_COMMENT: &str = "/** Block note. */\npub fn blocked() {}\n";

/// Zig docs use the same line prefix with its own grammar.
pub const ZIG_DOC_COMMENT: &str = "/// Zig note.\n/// More detail.\npub fn ziggy() void {}\n";

/// Python comments use the hash prefix.
pub const PYTHON_COMMENT: &str = "# Python note.\n# Second line.\ndef helper():\n    return 1\n";

/// Windows line endings must not break comment attachment.
pub const CRLF_DOC: &str = "/// First line.\r\n/// Second line.\r\npub fn crlf() {}\r\n";

/// String literals look like comments but must never count as scope.
pub const STRING_LITERAL_DECOY: &str =
    "pub fn decoy() -> &'static str {\n    \"/// not a comment\"\n}\n";

/// Python docstrings live in the body as the first string literal.
pub const PYTHON_DOCSTRING: &str =
    "def documented():\n    \"\"\"Summarize this.\"\"\"\n    return 1\n";

use super::*;

fn blocks_for(format: &str, source: &str) -> Vec<CommentBlock> {
    let mut parser = AstParser::new();
    parser
        .comment_blocks(format, source)
        .expect("comment collection parses")
}

fn block_for_name<'a>(blocks: &'a [CommentBlock], name: &str) -> &'a CommentBlock {
    blocks
        .iter()
        .find(|block| block.name == name)
        .unwrap_or_else(|| panic!("no block for {name}: {blocks:?}"))
}

#[test]
fn multiline_blocks_join_all_lines() {
    let blocks = blocks_for("rust", RUST_MULTILINE_DOC);
    let block = block_for_name(&blocks, "multi");
    assert_eq!(
        block.text, "First line.\nSecond line.\nThird line.",
        "{block:?}"
    );
}

#[test]
fn attributes_do_not_split_docs() {
    let blocks = blocks_for("rust", RUST_ATTRIBUTE_DOC);
    let block = block_for_name(&blocks, "Attr");
    assert!(block.text.contains("Leading note."), "{block:?}");
    assert!(block.text.contains("Trailing note."), "{block:?}");
}

#[test]
fn nested_methods_carry_their_own_docs() {
    let blocks = blocks_for("rust", RUST_NESTED_METHOD);
    let create = block_for_name(&blocks, "create");
    assert_eq!(create.text, "Create one.", "{create:?}");
    assert_eq!(create.scope, "Widget", "{create:?}");
    let get = block_for_name(&blocks, "get");
    assert_eq!(get.text, "Read the value.", "{get:?}");
}

#[test]
fn block_comments_attach() {
    let blocks = blocks_for("rust", RUST_BLOCK_COMMENT);
    let block = block_for_name(&blocks, "blocked");
    assert_eq!(block.text, "Block note.", "{block:?}");
}

#[test]
fn python_hash_comments_attach() {
    let blocks = blocks_for("python", PYTHON_COMMENT);
    let block = block_for_name(&blocks, "helper");
    assert!(block.text.contains("Python note."), "{block:?}");
    assert!(block.text.contains("Second line."), "{block:?}");
}

#[test]
fn python_docstrings_attach_behind_policy() {
    let blocks = blocks_for("python", PYTHON_DOCSTRING);
    let block = block_for_name(&blocks, "documented");
    assert!(block.text.contains("Summarize this."), "{block:?}");
}

#[test]
fn crlf_docs_attach() {
    let blocks = blocks_for("rust", CRLF_DOC);
    let block = block_for_name(&blocks, "crlf");
    assert!(!block.text.contains('\r'), "{block:?}");
    assert!(block.text.contains("First line."), "{block:?}");
    assert!(block.text.contains("Second line."), "{block:?}");
}

#[test]
fn string_literals_are_not_comments() {
    let blocks = blocks_for("rust", STRING_LITERAL_DECOY);
    assert!(
        blocks
            .iter()
            .all(|block| !block.text.contains("not a comment")),
        "{blocks:?}"
    );
}

#[test]
fn comment_hashes_are_stable() {
    let first = blocks_for("rust", RUST_MULTILINE_DOC);
    let second = blocks_for("rust", RUST_MULTILINE_DOC);
    assert_eq!(first, second);
    assert!(first.iter().all(|block| block.comment_hash.len() == 64));
}

#[test]
fn single_line_docs_still_work() {
    let blocks = blocks_for("rust", "/// Solo.\npub fn solo() {}\n");
    let block = block_for_name(&blocks, "solo");
    assert_eq!(block.text, "Solo.", "{block:?}");
}

#[test]
fn zig_docs_attach() {
    let blocks = blocks_for("zig", ZIG_DOC_COMMENT);
    let block = block_for_name(&blocks, "ziggy");
    assert!(block.text.contains("Zig note."), "{block:?}");
    assert!(block.text.contains("More detail."), "{block:?}");
}
