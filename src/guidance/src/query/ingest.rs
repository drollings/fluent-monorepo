//! P1 minimal ingestion: whole-file fragments + lemma rows + optional
//! embeddings into the fragment tables. Segmentation stays crude on purpose —
//! P2 extraction replaces the fragmenter, reusing this commit seam
//! (`upsert_fragments`) unchanged.
//!
//! Per-file isolation: a failed embed or parse degrades to an unembedded /
//! unlemmatized fragment (counted in stats), never an aborted ingest.

use search_vector::db::GuidanceDb;

use crate::query::db_storage::{file_record, fragment_lemma, fragment_record};
use crate::search_types::{EntityFragment, FileInfo, FragmentContent, FragmentSpan};

/// Ingestion outcome counts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IngestStats {
    /// Fragments committed.
    pub fragments: usize,
    /// Lemma rows committed.
    pub lemmas: usize,
    /// Fragments carrying an embedding.
    pub embedded: usize,
    /// Fragments stored without an embedding (backend absent or failed).
    pub skipped_embedding: usize,
}

/// Ingestion failure (database only — parse/embed degrade, see above).
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// Database failure.
    #[error("database error: {0}")]
    Db(String),
}

/// Ingest one text file as a single fragment (id = file id).
///
/// This is the live sync writer: whole-file range, no metadata,
/// vectors optional, so recall works with no embedding backend. It is
/// intentionally NOT the embedding pipeline's writer — `commit_file`
/// in `index_pipeline` stores extractor chunks (per-symbol ranges,
/// code metadata, mandatory per-fragment vectors) for the batch
/// embedding path. Same fixture through both writers yields different
/// fragment rows with agreeing file-level retrieval; unifying them
/// would rewrite every stored row the live path serves, for no
/// retrieval gain. Keep the split; see the pointer at `commit_file`.
pub fn ingest_text_file(
    db: &GuidanceDb,
    file: &FileInfo,
    text: &str,
    embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Result<IngestStats, IngestError> {
    let fragment = EntityFragment {
        id: file.id.clone(),
        group: None,
        file_id: file.id.clone(),
        range: FragmentSpan::File,
        content: FragmentContent::Text {
            text: text.to_string(),
        },
        metadata: None,
    };
    let mut record = fragment_record(&fragment);
    record.cjk_text = common_core::string::cjk_bigrams(text).join(" ");
    let mut stats = IngestStats::default();
    match embedder {
        Some(embedder) => match embedder.embed(text) {
            Ok(vector) if !vector.is_empty() => {
                record.embedding = Some(vector);
                stats.embedded = 1;
            }
            _ => stats.skipped_embedding = 1,
        },
        None => stats.skipped_embedding = 1,
    }
    let lemmas: Vec<search_vector::db::FragmentLemma> = query_lemmas(nlp, text)
        .into_iter()
        .map(|lemma| fragment_lemma(&file.id, &lemma, 1.0))
        .collect();
    stats.lemmas = lemmas.len();
    stats.fragments = db
        .upsert_fragments(&file_record(file), &[record], &lemmas)
        .map_err(|error| IngestError::Db(error.to_string()))?;
    Ok(stats)
}

/// Request/index lemma normalization: spacy rule lemmas when a pipeline is
/// wired, unioned with the lowercase alphanumeric fallback (deterministic —
/// L2 works with no model either way).
///
/// The union is backward compatible: rows ingested without a pipeline carry
/// fallback tokens only, and every query still carries its fallback tokens,
/// so old rows keep matching exactly as before while pipeline rows gain
/// inflection collapsing.
#[must_use]
pub fn query_lemmas(nlp: Option<&spacy_rs::pipeline::NlpPipeline>, text: &str) -> Vec<String> {
    let fallback = fallback_tokens(text);
    let Some(nlp) = nlp else {
        return fallback;
    };
    let mut lemmas = match nlp.process_sync(text, None) {
        Ok(doc) => {
            let strings = doc.vocab().strings();
            let mut lemmas = Vec::new();
            for token in doc.tokens() {
                let Some(lemma) = strings.get(token.lemma) else {
                    continue;
                };
                let normalized = lemma.to_lowercase();
                if !normalized.is_empty() && !lemmas.contains(&normalized) {
                    lemmas.push(normalized);
                }
            }
            lemmas
        }
        Err(_) => Vec::new(),
    };
    // Union with the fallback so pipeline and pipeline-less rows share
    // the fallback intersection (old rows never silently unmatch).
    for token in fallback {
        if !lemmas.contains(&token) {
            lemmas.push(token);
        }
    }
    lemmas
}

/// Hermetic default English pipeline for L2 (rule tokenizer + rule
/// lemmatizer; no model, no network, zero embedding cost). `None` when
/// construction fails — callers degrade to the fallback.
#[must_use]
pub fn default_en_pipeline() -> Option<spacy_rs::pipeline::NlpPipeline> {
    spacy_rs::pipeline::NlpPipeline::en_default().ok()
}

/// Lazily-built lemma pipeline: construction (the dominant fixed
/// per-invocation cost) happens at most once, on first actual lemma
/// need — never on paths that never lemmatize. The stock constructor
/// above is untouched; this only moves call sites behind the need
/// boundary. Need is task correctness (a lemma route will run, a file
/// will ingest), never a confidence judgment.
pub struct LazyNlp {
    cell: std::sync::OnceLock<Option<spacy_rs::pipeline::NlpPipeline>>,
}

impl LazyNlp {
    /// Empty holder: builds nothing until asked.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cell: std::sync::OnceLock::new(),
        }
    }

    /// Borrow the pipeline, constructing once on first need via the
    /// stock constructor.
    pub fn get(&self) -> Option<&spacy_rs::pipeline::NlpPipeline> {
        self.get_with(default_en_pipeline)
    }

    /// Borrow with an injectable constructor (tests count builds;
    /// production enters through [`LazyNlp::get`]).
    pub fn get_with(
        &self,
        build: impl FnOnce() -> Option<spacy_rs::pipeline::NlpPipeline>,
    ) -> Option<&spacy_rs::pipeline::NlpPipeline> {
        self.cell.get_or_init(build).as_ref()
    }
}

impl Default for LazyNlp {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "../../tests/query_ingest.rs"]
mod tests;

fn fallback_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for token in text.split(|c: char| !c.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        let lower = token.to_lowercase();
        if !tokens.contains(&lower) {
            tokens.push(lower);
        }
    }
    tokens
}
