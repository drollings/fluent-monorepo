use std::path::Path;

use thiserror::Error;

use guidance_types::GuidanceDoc;

#[derive(Error, Debug)]
pub enum SyncError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("source file not found: {0}")]
    SourceNotFound(String),
}

pub fn sync_comments(source_path: &Path, doc: &GuidanceDoc) -> Result<(), SyncError> {
    let source = std::fs::read_to_string(source_path)?;
    let modified = insert_comments(&source, doc);
    if modified != source {
        std::fs::write(source_path, modified)?;
    }
    Ok(())
}

fn insert_comments(source: &str, doc: &GuidanceDoc) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut insertions: Vec<(usize, Vec<String>)> = Vec::new();

    for member in &doc.members {
        if let (Some(ref comment_text), Some(line)) = (&member.comment, member.line) {
            if comment_text.is_empty() || member.comment_generated {
                continue;
            }
            let idx = (line as usize).saturating_sub(1);
            if idx > 0 && idx <= lines.len() {
                let prev_line = lines[idx - 1].trim();
                if prev_line.starts_with("///") || prev_line.starts_with("//!") {
                    continue;
                }
            }
            let comment_lines: Vec<String> = comment_text
                .as_str()
                .split('\n')
                .map(|l| format!("/// {l}"))
                .collect();
            if !comment_lines.is_empty() {
                insertions.push((idx, comment_lines));
            }
        }
    }

    if insertions.is_empty() {
        return source.to_string();
    }

    insertions.sort_by_key(|(idx, _)| *idx);

    let mut result = String::new();
    let mut line_idx = 0;

    for (insert_idx, comment_lines) in &insertions {
        while line_idx < *insert_idx && line_idx < lines.len() {
            result.push_str(lines[line_idx]);
            result.push('\n');
            line_idx += 1;
        }
        for comment_line in comment_lines {
            result.push_str(comment_line);
            result.push('\n');
        }
    }

    while line_idx < lines.len() {
        result.push_str(lines[line_idx]);
        result.push('\n');
        line_idx += 1;
    }

    result.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use guidance_types::{GuidanceDoc, Member, MemberType, Meta};

    #[test]
    fn test_insert_comment_on_function() {
        let source = "pub fn hello() void {\n    return;\n}\n";
        let doc = GuidanceDoc {
            meta: Meta {
                module: "test".into(),
                source: "test.zig".into(),
                language: "zig".into(),
            },
            members: vec![Member {
                type_name: MemberType::FnDecl,
                name: "hello".into(),
                comment: Some("Greets the user.".into()),
                line: Some(1),
                ..Member::default()
            }],
            ..GuidanceDoc::default()
        };

        let result = insert_comments(source, &doc);
        assert!(result.starts_with("/// Greets the user.\npub fn hello"));
    }

    #[test]
    fn test_skip_existing_comment() {
        let source = "/// Existing doc\npub fn foo() void {}\n";
        let doc = GuidanceDoc {
            meta: Meta {
                module: "test".into(),
                source: "test.zig".into(),
                language: "zig".into(),
            },
            members: vec![Member {
                type_name: MemberType::FnDecl,
                name: "foo".into(),
                comment: Some("Existing doc".into()),
                line: Some(2),
                ..Member::default()
            }],
            ..GuidanceDoc::default()
        };

        let result = insert_comments(source, &doc);
        assert_eq!(result, source);
    }

    #[test]
    fn test_no_comment_is_noop() {
        let source = "pub fn foo() void {}\n";
        let doc = GuidanceDoc {
            meta: Meta {
                module: "test".into(),
                source: "test.zig".into(),
                language: "zig".into(),
            },
            members: vec![Member {
                type_name: MemberType::FnDecl,
                name: "foo".into(),
                comment: None,
                line: Some(1),
                ..Member::default()
            }],
            ..GuidanceDoc::default()
        };

        let result = insert_comments(source, &doc);
        assert_eq!(result, source);
    }
}
