use std::path::Path;

use guidance_common::types::GuidanceDoc;
use guidance_common::word_index::WordIndex;
use thiserror::Error;

use crate::query::identifier;
use crate::query::llm_filter::{LlmFilter, LlmFilterBackend, NoopLlmFilter};
use crate::query::strategy::{self, QueryIntent};
use crate::query::synthesize::{Stage, Synthesizer};
use crate::vector::vector_db::GuidanceDb;

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
        }
    }

    pub fn new_with_filter(backend: Box<dyn LlmFilterBackend>) -> Self {
        Self {
            llm_filter: LlmFilter::new(Some(backend)),
            word_index: None,
        }
    }

    pub fn with_word_index(mut self, wi: WordIndex) -> Self {
        self.word_index = Some(wi);
        self
    }

    pub fn load_word_index(&mut self, guidance_dir: &Path) -> Result<(), QueryEngineError> {
        let src_dir = guidance_dir.join("src");
        if !src_dir.is_dir() {
            return Ok(());
        }
        let mut wi = WordIndex::new();
        Self::walk_and_index_source_files(&src_dir, "", &mut wi)?;
        self.word_index = Some(wi);
        Ok(())
    }

    pub fn update_word_index(&mut self, file_path: &str, content: &str) {
        if let Some(ref mut wi) = self.word_index {
            wi.remove_file(file_path);
            wi.index_file(file_path, content);
        }
    }

    fn walk_and_index_source_files(dir: &Path, prefix: &str, wi: &mut WordIndex) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let sub_prefix = if prefix.is_empty() {
                    entry.file_name().to_string_lossy().to_string()
                } else {
                    format!("{}/{}", prefix, entry.file_name().to_string_lossy())
                };
                Self::walk_and_index_source_files(&path, &sub_prefix, wi)?;
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if matches!(ext, "zig" | "zon" | "py" | "rs" | "md") {
                    let rel_path = if prefix.is_empty() {
                        entry.file_name().to_string_lossy().to_string()
                    } else {
                        format!("{}/{}", prefix, entry.file_name().to_string_lossy())
                    };
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        wi.index_file(&rel_path, &content);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn explain(&self, query: &str, doc: &GuidanceDoc) -> Result<Vec<Stage>, QueryEngineError> {
        let intent = strategy::classify_query(query);

        match intent {
            QueryIntent::IdentifierLookup | QueryIntent::SingleIdentifier => {
                self.explain_identifier(query, doc)
            }
            QueryIntent::CapabilityQuery | QueryIntent::MultiKeyword => {
                self.explain_capability(query, doc)
            }
            QueryIntent::Conceptual | QueryIntent::HowTo => self.explain_concept(query, doc),
            QueryIntent::FilePath => self.explain_file_path(query, doc),
            QueryIntent::GeneralSearch => self.explain_general(query, doc),
        }
    }

    fn explain_identifier(
        &self,
        query: &str,
        doc: &GuidanceDoc,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let matched_names: Vec<String> =
            identifier::find_members_by_name(doc, query).into_iter().map(|s| s.to_string()).collect();

        if !matched_names.is_empty() {
            return Ok(Synthesizer::synthesize(query, doc, &matched_names));
        }

        let sig_matches = identifier::find_members_by_signature(doc, query);
        if !sig_matches.is_empty() {
            let sig_names: Vec<String> = sig_matches.into_iter().map(|s| s.to_string()).collect();
            return Ok(Synthesizer::synthesize(query, doc, &sig_names));
        }

        // Fallback: try WordIndex if available
        if let Some(ref wi) = self.word_index {
            let hits = wi.search(query);
            if !hits.is_empty() {
                let source = doc.meta.source.as_str();
                let file_matches: Vec<String> = hits
                    .iter()
                    .filter(|hit| wi.hit_path(hit) == source)
                    .filter_map(|_| {
                        doc.members.iter().find_map(|m| {
                            let name_lower = m.name.as_str().to_lowercase();
                            let query_lower = query.to_lowercase();
                            if name_lower.contains(&query_lower) {
                                Some(m.name.as_str().to_string())
                            } else {
                                None
                            }
                        })
                    })
                    .collect();
                if !file_matches.is_empty() {
                    return Ok(Synthesizer::synthesize(query, doc, &file_matches));
                }
            }
        }

        Err(QueryEngineError::NoResults)
    }

    fn explain_capability(
        &self,
        query: &str,
        doc: &GuidanceDoc,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let keywords: Vec<&str> = query.split_whitespace().collect();
        let mut matched_names: Vec<String> = Vec::new();

        for member in doc.members.iter() {
            let member_lower = member.name.as_str().to_lowercase();
            let comment_lower = member
                .comment
                .as_ref()
                .map(|c| c.as_str().to_lowercase())
                .unwrap_or_default();

            if keywords
                .iter()
                .any(|k| member_lower.contains(&k.to_lowercase()) || comment_lower.contains(&k.to_lowercase()))
            {
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

    fn explain_file_path(
        &self,
        query: &str,
        doc: &GuidanceDoc,
    ) -> Result<Vec<Stage>, QueryEngineError> {
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
        if let Some(ref wi) = self.word_index {
            let hits = wi.search(query);
            if !hits.is_empty() {
                let source = doc.meta.source.as_str();
                let file_matches: Vec<String> = hits
                    .iter()
                    .filter(|hit| wi.hit_path(hit) == source)
                    .filter_map(|_| {
                        doc.members.iter().find_map(|m| {
                            let name_lower = m.name.as_str().to_lowercase();
                            if name_lower.contains(&lower_query)
                                || m.comment.as_ref().is_some_and(|c| c.as_str().to_lowercase().contains(&lower_query))
                            {
                                Some(m.name.as_str().to_string())
                            } else {
                                None
                            }
                        })
                    })
                    .collect();
                if !file_matches.is_empty() {
                    return Ok(Synthesizer::synthesize(query, doc, &file_matches));
                }
            }
        }

        Err(QueryEngineError::NoResults)
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

        let keyword_results = db.keyword_search(query).map_err(|e| QueryEngineError::Db(e.to_string()))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use guidance_common::types::{GuidanceDoc, Member, MemberType, Meta};
    use guidance_common::word_index::WordIndex;

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
        std::fs::write(src_dir.join("test.zig"), "pub fn hello_world() void {}")
            .expect("write");

        let mut engine = QueryEngine::new();
        engine.load_word_index(dir.path()).expect("load word index");

        assert!(engine.word_index.is_some());
        // The sub-token "hello" should be indexed from hello_world
        let hits = engine.word_index.as_ref().unwrap().search("hello");
        assert!(!hits.is_empty(), "should find hello in word index");
    }
}
