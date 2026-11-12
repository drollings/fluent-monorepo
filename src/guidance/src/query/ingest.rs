//! P1 minimal ingestion: whole-file fragments + lemma rows + optional
//! embeddings into the fragment tables. Segmentation stays crude on purpose —
//! P2 extraction replaces the fragmenter, reusing this commit seam
//! (`upsert_fragments`) unchanged.
//!
//! Per-file isolation: a failed embed or parse degrades to an unembedded /
//! unlemmatized fragment (counted in stats), never an aborted ingest.

use std::collections::HashSet;
use std::sync::OnceLock;

use search_vector::db::GuidanceDb;

use crate::ast_parser::{AstParser, CommentBlock};
use crate::query::db_storage::{file_record, fragment_lemma, fragment_record};
use crate::search_types::{EntityFragment, FileInfo, FileKind, FragmentContent, FragmentSpan};

fluent_concurrency::thread_local_resource!(static COMMENT_PARSER: AstParser);

/// Shortest lemma worth indexing. One-character code punctuation was
/// the largest single noise class in the pre-policy table.
const MIN_LEMMA_CHARS: usize = 2;

/// Function words and syntax keywords that carry no navigation signal.
/// They stay in FTS for phrase/BM25 behavior; the lemma route drops them.
const LEMMA_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "but", "by", "can", "could", "did", "do",
    "does", "for", "from", "had", "has", "have", "he", "her", "here", "him", "his", "how", "i",
    "if", "in", "into", "is", "it", "its", "just", "may", "me", "might", "more", "most", "must",
    "my", "no", "not", "of", "on", "or", "our", "out", "over", "should", "so", "some", "such",
    "than", "that", "the", "their", "them", "then", "there", "these", "they", "this", "those",
    "through", "to", "up", "us", "very", "was", "we", "were", "what", "when", "where", "which",
    "while", "who", "will", "with", "would", "you", "your", "async", "await", "const", "crate",
    "dyn", "else", "enum", "err", "extern", "false", "fn", "impl", "let", "loop", "match", "mod",
    "move", "mut", "none", "ok", "pub", "ref", "return", "self", "static", "struct", "super",
    "trait", "true", "type", "unsafe", "use",
];

fn stop_words() -> &'static HashSet<&'static str> {
    static STOP: OnceLock<HashSet<&'static str>> = OnceLock::new();
    STOP.get_or_init(|| LEMMA_STOP_WORDS.iter().copied().collect())
}

/// Keep only lowercase alphanumeric terms that carry navigation signal.
fn keep_lemma(lemma: &str) -> bool {
    lemma.chars().count() >= MIN_LEMMA_CHARS
        && lemma.chars().all(char::is_alphanumeric)
        && !stop_words().contains(lemma)
}

/// Comment-only lemmas over comment text slices: the single helper owning
/// which text is indexed, reusing the `query_lemmas` normalization.
#[must_use]
/// Comment-only lemmas over comment text slices: the single helper owning
/// which text is indexed, reusing the `query_lemmas` normalization.
pub fn lemmas_for_comment_texts(
    texts: &[&str],
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut lemmas = Vec::new();
    for text in texts {
        for lemma in query_lemmas(nlp, text) {
            if seen.insert(lemma.clone()) {
                lemmas.push(lemma);
            }
        }
    }
    lemmas
}

/// Comment-only lemmas over collected blocks, in block order.
#[must_use]
/// Comment-only lemmas over collected blocks, in block order.
pub fn lemmas_for_comment_blocks(
    blocks: &[CommentBlock],
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Vec<String> {
    let texts: Vec<&str> = blocks.iter().map(|block| block.text.as_str()).collect();
    lemmas_for_comment_texts(&texts, nlp)
}

/// Accept an extension or a canonical extractor format for comment parsing.
pub fn tree_sitter_format(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "zig" | "zon" => Some("zig"),
        "rust" => Some("rust"),
        "python" => Some("python"),
        "c" => Some("c"),
        "cpp" => Some("cpp"),
        "go" => Some("go"),
        "java" => Some("java"),
        "javascript" => Some("javascript"),
        "jsx" => Some("jsx"),
        "typescript" => Some("typescript"),
        "tsx" => Some("tsx"),
        _ => crate::extractor::adapter::format_for_extension(name),
    }
}

/// File kind for lemma scoping, inferring from the format when unset.
fn ingest_lemma_kind(file: &FileInfo) -> FileKind {
    if let Some(kind) = file.kind {
        return kind;
    }
    let format = file.format.to_ascii_lowercase();
    if fluent_types::file_kind::data_kind_format(&format).is_some() {
        return FileKind::Data;
    }
    if tree_sitter_format(&format).is_some() {
        return FileKind::Code;
    }
    FileKind::Text
}

/// Collect comment blocks with the thread-local parser: one parse per file.
fn comment_blocks_for_file(format: &str, text: &str) -> Vec<CommentBlock> {
    fluent_concurrency::thread_resource::with_tlr(&COMMENT_PARSER, |parser| {
        parser.comment_blocks(format, text).unwrap_or_default()
    })
}

/// Lemma source text for one ingested file: comments for code, whole text
/// for prose, nothing for structured data (FTS still serves every byte).
fn lemmas_for_file(
    file: &FileInfo,
    text: &str,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Vec<String> {
    match ingest_lemma_kind(file) {
        FileKind::Data => Vec::new(),
        FileKind::Code => match tree_sitter_format(&file.format) {
            Some(format) => {
                let blocks = comment_blocks_for_file(format, text);
                lemmas_for_comment_blocks(&blocks, nlp)
            }
            None => Vec::new(),
        },
        FileKind::Text | FileKind::Image => query_lemmas(nlp, text),
    }
}

/// Ingestion outcome counts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// Ingestion outcome counts.
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
/// Ingestion failure (database only — parse/embed degrade, see above).
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
    let lemmas: Vec<search_vector::db::FragmentLemma> = lemmas_for_file(file, text, nlp)
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
/// Request/index lemma normalization: spacy rule lemmas when a pipeline is
/// wired, unioned with the lowercase alphanumeric fallback (deterministic —
/// L2 works with no model either way).
///
/// The union is backward compatible: rows ingested without a pipeline carry
/// fallback tokens only, and every query still carries its fallback tokens,
/// so old rows keep matching exactly as before while pipeline rows gain
/// inflection collapsing.
pub fn query_lemmas(nlp: Option<&spacy_rs::pipeline::NlpPipeline>, text: &str) -> Vec<String> {
    let fallback = fallback_tokens(text);
    let Some(nlp) = nlp else {
        return fallback;
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut lemmas = Vec::new();
    if let Ok(doc) = nlp.process_sync(text, None) {
        let strings = doc.vocab().strings();
        for token in doc.tokens() {
            if let Some(lemma) = strings.get(token.lemma) {
                push_lemma(&mut lemmas, &mut seen, &lemma);
            }
        }
    }
    // Union with the fallback so pipeline and pipeline-less rows share
    // the fallback intersection (old rows never silently unmatch).
    for token in fallback {
        if seen.insert(token.clone()) {
            lemmas.push(token);
        }
    }
    lemmas
}

fn push_lemma(lemmas: &mut Vec<String>, seen: &mut HashSet<String>, lemma: &str) {
    let normalized = lemma.to_lowercase();
    if keep_lemma(&normalized) && seen.insert(normalized.clone()) {
        lemmas.push(normalized);
    }
}

/// Hermetic default English pipeline for L2 (rule tokenizer + rule
/// lemmatizer; no model, no network, zero embedding cost). `None` when
/// construction fails — callers degrade to the fallback.
#[must_use]
/// Hermetic default English pipeline for L2 (rule tokenizer + rule
/// lemmatizer; no model, no network, zero embedding cost). `None` when
/// construction fails — callers degrade to the fallback.
pub fn default_en_pipeline() -> Option<spacy_rs::pipeline::NlpPipeline> {
    spacy_rs::pipeline::NlpPipeline::en_default().ok()
}

/// Lazily-built lemma pipeline: construction (the dominant fixed
/// per-invocation cost) happens at most once, on first actual lemma
/// need — never on paths that never lemmatize. The stock constructor
/// above is untouched; this only moves call sites behind the need
/// boundary. Need is task correctness (a lemma route will run, a file
/// will ingest), never a confidence judgment.
///
/// M14.4: `LazyNlp` already composes the shared NLP seam —
/// `spacy_rs::pipeline::NlpPipeline` via `default_en_pipeline`
/// (`NlpPipeline::en_default()`), the same constructor the router's
/// retrieval/overlay paths build on. There is no narrower shared holder
/// to collapse onto: guidance must not import the router (shared-crate
/// import boundary), and the laziness itself is load-bearing (rg and
/// fully-skipped sync paths must never pay construction — an eager
/// `Arc<NlpPipeline>` would bill them). Stays — pinned by the
/// `query_ingest` construct-once / failure-cached / inert-holder tests
/// plus the `query_lemmas(None, …)` fallback goldens.
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
    let mut seen = HashSet::new();
    for token in text.split(|c: char| !c.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        let lower = token.to_lowercase();
        if keep_lemma(&lower) && seen.insert(lower.clone()) {
            tokens.push(lower);
        }
    }
    tokens
}
