use std::fmt::Write;

use crate::query::synthesize::Stage;
use fluent_types::StageKind;
use fluent_wvr::{ArcIntern, Describable, WorkContext, WorkError, WorkOutput, WorkUnit};

/// Output formatter — the fluent-wvr control plane for result presentation.
///
/// Each formatter converts a list of `Stage`s into a string representation.
/// The orchestrator dispatches to the registered formatter without branching
/// on format type.
///
/// M10.3: uniformity is compiler-enforced — every formatter is a
/// `fluent_wvr::Component`, so registration sites hold `Arc<dyn Component>`
/// handles and invoke through `execute` (structured `stages` handoff).
pub trait Formatter: fluent_wvr::Component {
    /// Format stages into the target representation.
    fn format(&self, stages: &[Stage]) -> String;
}

/// Markdown table output.
pub struct MarkdownFormatter;

impl Formatter for MarkdownFormatter {
    fn format(&self, stages: &[Stage]) -> String {
        let mut out = String::new();
        for stage in stages {
            let kind = match stage.kind {
                StageKind::Prose => "Prose",
                StageKind::Code => "Code",
                StageKind::Metadata => "Metadata",
                StageKind::Insight => "Insight",
                StageKind::SkillDoc => "Skill",
                StageKind::NotFound => "Not Found",
                StageKind::CapabilityDoc => "Capability",
            };
            let _ = write!(out, "## {kind}\n\n");
            let _ = write!(
                out,
                "*Source: {}:{}*\n\n",
                stage.source,
                stage.line.unwrap_or(0)
            );
            out.push_str(&stage.content);
            out.push_str("\n\n---\n\n");
        }
        out
    }
}

/// JSON output.
pub struct JsonFormatter;

impl Formatter for JsonFormatter {
    fn format(&self, stages: &[Stage]) -> String {
        serde_json::json!({
            "stages": stages,
            "count": stages.len(),
        })
        .to_string()
    }
}

/// Compact JSON with previews.
pub struct CompactFormatter;

impl Formatter for CompactFormatter {
    fn format(&self, stages: &[Stage]) -> String {
        let summaries: Vec<serde_json::Value> = stages
            .iter()
            .map(|s| {
                serde_json::json!({
                    "kind": format!("{:?}", s.kind),
                    "source": s.source,
                    "line": s.line,
                    "preview": s.content.chars().take(80).collect::<String>(),
                })
            })
            .collect();
        serde_json::json!({
            "stages": summaries,
            "count": stages.len(),
        })
        .to_string()
    }
}

/// Debug output with full content.
pub struct DebugFormatter;

impl Formatter for DebugFormatter {
    fn format(&self, stages: &[Stage]) -> String {
        let mut out = String::new();
        out.push_str("=== Query Debug ===\n");
        let _ = writeln!(out, "Stages: {}\n", stages.len());
        for (i, stage) in stages.iter().enumerate() {
            let kind_str = format!("{:?}", stage.kind);
            let _ = writeln!(out, "[{}. {}]", i + 1, kind_str);
            let _ = writeln!(
                out,
                "  Source: {}:{}",
                stage.source,
                stage.line.unwrap_or(0)
            );
            let preview: String = stage.content.chars().take(120).collect();
            let _ = writeln!(
                out,
                "  Content ({} chars): {}",
                stage.content.len(),
                preview
            );
            // P1: hybrid recall provenance renders under `--trace`/`--debug`.
            if let Some(trace) = &stage.trace {
                let _ = writeln!(
                    out,
                    "  Trace: {} rank {} score {:.6}",
                    trace.matched_by,
                    trace.rank,
                    trace.score.value()
                );
                for recall in &trace.recall {
                    let _ = writeln!(
                        out,
                        "    recall {} route {} rank {} found {}",
                        recall.path,
                        recall.route_id.as_deref().unwrap_or("-"),
                        recall
                            .rank
                            .map_or_else(|| "-".to_string(), |rank| rank.to_string()),
                        recall.found,
                    );
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::synthesize::Synthesizer;
    use crate::tests::common::make_test_doc;
    use fluent_types::{GuidanceDoc, Member, MemberType};

    fn test_stages() -> Vec<Stage> {
        let doc = GuidanceDoc {
            members: vec![Member {
                type_name: MemberType::FnDecl,
                name: "hello".into(),
                signature: Some("fn hello() void".into()),
                comment: Some("Says hello.".into()),
                line: Some(1),
                ..Member::default()
            }],
            ..make_test_doc()
        };
        Synthesizer::synthesize("hello", &doc, &["hello".to_string()])
    }

    #[test]
    fn test_markdown_formatter() {
        let stages = test_stages();
        let output = MarkdownFormatter.format(&stages);
        assert!(output.contains("## "));
        assert!(output.contains("hello"));
    }

    #[test]
    fn test_json_formatter() {
        let stages = test_stages();
        let output = JsonFormatter.format(&stages);
        assert!(output.contains("\"stages\""));
        assert!(output.contains("hello"));
    }

    #[test]
    fn test_compact_formatter() {
        let stages = test_stages();
        let output = CompactFormatter.format(&stages);
        assert!(output.contains("preview"));
    }

    #[test]
    fn test_debug_formatter() {
        let stages = test_stages();
        let output = DebugFormatter.format(&stages);
        assert!(output.contains("=== Query Debug ==="));
        assert!(output.contains("hello"));
    }

    // --- M10.1 characterization: byte-identical goldens (verbatim outputs) ---

    fn golden_stage() -> Stage {
        Stage {
            kind: fluent_types::StageKind::Code,
            content: "fn hello() void".to_string(),
            source: "src/test.zig".to_string(),
            line: Some(1),
            end_line: Some(1),
            member_name: Some("hello".to_string()),
            member_type: Some(fluent_types::MemberType::FnDecl),
            trace: None,
        }
    }

    #[test]
    fn m10_markdown_formatter_golden() {
        assert_eq!(
            MarkdownFormatter.format(&[golden_stage()]),
            "## Code\n\n*Source: src/test.zig:1*\n\nfn hello() void\n\n---\n\n"
        );
        assert_eq!(MarkdownFormatter.format(&[]), "");
    }

    #[test]
    fn m10_json_formatter_golden() {
        assert_eq!(
            JsonFormatter.format(&[golden_stage()]),
            r#"{"count":1,"stages":[{"content":"fn hello() void","kind":"code","line":1,"source":"src/test.zig"}]}"#
        );
        assert_eq!(JsonFormatter.format(&[]), r#"{"count":0,"stages":[]}"#);
    }

    #[test]
    fn m10_compact_formatter_golden() {
        assert_eq!(
            CompactFormatter.format(&[golden_stage()]),
            r#"{"count":1,"stages":[{"kind":"Code","line":1,"preview":"fn hello() void","source":"src/test.zig"}]}"#
        );
    }

    #[test]
    fn m10_debug_formatter_golden() {
        assert_eq!(
            DebugFormatter.format(&[golden_stage()]),
            "=== Query Debug ===\nStages: 1\n\n[1. Code]\n  Source: src/test.zig:1\n  Content (15 chars): fn hello() void\n"
        );
    }

    // --- M10.2: uniform-handle contract ------------------------------------

    use fluent_wvr::{Component, FieldAccess, WorkContext};

    fn ctx_with_stages(stages: &[Stage]) -> WorkContext {
        let mut ctx = WorkContext::default();
        ctx.structured.insert(
            "stages".to_string(),
            serde_json::to_value(stages).expect("stages serialize"),
        );
        ctx
    }

    #[test]
    fn m10_formatter_handles_execute_byte_identical_to_native() {
        // Uniform handle runs the same `format`: byte-identical outputs,
        // no orchestrator branch on format type.
        let cases: Vec<(&str, std::sync::Arc<dyn Component>)> = vec![
            ("formatter.markdown", std::sync::Arc::new(MarkdownFormatter)),
            ("formatter.json", std::sync::Arc::new(JsonFormatter)),
            ("formatter.compact", std::sync::Arc::new(CompactFormatter)),
            ("formatter.debug", std::sync::Arc::new(DebugFormatter)),
        ];
        let stages = vec![golden_stage()];
        let ctx = ctx_with_stages(&stages);
        for (name, handle) in &cases {
            assert_eq!(handle.name(), *name);
            let out = handle.execute(&ctx).expect("execute");
            assert!(out.success);
        }
        // Spot-check one leg byte-for-byte against the native call.
        let markdown = cases[0].1.execute(&ctx).expect("execute");
        assert_eq!(markdown.message, MarkdownFormatter.format(&stages));
        let json = cases[1].1.execute(&ctx).expect("execute");
        assert_eq!(json.message, JsonFormatter.format(&stages));
    }

    #[test]
    fn m10_formatter_execute_needs_stages_handoff() {
        let handle = std::sync::Arc::new(MarkdownFormatter);
        let err = handle.execute(&WorkContext::default()).expect_err("missing stages");
        assert!(matches!(err, fluent_wvr::WorkError::Execution(_)));
    }

    #[test]
    fn m10_formatter_handles_are_fieldless_with_schema() {
        let mut handle = MarkdownFormatter;
        assert_eq!(handle.field_names(), &[] as &[&str]);
        assert!(handle.get_field("format").is_err());
        assert!(handle.set_field("format", "json").is_err());
        assert_eq!(
            fluent_wvr::Describable::describe(&handle)["format"],
            serde_json::json!("markdown")
        );
    }
}

// --- M10.2: uniform handles (additive; native `format` untouched) ----------
//
// Each formatter is a `fluent_wvr::Component` so the orchestrator can hold
// `Arc<dyn Component>` handles. `execute` runs the same `format` through
// the structured handoff (`ctx.structured["stages"]`, JSON array of
// `Stage`); the native call sites keep calling `format` directly (no
// serialize round-trip on the presentation path).

/// Decode the `stages` structured handoff shared by every formatter unit.
fn stages_from_ctx(ctx: &WorkContext) -> Result<Vec<Stage>, WorkError> {
    let value = ctx.structured.get("stages").ok_or_else(|| {
        WorkError::Execution("formatter execute needs ctx.structured[\"stages\"]".into())
    })?;
    serde_json::from_value(value.clone())
        .map_err(|e| WorkError::Execution(format!("formatter execute: stages decode: {e}")))
}

macro_rules! impl_formatter_component {
    ($type:ty, $name:literal, $format:literal) => {
        impl WorkUnit for $type {
            fn name(&self) -> &str {
                $name
            }
            fn depends(&self) -> &[ArcIntern<str>] {
                &[]
            }
            fn provides(&self) -> &[ArcIntern<str>] {
                &[]
            }
            fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
                let stages = stages_from_ctx(ctx)?;
                Ok(WorkOutput::ok(self.format(&stages)))
            }
        }

        impl Describable for $type {
            fn describe(&self) -> serde_json::Value {
                serde_json::json!({
                    "kind": "formatter",
                    "format": $format,
                    "name": $name,
                })
            }
        }

        fluent_wvr::impl_fieldless!($type);
        fluent_wvr::impl_component!($type);
    };
}

impl_formatter_component!(MarkdownFormatter, "formatter.markdown", "markdown");
impl_formatter_component!(JsonFormatter, "formatter.json", "json");
impl_formatter_component!(CompactFormatter, "formatter.compact", "compact");
impl_formatter_component!(DebugFormatter, "formatter.debug", "debug");
