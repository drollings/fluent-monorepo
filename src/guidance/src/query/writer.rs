//! Unified fragment writer: item chunks for live and batch over one commit
//! core with an explicit vector policy.
//!
//! Readers dual-read both row shapes: recall serves file-level agreement
//! regardless of which writer committed the file. Rebuild converts v1 rows
//! by re-running the batch path from source, never by rewriting ids.

use search_vector::db::{FileRecord, FragmentLemma, GuidanceDb};

use crate::ast_parser::CommentBlock;
use crate::diff::ScannedFile;
use crate::extractor::{FragmentMetadata, PreparedFragment};
use crate::query::db_storage::{fragment_lemma, fragment_record, symbol_record};
use crate::query::ingest::{ingest_text_file, lemmas_for_comment_texts, query_lemmas, IngestStats};
use crate::search_types::{
    CodeEntityModifier, EntityFragment, EntityMetadata, FileInfo, FileKind, FragmentContent,
    FragmentSpan,
};

/// Writer schema version: v3 rows are item chunks with block-mapped lemmas.
pub const WRITER_SCHEMA_VERSION: u32 = 3;

/// Stored row shape for one entity id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Stored row shape for one entity id.
pub enum RowShape {
    /// Whole-file v1 row (live writer, no chunk metadata required).
    WholeFile,
    /// Chunked v2 row (batch writer).
    Chunked,
    /// No row stored.
    Missing,
}

/// Vector requirement for one commit: batch embeds every fragment, live
/// sync commits rows without vectors and backfills later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Vector requirement for one commit: batch embeds every fragment, live
/// sync commits rows without vectors and backfills later.
pub enum VectorPolicy {
    /// Exactly one vector per fragment, or the file fails.
    Required,
    /// No vectors, or exactly one per fragment.
    Optional,
}

/// One seam for both writers; no second ingestion path.
pub struct FragmentWriter;

impl FragmentWriter {
    /// Live write: whole-file range, vectors optional, recall works with
    /// no embedding backend.
    pub fn write_live(
        db: &GuidanceDb,
        file: &FileInfo,
        text: &str,
        embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
        nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
    ) -> Result<IngestStats, crate::query::ingest::IngestError> {
        ingest_text_file(db, file, text, embedder, nlp)
    }

    /// Item-chunk commit shared by the batch pipeline (`Required`) and live
    /// sync (`Optional`): per-fragment rows, block-mapped lemmas, and
    /// identity rows in one pass. Per-file isolation holds: a count
    /// mismatch fails the file before any row is written.
    #[allow(clippy::too_many_arguments)]
    pub fn write_items(
        db: &GuidanceDb,
        file: &FileRecord,
        kind: FileKind,
        fragments: &[PreparedFragment],
        blocks: &[CommentBlock],
        vectors: &[Vec<f32>],
        nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
        policy: VectorPolicy,
    ) -> Result<usize, String> {
        match policy {
            VectorPolicy::Required if vectors.len() != fragments.len() => {
                return Err(vector_mismatch(vectors.len(), fragments.len()));
            }
            VectorPolicy::Optional if !vectors.is_empty() && vectors.len() != fragments.len() => {
                return Err(vector_mismatch(vectors.len(), fragments.len()));
            }
            _ => {}
        }
        let mut records = Vec::with_capacity(fragments.len());
        let mut symbols = Vec::new();
        for (index, fragment) in fragments.iter().enumerate() {
            let entity = prepared_to_entity(&file.id, fragment);
            let mut row = fragment_record(&entity);
            row.cjk_text = common_core::string::cjk_bigrams(&fragment.content_text).join(" ");
            row.embedding = match policy {
                VectorPolicy::Required => Some(vectors[index].clone()),
                VectorPolicy::Optional => vectors.get(index).cloned(),
            };
            records.push(row);
            if let Some(symbol) = symbol_record(&entity) {
                symbols.push(symbol);
            }
        }
        let lemmas = lemmas_by_fragment(&file.id, kind, fragments, blocks, nlp);
        let count = db
            .replace_file(file, &records, &lemmas)
            .map_err(|e| format!("commit: {e}"))?;
        db.replace_symbols(&symbols)
            .map_err(|e| format!("symbols: {e}"))?;
        Ok(count)
    }

    /// Shape of one stored entity: whole-file, chunked, or missing.
    /// Dual-read input for migration decisions.
    #[must_use]
    pub fn row_shape(db: &GuidanceDb, entity_id: &str) -> RowShape {
        let Ok(rows) = db.fragments_for_entity(entity_id) else {
            return RowShape::Missing;
        };
        let Some(row) = rows
            .iter()
            .find(|row| row.id == entity_id)
            .or_else(|| rows.first())
        else {
            return RowShape::Missing;
        };
        if row.symbol_name.is_some() || row.heading.is_some() {
            RowShape::Chunked
        } else {
            RowShape::WholeFile
        }
    }

    /// Whether a file committed by the live writer still needs the batch
    /// pass: fragments exist but no identity rows were stored.
    #[must_use]
    pub fn needs_rebuild(db: &GuidanceDb, file_id: &str, entity_id: &str) -> bool {
        if Self::row_shape(db, entity_id) != RowShape::WholeFile {
            return false;
        }
        db.symbols_for_file(file_id)
            .is_ok_and(|rows| rows.is_empty())
    }

    /// Record identity rows for fragments already committed from the same
    /// harvest pass. Reads still serve both shapes afterwards.
    pub fn backfill_symbols(
        db: &GuidanceDb,
        fragments: &[EntityFragment],
    ) -> Result<usize, String> {
        let rows: Vec<search_vector::db::SymbolRecord> =
            fragments.iter().filter_map(symbol_record).collect();
        db.replace_symbols(&rows)
            .map_err(|e| format!("symbols: {e}"))
    }
}

/// Vector-count failure, shared by both policies.
fn vector_mismatch(vectors: usize, fragments: usize) -> String {
    format!("vector-count mismatch: {vectors} vectors for {fragments} fragments")
}

/// Lemma rows for one commit: block-mapped for code, whole-text for prose,
/// none for structured data. Unmapped code blocks fall back to the smallest
/// containing code fragment; fragments without a block contribute nothing.
fn lemmas_by_fragment(
    file_id: &str,
    kind: FileKind,
    fragments: &[PreparedFragment],
    blocks: &[CommentBlock],
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Vec<FragmentLemma> {
    match kind {
        FileKind::Data => Vec::new(),
        FileKind::Text | FileKind::Image => {
            let mut out = Vec::new();
            for fragment in fragments {
                for lemma in query_lemmas(nlp, &fragment.content_text) {
                    out.push(fragment_lemma(&fragment.id, &lemma, 1.0));
                }
            }
            out
        }
        FileKind::Code => {
            let mut by_name: std::collections::HashMap<(&str, &str), Vec<&str>> =
                std::collections::HashMap::new();
            for fragment in fragments {
                if let Some(FragmentMetadata::Code {
                    symbol_name: Some(name),
                    scope,
                    ..
                }) = &fragment.metadata
                {
                    by_name
                        .entry((name.as_str(), scope.as_deref().unwrap_or("")))
                        .or_default()
                        .push(fragment.id.as_str());
                }
            }
            let module_id = crate::extractor::code::module_fragment_id(file_id);
            let mut by_fragment: std::collections::HashMap<String, Vec<&str>> =
                std::collections::HashMap::new();
            let mut unmatched: Vec<&CommentBlock> = Vec::new();
            for block in blocks {
                if block.node_key == "$file" {
                    by_fragment
                        .entry(module_id.clone())
                        .or_default()
                        .push(block.text.as_str());
                    continue;
                }
                match by_name.get(&(block.name.as_str(), block.scope.as_str())) {
                    Some(ids) => {
                        for id in ids {
                            by_fragment
                                .entry((*id).to_string())
                                .or_default()
                                .push(block.text.as_str());
                        }
                    }
                    None => unmatched.push(block),
                }
            }
            for block in unmatched {
                let mut best: Option<(&str, usize)> = None;
                for fragment in fragments {
                    if !matches!(&fragment.metadata, Some(FragmentMetadata::Code { .. })) {
                        continue;
                    }
                    if fragment.range.start_offset <= block.start_byte
                        && block.end_byte <= fragment.range.end_offset
                    {
                        let len = fragment
                            .range
                            .end_offset
                            .saturating_sub(fragment.range.start_offset);
                        let narrower = match best {
                            None => true,
                            Some((_, best_len)) => len < best_len,
                        };
                        if narrower {
                            best = Some((fragment.id.as_str(), len));
                        }
                    }
                }
                if let Some((id, _)) = best {
                    by_fragment
                        .entry(id.to_string())
                        .or_default()
                        .push(block.text.as_str());
                }
            }
            let mut out = Vec::new();
            for (fragment_id, texts) in &by_fragment {
                for lemma in lemmas_for_comment_texts(texts, nlp) {
                    out.push(fragment_lemma(fragment_id, &lemma, 1.0));
                }
            }
            out
        }
    }
}

/// Stored file row for one scanned file.
pub(crate) fn scanned_record(file: &ScannedFile) -> FileRecord {
    FileRecord {
        id: file.id.clone(),
        absolute_path: file.absolute_path.clone(),
        relative_path: file.relative_path.clone(),
        root_path: file.root_path.clone(),
        size_bytes: file.size_bytes,
        last_modified_time: file.last_modified_time,
        kind: file.kind.clone(),
        format: file.format.clone(),
        content_hash: file.content_hash.clone(),
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    }
}

/// Stored fragment rows for one prepared chunk.
fn prepared_to_entity(file_id: &str, fragment: &PreparedFragment) -> EntityFragment {
    EntityFragment {
        id: fragment.id.clone(),
        group: fragment.group.clone(),
        file_id: file_id.to_string(),
        range: FragmentSpan::Text {
            start_line: fragment.range.start_line as u32,
            end_line: fragment.range.end_line as u32,
            start_offset: fragment.range.start_offset as u64,
            end_offset: fragment.range.end_offset as u64,
        },
        content: FragmentContent::Text {
            text: fragment.content_text.clone(),
        },
        metadata: fragment.metadata.as_ref().map(prepared_metadata),
    }
}

/// Stored metadata for one prepared chunk.
fn prepared_metadata(metadata: &FragmentMetadata) -> EntityMetadata {
    match metadata {
        FragmentMetadata::Code {
            symbol_type,
            symbol_name,
            scope,
            node_type,
            signature,
            doc,
            modifiers,
        } => EntityMetadata::Code {
            symbol_type: *symbol_type,
            symbol_name: symbol_name.clone(),
            scope: scope.clone(),
            node_type: Some(node_type.clone()),
            signature: signature.clone(),
            doc: doc.clone(),
            modifiers: modifiers
                .iter()
                .filter_map(|name| match name.as_str() {
                    "exported" => Some(CodeEntityModifier::Exported),
                    "async" => Some(CodeEntityModifier::Async),
                    "static" => Some(CodeEntityModifier::Static),
                    "public" => Some(CodeEntityModifier::Public),
                    "private" => Some(CodeEntityModifier::Private),
                    "protected" => Some(CodeEntityModifier::Protected),
                    "internal" => Some(CodeEntityModifier::Internal),
                    _ => None,
                })
                .collect(),
        },
        FragmentMetadata::Markdown {
            heading,
            level,
            scope,
        } => EntityMetadata::Markdown {
            heading: heading.clone(),
            level: *level,
            scope: scope.clone(),
        },
    }
}

#[cfg(test)]
#[path = "../../tests/writer_parity.rs"]
mod writer_parity_tests;