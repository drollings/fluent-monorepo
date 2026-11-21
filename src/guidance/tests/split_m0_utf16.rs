//! UTF-16 counting baseline: extractor agrees with std encoding.

use common_core::string::utf16_len;

#[test]
fn utf16_count_agreement() {
    for text in ["", "hello", "héllo", "𝄞", "a𝄞b", "日本語abc𝄞"] {
        let expected = text.encode_utf16().count();
        assert_eq!(utf16_len(text), expected, "text={text:?}");
    }
    // Astral characters cost two UTF-16 units, not one.
    assert_eq!(utf16_len("𝄞"), 2);
    assert_eq!(utf16_len("a𝄞b"), 4);
}
