//! Docs plane: capability-card retrieval with live-section proof.
//!
//! Cards rank over `capabilities_fts` (title + purpose + keywords)
//! plus lemma postings; only `ready` rows participate. Each card
//! resolves to its live doc section through the shared chunker and
//! the section hash is verified — a drifted section renders as a
//! gap, never trusted text. Doc paragraphs stay on the old
//! query-time walk until the card anchors replace them.

use std::path::Path;

use crate::query::search_db::{CapabilityRecord, GuidanceSearchDb};

use crate::query::capability_match::{rank_briefs, BriefCandidate, RankedBrief};

/// One retrieved card with its live proof.
#[derive(Debug, Clone, PartialEq)]
pub struct DocCardHit {
    /// Card id (`source_path#anchor`).
    pub id: String,
    /// Card title.
    pub title: String,
    /// Stored purpose.
    pub purpose: String,
    /// Owning crate.
    pub crate_name: String,
    /// Doc domain.
    pub domain: String,
    /// Workspace-relative doc path.
    pub source_path: String,
    /// Section anchor within the doc.
    pub anchor: String,
    /// SVO overlap against the query signals.
    pub overlap: crate::query::capability_match::CapabilityOverlap,
    /// FTS score over card text (`0.0` for lemma-only candidates).
    pub fts_score: f64,
    /// Stored SVO predicate lemma.
    pub predicate: String,
    /// Stored SVO role lemmas.
    pub roles: Vec<String>,
    /// Code paths the card points at.
    pub code_paths: Vec<String>,
    /// Symbols the card points at.
    pub symbols: Vec<String>,
    /// Live section excerpt (`None` when the anchor is gone).
    pub live_excerpt: Option<String>,
    /// True when the live hash matches the stored row.
    pub verified: bool,
    /// Judge label when judged, `None` when unjudged.
    pub label: Option<crate::read::decide::DocLabel>,
}

/// Candidate pool bound (matches the shipping branch width).
pub const DOC_CANDIDATE_LIMIT: usize = 20;

/// Retrieve capability cards for one query: FTS plus lemma recall,
/// SVO-ranked, live sections resolved and verified. Rank-only, never
/// filtered. `judge` labels verdicts when present; `None` leaves
/// everything unjudged (`considered` downstream).
#[must_use]
pub fn retrieve_cards(
    query: &str,
    workspace: Option<&Path>,
    db: &GuidanceSearchDb,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
    judge: Option<&crate::read::decide::DocJudge>,
) -> Vec<DocCardHit> {
    use crate::query::capability_match::funneled_query_signals;
    use crate::query::funnel::funnel_terms;
    let funnel = funnel_terms(query, &crate::query::aliases::SemanticAliases::new(), None);
    if funnel.is_empty() {
        return Vec::new();
    }
    let signals = funneled_query_signals(&funnel, nlp);
    let expanded = funnel.expanded.join(" ");
    let mut fts_scores: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    if let Ok(hits) = db.search_capabilities(&expanded, DOC_CANDIDATE_LIMIT) {
        for hit in hits {
            fts_scores.insert(hit.id, hit.score);
        }
    }
    let mut ids: std::collections::HashSet<String> = fts_scores.keys().cloned().collect();
    let lemmas = crate::query::ingest::query_lemmas(nlp, &expanded);
    if !lemmas.is_empty() {
        if let Ok(lemma_ids) = db.capabilities_for_lemmas(&lemmas, DOC_CANDIDATE_LIMIT) {
            ids.extend(lemma_ids);
        }
    }
    if ids.is_empty() {
        return Vec::new();
    }
    let mut ordered: Vec<String> = ids.into_iter().collect();
    ordered.sort();
    let rows = db.capabilities_for_ids(&ordered).unwrap_or_default();
    let candidates: Vec<BriefCandidate> = rows
        .iter()
        .map(|row| BriefCandidate {
            id: row.id.clone(),
            purpose: row.purpose.clone(),
            predicate: row.predicate.clone(),
            roles: serde_json::from_str(&row.roles_json).unwrap_or_default(),
            crate_name: row.crate_name.clone(),
            domain: row.domain.clone(),
            source_path: row.source_path.clone(),
            anchor: row.anchor.clone(),
            fts_score: fts_scores.remove(&row.id),
        })
        .collect();
    let ranked = rank_briefs(&signals, &candidates);
    let by_id: std::collections::HashMap<&str, &CapabilityRecord> =
        rows.iter().map(|row| (row.id.as_str(), row)).collect();
    let verdicts: std::collections::HashMap<String, crate::read::decide::DocLabel> = judge
        .map(|judge| {
            let inputs: Vec<(String, String)> = ranked
                .iter()
                .map(|brief| (brief.id.clone(), brief.purpose.clone()))
                .collect();
            judge
                .judge(query, &inputs)
                .into_iter()
                .map(|verdict| (verdict.id, verdict.label))
                .collect()
        })
        .unwrap_or_default();
    ranked
        .into_iter()
        .map(|brief| {
            let row = by_id.get(brief.id.as_str());
            let (live_excerpt, verified) = match row {
                Some(row) => verify_live_section(workspace, row),
                None => (None, false),
            };
            DocCardHit {
                id: brief.id.clone(),
                title: row.map(|row| row.title.clone()).unwrap_or_default(),
                purpose: brief.purpose.clone(),
                crate_name: brief.crate_name.clone(),
                domain: brief.domain.clone(),
                source_path: brief.source_path.clone(),
                anchor: brief.anchor.clone(),
                overlap: brief.overlap,
                fts_score: brief.fts_score,
                predicate: brief.predicate.clone(),
                roles: brief.roles.clone(),
                code_paths: row
                    .map(|row| json_list(&row.code_paths_json))
                    .unwrap_or_default(),
                symbols: row
                    .map(|row| json_list(&row.symbols_json))
                    .unwrap_or_default(),
                live_excerpt,
                verified,
                label: verdicts.get(&brief.id).copied(),
            }
        })
        .collect()
}

/// Resolve the live section and compare hashes: the stored row is a
/// claim about the doc, the live bytes are the proof. Missing files
/// and anchors read as unverified with no excerpt.
fn verify_live_section(workspace: Option<&Path>, row: &CapabilityRecord) -> (Option<String>, bool) {
    resolve_and_verify_section(workspace, &row.source_path, &row.anchor, &row.content_hash)
}

fn json_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Provenance tag for branch-2 pulls (rendered beside merged code).
#[must_use]
pub fn branch_provenance(card: &DocCardHit) -> String {
    format!("via capability: {}", card.crate_name)
}

/// One verified documentation section from the project-card doc plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocHit {
    /// Card key (`doc:<path>#<anchor>`).
    pub key: String,
    /// Section title (breadcrumb).
    pub title: String,
    /// Workspace-relative source path.
    pub source_path: String,
    /// Section anchor.
    pub anchor: String,
    /// Verbatim live section body; `None` when the source vanished.
    pub body: Option<String>,
    /// Live hash matches the stored section hash.
    pub verified: bool,
    /// Workspace-relative code scopes this section points at.
    pub code_paths: Vec<String>,
}

/// Resolve a live doc section and compare its hash with the stored one.
/// The one verifier shared by capability cards and doc cards.
#[must_use]
pub fn resolve_and_verify_section(
    workspace: Option<&Path>,
    source_path: &str,
    anchor: &str,
    content_hash: &str,
) -> (Option<String>, bool) {
    let Some(workspace) = workspace else {
        return (None, false);
    };
    let Ok(text) = std::fs::read_to_string(workspace.join(source_path)) else {
        return (None, false);
    };
    let section = crate::read::doc_cards::resolve_live_section(source_path, anchor, &text);
    match section {
        None => (None, false),
        Some(section) => {
            let live_hash = common_core::hash::blake3_hex(section.as_bytes());
            (Some(section), live_hash == content_hash)
        }
    }
}

/// Rank corroborated documentation sections for the docs plane. Ranked,
/// never filtered: callers decide whether an unverified section renders
/// with a label.
#[must_use]
pub fn retrieve_doc_sections(
    query: &str,
    workspace: Option<&Path>,
    db: &GuidanceSearchDb,
    limit: usize,
) -> Vec<DocHit> {
    retrieve_doc_sections_anchored(query, workspace, db, limit, None).0
}

/// Anchor-scoped doc sections plus the exclusion count: keeps the
/// candidates mentioning any anchor term (path, anchor, title, body,
/// or code scope), fail-open to the full set when the anchor would
/// remove everything. `None` anchors and shadow identifier anchors
/// pass through unchanged; only exact-crate anchors filter before
/// their calibration. The count rides `--trace`.
#[must_use]
pub fn retrieve_doc_sections_anchored(
    query: &str,
    workspace: Option<&Path>,
    db: &GuidanceSearchDb,
    limit: usize,
    anchor: Option<&crate::read::vocabulary::SubjectAnchor>,
) -> (Vec<DocHit>, usize) {
    let hits = retrieve_doc_sections_unscoped(query, workspace, db, limit);
    let Some(anchor) = anchor else {
        return (hits, 0);
    };
    if anchor.source != crate::read::vocabulary::AnchorSource::Crate {
        return (hits, 0);
    }
    let kept: Vec<DocHit> = hits
        .iter()
        .filter(|hit| {
            let mut text =
                format!("{} {} {}", hit.source_path, hit.anchor, hit.title).to_lowercase();
            text.push(' ');
            text.push_str(&hit.body.clone().unwrap_or_default().to_lowercase());
            text.push(' ');
            text.push_str(&hit.code_paths.join(" ").to_lowercase());
            anchor.terms.iter().any(|term| text.contains(term.as_str()))
        })
        .cloned()
        .collect();
    if kept.is_empty() {
        return (hits, 0);
    }
    let excluded = hits.len() - kept.len();
    (kept, excluded)
}

/// Unscoped retrieval: the one FTS-plus-verify path both entry
/// points share, so `None` anchors read byte-identically.
fn retrieve_doc_sections_unscoped(
    query: &str,
    workspace: Option<&Path>,
    db: &GuidanceSearchDb,
    limit: usize,
) -> Vec<DocHit> {
    let cards = db
        .search_project_cards("doc", query, limit.max(1))
        .unwrap_or_default();
    cards
        .into_iter()
        .map(|card| {
            let (body, verified) = resolve_and_verify_section(
                workspace,
                &card.source_path,
                &card.anchor,
                &card.content_hash,
            );
            DocHit {
                key: card.key,
                title: card.title,
                source_path: card.source_path,
                anchor: card.anchor,
                body,
                verified,
                code_paths: serde_json::from_str(&card.code_paths_json).unwrap_or_default(),
            }
        })
        .collect()
}

/// Render capability cards as evidence entries. External cards stay
/// `[considered]` until their own point calibrates.
#[must_use]
pub fn capability_entries(hits: &[DocCardHit]) -> Vec<(String, String)> {
    hits.iter()
        .map(|hit| {
            let scopes = hit
                .code_paths
                .iter()
                .chain(hit.symbols.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let body = if scopes.is_empty() {
                hit.purpose.clone()
            } else {
                format!("{} (scopes: {scopes})", hit.purpose)
            };
            (
                format!("[considered] {}/{} ({})", hit.crate_name, hit.title, hit.id),
                body,
            )
        })
        .collect()
}

/// Branch-2 scoped code pull: the rank-1 card scopes an FTS pull by
/// its `code_paths`; hits merge under the normal code section with
/// the card's provenance. No cross-source RRF — the card picks the
/// scope, code recall ranks inside it.
#[must_use]
pub fn branch2_pulls(
    card: &DocCardHit,
    db: &GuidanceSearchDb,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Vec<crate::read::engine::Finding> {
    use crate::query::db_storage::GuidanceDbStorage;
    use crate::query::recall::run_recall;
    use crate::search_types::{SearchPlan, SearchPlanRoute, SearchPlanRouteMode};
    if card.code_paths.is_empty() {
        return Vec::new();
    }
    let plan = SearchPlan {
        routes: vec![SearchPlanRoute {
            mode: SearchPlanRouteMode::Fts,
            query: card.purpose.clone(),
        }],
        include_paths: Some(card.code_paths.clone()),
        limit: Some(5),
        ..Default::default()
    };
    let storage = GuidanceDbStorage::new(db);
    let Ok(output) = run_recall(&plan, &storage, None, nlp) else {
        return Vec::new();
    };
    let provenance = branch_provenance(card);
    output
        .hits
        .into_iter()
        .map(|hit| {
            let (text, start_line, end_line) = hit
                .evidence
                .first()
                .map(|evidence| {
                    use crate::search_types::FragmentContent;
                    let text = match &evidence.content {
                        FragmentContent::Text { text } => text.clone(),
                        FragmentContent::Image { .. } => String::new(),
                    };
                    let (start, end) = match &evidence.range {
                        crate::search_types::FragmentSpan::Text {
                            start_line,
                            end_line,
                            ..
                        } => (Some(*start_line), Some(*end_line)),
                        _ => (None, None),
                    };
                    (text, start, end)
                })
                .unwrap_or_default();
            crate::read::engine::Finding {
                id: format!("{}#{}", hit.file.relative_path, hit.entity.id),
                producer: crate::read::engine::FindingProducer::Database,
                kind: crate::read::engine::FindingKind::Code,
                source: crate::source_ref::SourceRef::from_file(&hit.file),
                start_line,
                end_line,
                text,
                producer_score: hit.score.0,
                provenance: provenance.clone(),
            }
        })
        .collect()
}

/// Ranked briefs for the legacy shape: lossless back-mapping so the
/// shipping path renders byte-identically while the plane owns the
/// one retrieval implementation.
#[must_use]
pub fn ranked_briefs_for_legacy(hits: &[DocCardHit]) -> Vec<RankedBrief> {
    hits.iter()
        .enumerate()
        .map(|(index, hit)| RankedBrief {
            id: hit.id.clone(),
            purpose: hit.purpose.clone(),
            crate_name: hit.crate_name.clone(),
            domain: hit.domain.clone(),
            source_path: hit.source_path.clone(),
            anchor: hit.anchor.clone(),
            overlap: hit.overlap,
            fts_score: hit.fts_score,
            predicate: hit.predicate.clone(),
            roles: hit.roles.clone(),
            rank: index + 1,
        })
        .collect()
}

#[cfg(test)]
#[path = "../../../tests/doc_plane.rs"]
mod doc_plane_tests;
