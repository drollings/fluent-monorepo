use std::fmt::Write;
use std::path::Path;

use guidance_project_knowledge::word_index::WordIndex;
use guidance_types::GuidanceDoc;
use thiserror::Error;

use crate::ast_parser;
use crate::query::identifier;
use crate::query::llm_filter::{LlmFilter, LlmFilterBackend, NoopLlmFilter};
use crate::query::strategy::{self, QueryIntent};
use crate::query::synthesize::{Stage, Synthesizer};
use crate::walk;
use guidance_search_vector::GuidanceDb;
use guidance_search_vector::SemanticAliases;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Markdown,
    Json,
    Compact,
    Debug,
}

#[derive(Error, Debug)]
pub enum QueryEngineError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Db(String),
    #[error("LLM filter error: {0}")]
    LlmFilter(String),
    #[error("no results found")]
    NoResults,
}

pub struct QueryEngine {
    pub llm_filter: LlmFilter,
    pub word_index: Option<WordIndex>,
    pub aliases: Option<SemanticAliases>,
    pub no_llm: bool,
}

impl Default for QueryEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryEngine {
    pub fn new() -> Self {
        Self {
            llm_filter: LlmFilter::new(Some(Box::new(NoopLlmFilter))),
            word_index: None,
            aliases: None,
            no_llm: false,
        }
    }

    pub fn new_with_filter(backend: Box<dyn LlmFilterBackend>) -> Self {
        Self {
            llm_filter: LlmFilter::new(Some(backend)),
            word_index: None,
            aliases: None,
            no_llm: false,
        }
    }

    #[must_use]
    pub fn with_no_llm(mut self) -> Self {
        self.no_llm = true;
        self
    }

    #[must_use]
    pub fn with_word_index(mut self, wi: WordIndex) -> Self {
        self.word_index = Some(wi);
        self
    }

    #[must_use]
    pub fn with_aliases(mut self, aliases: SemanticAliases) -> Self {
        self.aliases = Some(aliases);
        self
    }

    /// WordIndex fallback: search for members matching the query in the current file.
    fn word_index_fallback(
        &self,
        query: &str,
        doc: &GuidanceDoc,
    ) -> Option<Vec<Stage>> {
        let wi = self.word_index.as_ref()?;
        let hits = wi.search(query);
        if hits.is_empty() {
            return None;
        }
        let source = doc.meta.source.as_str();
        let lower_query = query.to_lowercase();
        let file_matches: Vec<String> = hits
            .iter()
            .filter(|hit| wi.hit_path(hit) == source)
            .filter_map(|_| {
                doc.members.iter().find_map(|m| {
                    let name_lower = m.name.as_str().to_lowercase();
                    if name_lower.contains(&lower_query)
                        || m.signature.as_ref().is_some_and(|s| {
                            s.as_str().to_lowercase().contains(&lower_query)
                        })
                        || m.comment.as_ref().is_some_and(|c| {
                            c.as_str().to_lowercase().contains(&lower_query)
                        })
                    {
                        Some(m.name.as_str().to_string())
                    } else {
                        None
                    }
                })
            })
            .collect();
        if file_matches.is_empty() {
            None
        } else {
            Some(Synthesizer::synthesize(query, doc, &file_matches))
        }
    }

    pub fn load_word_index(&mut self, guidance_dir: &Path) -> Result<(), QueryEngineError> {
        let src_dir = guidance_dir.join("src");
        if !src_dir.is_dir() {
            return Ok(());
        }
        let mut wi = WordIndex::new();
        let root = src_dir.clone();
        walk::walk_files(&src_dir, walk::SOURCE_EXTENSIONS, |path| {
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            if let Ok(content) = std::fs::read_to_string(path) {
                wi.index_file(&rel, &content);
            }
        });
        self.word_index = Some(wi);
        Ok(())
    }

    pub fn update_word_index(&mut self, file_path: &str, content: &str) {
        if let Some(ref mut wi) = self.word_index {
            wi.remove_file(file_path);
            wi.index_file(file_path, content);
        }
    }

    pub fn explain(&self, query: &str, doc: &GuidanceDoc) -> Result<Vec<Stage>, QueryEngineError> {
        // Expand query with semantic aliases if available
        let expanded_query = if let Some(ref aliases) = self.aliases {
            let expansions = aliases.expand_query(query);
            // Use the first expansion (original or first alias set)
            expansions
                .into_iter()
                .next()
                .unwrap_or_else(|| query.to_string())
        } else {
            query.to_string()
        };

        let intent = strategy::classify_query(&expanded_query);

        match intent {
            QueryIntent::IdentifierLookup | QueryIntent::SingleIdentifier => {
                self.explain_identifier(&expanded_query, doc)
            }
            QueryIntent::CapabilityQuery | QueryIntent::MultiKeyword => {
                Self::explain_capability(&expanded_query, doc)
            }
            QueryIntent::Conceptual | QueryIntent::HowTo => {
                self.explain_concept(&expanded_query, doc)
            }
            QueryIntent::FilePath => Self::explain_file_path(&expanded_query, doc),
            QueryIntent::GeneralSearch => self.explain_general(&expanded_query, doc),
        }
    }

    fn explain_identifier(
        &self,
        query: &str,
        doc: &GuidanceDoc,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let matched_names: Vec<String> = identifier::find_members_by_name(doc, query)
            .into_iter()
            .map(ToString::to_string)
            .collect();

        if !matched_names.is_empty() {
            return Ok(Synthesizer::synthesize(query, doc, &matched_names));
        }

        let sig_matches = identifier::find_members_by_signature(doc, query);
        if !sig_matches.is_empty() {
            let sig_names: Vec<String> = sig_matches.into_iter().map(ToString::to_string).collect();
            return Ok(Synthesizer::synthesize(query, doc, &sig_names));
        }

        // Fallback: try WordIndex if available
        if let Some(stages) = self.word_index_fallback(query, doc) {
            return Ok(stages);
        }

        Err(QueryEngineError::NoResults)
    }

    fn explain_capability(query: &str, doc: &GuidanceDoc) -> Result<Vec<Stage>, QueryEngineError> {
        let keywords: Vec<&str> = query.split_whitespace().collect();
        let mut matched_names: Vec<String> = Vec::new();

        for member in &doc.members {
            let member_lower = member.name.as_str().to_lowercase();
            let comment_lower = member
                .comment
                .as_ref()
                .map(|c| c.as_str().to_lowercase())
                .unwrap_or_default();

            if keywords.iter().any(|k| {
                member_lower.contains(&k.to_lowercase())
                    || comment_lower.contains(&k.to_lowercase())
            }) {
                matched_names.push(member.name.as_str().to_string());
            }
        }

        if matched_names.is_empty() {
            return Err(QueryEngineError::NoResults);
        }

        Ok(Synthesizer::synthesize(query, doc, &matched_names))
    }

    fn explain_concept(
        &self,
        query: &str,
        doc: &GuidanceDoc,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let scores = self
            .llm_filter
            .filter_candidates(query, doc, 10)
            .map_err(|e| QueryEngineError::LlmFilter(e.to_string()))?;

        if scores.is_empty() {
            return Err(QueryEngineError::NoResults);
        }

        let matched_names: Vec<String> = scores.into_iter().map(|s| s.member_name).collect();
        Ok(Synthesizer::synthesize(query, doc, &matched_names))
    }

    fn explain_file_path(query: &str, doc: &GuidanceDoc) -> Result<Vec<Stage>, QueryEngineError> {
        let lower_query = query.to_lowercase();
        let matched_names: Vec<String> = doc
            .members
            .iter()
            .filter(|m| {
                let src_lower = doc.meta.source.as_str().to_lowercase();
                src_lower.contains(&lower_query)
                    || m.name.as_str().to_lowercase().contains(&lower_query)
            })
            .map(|m| m.name.as_str().to_string())
            .collect();

        if matched_names.is_empty() {
            return Err(QueryEngineError::NoResults);
        }

        Ok(Synthesizer::synthesize(query, doc, &matched_names))
    }

    fn explain_general(
        &self,
        query: &str,
        doc: &GuidanceDoc,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let lower_query = query.to_lowercase();
        let matched_names: Vec<String> = doc
            .members
            .iter()
            .filter(|m| {
                m.name.as_str().to_lowercase().contains(&lower_query)
                    || m.signature
                        .as_ref()
                        .is_some_and(|s| s.as_str().to_lowercase().contains(&lower_query))
                    || m.comment
                        .as_ref()
                        .is_some_and(|c| c.as_str().to_lowercase().contains(&lower_query))
            })
            .map(|m| m.name.as_str().to_string())
            .collect();

        if !matched_names.is_empty() {
            return Ok(Synthesizer::synthesize(query, doc, &matched_names));
        }

        // Fallback: try WordIndex for broader keyword matching
        if let Some(stages) = self.word_index_fallback(query, doc) {
            return Ok(stages);
        }

        Err(QueryEngineError::NoResults)
    }

    /// Format stages into the specified output format.
    pub fn format_stages(stages: &[Stage], format: OutputFormat) -> String {
        let mut resolved = stages.to_vec();
        let mut parser = ast_parser::AstParser::new();
        resolve_stage_lines(&mut resolved, &mut parser);
        match format {
            OutputFormat::Markdown => Self::format_markdown(&resolved),
            OutputFormat::Json => Self::format_json(&resolved),
            OutputFormat::Compact => Self::format_compact(&resolved),
            OutputFormat::Debug => Self::format_debug(&resolved),
        }
    }

    fn format_markdown(stages: &[Stage]) -> String {
        let mut out = String::new();
        for stage in stages {
            let kind = match stage.kind {
                guidance_types::StageKind::Prose => "💬 Prose",
                guidance_types::StageKind::Code => "📝 Code",
                guidance_types::StageKind::Metadata => "📋 Metadata",
                guidance_types::StageKind::Insight => "💡 Insight",
                guidance_types::StageKind::SkillDoc => "🔧 Skill",
                _ => "❓",
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

    fn format_json(stages: &[Stage]) -> String {
        serde_json::json!({
            "stages": stages,
            "count": stages.len(),
        })
        .to_string()
    }

    fn format_compact(stages: &[Stage]) -> String {
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

    fn format_debug(stages: &[Stage]) -> String {
        let mut out = String::new();
        out.push_str("=== Query Debug ===\n");
        let _ = writeln!(out, "No LLM: true");
        let _ = write!(out, "Stages: {}\n\n", stages.len());
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
        }
        out
    }

    /// Explain with no-llm support: when no_llm is set, skip LLM filter phase
    /// and return raw structural stages.
    pub fn explain_with_mode(
        &self,
        query: &str,
        doc: &GuidanceDoc,
        format: OutputFormat,
    ) -> Result<String, QueryEngineError> {
        if self.no_llm {
            // Skip LLM filter: use keyword matching only
            let intent = strategy::classify_query(query);
            let stages = match intent {
                QueryIntent::IdentifierLookup | QueryIntent::SingleIdentifier => {
                    let names: Vec<String> = identifier::find_members_by_name(doc, query)
                        .iter()
                        .map(ToString::to_string)
                        .collect();
                    if names.is_empty() {
                        vec![Stage::not_found(query, doc)]
                    } else {
                        Synthesizer::synthesize(query, doc, &names)
                    }
                }
                _ => {
                    let lower = query.to_lowercase();
                    let names: Vec<String> = doc
                        .members
                        .iter()
                        .filter(|m| m.name.as_str().to_lowercase().contains(&lower))
                        .map(|m| m.name.as_str().to_string())
                        .collect();
                    if names.is_empty() {
                        vec![Stage::not_found(query, doc)]
                    } else {
                        Synthesizer::synthesize(query, doc, &names)
                    }
                }
            };
            Ok(Self::format_stages(&stages, format))
        } else {
            // Normal path with LLM filter
            let stages = self.explain(query, doc)?;
            Ok(Self::format_stages(&stages, format))
        }
    }

    pub fn vector_explain(
        &self,
        query: &str,
        query_vec: &[f32],
        db: &GuidanceDb,
        doc: &GuidanceDoc,
        k: usize,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let vector_results = db
            .vector_search(query_vec, k)
            .map_err(|e| QueryEngineError::Db(e.to_string()))?;

        let keyword_results = db
            .keyword_search(query)
            .map_err(|e| QueryEngineError::Db(e.to_string()))?;

        let mut combined: Vec<String> = Vec::new();
        for r in &vector_results {
            combined.push(r.name.clone());
        }
        for r in &keyword_results {
            if !combined.contains(&r.name) {
                combined.push(r.name.clone());
            }
        }

        if combined.is_empty() {
            return Err(QueryEngineError::NoResults);
        }

        Ok(Synthesizer::synthesize(query, doc, &combined))
    }
}

/// Resolve line numbers for stages that have member metadata but no lines.
/// Re-parses the source file with tree-sitter to get fresh positions.
fn resolve_stage_lines(stages: &mut [Stage], parser: &mut ast_parser::AstParser) {
    let mut cache: Option<(std::path::PathBuf, String, guidance_types::GuidanceDoc)> = None;

    for stage in stages.iter_mut() {
        if stage.line.is_some() {
            continue;
        }
        let (Some(ref name), Some(mt)) = (&stage.member_name, stage.member_type) else {
            continue;
        };
        let path = std::path::PathBuf::from(&stage.source);
        if !path.exists() {
            continue;
        }
        let source_changed = cache.as_ref().is_none_or(|(p, _, _)| *p != path);
        if source_changed {
            if let Ok(src) = std::fs::read_to_string(&path) {
                if let Ok(doc) = parser.parse_file(&path, &src) {
                    cache = Some((path, src, doc));
                } else {
                    continue;
                }
            } else {
                continue;
            }
        }
        if let Some((_, ref src, ref doc)) = cache {
            if let Some(fresh_member) = doc
                .members
                .iter()
                .find(|m| m.name.as_str() == name.as_str() && m.type_name == mt)
            {
                if let Some(line) = fresh_member.line {
                    stage.line = Some(line);
                    let _ = src;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guidance_project_knowledge::word_index::WordIndex;
    use guidance_types::{GuidanceDoc, Member, MemberType, Meta};

    fn make_test_doc() -> GuidanceDoc {
        GuidanceDoc {
            meta: Meta {
                module: "test".into(),
                source: "src/test.zig".into(),
                language: "zig".into(),
            },
            comment: Some("Test module for query engine.".into()),
            members: vec![
                Member {
                    type_name: MemberType::FnDecl,
                    name: "helloWorld".into(),
                    signature: Some("fn helloWorld() void".into()),
                    comment: Some("Prints hello world.".into()),
                    is_pub: true,
                    line: Some(1),
                    ..Member::default()
                },
                Member {
                    type_name: MemberType::FnDecl,
                    name: "addNumbers".into(),
                    signature: Some("fn addNumbers(a: i32, b: i32) i32".into()),
                    comment: Some("Adds two integers.".into()),
                    is_pub: true,
                    line: Some(5),
                    ..Member::default()
                },
            ],
            ..GuidanceDoc::default()
        }
    }

    #[test]
    fn test_explain_identifier() {
        let engine = QueryEngine::new();
        let doc = make_test_doc();
        let stages = engine.explain("helloWorld", &doc).expect("explain");
        assert!(!stages.is_empty());
        assert!(stages.iter().any(|s| s.content.contains("helloWorld")));
    }

    #[test]
    fn test_explain_capability() {
        let engine = QueryEngine::new();
        let doc = make_test_doc();
        let stages = engine.explain("add numbers", &doc).expect("explain");
        assert!(!stages.is_empty());
    }

    #[test]
    fn test_explain_general() {
        let engine = QueryEngine::new();
        let doc = make_test_doc();
        let stages = engine.explain("hello", &doc).expect("explain");
        assert!(!stages.is_empty());
    }

    #[test]
    fn test_explain_no_results() {
        let engine = QueryEngine::new();
        let doc = make_test_doc();
        let result = engine.explain("zzzzNotHere", &doc);
        assert!(result.is_err());
    }

    #[test]
    fn test_word_index_identifier_fallback() {
        let mut wi = WordIndex::new();
        wi.index_file("src/test.zig", "pub fn helloWorld() void {}");
        let engine = QueryEngine::new().with_word_index(wi);
        let doc = make_test_doc();
        // helloWorld is already in doc, so direct match works
        let stages = engine.explain("helloWorld", &doc).expect("explain");
        assert!(!stages.is_empty());
    }

    #[test]
    fn test_word_index_identifier_fallback_not_found() {
        let mut wi = WordIndex::new();
        wi.index_file("src/test.zig", "pub fn unknownFn() void {}");
        let engine = QueryEngine::new().with_word_index(wi);
        let doc = make_test_doc();
        // The member "unknownFn" doesn't exist in the doc, so WordIndex
        // hits won't produce results either
        let result = engine.explain("unknownFn", &doc);
        assert!(result.is_err());
    }

    #[test]
    fn test_word_index_update() {
        let mut engine = QueryEngine::new();
        let mut wi = WordIndex::new();
        wi.index_file("src/test.zig", "pub fn oldName() void {}");
        engine.word_index = Some(wi);

        // Update the word index
        engine.update_word_index("src/test.zig", "pub fn newName() void {}");

        // Verify the old name is gone
        let old_hits = engine.word_index.as_ref().unwrap().search("oldName");
        assert!(old_hits.is_empty());

        // Verify the new name is indexed
        let new_hits = engine.word_index.as_ref().unwrap().search("newName");
        assert!(!new_hits.is_empty());
    }

    #[test]
    fn test_load_word_index_from_dir() {
        let dir = tempfile::tempdir().expect("temp dir");
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).expect("create src dir");
        std::fs::write(src_dir.join("test.zig"), "pub fn hello_world() void {}").expect("write");

        let mut engine = QueryEngine::new();
        engine.load_word_index(dir.path()).expect("load word index");

        assert!(engine.word_index.is_some());
        // The sub-token "hello" should be indexed from hello_world
        let hits = engine.word_index.as_ref().unwrap().search("hello");
        assert!(!hits.is_empty(), "should find hello in word index");
    }
}
