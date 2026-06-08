use guidance_common::types::{GuidanceDoc, StageKind};

#[derive(Debug, Clone)]
pub struct Stage {
    pub kind: StageKind,
    pub content: String,
    pub source: String,
    pub line: Option<u32>,
}

pub struct Synthesizer;

impl Synthesizer {
    pub fn synthesize(query: &str, doc: &GuidanceDoc, matched_names: &[String]) -> Vec<Stage> {
        let mut stages = Vec::new();

        if let Some(ref comment) = doc.comment {
            stages.push(Stage {
                kind: StageKind::Prose,
                content: comment.as_str().to_string(),
                source: doc.meta.source.as_str().to_string(),
                line: None,
            });
        }

        for name in matched_names {
            if let Some(member) = doc.members.iter().find(|m| m.name.as_str() == name) {
                stages.push(Stage {
                    kind: StageKind::Code,
                    content: member
                        .signature
                        .clone()
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_else(|| member.name.as_str().to_string()),
                    source: doc.meta.source.as_str().to_string(),
                    line: member.line,
                });

                if let Some(ref comment) = member.comment {
                    stages.push(Stage {
                        kind: StageKind::Prose,
                        content: comment.as_str().to_string(),
                        source: doc.meta.source.as_str().to_string(),
                        line: member.line,
                    });
                }
            }
        }

        if stages.is_empty() {
            stages.push(Stage {
                kind: StageKind::NotFound,
                content: format!("No results found for query: {query}"),
                source: String::new(),
                line: None,
            });
        }

        stages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guidance_common::types::{GuidanceDoc, Member, MemberType, Meta};

    #[test]
    fn test_synthesize_with_matches() {
        let doc = GuidanceDoc {
            meta: Meta {
                module: "test".into(),
                source: "src/test.zig".into(),
                language: "zig".into(),
            },
            comment: Some("A test module.".into()),
            members: vec![Member {
                type_name: MemberType::FnDecl,
                name: "hello".into(),
                signature: Some("fn hello() void".into()),
                comment: Some("Says hello.".into()),
                line: Some(1),
                ..Member::default()
            }],
            ..GuidanceDoc::default()
        };

        let stages = Synthesizer::synthesize("hello", &doc, &["hello".to_string()]);
        assert!(!stages.is_empty());

        let has_code = stages.iter().any(|s| matches!(s.kind, StageKind::Code));
        assert!(has_code, "should have code stage");

        let has_prose = stages.iter().any(|s| matches!(s.kind, StageKind::Prose));
        assert!(has_prose, "should have prose stage");
    }

    #[test]
    fn test_synthesize_no_matches() {
        let doc = GuidanceDoc::default();
        let stages = Synthesizer::synthesize("nothing", &doc, &[]);
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].kind, StageKind::NotFound);
        assert!(stages[0].content.contains("nothing"));
    }
}
