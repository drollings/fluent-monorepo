use std::path::Path;

use guidance_types::{FileType, GuidanceDoc, Member, MemberType, Meta, Param};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("failed to read file: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse source: {0}")]
    Syntax(String),
    #[error("unsupported language for file: {0}")]
    UnsupportedLanguage(String),
}

pub struct AstParser {
    zig_parser: tree_sitter::Parser,
    python_parser: tree_sitter::Parser,
}

impl Default for AstParser {
    fn default() -> Self {
        Self::new()
    }
}

impl AstParser {
    pub fn new() -> Self {
        let mut zig_parser = tree_sitter::Parser::new();
        zig_parser
            .set_language(&tree_sitter_zig::LANGUAGE.into())
            .expect("tree-sitter-zig language should be valid");

        let mut python_parser = tree_sitter::Parser::new();
        python_parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("tree-sitter-python language should be valid");

        Self {
            zig_parser,
            python_parser,
        }
    }

    pub fn parse_file(&mut self, path: &Path, source: &str) -> Result<GuidanceDoc, ParseError> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language = match ext {
            "zig" | "zon" => "zig",
            "py" => "python",
            _ => return Err(ParseError::UnsupportedLanguage(path.display().to_string())),
        };

        let tree = match language {
            "zig" => self
                .zig_parser
                .parse(source, None)
                .ok_or_else(|| ParseError::Syntax("tree-sitter returned no tree".into()))?,
            "python" => self
                .python_parser
                .parse(source, None)
                .ok_or_else(|| ParseError::Syntax("tree-sitter returned no tree".into()))?,
            _ => unreachable!(),
        };

        let source_path = path.to_string_lossy().to_string();
        let _module_name = source_path
            .strip_suffix(&format!(".{ext}"))
            .unwrap_or(&source_path)
            .replace(['/', '\\'], ".");

        let root = tree.root_node();
        let cursor = &mut tree.walk();

        let module_comment = extract_module_comment(&root, source);
        let members = extract_members(&root, source, cursor, language);

        let meta = Meta {
            module: source_path.as_str().into(),
            source: source_path.as_str().into(),
            language: language.into(),
        };

        Ok(GuidanceDoc {
            meta,
            comment: module_comment.map(|c| c.as_str().into()),
            members,
            ..GuidanceDoc::default()
        })
    }

    pub fn has_errors(&mut self, source: &str, language: &str) -> bool {
        let parser = match language {
            "zig" => &mut self.zig_parser,
            "python" => &mut self.python_parser,
            _ => return true,
        };
        let tree = match parser.parse(source, None) {
            Some(t) => t,
            None => return true,
        };
        tree.root_node().has_error()
    }
}

fn extract_module_comment(root: &tree_sitter::Node, source: &str) -> Option<String> {
    let mut doc_comments = Vec::new();
    let mut child = root.walk();
    for node in root.children(&mut child) {
        if node.kind() == "comment" || node.kind() == "doc_comment" {
            let text = node.utf8_text(source.as_bytes()).ok()?;
            let trimmed = trim_doc_prefix(text);
            doc_comments.push(trimmed);
        } else {
            break;
        }
    }
    if doc_comments.is_empty() {
        None
    } else {
        Some(doc_comments.join("\n"))
    }
}

fn extract_members(
    root: &tree_sitter::Node,
    source: &str,
    _cursor: &mut tree_sitter::TreeCursor,
    language: &str,
) -> Vec<Member> {
    let mut members = Vec::new();
    let mut child = root.walk();
    for node in root.children(&mut child) {
        if let Some(member) = extract_member(&node, source, _cursor, language) {
            members.push(member);
        }
    }
    members
}

fn extract_member(
    node: &tree_sitter::Node,
    source: &str,
    cursor: &mut tree_sitter::TreeCursor,
    language: &str,
) -> Option<Member> {
    match node.kind() {
        "function_declaration" | "function_definition" => {
            extract_function(node, source, cursor, language)
        }
        "struct_declaration" | "class_definition" => {
            extract_struct_or_class(node, source, cursor, MemberType::Struct)
        }
        "enum_declaration" => extract_struct_or_class(node, source, cursor, MemberType::Enum),
        "union_declaration" => extract_struct_or_class(node, source, cursor, MemberType::Union),
        "test_declaration" => extract_test(node, source),
        "variable_declaration" => extract_var_decl(node, source, cursor),
        "comptime_expression" => extract_comptime_block(node, source, cursor, language),
        _ => None,
    }
}

fn extract_comptime_block(
    node: &tree_sitter::Node,
    source: &str,
    cursor: &mut tree_sitter::TreeCursor,
    language: &str,
) -> Option<Member> {
    let line = node.start_position().row + 1;
    // Walk children of the comptime block to find inner declarations
    let mut child_members = Vec::new();
    let mut child = node.walk();
    for child_node in node.children(&mut child) {
        if let Some(inner) = extract_member(&child_node, source, cursor, language) {
            child_members.push(inner);
        }
    }

    let name = node
        .child_by_field_name("name")
        .or_else(|| {
            // For inline comptime { ... } blocks without name, use first child's name
            node.children(&mut node.walk())
                .find_map(|c| c.child_by_field_name("name"))
        })
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .unwrap_or("comptime");

    Some(Member {
        type_name: MemberType::ComptimeBlock,
        name: name.into(),
        line: Some(line as u32),
        members: child_members,
        ..Member::default()
    })
}

fn extract_var_decl(
    node: &tree_sitter::Node,
    source: &str,
    _cursor: &mut tree_sitter::TreeCursor,
) -> Option<Member> {
    let name = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "identifier")
        .and_then(|c| c.utf8_text(source.as_bytes()).ok())?;
    let is_pub = check_visibility(node, source, "zig");

    let mut child = node.walk();
    for child_node in node.children(&mut child) {
        match child_node.kind() {
            "struct_declaration" => {
                return extract_named_struct_or_class(
                    &child_node,
                    source,
                    name,
                    MemberType::Struct,
                    is_pub,
                );
            }
            "enum_declaration" => {
                return extract_named_struct_or_class(
                    &child_node,
                    source,
                    name,
                    MemberType::Enum,
                    is_pub,
                );
            }
            "union_declaration" => {
                return extract_named_struct_or_class(
                    &child_node,
                    source,
                    name,
                    MemberType::Union,
                    is_pub,
                );
            }
            _ => {}
        }
    }

    None
}

fn extract_named_struct_or_class(
    node: &tree_sitter::Node,
    source: &str,
    name: &str,
    member_type: MemberType,
    is_pub: bool,
) -> Option<Member> {
    let doc_comment = extract_preceding_doc_comment(node, source);

    let child_members = extract_body_members(node, source)?;

    let sig = format!(
        "{} {} {{ ... }}",
        match member_type {
            MemberType::Struct => "struct",
            MemberType::Enum => "enum",
            MemberType::Union => "union",
            _ => "class",
        },
        name
    );

    let line = node.start_position().row + 1;

    Some(Member {
        type_name: member_type,
        name: name.into(),
        signature: Some(sig.as_str().into()),
        comment: doc_comment.map(|c| c.as_str().into()),
        is_pub,
        members: child_members,
        line: Some(line as u32),
        ..Member::default()
    })
}

fn extract_function(
    node: &tree_sitter::Node,
    source: &str,
    _cursor: &mut tree_sitter::TreeCursor,
    language: &str,
) -> Option<Member> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?;

    let doc_comment = extract_preceding_doc_comment(node, source);

    let params = match language {
        "zig" => extract_zig_params(node, source),
        "python" => extract_python_params(node, source)?,
        _ => Vec::new(),
    };

    let parameters = params
        .iter()
        .map(|(n, t)| Param {
            name: n.as_str().into(),
            type_name: t.as_ref().map(|s| s.as_str().into()),
            default: None,
        })
        .collect();

    let return_type = match language {
        "zig" => extract_zig_return_type(node, source),
        "python" => extract_python_return_type(node, source),
        _ => None,
    };

    let sig = build_signature(
        if language == "python" { "def" } else { "fn" },
        name,
        &params,
        return_type.as_deref(),
    );

    let is_pub = check_visibility(node, source, language);
    let line = node.start_position().row + 1;

    Some(Member {
        type_name: if is_pub {
            MemberType::FnDecl
        } else {
            MemberType::FnPrivate
        },
        name: name.into(),
        signature: Some(sig.as_str().into()),
        params: parameters,
        returns: return_type.map(|r| r.as_str().into()),
        comment: doc_comment.map(|c| c.as_str().into()),
        is_pub,
        line: Some(line as u32),
        ..Member::default()
    })
}

fn extract_zig_params(node: &tree_sitter::Node, source: &str) -> Vec<(String, Option<String>)> {
    let mut params = Vec::new();
    let mut child = node.walk();
    for child_node in node.children(&mut child) {
        if child_node.kind() == "parameter" {
            let param_name = child_node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(String::from)
                .unwrap_or_else(|| "_".into());

            let param_type = child_node
                .child_by_field_name("type")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(String::from);

            params.push((param_name, param_type));
        }
    }
    params
}

fn extract_zig_return_type(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let mut child = node.walk();
    for child_node in node.children(&mut child) {
        for sub in child_node.children(&mut child_node.walk()) {
            if sub.kind() == "return_type" {
                return sub.utf8_text(source.as_bytes()).ok().map(String::from);
            }
        }
    }
    None
}

fn extract_python_params(
    node: &tree_sitter::Node,
    source: &str,
) -> Option<Vec<(String, Option<String>)>> {
    let mut params = Vec::new();
    let parameters = node.child_by_field_name("parameters")?;
    let mut child = parameters.walk();
    for child_node in parameters.children(&mut child) {
        let param_name = child_node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(String::from);

        let param_type = child_node
            .child_by_field_name("type")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(String::from);

        if let Some(ref name) = param_name {
            if name != "self" && name != "cls" {
                params.push((param_name.unwrap(), param_type));
            }
        }
    }
    Some(params)
}

fn extract_python_return_type(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let return_type = node.child_by_field_name("return_type")?;
    return_type
        .utf8_text(source.as_bytes())
        .ok()
        .map(String::from)
}

fn extract_struct_or_class(
    node: &tree_sitter::Node,
    source: &str,
    _cursor: &mut tree_sitter::TreeCursor,
    member_type: MemberType,
) -> Option<Member> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?;

    let doc_comment = extract_preceding_doc_comment(node, source);

    let child_members = extract_body_members(node, source)?;

    let sig = format!(
        "{} {} {{ ... }}",
        match member_type {
            MemberType::Struct => "struct",
            MemberType::Enum => "enum",
            MemberType::Union => "union",
            _ => "class",
        },
        name
    );

    let line = node.start_position().row + 1;

    Some(Member {
        type_name: member_type,
        name: name.into(),
        signature: Some(sig.as_str().into()),
        comment: doc_comment.map(|c| c.as_str().into()),
        is_pub: true,
        members: child_members,
        line: Some(line as u32),
        ..Member::default()
    })
}

fn extract_body_members(node: &tree_sitter::Node, source: &str) -> Option<Vec<Member>> {
    let mut members = Vec::new();

    let children_iter: Vec<tree_sitter::Node> = if let Some(body) = node.child_by_field_name("body")
    {
        let mut child = body.walk();
        body.children(&mut child).collect()
    } else {
        let mut child = node.walk();
        node.children(&mut child).collect()
    };

    for child_node in children_iter {
        match child_node.kind() {
            "function_declaration" | "function_definition" => {
                let name_node = child_node.child_by_field_name("name")?;
                let name = name_node.utf8_text(source.as_bytes()).ok()?;
                let line = child_node.start_position().row + 1;
                let dc = extract_preceding_doc_comment(&child_node, source);
                members.push(Member {
                    type_name: MemberType::Method,
                    name: name.into(),
                    comment: dc.map(|c| c.as_str().into()),
                    line: Some(line as u32),
                    ..Member::default()
                });
            }
            _ => {}
        }
    }
    Some(members)
}

fn extract_test(node: &tree_sitter::Node, source: &str) -> Option<Member> {
    // Try field name first
    if let Some(name_node) = node.child_by_field_name("name") {
        let raw_name = name_node.utf8_text(source.as_bytes()).ok()?;
        let clean_name = raw_name
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(raw_name);
        let line = node.start_position().row + 1;
        return Some(Member {
            type_name: MemberType::TestDecl,
            name: clean_name.into(),
            line: Some(line as u32),
            ..Member::default()
        });
    }

    // Fallback: find string child among direct children
    let mut child = node.walk();
    let name_node = node
        .children(&mut child)
        .find(|c| c.kind() == "string_literal" || c.kind() == "string")?;
    let raw_name = name_node.utf8_text(source.as_bytes()).ok()?;
    let clean_name = raw_name
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw_name);
    // clean_name already set above

    let line = node.start_position().row + 1;

    Some(Member {
        type_name: MemberType::TestDecl,
        name: clean_name.into(),
        line: Some(line as u32),
        ..Member::default()
    })
}

fn extract_preceding_doc_comment(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let prev_node = node.prev_sibling()?;
    if prev_node.kind() == "doc_comment" || prev_node.kind() == "comment" {
        let text = prev_node.utf8_text(source.as_bytes()).ok()?;
        Some(trim_doc_prefix(text))
    } else {
        None
    }
}

fn check_visibility(node: &tree_sitter::Node, source: &str, language: &str) -> bool {
    if language == "python" {
        let name_node = match node.child_by_field_name("name") {
            Some(n) => n,
            None => return true,
        };
        if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
            return !name.starts_with('_');
        }
        return true;
    }

    let mut child = node.walk();
    for child_node in node.children(&mut child) {
        if child_node.kind() == "pub" {
            return true;
        }
    }
    false
}

fn trim_doc_prefix(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    for line in lines.iter_mut() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("///") {
            *line = rest.strip_prefix(' ').unwrap_or(rest);
        } else if let Some(rest) = trimmed.strip_prefix("//!") {
            *line = rest.strip_prefix(' ').unwrap_or(rest);
        } else if let Some(rest) = trimmed.strip_prefix('#') {
            *line = rest.strip_prefix(' ').unwrap_or(rest);
        }
    }
    lines.join("\n")
}

fn build_signature(
    kind: &str,
    name: &str,
    params: &[(String, Option<String>)],
    return_type: Option<&str>,
) -> String {
    let mut sig = String::new();
    sig.push_str(kind);
    sig.push(' ');
    sig.push_str(name);
    sig.push('(');
    for (i, (pname, ptype)) in params.iter().enumerate() {
        if i > 0 {
            sig.push_str(", ");
        }
        sig.push_str(pname);
        if let Some(t) = ptype {
            sig.push_str(": ");
            sig.push_str(t);
        }
    }
    sig.push(')');
    if let Some(r) = return_type {
        sig.push_str(" -> ");
        sig.push_str(r);
    }
    sig
}

pub fn file_type_from_extension(ext: &str) -> FileType {
    if ext.starts_with('.') {
        FileType::from_extension(ext)
    } else {
        FileType::from_extension(&format!(".{ext}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_parse_helper(source: &str, filename: &str) -> GuidanceDoc {
        let mut parser = AstParser::new();
        parser
            .parse_file(Path::new(filename), source)
            .expect("should parse")
    }

    #[test]
    fn test_parse_zig_file() {
        let source = r#"/// Sample Zig file for AST parsing tests
const std = @import("std");

pub fn greet(name: []const u8) []const u8 {
    return "Hello, " ++ name;
}

pub const Config = struct {
    port: u16 = 8080,
    host: []const u8 = "localhost",
};
"#;

        let doc = test_parse_helper(source, "main.zig");
        assert_eq!(doc.meta.language.as_str(), "zig");
        assert!(
            !doc.members.is_empty(),
            "should have at least one member, got 0"
        );
        for m in &doc.members {
            eprintln!("member: name={:?} type={:?}", m.name, m.type_name);
        }
        let greet = doc.members.iter().find(|m| m.name == "greet");
        assert!(greet.is_some(), "should find greet function");
        let config = doc.members.iter().find(|m| m.name == "Config");
        assert!(config.is_some(), "should find Config struct");
    }

    #[test]
    fn test_parse_python_file() {
        let source = r#"def hello(name: str) -> str:
    return f"Hello, {name}"

class MyClass:
    def method(self, x: int) -> int:
        return x * 2
"#;

        let doc = test_parse_helper(source, "main.py");
        assert_eq!(doc.meta.language.as_str(), "python");
        assert!(doc.members.len() >= 2);
        let hello = doc.members.iter().find(|m| m.name == "hello");
        assert!(hello.is_some(), "should find hello function");
        let cls = doc.members.iter().find(|m| m.name == "MyClass");
        assert!(cls.is_some(), "should find MyClass");
    }

    #[test]
    fn test_extract_zig_doc_comment() {
        let doc = test_parse_helper(
            "/// This is a test function\npub fn test_fn() void {}\n",
            "test.zig",
        );
        let func = doc.members.first().expect("should have a function");
        assert_eq!(
            func.comment.as_ref().map(|c| c.as_str()),
            Some("This is a test function")
        );
    }

    #[test]
    fn test_extract_line_numbers() {
        let doc = test_parse_helper("pub fn foo() void {}\n\npub fn bar() void {}", "foo.zig");
        assert!(!doc.members.is_empty());
    }

    #[test]
    fn test_unsupported_language() {
        let mut parser = AstParser::new();
        let result = parser.parse_file(Path::new("main.rs"), "fn main() {}");
        assert!(result.is_err());
        match result {
            Err(ParseError::UnsupportedLanguage(_)) => {}
            _ => panic!("expected UnsupportedLanguage error"),
        }
    }

    #[test]
    fn test_extract_comptime_block() {
        let source = r#"const std = @import("std");

pub fn main() void {
    comptime {
        const x: u32 = 42;
        const y: u32 = x + 1;
    }
}
"#;
        let doc = test_parse_helper(source, "comptime.zig");
        assert_eq!(doc.meta.language.as_str(), "zig");
        let main_fn = doc.members.iter().find(|m| m.name == "main");
        assert!(main_fn.is_some(), "should find main function");
    }

    #[test]
    fn test_extract_test_decl() {
        let source = r#"test "hello test" {
    try std.testing.expectEqual(1, 1);
}
"#;
        let doc = test_parse_helper(source, "test.zig");
        let test_decl = doc.members.iter().find(|m| m.name == "hello test");
        assert!(test_decl.is_some(), "should find test declaration");
        if let Some(t) = test_decl {
            assert_eq!(t.type_name, MemberType::TestDecl, "should be TestDecl type");
        }
    }

    #[test]
    fn test_file_type_from_extension() {
        assert_eq!(file_type_from_extension("zig"), FileType::Source);
        assert_eq!(file_type_from_extension("py"), FileType::Source);
        assert_eq!(file_type_from_extension(".zig"), FileType::Source);
        assert_eq!(file_type_from_extension("md"), FileType::Markdown);
        assert_eq!(file_type_from_extension("json"), FileType::Config);
        assert_eq!(file_type_from_extension("xyz"), FileType::Unknown);
    }
}
