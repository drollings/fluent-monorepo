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
        matches!(self, NodeType::StructDecl | NodeType::EnumDecl | NodeType::UnionDecl)
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

    if node_type.is_function() {
        let mut brace_depth: i32 = 0;
        let mut found_open = false;
        let mut result_lines = Vec::new();
        for i in start..end {
            let line = lines[i];
            if line.trim().starts_with("// ---") {
                continue;
            }
            for ch in line.chars() {
                match ch {
                    '{' => {
                        brace_depth += 1;
                        found_open = true;
                    }
                    '}' => {
                        brace_depth -= 1;
                    }
                    _ => {}
                }
            }
            result_lines.push(line);
            if found_open && brace_depth <= 0 {
                break;
            }
        }
        result_lines.join("\n")
    } else if node_type.is_container() {
        let mut brace_depth: i32 = 0;
        let mut found_open = false;
        let mut result_lines = Vec::new();
        for i in start..end {
            let line = lines[i];
            if line.trim().starts_with("// ---") {
                continue;
            }
            for ch in line.chars() {
                match ch {
                    '{' => {
                        brace_depth += 1;
                        found_open = true;
                    }
                    '}' => {
                        brace_depth -= 1;
                    }
                    _ => {}
                }
            }
            result_lines.push(line);
            if found_open && brace_depth <= 0 {
                break;
            }
        }
        result_lines.join("\n")
    } else {
        lines[start..end].join("\n")
    }
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
}
