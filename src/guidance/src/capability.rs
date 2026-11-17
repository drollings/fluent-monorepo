//! Capability corpus types, calibration, and SVO normalization.
//!
//! The corpus is an external input (`capabilities_dir/corpus.json`):
//! `sync` imports it through `read::doc_cards`, which resolves each
//! card to a live doc section and verifies the section hash. Nothing
//! here generates briefs; there is no model call on the import path.
//! Calibration measures the rank path over the corpus `cases`; import
//! works without them.

//! SVO normalization for a stored brief: predicate plus role lemmas
/// through the shared routing helpers. No pipeline means empty SVO —
/// the brief keeps its text and recall falls back to FTS.
#[must_use]
/// SVO normalization for a stored brief: predicate plus role lemmas
/// through the shared routing helpers. No pipeline means empty SVO —
/// the brief keeps its text and recall falls back to FTS.
pub fn brief_svo(
    purpose: &str,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> (String, Vec<String>) {
    let Some(nlp) = nlp else {
        return (String::new(), Vec::new());
    };
    let Ok(doc) = nlp.process_sync(purpose, None) else {
        return (String::new(), Vec::new());
    };
    let signals =
        crate::graph_index::signals_from_routing(&spacy_rs::extract_routing_signals(&doc));
    let mut roles = signals.roles;
    roles.sort();
    roles.dedup();
    (
        signals.predicate.into_iter().next().unwrap_or_default(),
        roles,
    )
}

/// Decision-point name for the capability judge.
pub const CAPABILITY_POINT: &str = "dp.capability";

/// First-measurement bars, recorded from the checked-in golden run:
/// SVO-rank recall@1 over the golden set with zero control fires.
pub const MIN_CAPABILITY_RECALL_AT_1: f64 = 1.0;

/// One external capability card: a human-authored purpose plus the
/// live doc section it anchors to and the code scopes it points at.
/// SVO is harness-computed at measure time, exactly like the ingest
/// path — never stored. `CapabilityCorpusBrief` stays as an alias so
/// existing readers do not fork the shape.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One external capability card: a human-authored purpose plus the
/// live doc section it anchors to and the code scopes it points at.
pub struct CapabilityCard {
    /// Stable chunk id (`source_path#anchor`).
    pub id: String,
    /// Card title (empty when the corpus omits it).
    pub title: String,
    /// Human-authored purpose brief.
    pub purpose: String,
    /// Owning crate from the path decomposition.
    pub crate_name: String,
    /// Doc domain from the path decomposition.
    pub domain: String,
    /// Workspace-relative doc path.
    pub source_path: String,
    /// Section anchor within the doc.
    pub anchor: String,
    /// Corpus keywords (empty when omitted).
    pub keywords: Vec<String>,
    /// Workspace-relative code paths the card points at.
    pub code_paths: Vec<String>,
    /// Symbols the card points at.
    pub symbols: Vec<String>,
    /// Blake3 of the live section at authoring time (absent means the
    /// import trusts the live bytes and records their hash).
    pub content_hash: Option<String>,
}

/// Compatibility alias: one name for the card, everywhere.
pub type CapabilityCorpusBrief = CapabilityCard;

/// One golden capability case: a query plus the brief ids that answer
/// it. Empty `relevant` with `control` marks out-of-corpus queries
/// (must never promote).
#[derive(Debug, Clone, PartialEq, Eq)]
/// One golden capability case: a query plus the brief ids that answer
/// it. Empty `relevant` with `control` marks out-of-corpus queries
/// (must never promote).
pub struct CapabilityGoldCase {
    /// Natural-language orientation query.
    pub query: String,
    /// Brief ids that answer it (empty for controls).
    pub relevant: Vec<String>,
    /// Out-of-corpus control: a confident-looking rank-1 fires it.
    pub control: bool,
}

/// Golden capability corpus: the card universe plus optional
/// relevance labels. `cases` is optional for import and required for
/// calibration: cards without labels stay `considered` and never feed
/// synthesis or traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Golden capability corpus: the card universe plus optional
/// relevance labels.
pub struct CapabilityCorpus {
    /// Every card the harness may rank.
    pub briefs: Vec<CapabilityCorpusBrief>,
    /// Relevance labels, controls included (`None` = unlabelled import).
    pub cases: Option<Vec<CapabilityGoldCase>>,
}

/// Required brief keys; everything else must be a known optional.
const BRIEF_REQUIRED_KEYS: &[&str] = &["id", "purpose", "crate", "domain", "source_path", "anchor"];

/// Known optional brief keys (`title`, `keywords`, `code_paths`,
/// `symbols`, `content_hash`).
const BRIEF_OPTIONAL_KEYS: &[&str] =
    &["title", "keywords", "code_paths", "symbols", "content_hash"];

/// Load an external capability corpus (`capabilities_dir/corpus.json`
/// shape): card rows plus optional query/relevance cases. Unknown or
/// malformed fields fail loudly — the importer must never guess.
pub fn load_capability_corpus(path: &std::path::Path) -> Result<CapabilityCorpus, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("corpus read {}: {error}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| format!("corpus parse: {error}"))?;
    let top = value
        .as_object()
        .ok_or_else(|| "corpus is not an object".to_string())?;
    for key in top.keys() {
        if key != "briefs" && key != "cases" {
            return Err(format!("corpus has unknown field {key:?}"));
        }
    }
    let read_strings = |entry: &serde_json::Value, key: &str| -> Result<Vec<String>, String> {
        let Some(list) = entry.get(key) else {
            return Ok(Vec::new());
        };
        let items = list
            .as_array()
            .ok_or_else(|| format!("corpus brief has a non-array {key}"))?;
        items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("corpus brief has a non-string {key} entry"))
            })
            .collect()
    };
    let read_briefs = || -> Result<Vec<CapabilityCorpusBrief>, String> {
        let entries = value
            .get("briefs")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "corpus has no briefs array".to_string())?;
        let mut briefs = Vec::with_capacity(entries.len());
        for entry in entries {
            let fields = entry
                .as_object()
                .ok_or_else(|| "corpus brief is not an object".to_string())?;
            for key in fields.keys() {
                if !BRIEF_REQUIRED_KEYS.contains(&key.as_str())
                    && !BRIEF_OPTIONAL_KEYS.contains(&key.as_str())
                {
                    return Err(format!("corpus brief has unknown field {key:?}"));
                }
            }
            let string = |key: &str| {
                entry
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| format!("corpus brief lacks {key}"))
            };
            briefs.push(CapabilityCorpusBrief {
                id: string("id")?,
                title: entry
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                purpose: string("purpose")?,
                crate_name: string("crate")?,
                domain: string("domain")?,
                source_path: string("source_path")?,
                anchor: string("anchor")?,
                keywords: read_strings(entry, "keywords")?,
                code_paths: read_strings(entry, "code_paths")?,
                symbols: read_strings(entry, "symbols")?,
                content_hash: entry
                    .get("content_hash")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            });
        }
        Ok(briefs)
    };
    let read_cases = || -> Result<Option<Vec<CapabilityGoldCase>>, String> {
        let Some(entries) = value.get("cases") else {
            return Ok(None);
        };
        let entries = entries
            .as_array()
            .ok_or_else(|| "corpus cases is not an array".to_string())?;
        let mut cases = Vec::with_capacity(entries.len());
        for entry in entries {
            if let Some(fields) = entry.as_object() {
                for key in fields.keys() {
                    if key != "query" && key != "relevant" && key != "control" {
                        return Err(format!("corpus case has unknown field {key:?}"));
                    }
                }
            }
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
            cases.push(CapabilityGoldCase {
                query: query.to_string(),
                relevant: ids,
                control: entry
                    .get("control")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            });
        }
        Ok(Some(cases))
    };
    let briefs = read_briefs()?;
    if briefs.is_empty() {
        return Err("corpus has no briefs".to_string());
    }
    let mut seen = std::collections::HashSet::new();
    for brief in &briefs {
        if !seen.insert(brief.id.clone()) {
            return Err(format!("corpus has a duplicate brief id {:?}", brief.id));
        }
    }
    Ok(CapabilityCorpus {
        briefs,
        cases: read_cases()?,
    })
}

/// Outcome of one capability calibration run: SVO-rank recall@1 over
/// the golden set plus out-of-corpus control fires. The entry stores
/// only when the bars pass — and even then only by an explicit human
/// promotion (the command below always reports HELD).
#[derive(Debug, Clone, PartialEq)]
/// Outcome of one capability calibration run: SVO-rank recall@1 over
/// the golden set plus out-of-corpus control fires. The entry stores
/// only when the bars pass — and even then only by an explicit human
/// promotion (the command below always reports HELD).
pub struct CapabilityCalibration {
    /// Share of golden queries whose rank-1 brief is relevant.
    pub recall_at_1: f64,
    /// Golden (non-control) case count.
    pub support: usize,
    /// Controls whose rank-1 carries nonzero overlap.
    pub control_fires: usize,
    /// Control case count.
    pub control_total: usize,
    /// Bars cleared (recall bar plus zero control fires).
    pub passed: bool,
    /// blake3 of the corpus file (rotation detection).
    pub dataset_hash: String,
}

/// Calibrate the capability point: index the corpus briefs in a
/// throwaway in-memory index (same rows, SVO, and lemma postings the
/// ingest pass persists), then funnel each golden query through the
/// full branch-1 path (SVO overlap plus FTS/lemma recall, ranked
/// never filtered) and score rank-1 against relevance. Stores
/// nothing; the command reports the verdict and a human promotes
/// explicitly.
pub fn calibrate_capability_corpus(
    dataset: &std::path::Path,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> Result<CapabilityCalibration, String> {
    use crate::query::capability_match::{funneled_query_signals, rank_briefs, BriefCandidate};
    use crate::query::funnel::funnel_terms;
    use crate::query::search_db::{CapabilityLemma, CapabilityRecord, CAPABILITY_SCHEMA_VERSION};
    let corpus = load_capability_corpus(dataset)?;
    let Some(cases) = &corpus.cases else {
        return Err("corpus has no cases: labels are required for calibration".to_string());
    };
    if cases.is_empty() {
        return Err("corpus has no cases: labels are required for calibration".to_string());
    }
    let aliases = crate::query::aliases::SemanticAliases::new();
    // Throwaway index over the corpus briefs: same rows, SVO, and
    // lemma postings the ingest pass persists, so the harness
    // measures the production branch-1 path end to end.
    let db = crate::query::search_db::GuidanceSearchDb::open_in_memory()
        .map_err(|error| format!("calibration index: {error}"))?;
    let mut rows = Vec::with_capacity(corpus.briefs.len());
    let mut postings = Vec::new();
    for brief in &corpus.briefs {
        let (predicate, roles) = brief_svo(&brief.purpose, nlp);
        for lemma in crate::query::ingest::query_lemmas(nlp, &brief.purpose) {
            postings.push(CapabilityLemma {
                capability_id: brief.id.clone(),
                lemma,
                confidence: 1.0,
            });
        }
        rows.push(CapabilityRecord {
            id: brief.id.clone(),
            project: "calibration".to_string(),
            crate_name: brief.crate_name.clone(),
            domain: brief.domain.clone(),
            title: brief.title.clone(),
            purpose: brief.purpose.clone(),
            predicate,
            roles_json: serde_json::to_string(&roles).unwrap_or_else(|_| "[]".to_string()),
            keywords_json: serde_json::to_string(&brief.keywords)
                .unwrap_or_else(|_| "[]".to_string()),
            code_paths_json: serde_json::to_string(&brief.code_paths)
                .unwrap_or_else(|_| "[]".to_string()),
            symbols_json: serde_json::to_string(&brief.symbols)
                .unwrap_or_else(|_| "[]".to_string()),
            source_path: brief.source_path.clone(),
            anchor: brief.anchor.clone(),
            content_hash: common_core::hash::blake3_hex(brief.purpose.as_bytes()),
            model: "calibration".to_string(),
            prompt_version: CAPABILITY_SCHEMA_VERSION,
            origin: "calibration".to_string(),
            corpus_hash: String::new(),
            status: "ready".to_string(),
            gap_reason: None,
            generated_at: 0,
        });
    }
    db.replace_capabilities(&rows)
        .map_err(|error| format!("calibration index: {error}"))?;
    db.insert_capability_lemmas(&postings)
        .map_err(|error| format!("calibration index: {error}"))?;
    let mut hits = 0usize;
    let mut support = 0usize;
    let mut control_fires = 0usize;
    let mut control_total = 0usize;
    for case in cases {
        let funnel = funnel_terms(&case.query, &aliases, None);
        let signals = funneled_query_signals(&funnel, nlp);
        let expanded = funnel.expanded.join(" ");
        let mut fts_scores: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        if let Ok(found) = db.search_capabilities(&expanded, 20) {
            for hit in found {
                fts_scores.insert(hit.id, hit.score);
            }
        }
        let mut ids: std::collections::HashSet<String> = fts_scores.keys().cloned().collect();
        let lemmas = crate::query::ingest::query_lemmas(nlp, &expanded);
        if !lemmas.is_empty() {
            if let Ok(lemma_ids) = db.capabilities_for_lemmas(&lemmas, 20) {
                ids.extend(lemma_ids);
            }
        }
        let mut ordered: Vec<String> = ids.into_iter().collect();
        ordered.sort();
        let stored = db.capabilities_for_ids(&ordered).unwrap_or_default();
        let candidates: Vec<BriefCandidate> = stored
            .into_iter()
            .map(|row| {
                let roles: Vec<String> = serde_json::from_str(&row.roles_json).unwrap_or_default();
                BriefCandidate {
                    id: row.id.clone(),
                    purpose: row.purpose,
                    predicate: row.predicate,
                    roles,
                    crate_name: row.crate_name,
                    domain: row.domain,
                    source_path: row.source_path,
                    anchor: row.anchor,
                    fts_score: fts_scores.remove(&row.id),
                }
            })
            .collect();
        let ranked = rank_briefs(&signals, &candidates);
        if case.control {
            // Out-of-corpus controls count even with no candidates:
            // silence is the correct answer, and only a confident
            // rank-1 fires.
            control_total += 1;
            if ranked.first().is_some_and(|top| !top.overlap.is_empty()) {
                control_fires += 1;
            }
            continue;
        }
        let Some(top) = ranked.first() else {
            continue;
        };
        support += 1;
        if case.relevant.iter().any(|id| id == &top.id) {
            hits += 1;
        }
    }
    let recall_at_1 = hits as f64 / support.max(1) as f64;
    let passed = recall_at_1 >= MIN_CAPABILITY_RECALL_AT_1 && control_fires == 0;
    let dataset_hash = common_core::hash::blake3_hex(
        &std::fs::read(dataset)
            .map_err(|error| format!("corpus read {}: {error}", dataset.display()))?,
    );
    Ok(CapabilityCalibration {
        recall_at_1,
        support,
        control_fires,
        control_total,
        passed,
        dataset_hash,
    })
}

#[cfg(test)]
#[path = "../tests/query_capability.rs"]
mod tests;
