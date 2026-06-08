use guidance_common::types::GuidanceDoc;
use thiserror::Error;

use crate::query::identifier;
use crate::query::llm_filter::{LlmFilter, LlmFilterBackend, NoopLlmFilter};
use crate::query::strategy::{self, QueryIntent};
use crate::query::synthesize::{Stage, Synthesizer};
use crate::vector::vector_db::GuidanceDb;

#[derive(Error, Debug)]
pub enum QueryEngineError {
    #[error("database error: {0}")]
    Db(String),
    #[error("LLM filter error: {0}")]
    LlmFilter(String),
    #[error("no results found")]
    NoResults,
}

pub struct QueryEngine {
    pub llm_filter: LlmFilter,
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
        }
    }

    pub fn new_with_filter(backend: Box<dyn LlmFilterBackend>) -> Self {
        Self {
            llm_filter: LlmFilter::new(Some(backend)),
        }
    }

    pub fn explain(&self, query: &str, doc: &GuidanceDoc) -> Result<Vec<Stage>, QueryEngineError> {
        let intent = strategy::classify_query(query);

        match intent {
            QueryIntent::IdentifierLookup => self.explain_identifier(query, doc),
            QueryIntent::CapabilityQuery => self.explain_capability(query, doc),
            QueryIntent::ConceptQuery => self.explain_concept(query, doc),
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

        if matched_names.is_empty() {
            let sig_matches = identifier::find_members_by_signature(doc, query);
            let sig_names: Vec<String> = sig_matches.into_iter().map(|s| s.to_string()).collect();
            if sig_names.is_empty() {
                return Err(QueryEngineError::NoResults);
            }
            return Ok(Synthesizer::synthesize(query, doc, &sig_names));
        }

        Ok(Synthesizer::synthesize(query, doc, &matched_names))
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

        if matched_names.is_empty() {
            return Err(QueryEngineError::NoResults);
        }

        Ok(Synthesizer::synthesize(query, doc, &matched_names))
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
}
