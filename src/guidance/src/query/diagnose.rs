//! Recall diagnosis: why an entity/file does or does not surface.
//!
//! Moved verbatim from `query/recall.rs` (composability split). Composes the
//! run loop (`super::recall::run_recall`) and plan errors (`super::plan`);
//! storage arrives as the injected `RecallStorage` seam.

use super::plan::RecallError;
use super::recall::{run_recall, RecallOutput, RecallStorage};
use crate::search_types::{Entity, FileInfo, SearchPlan, SearchPlanRouteMode};

/// Entity-search diagnosis: the stored entity + file plus a traced,
/// force-tracked search. Source: `diagnoseEntitySearch`.
#[derive(Debug)]
pub struct EntityDiagnosis {
    /// Diagnosed entity.
    pub entity: Entity,
    /// Owning file.
    pub file: FileInfo,
    /// Traced search with the entity force-tracked.
    pub search: RecallOutput,
}

/// Diagnose why an entity does or does not surface for a query.
pub fn diagnose_entity(
    query: &str,
    entity_id: &str,
    storage: &dyn RecallStorage,
    embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Result<EntityDiagnosis, RecallError> {
    let Some((entity, file)) = storage.get_entity(entity_id)? else {
        return Err(RecallError::EntityNotFound(entity_id.to_string()));
    };
    let plan = SearchPlan {
        routes: vec![
            crate::search_types::SearchPlanRoute {
                mode: SearchPlanRouteMode::Fts,
                query: query.to_string(),
            },
            crate::search_types::SearchPlanRoute {
                mode: SearchPlanRouteMode::Vector,
                query: query.to_string(),
            },
        ],
        trace: true,
        track_entity_id: Some(entity_id.to_string()),
        ..Default::default()
    };
    let search = run_recall(&plan, storage, embedder, nlp)?;
    Ok(EntityDiagnosis {
        entity,
        file,
        search,
    })
}

/// File-search diagnosis: resolve the file by absolute path, run the query,
/// and take the first hit's entity — falling back to the file's first
/// indexed entity when recall is empty. Source: `diagnoseFileSearch`.
pub fn diagnose_file(
    query: &str,
    absolute_path: &str,
    storage: &dyn RecallStorage,
    embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Result<Option<EntityDiagnosis>, RecallError> {
    let files = storage.list_files()?;
    let Some(file) = files
        .iter()
        .find(|file| file.absolute_path == absolute_path)
    else {
        return Ok(None);
    };
    let search = run_recall(
        &SearchPlan {
            routes: vec![
                crate::search_types::SearchPlanRoute {
                    mode: SearchPlanRouteMode::Fts,
                    query: query.to_string(),
                },
                crate::search_types::SearchPlanRoute {
                    mode: SearchPlanRouteMode::Vector,
                    query: query.to_string(),
                },
            ],
            ..Default::default()
        },
        storage,
        embedder,
        nlp,
    )?;
    if let Some(hit) = search.hits.first() {
        return Ok(Some(EntityDiagnosis {
            entity: hit.entity.clone(),
            file: hit.file.clone(),
            search,
        }));
    }
    let mut entities = storage.list_entities_by_file(&file.id)?;
    if entities.is_empty() {
        return Ok(None);
    }
    let (entity, file) = entities.remove(0);
    Ok(Some(EntityDiagnosis {
        entity,
        file,
        search,
    }))
}
