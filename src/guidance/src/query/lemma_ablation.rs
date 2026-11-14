//! Lemma-route value ablation (measurement only): inflection-heavy
//! recall with the lemma route on vs off. No product code changes —
//! the OFF arm re-fuses recorded per-route evidence through the
//! shared kernel minus the lemma postings, so the matcher itself is
//! never reimplemented and never toggled.
//!
//! Pure harness over a golden corpus (`corpora/lemma_ablation_seed.json`):
//! inflection-heavy queries plus verbatim exact-match controls. The
//! report measures; the keep/trim/restructure decision is recorded
//! by a human, never asserted here.

use std::collections::{HashMap, HashSet};

/// One ablation fragment: indexed text under a stable id.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One ablation fragment: indexed text under a stable id.
pub struct LemmaAblationFragment {
    /// Fragment id (also the public entity id: ungrouped rows).
    pub id: String,
    /// Indexed text.
    pub text: String,
}

/// One golden case: a query plus the fragment ids that answer it.
/// Empty `relevant` with `control` marks no-fire queries (both arms
/// must stay empty).
#[derive(Debug, Clone, PartialEq, Eq)]
/// One golden case: a query plus the fragment ids that answer it.
/// Empty `relevant` with `control` marks no-fire queries (both arms
/// must stay empty).
pub struct LemmaAblationCase {
    /// Natural-language query.
    pub query: String,
    /// Fragment ids that answer it (empty for no-fire controls).
    pub relevant: Vec<String>,
    /// Verbatim exact-match control: recall must not move.
    pub control: bool,
}

/// Golden ablation corpus: the fragment universe plus labels.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Golden ablation corpus: the fragment universe plus labels.
pub struct LemmaAblationCorpus {
    /// Every fragment the harness indexes.
    pub fragments: Vec<LemmaAblationFragment>,
    /// Queries with relevance labels, controls included.
    pub cases: Vec<LemmaAblationCase>,
}

/// Load a lemma-ablation corpus (`corpora/lemma_ablation_seed.json`
/// shape). Unknown shapes refuse the file rather than guessing.
pub fn load_lemma_ablation_corpus(path: &std::path::Path) -> Result<LemmaAblationCorpus, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("corpus read {}: {error}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| format!("corpus parse: {error}"))?;
    let read_fragments = || -> Result<Vec<LemmaAblationFragment>, String> {
        let entries = value
            .get("fragments")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "corpus has no fragments array".to_string())?;
        let mut fragments = Vec::with_capacity(entries.len());
        for entry in entries {
            let id = entry
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "corpus fragment lacks an id string".to_string())?;
            let text = entry
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("corpus fragment lacks text: {id}"))?;
            fragments.push(LemmaAblationFragment {
                id: id.to_string(),
                text: text.to_string(),
            });
        }
        Ok(fragments)
    };
    let read_cases = || -> Result<Vec<LemmaAblationCase>, String> {
        let entries = value
            .get("cases")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "corpus has no cases array".to_string())?;
        let mut cases = Vec::with_capacity(entries.len());
        for entry in entries {
            let query = entry
                .get("query")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "corpus case lacks a query string".to_string())?;
            let relevant = entry
                .get("relevant")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| format!("corpus case lacks relevant ids: {query}"))?;
            let mut ids = Vec::with_capacity(relevant.len());
            for id in relevant {
                ids.push(
                    id.as_str()
                        .ok_or_else(|| format!("corpus case has a non-string id: {query}"))?
                        .to_string(),
                );
            }
            cases.push(LemmaAblationCase {
                query: query.to_string(),
                relevant: ids,
                control: entry
                    .get("control")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            });
        }
        Ok(cases)
    };
    let fragments = read_fragments()?;
    if fragments.is_empty() {
        return Err("corpus has no fragments".to_string());
    }
    let cases = read_cases()?;
    if cases.is_empty() {
        return Err("corpus has no cases".to_string());
    }
    Ok(LemmaAblationCorpus { fragments, cases })
}

/// One case measured both ways: fused top-k with the lemma route
/// (ON) and re-fused without it (OFF), each scored against relevance.
#[derive(Debug, Clone, PartialEq)]
/// One case measured both ways: fused top-k with the lemma route
/// (ON) and re-fused without it (OFF), each scored against relevance.
pub struct LemmaAblationCaseReport {
    /// Case query.
    pub query: String,
    /// Control case (verbatim or no-fire).
    pub control: bool,
    /// Relevant ids found in the ON top-k.
    pub on_hits: usize,
    /// Relevant ids found in the OFF top-k.
    pub off_hits: usize,
    /// Relevant id count (denominator).
    pub support: usize,
    /// ON top-k entity ids in rank order.
    pub on_topk: Vec<String>,
    /// OFF top-k entity ids in rank order.
    pub off_topk: Vec<String>,
}

/// Corpus-level ablation outcome: per-case reports plus the macro
/// averages that carry the keep/trim/restructure decision. No bars,
/// no verdict — measurement only.
#[derive(Debug, Clone, PartialEq)]
/// Corpus-level ablation outcome: per-case reports plus the macro
/// averages that carry the keep/trim/restructure decision. No bars,
/// no verdict — measurement only.
pub struct LemmaAblationReport {
    /// Per-case results in corpus order.
    pub cases: Vec<LemmaAblationCaseReport>,
    /// Mean ON recall@k over non-control cases.
    pub on_recall_at_k: f64,
    /// Mean OFF recall@k over non-control cases.
    pub off_recall_at_k: f64,
    /// ON minus OFF (positive means the route earns its rows).
    pub delta_recall_at_k: f64,
    /// Non-control case count.
    pub support: usize,
    /// Control cases whose recall moved between arms.
    pub control_moved: usize,
    /// Control case count.
    pub control_total: usize,
}

/// Measure the lemma route: index the corpus fragments in a throwaway
/// in-memory index (same rows plus `query_lemmas` postings the ingest
/// pass persists), run each golden query through full recall, and
/// re-fuse the recorded per-route evidence without the lemma postings
/// through the shared kernel. Stores nothing.
pub fn measure_lemma_route(
    corpus: &LemmaAblationCorpus,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
    k: usize,
) -> Result<LemmaAblationReport, String> {
    use search_vector::db::{FileRecord, FragmentLemma, FragmentRecord};
    let db = search_vector::db::GuidanceDb::open_in_memory()
        .map_err(|error| format!("ablation index: {error}"))?;
    let file = FileRecord {
        id: "ablation".to_string(),
        absolute_path: "/ablation.rs".to_string(),
        relative_path: "ablation.rs".to_string(),
        root_path: "/".to_string(),
        size_bytes: 64,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    };
    let records: Vec<FragmentRecord> = corpus
        .fragments
        .iter()
        .map(|fragment| FragmentRecord {
            id: fragment.id.clone(),
            group: None,
            file_id: "ablation".to_string(),
            range_json: "{\"kind\":\"file\"}".to_string(),
            content_kind: "text".to_string(),
            content_text: Some(fragment.text.clone()),
            cjk_text: String::new(),
            symbol_type: None,
            symbol_name: None,
            scope: None,
            signature: None,
            doc: None,
            modifiers: String::new(),
            heading: None,
            heading_level: None,
            embedding: None,
        })
        .collect();
    db.upsert_fragments(&file, &records, &[])
        .map_err(|error| format!("ablation index: {error}"))?;
    let mut postings = Vec::new();
    for fragment in &corpus.fragments {
        for lemma in crate::query::ingest::query_lemmas(nlp, &fragment.text) {
            postings.push(FragmentLemma {
                fragment_id: fragment.id.clone(),
                lemma,
                confidence: 1.0,
            });
        }
    }
    db.insert_fragment_lemmas(&postings)
        .map_err(|error| format!("ablation index: {error}"))?;
    let storage = crate::query::db_storage::GuidanceDbStorage::new(&db);
    let mut cases = Vec::with_capacity(corpus.cases.len());
    for case in &corpus.cases {
        let plan = crate::query::hybrid::plan_from_query(
            &case.query,
            crate::query::strategy::QueryIntent::GeneralSearch,
            k,
        );
        let output = crate::query::recall::run_recall(&plan, &storage, None, nlp)
            .map_err(|error| format!("recall {}: {error:?}", case.query))?;
        let on_topk: Vec<String> = output
            .hits
            .iter()
            .take(k)
            .map(|hit| hit.entity.id.clone())
            .collect();
        let off_topk = refuse_lemma_evidence(&output.hits, k);
        let relevant: HashSet<&str> = case.relevant.iter().map(String::as_str).collect();
        let on_hits = on_topk
            .iter()
            .filter(|id| relevant.contains(id.as_str()))
            .count();
        let off_hits = off_topk
            .iter()
            .filter(|id| relevant.contains(id.as_str()))
            .count();
        cases.push(LemmaAblationCaseReport {
            query: case.query.clone(),
            control: case.control,
            on_hits,
            off_hits,
            support: relevant.len(),
            on_topk,
            off_topk,
        });
    }
    let mut on_sum = 0.0;
    let mut off_sum = 0.0;
    let mut support = 0usize;
    let mut control_moved = 0usize;
    let mut control_total = 0usize;
    for (case, report) in corpus.cases.iter().zip(cases.iter()) {
        if case.control {
            control_total += 1;
            let on_set: HashSet<&str> = report.on_topk.iter().map(String::as_str).collect();
            let off_set: HashSet<&str> = report.off_topk.iter().map(String::as_str).collect();
            if on_set != off_set {
                control_moved += 1;
            }
            continue;
        }
        support += 1;
        let denom = report.support.max(1) as f64;
        on_sum += report.on_hits as f64 / denom;
        off_sum += report.off_hits as f64 / denom;
    }
    let on_recall_at_k = on_sum / support.max(1) as f64;
    let off_recall_at_k = off_sum / support.max(1) as f64;
    Ok(LemmaAblationReport {
        cases,
        on_recall_at_k,
        off_recall_at_k,
        delta_recall_at_k: on_recall_at_k - off_recall_at_k,
        support,
        control_moved,
        control_total,
    })
}

/// OFF arm: re-fuse recorded per-route evidence minus the lemma
/// postings through the shared kernel (same K, same lexicographic
/// tie-break as production fusion). No new matching, no new ranking —
/// the recorded ranks are the inputs.
fn refuse_lemma_evidence(hits: &[crate::search_types::SearchHit], k: usize) -> Vec<String> {
    use crate::search_constants::RRF_K;
    use crate::search_types::RecallPath;
    let mut by_route: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for hit in hits {
        for evidence in &hit.evidence {
            if evidence.path == RecallPath::Lemma {
                continue;
            }
            let Some(rank) = evidence.rank else {
                continue;
            };
            by_route
                .entry(evidence.route_id.clone().unwrap_or_default())
                .or_default()
                .push((rank, hit.entity.id.clone()));
        }
    }
    let mut lists: Vec<Vec<String>> = by_route
        .into_values()
        .map(|mut entries| {
            entries.sort();
            entries.dedup();
            entries.into_iter().map(|(_, id)| id).collect()
        })
        .collect();
    lists.sort();
    let postings: Vec<(String, usize)> = lists
        .iter()
        .flat_map(|list| {
            list.iter()
                .enumerate()
                .map(|(pos, id)| (id.clone(), pos + 1))
        })
        .collect();
    let scores = search_vector::fusion::score_postings(postings, RRF_K);
    let mut fused: Vec<(String, f64)> = scores.into_iter().collect();
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    fused.into_iter().take(k).map(|(id, _)| id).collect()
}

#[cfg(test)]
#[path = "../../tests/lemma_ablation.rs"]
mod tests;
