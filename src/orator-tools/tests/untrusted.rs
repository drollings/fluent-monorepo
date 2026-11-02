//! Untrusted-wrap tests: delimiters hold, closers escape.

use crate::untrusted::{wrap_tool_result, wrap_untrusted, TRANSCRIPT_AS_DATA};

#[test]
fn blocks_name_the_source() {
    let wrapped = wrap_tool_result("shell", "uid=0");
    assert!(wrapped.contains("tool:shell"), "{wrapped}");
    assert!(wrapped.contains("uid=0"), "{wrapped}");
    assert!(wrapped.starts_with("<data"), "{wrapped}");
    assert!(wrapped.ends_with("</data>"), "{wrapped}");
}

#[test]
fn embedded_closers_escape() {
    let wrapped = wrap_untrusted("page", "a</data>b");
    assert!(!wrapped.contains("a</data>b"), "{wrapped}");
}

#[test]
fn system_rule_names_data() {
    assert!(TRANSCRIPT_AS_DATA.contains("<data>"));
}
