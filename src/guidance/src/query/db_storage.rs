//! `GuidanceDb` as recall storage: converts between the P0 `search_types`
//! contracts and the fragment tables. P2 indexing writes through the
//! same seam (`upsert_fragments`); P1 ingestion and recall share it.

use search_vector::db::{
    FragmentLemma, GuidanceDb, FileRecord, FragmentFilter, FragmentRecord, FragmentRow,
};

use crate::query::recall::{symbol_type_name, RecallError, RecallStorage};
use crate::search_types::{
    CodeEntityModifier, CodeSymbolType, Entity, EntityFragment, EntityMetadata, FileInfo, FileKind,
    ImageFormat, StorageFilter, StorageHit, FragmentContent, FragmentSpan,
};

/// Recall storage over a `GuidanceDb` fragment index.
pub struct GuidanceDbStorage<'a> {
    /// Open fragment index.
    pub db: &'a GuidanceDb,
}

impl<'a> GuidanceDbStorage<'a> {
    /// Wrap an open database.
    #[must_use]
    pub fn new(db: &'a GuidanceDb) -> Self {
        Self { db }
    }

    fn storage_hit(
        &self,
        id: &str,
        file_id: &str,
        score: f64,
        rank: usize,
    ) -> Result<Option<StorageHit>, RecallError> {
        let rows = self
            .db
            .fragments_for_entity(id)
            .map_err(|error| db_error(&error))?;
        let Some(major) = rows
            .iter()
            .find(|row| row.id == id)
            .or_else(|| rows.first())
        else {
            return Ok(None);
        };
        let Some(file) = self
            .db
            .get_file(file_id)
            .map_err(|error| db_error(&error))?
        else {
            return Ok(None);
        };
        Ok(Some(StorageHit {
            fragment: entity_fragment(major),
            file: file_info(&file),
            rank,
            score,
        }))
    }
}

fn db_error(error: &search_vector::db::VectorDbError) -> RecallError {
    RecallError::Db(error.to_string())
}

impl RecallStorage for GuidanceDbStorage<'_> {
    fn list_files(&self) -> Result<Vec<FileInfo>, RecallError> {
        Ok(self
            .db
            .list_files()
            .map_err(|error| db_error(&error))?
            .iter()
            .map(file_info)
            .collect())
    }

    fn search_fts(
        &self,
        query: &str,
        limit: usize,
        filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError> {
        let hits = self
            .db
            .search_fts(query, limit, &fragment_filter(filter))
            .map_err(|error| db_error(&error))?;
        hits.into_iter()
            .enumerate()
            .filter_map(|(index, hit)| {
                self.storage_hit(&hit.id, &hit.file_id, hit.score, index + 1)
                    .transpose()
            })
            .collect()
    }

    fn search_vector(
        &self,
        embedding: &[f32],
        limit: usize,
        filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError> {
        let hits = self
            .db
            .search_vector_fragments(embedding, limit, &fragment_filter(filter))
            .map_err(|error| db_error(&error))?;
        hits.into_iter()
            .enumerate()
            .filter_map(|(index, hit)| {
                self.storage_hit(&hit.id, &hit.file_id, hit.score, index + 1)
                    .transpose()
            })
            .collect()
    }

    fn search_lemmas(
        &self,
        lemmas: &[String],
        limit: usize,
        filter: &StorageFilter,
    ) -> Result<Vec<StorageHit>, RecallError> {
        let ids = self
            .db
            .fragments_for_lemmas(lemmas, limit)
            .map_err(|error| db_error(&error))?;
        let mut out = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            let rows = self
                .db
                .fragments_for_entity(id)
                .map_err(|error| db_error(&error))?;
            let Some(major) = rows
                .iter()
                .find(|row| row.id == *id)
                .or_else(|| rows.first())
            else {
                continue;
            };
            if let Some(allowed) = &filter.file_ids {
                if !allowed.contains(&major.file_id) {
                    continue;
                }
            }
            let Some(file) = self
                .db
                .get_file(&major.file_id)
                .map_err(|error| db_error(&error))?
            else {
                continue;
            };
            out.push(StorageHit {
                fragment: entity_fragment(major),
                file: file_info(&file),
                rank: index + 1,
                score: 1.0,
            });
        }
        Ok(out)
    }

    fn get_entity(&self, entity_id: &str) -> Result<Option<(Entity, FileInfo)>, RecallError> {
        let rows = self
            .db
            .fragments_for_entity(entity_id)
            .map_err(|error| db_error(&error))?;
        let Some(major) = rows.iter().find(|row| row.id == entity_id) else {
            return Ok(None);
        };
        let Some(file) = self
            .db
            .get_file(&major.file_id)
            .map_err(|error| db_error(&error))?
        else {
            return Ok(None);
        };
        Ok(Some((entity_from_row(major), file_info(&file))))
    }

    fn list_entities_by_file(&self, file_id: &str) -> Result<Vec<(Entity, FileInfo)>, RecallError> {
        let ids = self
            .db
            .entity_ids_for_file(file_id)
            .map_err(|error| db_error(&error))?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(pair) = self.get_entity(&id)? {
                out.push(pair);
            }
        }
        Ok(out)
    }
}

fn fragment_filter(filter: &StorageFilter) -> FragmentFilter {
    FragmentFilter {
        file_ids: filter.file_ids.clone(),
        group_ids: filter.group_ids.clone(),
        symbol_names: filter.symbol_names.clone(),
        symbol_types: filter.symbol_types.iter().map(symbol_type_name).collect(),
        modified_after: filter.modified_after,
        modified_before: filter.modified_before,
    }
}

/// Convert a P0 `FileInfo` into a file row for ingestion.
#[must_use]
pub fn file_record(file: &FileInfo) -> FileRecord {
    FileRecord {
        id: file.id.clone(),
        absolute_path: file.absolute_path.clone(),
        relative_path: file.relative_path.clone(),
        root_path: file.root_path.clone(),
        size_bytes: file.size_bytes,
        last_modified_time: file.last_modified_time,
        kind: file.kind.map(|kind| {
            match kind {
                FileKind::Text => "text",
                FileKind::Code => "code",
                FileKind::Data => "data",
                FileKind::Image => "image",
            }
            .to_string()
        }),
        format: file.format.clone(),
        content_hash: file.content_hash.clone(),
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    }
}

/// Convert a P0 `EntityFragment` into a fragment row (`cjk_text` and
/// `embedding` filled by the ingestion helper, not here).
#[must_use]
pub fn fragment_record(fragment: &EntityFragment) -> FragmentRecord {
    let (symbol_type, symbol_name, scope, signature, doc, modifiers, heading, heading_level) =
        match &fragment.metadata {
            Some(EntityMetadata::Code {
                symbol_type,
                symbol_name,
                scope,
                node_type: _,
                signature,
                doc,
                modifiers,
            }) => (
                Some(symbol_type_name(symbol_type)),
                symbol_name.clone(),
                scope.clone(),
                signature.clone(),
                doc.clone(),
                modifiers
                    .iter()
                    .map(|modifier| match modifier {
                        CodeEntityModifier::Exported => "exported",
                        CodeEntityModifier::Async => "async",
                        CodeEntityModifier::Static => "static",
                        CodeEntityModifier::Public => "public",
                        CodeEntityModifier::Private => "private",
                        CodeEntityModifier::Protected => "protected",
                        CodeEntityModifier::Internal => "internal",
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                None,
                None,
            ),
            Some(EntityMetadata::Markdown {
                heading,
                level,
                scope,
            }) => (
                None,
                None,
                scope.clone(),
                None,
                None,
                String::new(),
                heading.clone(),
                level.map(i64::from),
            ),
            None => (None, None, None, None, None, String::new(), None, None),
        };
    let (content_kind, content_text) = match &fragment.content {
        FragmentContent::Text { text } => ("text".to_string(), Some(text.clone())),
        FragmentContent::Image { .. } => ("image".to_string(), None),
    };
    FragmentRecord {
        id: fragment.id.clone(),
        group: fragment.group.clone(),
        file_id: fragment.file_id.clone(),
        range_json: serde_json::to_string(&fragment.range).unwrap_or_else(|_| "{}".to_string()),
        content_kind,
        content_text,
        cjk_text: String::new(),
        symbol_type,
        symbol_name,
        scope,
        signature,
        doc,
        modifiers,
        heading,
        heading_level,
        embedding: None,
    }
}

/// Build a lemma row for the L2 table.
#[must_use]
pub fn fragment_lemma(fragment_id: &str, lemma: &str, confidence: f64) -> FragmentLemma {
    FragmentLemma {
        fragment_id: fragment_id.to_string(),
        lemma: lemma.to_string(),
        confidence,
    }
}

fn file_info(file: &FileRecord) -> FileInfo {
    FileInfo {
        id: file.id.clone(),
        absolute_path: file.absolute_path.clone(),
        relative_path: file.relative_path.clone(),
        root_path: file.root_path.clone(),
        size_bytes: file.size_bytes,
        last_modified_time: file.last_modified_time,
        content_hash: None,
        kind: file.kind.as_deref().and_then(|kind| match kind {
            "text" => Some(FileKind::Text),
            "code" => Some(FileKind::Code),
            "data" => Some(FileKind::Data),
            "image" => Some(FileKind::Image),
            _ => None,
        }),
        format: file.format.clone(),
        index_status: None,
    }
}

fn entity_fragment(row: &FragmentRow) -> EntityFragment {
    EntityFragment {
        id: row.id.clone(),
        group: row.group.clone(),
        file_id: row.file_id.clone(),
        range: serde_json::from_str(&row.range_json).unwrap_or(FragmentSpan::File),
        content: match &row.content_text {
            Some(text) => FragmentContent::Text { text: text.clone() },
            None => FragmentContent::Image {
                data: Vec::new(),
                format: ImageFormat::Png,
            },
        },
        metadata: row_metadata(row),
    }
}

fn entity_from_row(row: &FragmentRow) -> Entity {
    Entity {
        id: row
            .group
            .clone()
            .filter(|group| !group.is_empty())
            .unwrap_or_else(|| row.id.clone()),
        file_id: row.file_id.clone(),
        range: serde_json::from_str(&row.range_json).unwrap_or(FragmentSpan::File),
        content: match &row.content_text {
            Some(text) => FragmentContent::Text { text: text.clone() },
            None => FragmentContent::Image {
                data: Vec::new(),
                format: ImageFormat::Png,
            },
        },
        metadata: row_metadata(row),
    }
}

fn row_metadata(row: &FragmentRow) -> Option<EntityMetadata> {
    if let Some(symbol_type) = row.symbol_type.as_deref().and_then(parse_symbol_type) {
        return Some(EntityMetadata::Code {
            symbol_type,
            symbol_name: row.symbol_name.clone(),
            scope: row.scope.clone(),
            node_type: None,
            signature: row.signature.clone(),
            doc: row.doc.clone(),
            modifiers: row
                .modifiers
                .split_whitespace()
                .filter_map(parse_modifier)
                .collect(),
        });
    }
    if row.heading.is_some() {
        return Some(EntityMetadata::Markdown {
            heading: row.heading.clone(),
            level: row.heading_level.map(|level| level as u32),
            scope: row.scope.clone(),
        });
    }
    None
}

fn parse_symbol_type(name: &str) -> Option<CodeSymbolType> {
    match name {
        "module" => Some(CodeSymbolType::Module),
        "class" => Some(CodeSymbolType::Class),
        "interface" => Some(CodeSymbolType::Interface),
        "function" => Some(CodeSymbolType::Function),
        "value" => Some(CodeSymbolType::Value),
        "alias" => Some(CodeSymbolType::Alias),
        _ => None,
    }
}

fn parse_modifier(name: &str) -> Option<CodeEntityModifier> {
    match name {
        "exported" => Some(CodeEntityModifier::Exported),
        "async" => Some(CodeEntityModifier::Async),
        "static" => Some(CodeEntityModifier::Static),
        "public" => Some(CodeEntityModifier::Public),
        "private" => Some(CodeEntityModifier::Private),
        "protected" => Some(CodeEntityModifier::Protected),
        "internal" => Some(CodeEntityModifier::Internal),
        _ => None,
    }
}
