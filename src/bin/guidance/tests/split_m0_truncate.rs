//! Truncation identity: the local copy matches the shared helper.

use super::truncate_chars;

#[test]
fn mcp_truncate_identity() {
    for (text, max) in [
        ("", 5),
        ("hi", 5),
        ("hello", 5),
        ("hello world", 5),
        ("héllo wörld", 5),
        ("a𝄞bcd", 3),
        ("abcdef", 0),
        ("𝄞𝄞𝄞", 2),
    ] {
        assert_eq!(
            truncate_chars(text, max),
            common_core::string::truncate_chars(text, max),
            "text={text:?} max={max}"
        );
    }
}
