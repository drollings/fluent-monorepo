pub const DEFAULT_MAX_LINES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    FnDecl,
    FnPrivate,
    Method,
    MethodPrivate,
    StructDecl,
    EnumDecl,
    UnionDecl,
    TestDecl,
    EnumField,
    Other,
}

impl NodeType {
    pub fn is_function(self) -> bool {
        matches!(
            self,
            NodeType::FnDecl | NodeType::FnPrivate | NodeType::Method | NodeType::MethodPrivate
        )
    }

    pub fn is_container(self) -> bool {
        matches!(
            self,
            NodeType::StructDecl | NodeType::EnumDecl | NodeType::UnionDecl
        )
    }

    pub fn from_string(s: &str) -> Self {
        match s {
            "fn_decl" | "fn" => NodeType::FnDecl,
            "struct" => NodeType::StructDecl,
            "enum" => NodeType::EnumDecl,
            "union" => NodeType::UnionDecl,
            "method" => NodeType::Method,
            _ => NodeType::Other,
        }
    }
}

pub fn extract_excerpt(
    src: &str,
    start_line: u32,
    node_type: NodeType,
    max_lines: usize,
) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let start = (start_line as usize).saturating_sub(1);
    if start >= lines.len() {
        return String::new();
    }
    let end = (start + max_lines).min(lines.len());

    let mut brace_depth: i32 = 0;
    let mut found_open = false;
    let mut result_lines: Vec<String> = Vec::new();

    let is_function = node_type.is_function();
    let is_container = node_type.is_container();

    for line in lines.iter().take(end).skip(start) {
        if line.trim().starts_with("// ---") {
            continue;
        }

        let open_count = line.chars().filter(|&c| c == '{').count();
        let close_count = line.chars().filter(|&c| c == '}').count();

        // For containers, skip inner container bodies
        if is_container && found_open && brace_depth > 0 {
            let trimmed = line.trim();
            if (trimmed.starts_with("struct ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("union "))
                && open_count > 0
            {
                brace_depth += open_count as i32 - close_count as i32;
                continue;
            }
        }

        if !found_open {
            if open_count > 0 {
                found_open = true;
            }
            if is_function && !found_open {
                continue;
            }
        }

        // Stop at next top-level declaration for functions when scope hasn't opened yet
        if is_function && !found_open {
            let trimmed = line.trim();
            if (trimmed.starts_with("pub ")
                || trimmed.starts_with("fn ")
                || trimmed.starts_with("const ")
                || trimmed.starts_with("var ")
                || trimmed.starts_with("test ")
                || trimmed.starts_with("///"))
                && !result_lines.is_empty()
            {
                break;
            }
        }

        brace_depth += open_count as i32 - close_count as i32;
        result_lines.push(line.to_string());

        if found_open && brace_depth <= 0 {
            break;
        }
    }

    while result_lines
        .last()
        .is_some_and(|l| l.trim().is_empty() || l.trim().starts_with("//"))
    {
        result_lines.pop();
    }

    result_lines.join("\n")
}

pub fn extract_simple_excerpt(src: &str, start_line: u32, max_lines: usize) -> String {
    extract_excerpt(src, start_line, NodeType::Other, max_lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_type_classification() {
        assert!(NodeType::FnDecl.is_function());
        assert!(NodeType::Method.is_function());
        assert!(!NodeType::StructDecl.is_function());
        assert!(NodeType::StructDecl.is_container());
        assert!(NodeType::EnumDecl.is_container());
        assert!(!NodeType::FnDecl.is_container());
    }

    #[test]
    fn node_type_from_string() {
        assert_eq!(NodeType::from_string("fn"), NodeType::FnDecl);
        assert_eq!(NodeType::from_string("struct"), NodeType::StructDecl);
        assert_eq!(NodeType::from_string("enum"), NodeType::EnumDecl);
        assert_eq!(NodeType::from_string("union"), NodeType::UnionDecl);
        assert_eq!(NodeType::from_string("method"), NodeType::Method);
        assert_eq!(NodeType::from_string("other"), NodeType::Other);
    }

    #[test]
    fn extract_excerpt_function_body() {
        let src = "fn hello() {\n    return 1;\n}\n";
        let excerpt = extract_excerpt(src, 1, NodeType::FnDecl, 10);
        assert!(excerpt.contains("fn hello()"));
        assert!(excerpt.contains("return 1;"));
    }

    #[test]
    fn extract_excerpt_container() {
        let src = "struct Foo {\n    x: i32,\n}\n";
        let excerpt = extract_excerpt(src, 1, NodeType::StructDecl, 10);
        assert!(excerpt.contains("struct Foo"));
        assert!(excerpt.contains("x: i32"));
    }

    #[test]
    fn container_with_nested_struct() {
        let src =
            "struct Outer {\n    inner: Inner,\n    struct Inner {\n        val: u32,\n    }\n}\n";
        let excerpt = extract_excerpt(src, 1, NodeType::StructDecl, 10);
        assert!(excerpt.contains("struct Outer"));
        assert!(excerpt.contains("inner: Inner"));
    }

    #[test]
    fn container_with_nested_fn() {
        let src = "struct Foo {\n    fn bar() void { return; }\n    x: i32,\n}\n";
        let excerpt = extract_excerpt(src, 1, NodeType::StructDecl, 10);
        assert!(excerpt.contains("struct Foo"));
        assert!(excerpt.contains("x: i32"));
    }

    #[test]
    fn excerpt_with_trailing_blanks() {
        let src = "fn foo() {\n    return;\n}\n\n\n";
        let excerpt = extract_excerpt(src, 1, NodeType::FnDecl, 10);
        assert_eq!(excerpt.trim_end(), excerpt);
    }

    #[test]
    fn excerpt_trailing_comment_pruned() {
        let src = "fn foo() {\n    return;\n}\n// trailing comment\n";
        let excerpt = extract_excerpt(src, 1, NodeType::FnDecl, 10);
        assert!(!excerpt.contains("trailing comment"));
    }

    #[test]
    fn function_followed_by_another_function() {
        let src = "fn first() {\n    return 1;\n}\n\nfn second() {\n    return 2;\n}\n";
        let excerpt = extract_excerpt(src, 1, NodeType::FnDecl, 10);
        assert!(excerpt.contains("first"));
        assert!(!excerpt.contains("second"));
    }

    #[test]
    fn node_type_from_string_fn_decl() {
        assert_eq!(NodeType::from_string("fn_decl"), NodeType::FnDecl);
    }

    #[test]
    fn node_type_from_string_default() {
        assert_eq!(NodeType::from_string("unknown_thing"), NodeType::Other);
    }

    #[test]
    fn is_function_returns_true_for_method_types() {
        assert!(NodeType::FnDecl.is_function());
        assert!(NodeType::FnPrivate.is_function());
        assert!(NodeType::Method.is_function());
        assert!(NodeType::MethodPrivate.is_function());
        assert!(!NodeType::Other.is_function());
    }

    #[test]
    fn is_container_returns_true_for_union() {
        assert!(NodeType::UnionDecl.is_container());
        assert!(!NodeType::Other.is_container());
    }

    #[test]
    fn extract_excerpt_return_type_extracts() {
        let src = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let excerpt = extract_excerpt(src, 1, NodeType::FnDecl, 10);
        assert!(excerpt.contains("fn add"));
        assert!(excerpt.contains("a + b"));
    }

    #[test]
    fn extract_excerpt_start_beyond_file() {
        let src = "line1\nline2\n";
        let excerpt = extract_excerpt(src, 100, NodeType::FnDecl, 10);
        assert_eq!(excerpt, "");
    }

    #[test]
    fn extract_excerpt_skips_separator() {
        let src = "// ---\nfn foo() {\n    return;\n}\n";
        let excerpt = extract_excerpt(src, 1, NodeType::FnDecl, 10);
        assert!(excerpt.contains("fn foo"));
    }

    #[test]
    fn extract_excerpt_with_node_type_other() {
        let src = "some random text\nwith { braces }\nand more\n";
        let excerpt = extract_excerpt(src, 1, NodeType::Other, 10);
        assert!(excerpt.contains("some random text"));
        assert!(excerpt.contains("braces"));
    }

    #[test]
    fn extract_excerpt_stops_at_next_fn_before_open_brace() {
        let src = "pub fn first() {}\npub fn second() {}\n";
        let excerpt = extract_excerpt(src, 1, NodeType::FnDecl, 10);
        assert!(excerpt.contains("first"));
        assert!(!excerpt.contains("second"));
    }

    #[test]
    fn extract_simple_excerpt_uses_other_type() {
        let src = "line1\nline2\nline3\n";
        let excerpt = extract_simple_excerpt(src, 2, 2);
        assert_eq!(excerpt, "line2\nline3");
    }

    #[test]
    fn extract_excerpt_strips_trailing_comments() {
        let src = "fn foo() {\n    return;\n}\n// trailing\n ";
        let excerpt = extract_excerpt(src, 1, NodeType::FnDecl, 10);
        assert!(!excerpt.contains("trailing"));
        assert!(!excerpt.ends_with(' '));
    }

    #[test]
    fn extract_excerpt_stops_at_const_before_open_brace() {
        let src = "fn first() {}\nconst VERSION = 1;\n";
        let excerpt = extract_excerpt(src, 1, NodeType::FnDecl, 10);
        assert!(excerpt.contains("first"));
        assert!(!excerpt.contains("VERSION"));
    }

    #[test]
    fn extract_excerpt_stops_at_test_at_top_level() {
        let src = "fn helper() {}\ntest \"basic\" {\n    try testing.expect(true);\n}\n";
        let excerpt = extract_excerpt(src, 1, NodeType::FnDecl, 10);
        assert!(excerpt.contains("helper"));
        assert!(!excerpt.contains("testing.expect"));
    }
}
