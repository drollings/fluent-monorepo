//! Hybrid orchestration: query → plan route construction, hit → stage
//! conversion with trace provenance. The fused recall itself lives in
//! `recall::run_recall`; this module is the thin shell around it.

use crate::query::strategy::QueryIntent;
use crate::query::synthesize::{Stage, StageRecall, StageTrace};
use crate::search_types::{
    EntityMetadata, FragmentContent, FragmentSpan, SearchHit, SearchMatchedBy, SearchPlan,
    SearchPlanRoute,
};

/// Build a two-route hybrid plan from a query string. `FsmEngine` feeds
/// route construction (not narrowing): identifier-shaped intents prefer
/// symbol-filtered recall downstream.
#[must_use]
pub fn plan_from_query(query: &str, intent: QueryIntent, limit: usize) -> SearchPlan {
    use crate::search_types::SearchPlanRouteMode::{Fts, Vector};
    SearchPlan {
        routes: vec![
            SearchPlanRoute {
                mode: Fts,
                query: query.to_string(),
            },
            SearchPlanRoute {
                mode: Vector,
                query: query.to_string(),
            },
        ],
        limit: Some(limit),
        trace: true,
        prefer_symbol: Some(matches!(
            intent,
            QueryIntent::SingleIdentifier | QueryIntent::IdentifierLookup
        )),
        ..Default::default()
    }
}

/// Convert fused hits into stages with trace provenance.
/// Grounding gate (G-1.ground): `Code` stages without a verifiable
/// `file:line` are dropped before render — unverified hits are downgraded,
/// never cited. (`verify_citations` itself checks citations in synthesis
/// output against these stages downstream.)
#[must_use]
pub fn stages_from_hits(hits: &[SearchHit]) -> Vec<Stage> {
    hits.iter()
        .map(stage_from_hit)
        .filter(|stage| {
            stage.kind != fluent_types::StageKind::Code
                || (!stage.source.is_empty() && stage.line.is_some())
        })
        .collect()
}

/// Convert one fused hit into a stage.
#[must_use]
pub fn stage_from_hit(hit: &SearchHit) -> Stage {
    let (kind, content) = match &hit.entity.metadata {
        Some(EntityMetadata::Code { signature, .. }) => (
            fluent_types::StageKind::Code,
            signature.clone().unwrap_or_else(|| hit_text(hit)),
        ),
        _ => (fluent_types::StageKind::Prose, hit_text(hit)),
    };
    Stage {
        kind,
        content,
        source: hit.file.relative_path.clone(),
        line: match &hit.entity.range {
            FragmentSpan::Text { start_line, .. } => Some(*start_line),
            _ => None,
        },
        end_line: match &hit.entity.range {
            FragmentSpan::Text { end_line, .. } => Some(*end_line),
            _ => None,
        },
        member_name: match &hit.entity.metadata {
            Some(EntityMetadata::Code { symbol_name, .. }) => symbol_name.clone(),
            _ => None,
        },
        member_type: None,
        trace: hit.trace.as_ref().map(|trace| StageTrace {
            matched_by: matched_by_name(hit.matched_by).to_string(),
            score: hit.score,
            rank: hit.rank,
            recall: trace
                .recall
                .iter()
                .map(|recall| StageRecall {
                    path: format!("{:?}", recall.path).to_lowercase(),
                    route_id: recall.route_id.clone(),
                    rank: recall.rank.map(|rank| rank as u32),
                    score: recall.score,
                    found: recall.found,
                })
                .collect(),
        }),
    }
}

/// zvec `matchedBy` vocabulary for a fused hit.
#[must_use]
pub fn matched_by_name(matched_by: SearchMatchedBy) -> &'static str {
    match matched_by {
        SearchMatchedBy::Fts => "fts",
        SearchMatchedBy::Vector => "vector",
        SearchMatchedBy::FtsAndVector => "fts+vector",
    }
}

fn hit_text(hit: &SearchHit) -> String {
    match &hit.entity.content {
        FragmentContent::Text { text } => text.clone(),
        FragmentContent::Image { .. } => "[image]".to_string(),
    }
}

/// Convert one storage-level hit into a stage (lemma / single-route paths
/// that skip full fusion).
#[must_use]
pub fn stage_from_storage_hit(hit: &crate::search_types::StorageHit) -> Stage {
    let (kind, content, member_name) = match &hit.fragment.metadata {
        Some(EntityMetadata::Code {
            signature,
            symbol_name,
            ..
        }) => (
            fluent_types::StageKind::Code,
            signature.clone().unwrap_or_else(|| storage_text(hit)),
            symbol_name.clone(),
        ),
        _ => (fluent_types::StageKind::Prose, storage_text(hit), None),
    };
    Stage {
        kind,
        content,
        source: hit.file.relative_path.clone(),
        line: match &hit.fragment.range {
            FragmentSpan::Text { start_line, .. } => Some(*start_line),
            _ => None,
        },
        end_line: match &hit.fragment.range {
            FragmentSpan::Text { end_line, .. } => Some(*end_line),
            _ => None,
        },
        member_name,
        member_type: None,
        trace: None,
    }
}

fn storage_text(hit: &crate::search_types::StorageHit) -> String {
    match &hit.fragment.content {
        FragmentContent::Text { text } => text.clone(),
        FragmentContent::Image { .. } => "[image]".to_string(),
    }
}

#[cfg(test)]
#[path = "../../tests/query_hybrid.rs"]
mod tests;
