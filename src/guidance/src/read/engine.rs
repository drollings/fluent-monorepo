//! The read engine: intent-routed, budget-capped answer assembly.
//!
//! `ReadEngine` is the single DB-backed read path: intent
//! classification, per-intent budgets, planes, and section assembly
//! with token accounting. Nothing here calls a model.

use crate::query::strategy::QueryIntent;
use crate::read::planes::symbol::StructureStore as _;

/// Which producer surfaced a finding. `database` is FTS/vector/lemma
/// recall, `ast` is symbol/edge extraction, `ripgrep` is the
/// exhaustive L0 arm, and `cards` is project/capability/doc metadata.
/// Every producer emits the same `Finding` through the same filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingProducer {
    Cards,
    Database,
    Ast,
    Ripgrep,
}

/// What a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    Project,
    Capability,
    Doc,
    Symbol,
    Edge,
    Code,
}

/// One candidate from any producer: the model judges findings, it
/// never selects retrieval, so every producer funnels through this
/// one shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// Stable finding id.
    pub id: String,
    /// Which producer surfaced it.
    pub producer: FindingProducer,
    /// What it is.
    pub kind: FindingKind,
    /// Source identity (workspace, root, absolute paths).
    pub source: crate::source_ref::SourceRef,
    /// 1-based start line, when the source is a file.
    pub start_line: Option<u32>,
    /// 1-based end line, when the source is a file.
    pub end_line: Option<u32>,
    /// Finding text (excerpt or card body).
    pub text: String,
    /// Producer score (rank only, never a relevance gate).
    pub producer_score: f64,
    /// Provenance line (`via capability: x`, plane name, …).
    pub provenance: String,
}

impl Finding {
    /// Owning Cargo package via the gate table (no second resolver).
    #[must_use]
    pub fn crate_name(&self) -> String {
        crate::component_map::component_gate_for_path(self.source.display()).package
    }

    /// Owning component via the gate table (`None` outside the matrix).
    #[must_use]
    pub fn domain(&self) -> Option<String> {
        crate::component_map::component_gate_for_path(self.source.display()).component
    }
}

/// One rendered answer section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadSection {
    /// Section title (`From the project map`, `Top matches`, …).
    pub title: String,
    /// Markdown body, budget-truncated at assembly.
    pub body: String,
}

/// One read request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    /// Natural-language question, run verbatim.
    pub query: String,
    /// Section budget override (tokens); per-intent table otherwise.
    pub budget_override: Option<u64>,
}

/// One explain request: evidence assembly over one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainRequest {
    /// Natural-language question, run verbatim.
    pub query: String,
    /// Code finding limit.
    pub limit: usize,
    /// Section budget override (tokens); per-intent table otherwise.
    pub budget_override: Option<u64>,
    /// Extractive evidence only: no synthesis call on any path.
    pub no_llm: bool,
    /// Include fixture-scope rows (default excludes them).
    pub include_fixtures: bool,
    /// Answer assembly policy: extractive, evidence-only, or legacy
    /// prose. `Auto` is the shipped default.
    pub answer_mode: AnswerMode,
}

/// Answer assembly policy: extractive for factual intents,
/// evidence-only, or the legacy generative prose path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnswerMode {
    /// Factual intents render top admitted rows verbatim;
    /// explanatory intents with enough admitted docs synthesize;
    /// otherwise evidence-only.
    #[default]
    Auto,
    /// Never synthesize: evidence sections only.
    Evidence,
    /// Legacy prose path: synthesize over admitted evidence exactly
    /// like before (escape hatch and regression comparator).
    Synthesis,
}

/// Retrieval context for one explain call (borrowed, no clones).
pub struct ExplainContext<'a> {
    /// Workspace root (live reads resolve under it).
    pub workspace: &'a std::path::Path,
    /// Committed index.
    pub db: &'a crate::query::search_db::GuidanceSearchDb,
    /// Lemma pipeline, when built.
    pub nlp: Option<&'a spacy_rs::pipeline::NlpPipeline>,
    /// Coverage snapshot for the Coverage section and envelope.
    pub coverage: Option<crate::read::readiness::Coverage>,
}

/// Model synthesis seam: the engine builds the evidence prompt, the
/// caller (usually the `code` role client) synthesizes. Core stays
/// model-free; synthesis errors degrade to a labelled line, never a
/// hard error.
pub trait Synthesizer {
    /// Synthesize an answer over the evidence prompt. `None` means
    /// the model declined (empty content); `Err` is transport noise.
    fn synthesize(&self, prompt: &str) -> Result<Option<String>, String>;

    /// Model identity for manifest cache keys. Ad-hoc synthesizers
    /// stay unkeyed; the shipping client reports its role model.
    fn model_identity(&self) -> &str {
        ""
    }
}

/// Judge input for shadow labelling: backend plus fingerprint. `None`
/// fingerprint means shadow-only (M7 authorizes filtering).
pub struct JudgeCtx<'a> {
    /// Decision backend (SystemOne).
    pub backend: &'a dyn fluent_llm::decision::DecideBackend,
    /// Model name for the backend.
    pub model: String,
    /// Timeout per question.
    pub timeout: std::time::Duration,
    /// Fingerprint authorizing filtering (absent is shadow-only).
    pub fingerprint: Option<String>,
}

/// Bounded ripgrep arm caps: file count and excerpt chars. Cost caps,
/// never relevance thresholds — truncation reports a gap.
pub const RG_FILE_CAP: usize = 10;
/// Max chars per ripgrep excerpt (cost cap, not relevance).
pub const RG_EXCERPT_CHAR_CAP: usize = 600;

/// Fingerprint-scoped judge cache: the single `DecisionCache`, no new
/// cache. Cold until a point authorizes; shadow never stores.
static FINDING_CACHE: std::sync::OnceLock<fluent_llm::decision::DecisionCache> =
    std::sync::OnceLock::new();

fn finding_cache() -> &'static fluent_llm::decision::DecisionCache {
    FINDING_CACHE.get_or_init(|| {
        fluent_llm::decision::DecisionCache::try_new(256).expect("judge cache capacity")
    })
}

/// The execution plan for one request: intent plus the producers and
/// budgets each plane gets. Pure and testable without any retrieval.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ReadPlan {
    /// Routed intent (after measured demotion — the safe route).
    pub intent: QueryIntent,
    /// Table prediction before demotion (`None` when the route stood).
    pub demoted_from: Option<QueryIntent>,
    /// Primary plane route.
    pub route: String,
    /// Per-section token allocation.
    pub budgets: crate::read::budget::SectionBudgets,
    /// Whether symbol extraction runs (Symbol/Graph only).
    pub extract_symbols: bool,
    /// Whether the bounded ripgrep arm runs (exact/symbol/graph/path
    /// only; broad language stays index-only).
    pub ripgrep: bool,
    /// Soft crate scope named by the query (≤2 owning packages;
    /// empty reads unscoped). Render-side budget split only: recall
    /// never constrains on this, so cross-crate answers keep full
    /// recall.
    pub scope_crates: Vec<String>,
    /// True when graph edges resolved through scope-owned
    /// definitions. Declared in Coverage beside the scope.
    pub scope_narrowed: bool,
    /// Render the whole project map even when the query names a scope.
    pub full_map: bool,
}

/// Primary plane route for one intent.
#[must_use]
pub fn route_for(intent: QueryIntent) -> String {
    match intent {
        QueryIntent::SingleIdentifier => "symbol",
        QueryIntent::Graph => "graph",
        QueryIntent::Ownership => "ownership",
        QueryIntent::Orientation => "orientation",
        QueryIntent::HowTo
        | QueryIntent::Conceptual
        | QueryIntent::FilePath
        | QueryIntent::MultiKeyword
        | QueryIntent::GeneralSearch => "code",
        QueryIntent::Provenance | QueryIntent::CapabilityQuery => "cards",
    }
    .to_string()
}

/// One per-candidate trace row: how a doc or code candidate fared
/// through anchor, judge, admission, and budget. Deterministic data,
/// collected on every run and rendered only under `--trace` — the
/// trace mechanism never triggers a model call itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceRow {
    /// Which plane produced the candidate (`docs`, `code`).
    pub plane: String,
    /// Candidate identity (doc key, finding id, or `(anchor-filtered)`).
    pub candidate: String,
    /// Anchor decision affecting the candidate.
    pub anchor: String,
    /// Judge label, `unjudged` when no verdict ran, `-` when no
    /// verdict could run.
    pub judge: String,
    /// Admission/budget outcome (`rendered`, `extractive`,
    /// `dropped:<label>`, `budget-cap`, `excluded:<n>`).
    pub outcome: String,
}

/// One read answer: intent, budgeted sections, and coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadAnswer {
    /// The asked query.
    pub query: String,
    /// Routed intent.
    pub intent: QueryIntent,
    /// Primary plane route (`symbol`, `graph`, `ownership`,
    /// `orientation`, `code`, `cards`).
    pub route: String,
    /// Rendered sections in roadmap order.
    pub sections: Vec<ReadSection>,
    /// Coverage snapshot when the caller supplies one.
    pub coverage: Option<crate::read::readiness::Coverage>,
    /// Claims extracted from the synthesized answer (empty when
    /// evidence-only).
    pub claims: Vec<crate::read::claims::Claim>,
    /// Hash-verifiable citations with source identity and status.
    pub citations: Vec<crate::source_ref::Citation>,
    /// Honest gaps (empty planes, unverified sections, no support).
    pub gaps: Vec<String>,
    /// True when any plane degraded (gaps rendered).
    pub degraded: bool,
    /// Estimated tokens across sections.
    pub tokens_used: u64,
    /// Query subject for the envelope: crate anchor, free-term
    /// shadow, or none. Free terms never filter before calibration.
    pub subject: Option<crate::read::vocabulary::SubjectAnchor>,
    /// True when the `Answer` came from the manifest cache. A hit is
    /// reproducibility, never correctness: the support verdict rides
    /// along, and the envelope names the hit.
    pub cache_hit: bool,
    /// Per-candidate trace rows (anchor, judge, admission/budget).
    /// Collected on every run; rendered only under `--trace`.
    pub trace: Vec<TraceRow>,
}

/// The read engine. Planes are not yet implemented: `plan` resolves
/// intent and budgets, `answer` assembles (currently empty) sections
/// with the caller's coverage.
pub struct ReadEngine {
    full_map: bool,
    generation: String,
}

impl ReadEngine {
    /// Build an engine. Configuration rides the project config
    /// Construction is cheap; callers own the workspace and index.
    #[must_use]
    pub fn new() -> Self {
        Self {
            full_map: false,
            generation: String::new(),
        }
    }

    /// Key answers to one index generation: a changed marker misses
    /// (and drops) every cached row computed over the old one.
    #[must_use]
    pub fn with_generation(mut self, generation: impl Into<String>) -> Self {
        self.generation = generation.into();
        self
    }

    /// Render the whole project map even when the query names a scope.
    #[must_use]
    pub fn with_full_map(mut self) -> Self {
        self.full_map = true;
        self
    }

    /// Resolve the execution plan: rule-table intent, measured
    /// demotion for below-bar classes, per-intent budgets, and
    /// symbol-extraction scope. Demotion fails safe to the `General`
    /// code route with `demoted_from` set so callers log a gap.
    #[must_use]
    pub fn plan(&self, request: &ReadRequest) -> ReadPlan {
        let predicted = crate::read::intent::classify(&request.query);
        // Class precision labels a weak route; it never deletes the
        // deterministic plane (a measured task-value bar is not a router).
        let intent = predicted;
        let mut budgets = crate::read::budget::budgets_for(intent);
        if let Some(total) = request.budget_override {
            budgets.total = total;
        }
        ReadPlan {
            route: route_for(intent),
            demoted_from: if crate::read::calibration::WEAK_INTENT_CLASSES.contains(&intent) {
                Some(intent)
            } else {
                None
            },
            intent,
            budgets,
            extract_symbols: crate::read::intent::extracts_symbols(intent),
            ripgrep: matches!(
                intent,
                QueryIntent::SingleIdentifier | QueryIntent::FilePath | QueryIntent::Graph
            ),
            scope_crates: crate::component_map::scope_packages_for_query(&request.query),
            scope_narrowed: false,
            full_map: self.full_map,
        }
    }

    /// Assemble the answer skeleton: empty sections with the caller's
    /// coverage and token accounting.
    #[must_use]
    pub fn answer(
        &self,
        request: &ReadRequest,
        coverage: Option<crate::read::readiness::Coverage>,
    ) -> ReadAnswer {
        let plan = self.plan(request);
        let mut tokens_used = 0u64;
        let mut sections = Vec::new();
        for title in [
            "From the project map",
            "From the docs",
            "From the capabilities",
            "Top matches",
            "Citations",
            "Gaps",
        ] {
            // Skeleton sections carry their title with no content: the
            // planes fill them under per-section budgets later.
            let body = format!("### {title}\n");
            tokens_used += crate::read::budget::tokens_for(&body);
            sections.push(ReadSection {
                title: title.to_string(),
                body,
            });
        }
        let gaps = match plan.demoted_from {
            Some(predicted) => crate::read::calibration::intent_coverage_gap(
                predicted,
                crate::read::calibration::WEAK_INTENT_CLASSES,
            )
            .into_iter()
            .collect(),
            None => Vec::new(),
        };
        let degraded = !gaps.is_empty();
        ReadAnswer {
            query: request.query.clone(),
            intent: plan.intent,
            route: plan.route.clone(),
            sections,
            coverage,
            claims: Vec::new(),
            citations: Vec::new(),
            gaps,
            degraded,
            tokens_used,
            subject: None,
            cache_hit: false,
            trace: Vec::new(),
        }
    }

    /// Assemble the full explain answer: coverage, project cards,
    /// docs, capabilities, code findings, citations, and gaps, plus a
    /// support-gated synthesis when a synthesizer is provided.
    /// Model-free except for the injected synthesizer (`None` means
    /// evidence-only). Judges run unjudged here; verdicts land with
    /// calibration.
    #[must_use]
    pub fn explain(
        &self,
        request: &ExplainRequest,
        ctx: &ExplainContext<'_>,
        synthesize: Option<&dyn Synthesizer>,
    ) -> ReadAnswer {
        self.explain_with_judge(request, ctx, synthesize, None)
    }

    /// Explain with an optional shadow judge. `None` judge is
    /// byte-identical to [`Self::explain`]; `Some` adds shadow labels
    /// without filtering and a gap when the judge is unavailable.
    #[must_use]
    pub fn explain_with_judge(
        &self,
        request: &ExplainRequest,
        ctx: &ExplainContext<'_>,
        synthesize: Option<&dyn Synthesizer>,
        judge: Option<&JudgeCtx<'_>>,
    ) -> ReadAnswer {
        let _read = crate::profile::Phase::new("read.explain");
        use crate::read::budget::tokens_for;
        let plan_request = ReadRequest {
            query: request.query.clone(),
            budget_override: request.budget_override,
        };
        let mut plan = self.plan(&plan_request);
        let mut sections = Vec::new();
        let mut tokens_used = 0u64;
        let mut push = |title: &str, body: String, tokens_used: &mut u64| {
            *tokens_used += tokens_for(&body);
            sections.push(ReadSection {
                title: title.to_string(),
                body,
            });
        };
        // Symbol/Graph intents resolve through the structure/graph
        // planes; scope narrowing is known before Coverage renders.
        let scope_prefixes = crate::component_map::prefixes_for_scope(&plan.scope_crates);
        let (plane_findings, graph_narrowed) = structure_findings(
            &request.query,
            plan.intent,
            ctx.workspace,
            ctx.db,
            &scope_prefixes,
        );
        plan.scope_narrowed = graph_narrowed;
        push(
            "Coverage",
            coverage_section(ctx.coverage.as_ref(), &plan),
            &mut tokens_used,
        );
        // Query subject: exact crate names filter (M3), free terms
        // stay shadow until measured. A free term matching more than
        // half of the doc cards is not selective and reads as no
        // anchor (fail-open); the trace count lands with `--trace`.
        let subject = subject_anchor_for(&request.query, ctx.db, plan.intent);
        let enforced = subject
            .clone()
            .filter(|anchor| anchor.source == crate::read::vocabulary::AnchorSource::Crate);
        // Anchor decision in trace vocabulary: the exact-crate anchor
        // enforces, a free-term anchor stays shadow, none passes
        // through. One string for every doc row, so the trace needs
        // no second anchor computation.
        let anchor_decision = match (&subject, &enforced) {
            (Some(_), Some(anchor)) => format!("crate:{} enforced", anchor.terms.join(",")),
            (Some(_), None) => "free-term shadow".to_string(),
            (None, _) => "none".to_string(),
        };
        let (mut cards, card_gap) =
            orientation_cards(&plan, &request.query, ctx.db, enforced.as_ref());
        if !plan.full_map && !plan.scope_crates.is_empty() {
            cards = scoped_project_cards(cards, &plan.scope_crates);
        }
        cards.retain(|card| {
            crate::query::rerank::scope_row_admitted(&card.source, request.include_fixtures)
        });
        push(
            "From the project map",
            crate::read::render::render_card_section(
                "From the project map",
                &cards
                    .iter()
                    .map(|card| (card.title.clone(), card.body.clone()))
                    .collect::<Vec<_>>(),
                plan.budgets.project,
            ),
            &mut tokens_used,
        );
        let mut cap_hits = crate::read::planes::docs::retrieve_cards(
            &request.query,
            Some(ctx.workspace),
            ctx.db,
            ctx.nlp,
            None,
        );
        cap_hits.retain(|hit| {
            crate::query::rerank::scope_row_admitted(&hit.source_path, request.include_fixtures)
        });
        let mut doc_hits;
        let anchor_excluded;
        if matches!(
            plan.intent,
            QueryIntent::SingleIdentifier | QueryIntent::FilePath | QueryIntent::Graph
        ) {
            doc_hits = Vec::new();
            anchor_excluded = 0;
        } else {
            // Exact-crate anchors filter here; free terms stay shadow
            // and read unscoped until their calibration.
            let (hits, excluded) = crate::read::planes::docs::retrieve_doc_sections_anchored(
                &request.query,
                Some(ctx.workspace),
                ctx.db,
                // Bounded doc funnel: cost cap, not relevance. Fewer live
                // verifications keep p50/p95 within budget; recall is held
                // by the budget-capped evidence sections.
                (plan.budgets.evidence.max(8) / 32) as usize,
                enforced.as_ref(),
            );
            doc_hits = hits;
            anchor_excluded = excluded;
        }
        doc_hits.retain(|hit| {
            crate::query::rerank::scope_row_admitted(&hit.source_path, request.include_fixtures)
        });
        // Rerank briefs and doc cards (rank-only): judge via the seam,
        // demote by tier, never filter. Admission filters top-k only
        // when its own point calibrates; uncalibrated reads exactly
        // like today. Unavailable keeps prior order.
        let mut admission_drops: Vec<String> = Vec::new();
        // Trace mirrors: per-doc judge labels and admission drops in
        // prior order, so `--trace` needs no second judge run.
        let mut doc_labels: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut dropped_docs: Vec<(String, String)> = Vec::new();
        let admission_registry = judge.as_ref().map(|_| {
            let registry_path = ctx.workspace.join(".guidance/state/calibration.json");
            std::fs::read(&registry_path)
                .ok()
                .and_then(|bytes| {
                    serde_json::from_slice::<fluent_llm::decision::CalibrationRegistry>(&bytes).ok()
                })
                .unwrap_or_else(fluent_llm::decision::CalibrationRegistry::empty)
        });
        if let Some(judge_ctx) = judge.as_ref() {
            let doc_judge = crate::read::decide::DocJudge::new(
                judge_ctx.backend,
                judge_ctx.model.clone(),
                judge_ctx.timeout,
            );
            let cap_inputs: Vec<(String, String)> = cap_hits
                .iter()
                .map(|hit| (hit.id.clone(), hit.purpose.clone()))
                .collect();
            if !cap_inputs.is_empty() {
                let cap_verdicts = doc_judge.judge(&request.query, &cap_inputs);
                let prior: Vec<String> = cap_hits.iter().map(|hit| hit.id.clone()).collect();
                let model = crate::query::rerank::demote_by_verdict(&prior, &cap_verdicts);
                if model != prior {
                    let mut by_id = std::collections::HashMap::new();
                    for hit in cap_hits {
                        by_id.insert(hit.id.clone(), hit);
                    }
                    cap_hits = model.iter().filter_map(|id| by_id.remove(id)).collect();
                    let decisions = crate::query::rerank::verdict_decisions(
                        &cap_verdicts,
                        &judge_ctx.model,
                        crate::query::decision_points::RERANK_VERSION,
                    );
                    let _ = crate::query::shadow::rerank_shadow(&prior, &model, &decisions);
                }
            }
            let doc_inputs: Vec<(String, String)> = doc_hits
                .iter()
                .filter_map(|hit| hit.body.clone().map(|body| (hit.key.clone(), body)))
                .collect();
            if !doc_inputs.is_empty() {
                let doc_verdicts = doc_judge.judge(&request.query, &doc_inputs);
                for verdict in &doc_verdicts {
                    doc_labels.insert(verdict.id.clone(), verdict.label.as_str().to_string());
                }
                let prior: Vec<String> = doc_hits.iter().map(|hit| hit.key.clone()).collect();
                let model = crate::query::rerank::demote_by_verdict(&prior, &doc_verdicts);
                if model != prior {
                    let mut by_key = std::collections::HashMap::new();
                    for hit in doc_hits {
                        by_key.insert(hit.key.clone(), hit);
                    }
                    doc_hits = model.iter().filter_map(|key| by_key.remove(key)).collect();
                    let decisions = crate::query::rerank::verdict_decisions(
                        &doc_verdicts,
                        &judge_ctx.model,
                        crate::query::decision_points::RERANK_VERSION,
                    );
                    let _ = crate::query::shadow::rerank_shadow(&prior, &model, &decisions);
                }
                // Advisory admission: top-2 judged docs only when the
                // admission point calibrates. Every drop names its
                // label; unavailable keeps the prior order.
                if admission_authorized(judge_ctx, admission_registry.as_ref())
                    && !doc_verdicts.is_empty()
                    && crate::query::rerank::rerank_status(&doc_verdicts)
                        == crate::query::rerank::RerankStatus::Ranked
                {
                    let admitted: Vec<String> =
                        crate::query::rerank::admit_top(&prior, &doc_verdicts, 2);
                    if admitted.len() != doc_hits.len() {
                        let labels: std::collections::HashMap<&str, &str> = doc_verdicts
                            .iter()
                            .map(|verdict| (verdict.id.as_str(), verdict.label.as_str()))
                            .collect();
                        for key in &prior {
                            if !admitted.contains(key) {
                                let label = labels.get(key.as_str()).copied().unwrap_or("unjudged");
                                admission_drops.push(format!("admission dropped {key} ({label})"));
                                dropped_docs.push((key.clone(), label.to_string()));
                            }
                        }
                        let mut by_key = std::collections::HashMap::new();
                        for hit in doc_hits {
                            by_key.insert(hit.key.clone(), hit);
                        }
                        doc_hits = admitted
                            .iter()
                            .filter_map(|key| by_key.remove(key))
                            .collect();
                    }
                }
            }
        }
        // Extractive docs for factual intents: consumed rows leave
        // the later sections, so no citation text appears twice. The
        // citation set below still covers the admitted rows. Rows that
        // name no asked subject are never Answer material: they stay
        // in their labelled sections and the contract gap reports the
        // covered index instead.
        // Extractive rows are model-free, so every answer mode renders
        // them; `Synthesizer` remains the only model seam.
        let (mut extractive_rows, used_doc_keys, docs_grounded) = if is_factual_intent(plan.intent)
        {
            let (rows, used) = extractive_doc_rows(&doc_hits);
            let grounded = rows.iter().any(|(_, body)| {
                crate::read::vocabulary::evidence_names_subject(&request.query, body)
            });
            if grounded || rows.is_empty() {
                (rows, used, grounded)
            } else {
                (Vec::new(), std::collections::HashSet::new(), false)
            }
        } else {
            (Vec::new(), std::collections::HashSet::new(), false)
        };
        let cited_doc_hits = doc_hits.clone();
        if !used_doc_keys.is_empty() {
            doc_hits.retain(|hit| !used_doc_keys.contains(&hit.key));
        }
        let doc_entries: Vec<(String, String)> = doc_hits
            .iter()
            .filter_map(|hit| {
                let body = hit.body.clone()?;
                let label = if hit.verified {
                    "doc"
                } else {
                    "unverified doc"
                };
                Some((
                    format!("[{label}] {}#{}", hit.source_path, hit.anchor),
                    body,
                ))
            })
            .collect();
        let cap_entries = crate::read::planes::docs::capability_entries(&cap_hits);
        let mut gaps: Vec<String> = Vec::new();
        if let Some(gap) = card_gap {
            gaps.push(gap);
        }
        gaps.extend(admission_drops);
        if let Some(predicted) = plan.demoted_from {
            if let Some(gap) = crate::read::calibration::intent_coverage_gap(
                predicted,
                crate::read::calibration::WEAK_INTENT_CLASSES,
            ) {
                gaps.push(gap);
            }
        }
        let half_evidence = plan.budgets.evidence / 2;
        push(
            "From the docs",
            crate::read::render::render_evidence_section(
                "From the docs",
                &doc_entries,
                half_evidence,
            ),
            &mut tokens_used,
        );
        if cap_entries.is_empty() {
            gaps.push(
                "capability corpus unpopulated: no briefs indexed; run the capability import to populate".to_string(),
            );
        } else {
            push(
                "From the capabilities",
                crate::read::render::render_evidence_section(
                    "From the capabilities",
                    &cap_entries,
                    plan.budgets.evidence.saturating_sub(half_evidence),
                ),
                &mut tokens_used,
            );
        }
        // A drifted doc still renders with an `[unverified]` label; it
        // does not by itself mark the whole answer degraded.
        let code_plan = crate::read::planes::code::code_plan(
            &request.query,
            request.limit,
            plan.extract_symbols,
        );
        let storage = crate::query::db_storage::GuidanceDbStorage::new(ctx.db);
        let code_hits = crate::query::recall::run_recall(&code_plan, &storage, None, ctx.nlp)
            .map(|output| output.hits)
            .unwrap_or_default();
        let mut code_findings = crate::read::planes::code::search_hits_to_findings(&code_hits);
        // Bounded doc-scoped recall: one extra recall restricted to the
        // top doc's code scopes. Cost cap, not relevance.
        if matches!(
            plan.intent,
            QueryIntent::Ownership
                | QueryIntent::HowTo
                | QueryIntent::Conceptual
                | QueryIntent::Provenance
        ) {
            let mut scopes: Vec<String> = Vec::new();
            for hit in doc_hits.iter().take(2) {
                scopes.extend(hit.code_paths.iter().cloned());
                if scopes.len() >= 4 {
                    break;
                }
            }
            scopes.sort();
            scopes.dedup();
            if !scopes.is_empty() {
                let mut scoped = crate::read::planes::code::code_plan(
                    &request.query,
                    request.limit,
                    plan.extract_symbols,
                );
                scoped.include_paths = Some(scopes);
                if let Ok(output) =
                    crate::query::recall::run_recall(&scoped, &storage, None, ctx.nlp)
                {
                    for mut finding in
                        crate::read::planes::code::search_hits_to_findings(&output.hits)
                    {
                        finding.provenance = "via doc".to_string();
                        code_findings.push(finding);
                    }
                }
            }
        }
        // Symbol/Graph findings resolved above; exact sites head Top
        // matches, code recall follows. Unknown symbols read as honest
        // gaps. Bounded ripgrep arm: exact/symbol/graph/path intents
        // only when the index yields no product code, so a stale index
        // cannot hide a literal. Broad language stays index-only. Cost
        // caps, never silent truncation.
        let mut rg_gaps: Vec<String> = Vec::new();
        let mut rg_findings: Vec<crate::read::engine::Finding> = Vec::new();
        let index_empty = plane_findings.is_empty() && code_findings.is_empty();
        if plan.ripgrep && index_empty {
            if let Some(text) = rg_query_text(plan.intent, &request.query) {
                let (hits, gap) = run_rg_findings(ctx.workspace, &text);
                if let Some(gap) = gap {
                    rg_gaps.push(gap);
                }
                rg_findings = hits;
            }
        }
        let mut evidence = crate::read::evidence::EvidenceSet::new();
        evidence.extend(plane_findings);
        evidence.extend(code_findings);
        evidence.extend(rg_findings);
        evidence.dedupe();
        let mut code_findings = evidence.into_findings();
        if !request.include_fixtures {
            code_findings = crate::query::rerank::exclude_fixtures(code_findings, false);
        }
        // Scope hypothesis (shadow-only): funnel terms first, decision
        // role judges the pinned crate choice via the ladder; the
        // verdict adjusts rank priors only, never filters. Unavailable
        // falls back to funnel terms, labelled.
        let funnel = crate::query::funnel::funnel_terms(
            &request.query,
            &crate::query::aliases::SemanticAliases::new(),
            None,
        );
        let mut scope_crates: Vec<String> = code_findings
            .iter()
            .map(crate::read::engine::Finding::crate_name)
            .filter(|name| !name.is_empty())
            .collect();
        scope_crates.sort();
        scope_crates.dedup();
        scope_crates.truncate(10);
        let scope_hypothesis: Option<String> = match judge.as_ref() {
            Some(judge_ctx) => {
                let spec = crate::query::decision_points::scope_spec(&scope_crates);
                let state = crate::query::decision_points::scope_state(
                    &request.query,
                    &funnel.significant,
                    &scope_crates,
                );
                let question = fluent_llm::decision::Question {
                    spec: &spec,
                    state: &state,
                    question: &request.query,
                };
                let prompt = question.render();
                let options: Vec<fluent_llm::decision::DecisionOption> = spec
                    .options()
                    .into_iter()
                    .map(|label| fluent_llm::decision::DecisionOption {
                        label,
                        description: String::new(),
                    })
                    .collect();
                let request_decide = fluent_llm::decision::DecideRequest {
                    prompt,
                    state,
                    question: request.query.clone(),
                    options,
                    kind: fluent_llm::decision::DecisionKind::Choice,
                    version: crate::query::decision_points::SCOPE_VERSION,
                    timeout: judge_ctx.timeout,
                    model: judge_ctx.model.clone(),
                };
                let fallback = fluent_llm::decision::Decision::from_distribution(
                    "unknown",
                    vec![("unknown".to_string(), 1.0)],
                    judge_ctx.model.clone(),
                    crate::query::decision_points::SCOPE_VERSION,
                    0,
                    false,
                )
                .expect("fallback");
                let key = common_core::hash::blake3_hex(
                    format!("scope|{}|{}", request.query, scope_crates.join(",")).as_bytes(),
                );
                let breaker =
                    fluent_llm::decision::DecisionBreaker::new(3, 60_000).expect("breaker");
                let backend = judge_ctx.backend;
                let outcome =
                    fluent_llm::client::block_on(fluent_llm::decision::decide_with_ladder(
                        None,
                        finding_cache(),
                        key,
                        false,
                        &breaker,
                        || async { backend.decide(request_decide).await },
                        fallback,
                    ));
                match outcome {
                    Ok(decision) if scope_crates.contains(&decision.label) => {
                        let _ = crate::query::shadow::scope_shadow(
                            Some(&decision),
                            &funnel.significant,
                        );
                        Some(decision.label)
                    }
                    Ok(_) | Err(_) => None,
                }
            }
            None => None,
        };
        if let Some(want) = scope_hypothesis.as_deref() {
            code_findings.sort_by(|a, b| {
                let a_hit = a.crate_name() == want;
                let b_hit = b.crate_name() == want;
                b_hit.cmp(&a_hit).then_with(|| {
                    b.producer_score
                        .partial_cmp(&a.producer_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            });
        }
        gaps.extend(rg_gaps);
        // Negative-evidence floor: exact symbol/edge, product code, or a
        // verified doc/card satisfies it; project cards satisfy it only for
        // Orientation (they are context elsewhere). Otherwise suppress with
        // an honest gap. Lexical scores alone never satisfy it.
        let floor_met = !code_findings.is_empty()
            || cited_doc_hits.iter().any(|hit| hit.verified)
            || cap_hits.iter().any(|hit| hit.verified)
            || (plan.intent == QueryIntent::Orientation && !cards.is_empty());
        if !floor_met {
            push_covered_index_contract(&mut gaps, &plan, request.include_fixtures);
        }
        // Stored identity grounds the answer even when live bytes moved:
        // a renamed symbol resolves to its new text, marked stale.
        let stored_grounded: std::collections::HashSet<String> = code_findings
            .iter()
            .filter(|finding| {
                crate::read::vocabulary::evidence_names_subject(&request.query, &finding.text)
            })
            .map(|finding| finding.id.clone())
            .collect();
        // Live bytes win over stored ranges: edits shift lines without
        // re-syncing, so excerpts re-resolve before capping — capping
        // after resolve keeps the budget honest when live text grows.
        // Renamed ranges render live bytes, marked stale at render time.
        let (resolved_all, resolved_stale, live_gaps) =
            crate::read::planes::code::resolve_live_findings(ctx.workspace, code_findings);
        gaps.extend(live_gaps);
        let stale_ids: std::collections::HashSet<String> = resolved_all
            .iter()
            .zip(resolved_stale.iter())
            .filter(|(_, stale)| **stale)
            .map(|(finding, _)| finding.id.clone())
            .collect();
        let clipped_all: Vec<crate::read::engine::Finding> = resolved_all
            .into_iter()
            .map(|mut finding| {
                finding.text = crate::read::planes::code::clip_finding_text(
                    &finding.text,
                    crate::read::engine::RG_EXCERPT_CHAR_CAP,
                );
                finding
            })
            .collect();
        // Whole findings only: truncation cuts between findings, never
        // mid-fence, so citations always name a rendered row.
        let mut kept_code = cap_findings_by_budget(&clipped_all, plan.budgets.code);
        // Budget-cut trace: token-capped ids in clipped order, so
        // `--trace` names the cost cap's victims without re-running it.
        let kept_ids: std::collections::HashSet<&str> = kept_code
            .iter()
            .map(|finding| finding.id.as_str())
            .collect();
        let budget_cut: Vec<String> = clipped_all
            .iter()
            .filter(|finding| !kept_ids.contains(finding.id.as_str()))
            .map(|finding| finding.id.clone())
            .collect();
        // Deterministic spam handling first: manifest rows collapse to
        // one with a count, fixtures re-assert exclusion. Rank-only.
        let (collapsed_code, manifest_dropped) =
            crate::query::rerank::collapse_manifest_rows(kept_code);
        kept_code = collapsed_code;
        if manifest_dropped > 0 {
            gaps.push(format!(
                "manifest rows collapsed: kept 1 of {}",
                manifest_dropped + 1
            ));
        }
        kept_code = crate::query::rerank::exclude_fixtures(kept_code, request.include_fixtures);
        // Shadow judge: labels render, nothing filters, rank order kept.
        // `None` judge is byte-identical to the extractive path. Authorized
        // filtering (M7) drops `Irrelevant`, gaps `Insufficient`, and keeps
        // `Relevant` plus deterministic exact evidence; support gating needs
        // its own point. Caches stay cold until authorized.
        let mut judge_labels: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        // Trace mirrors: code admission/insufficient drops in encounter
        // order, so `--trace` names every filtered finding with its label.
        let mut dropped_code: Vec<(String, String)> = Vec::new();
        let mut finding_authorized = false;
        let mut support_authorized = false;
        if let Some(judge_ctx) = judge.as_ref() {
            // Authorization through the loaded registry, never a model
            // verdict: only `Calibrated` filters.
            let registry = admission_registry
                .clone()
                .unwrap_or_else(fluent_llm::decision::CalibrationRegistry::empty);
            if let Some(fingerprint) = judge_ctx.fingerprint.as_deref() {
                use crate::query::decision_points::authorize_point_with;
                finding_authorized = matches!(
                    authorize_point_with(
                        &registry,
                        crate::read::calibration::FINDING_POINT,
                        fingerprint,
                        crate::read::calibration::FINDING_QUESTION_VERSION,
                    ),
                    fluent_llm::decision::CalibrationStatus::Calibrated(_)
                );
                support_authorized = matches!(
                    authorize_point_with(
                        &registry,
                        crate::read::calibration::SUPPORT_POINT,
                        fingerprint,
                        crate::read::calibration::SUPPORT_QUESTION_VERSION,
                    ),
                    fluent_llm::decision::CalibrationStatus::Calibrated(_)
                );
            }
            let judge_inputs: Vec<(String, String)> = kept_code
                .iter()
                .map(|finding| (finding.id.clone(), finding.text.clone()))
                .collect();
            let backend = judge_ctx.backend;
            // Fingerprint-scoped cache, used only when authorized.
            let mut verdicts: Vec<crate::read::decide::FindingVerdict> = Vec::new();
            let mut misses: Vec<(String, String)> = Vec::new();
            if finding_authorized {
                for (id, excerpt) in &judge_inputs {
                    let key = common_core::hash::blake3_hex(
                        format!(
                            "{}|{}|{}|{}",
                            request.query,
                            id,
                            excerpt,
                            judge_ctx.fingerprint.as_deref().unwrap_or("")
                        )
                        .as_bytes(),
                    );
                    if let Some(hit) = finding_cache().get(&key) {
                        verdicts.push(crate::read::decide::FindingVerdict {
                            id: id.clone(),
                            label: crate::read::decide::CandidateLabel::from_label(&hit.label),
                            confidence: hit.confidence,
                        });
                    } else {
                        misses.push((id.clone(), excerpt.clone()));
                    }
                }
            } else {
                misses.clone_from(&judge_inputs);
            }
            if !misses.is_empty() {
                let finding_judge = crate::read::decide::FindingJudge::new(
                    backend,
                    judge_ctx.model.clone(),
                    judge_ctx.timeout,
                );
                let mut miss_verdicts = finding_judge.judge(&request.query, &misses);
                if finding_authorized {
                    for verdict in &miss_verdicts {
                        let excerpt = misses
                            .iter()
                            .find(|(id, _)| id == &verdict.id)
                            .map_or("", |(_, excerpt)| excerpt.as_str());
                        let key = common_core::hash::blake3_hex(
                            format!(
                                "{}|{}|{}|{}",
                                request.query,
                                verdict.id,
                                excerpt,
                                judge_ctx.fingerprint.as_deref().unwrap_or("")
                            )
                            .as_bytes(),
                        );
                        if let Ok(decision) = fluent_llm::decision::Decision::from_distribution(
                            verdict.label.as_str(),
                            vec![(verdict.label.as_str().to_string(), verdict.confidence)],
                            judge_ctx.model.clone(),
                            crate::read::calibration::FINDING_QUESTION_VERSION,
                            0,
                            false,
                        ) {
                            finding_cache().insert(key, decision);
                        }
                    }
                }
                // Preserve input order: cached hits first in rank order,
                // then misses in rank order (both rank-ordered, no reorder).
                // Re-sort by original rank for determinism.
                let mut by_id: std::collections::HashMap<
                    String,
                    crate::read::decide::FindingVerdict,
                > = verdicts.into_iter().map(|v| (v.id.clone(), v)).collect();
                for v in miss_verdicts.drain(..) {
                    by_id.insert(v.id.clone(), v);
                }
                verdicts = judge_inputs
                    .iter()
                    .filter_map(|(id, _)| by_id.remove(id))
                    .collect();
            }
            let mut all_zero = !verdicts.is_empty();
            for (index, verdict) in verdicts.iter().enumerate() {
                let _ = crate::query::shadow::finding_judge_shadow(
                    &verdict.id,
                    verdict.label.as_str(),
                    verdict.confidence,
                    index + 1,
                );
                judge_labels.insert(verdict.id.clone(), verdict.label.as_str().to_string());
                if verdict.confidence != 0.0 {
                    all_zero = false;
                }
            }
            if all_zero && !verdicts.is_empty() {
                gaps.push("judge unavailable: shadow-only evidence".to_string());
            }
            // Rerank (rank-only): demote by verdict tier via the seam.
            // Unavailable keeps prior order (ranked-unfiltered); hard
            // drops wait for a passing registry entry, never a verdict.
            if !verdicts.is_empty() {
                let prior_ids: Vec<String> =
                    kept_code.iter().map(|finding| finding.id.clone()).collect();
                let model_ids = crate::query::rerank::demote_by_verdict(&prior_ids, &verdicts);
                if model_ids != prior_ids {
                    let mut by_id = std::collections::HashMap::new();
                    for finding in kept_code {
                        by_id.insert(finding.id.clone(), finding);
                    }
                    kept_code = model_ids.iter().filter_map(|id| by_id.remove(id)).collect();
                    let decisions = crate::query::rerank::verdict_decisions(
                        &verdicts,
                        &judge_ctx.model,
                        crate::query::decision_points::RERANK_VERSION,
                    );
                    let _ = crate::query::shadow::rerank_shadow(&prior_ids, &model_ids, &decisions);
                }
            }
            if judge_ctx.fingerprint.is_none() {
                gaps.push("decision fingerprint absent: shadow-only".to_string());
            }
            // Advisory admission: top-5 judged findings only when the
            // admission point calibrates. Every drop names its label;
            // unavailable keeps the prior order, exactly like today.
            if admission_authorized(judge_ctx, Some(&registry))
                && !verdicts.is_empty()
                && crate::query::rerank::rerank_status(&verdicts)
                    == crate::query::rerank::RerankStatus::Ranked
            {
                let prior_ids: Vec<String> =
                    kept_code.iter().map(|finding| finding.id.clone()).collect();
                let admitted: Vec<String> =
                    crate::query::rerank::admit_top(&prior_ids, &verdicts, 5);
                if admitted.len() != kept_code.len() {
                    for finding in &kept_code {
                        if !admitted.contains(&finding.id) {
                            let label = judge_labels
                                .get(&finding.id)
                                .cloned()
                                .unwrap_or_else(|| "unjudged".to_string());
                            gaps.push(format!("admission dropped {} ({label})", finding.id));
                            dropped_code.push((finding.id.clone(), label));
                        }
                    }
                    let mut by_id = std::collections::HashMap::new();
                    for finding in kept_code {
                        by_id.insert(finding.id.clone(), finding);
                    }
                    kept_code = admitted.iter().filter_map(|id| by_id.remove(id)).collect();
                }
            }
            if finding_authorized {
                use crate::read::decide::FindingLabel;
                let mut filtered = Vec::new();
                for finding in kept_code {
                    let label =
                        judge_labels
                            .get(&finding.id)
                            .map_or(FindingLabel::Insufficient, |label| {
                                <FindingLabel as crate::read::decide::CandidateLabel>::from_label(
                                    label,
                                )
                            });
                    match label {
                        FindingLabel::Relevant => filtered.push(finding),
                        FindingLabel::Insufficient => {
                            gaps.push(format!("insufficient: {}", finding.id));
                            dropped_code.push((finding.id.clone(), "insufficient".to_string()));
                        }
                        FindingLabel::Irrelevant => {
                            // Deterministic exact evidence is never dropped:
                            // symbol/graph structure is authoritative.
                            if finding.producer == FindingProducer::Ast {
                                filtered.push(finding);
                            } else {
                                dropped_code.push((finding.id.clone(), "irrelevant".to_string()));
                            }
                        }
                    }
                }
                kept_code = filtered;
            }
        }
        // Extractive code rows for factual intents: consumed rows
        // leave Top matches and the rendered citation set. The
        // envelope citation set below still covers the admitted rows.
        let (code_rows, used_code_ids) = match request.answer_mode {
            // Extractive rows are model-free under every mode.
            _ if is_factual_intent(plan.intent) && plan.intent != QueryIntent::Ownership => {
                let (rows, used) = extractive_code_rows(&kept_code, &stale_ids);
                let grounded = rows.iter().any(|(_, body)| {
                    crate::read::vocabulary::evidence_names_subject(&request.query, body)
                }) || used.iter().any(|id| stored_grounded.contains(id));
                if grounded || rows.is_empty() {
                    (rows, used)
                } else {
                    (Vec::new(), std::collections::HashSet::new())
                }
            }
            _ => (Vec::new(), std::collections::HashSet::new()),
        };
        let doc_row_count = extractive_rows.len();
        extractive_rows.extend(code_rows);
        let cited_code_findings = kept_code.clone();
        let code_evidence_grounded = cited_code_findings.iter().any(|finding| {
            crate::read::vocabulary::evidence_names_subject(&request.query, &finding.text)
                || stored_grounded.contains(&finding.id)
        });
        // Ownership answers are the owner list: one row per component
        // package in evidence rank order, with the verified doc rows
        // kept as context. Multi-owner is the truth, never ambiguity.
        if plan.intent == QueryIntent::Ownership && (docs_grounded || code_evidence_grounded) {
            let anchored_owner = enforced
                .as_ref()
                .and_then(|anchor| anchor.terms.first().map(String::as_str));
            let owners = ownership_rows(&cited_code_findings, anchored_owner, &stale_ids);
            if !owners.is_empty() {
                let docs: Vec<(String, String)> = extractive_rows.drain(..doc_row_count).collect();
                extractive_rows = owners;
                extractive_rows.extend(docs);
            }
        }
        if !used_code_ids.is_empty() {
            kept_code.retain(|finding| !used_code_ids.contains(&finding.id));
        }
        let (edge_findings, code_rest): (Vec<Finding>, Vec<Finding>) = kept_code
            .into_iter()
            .partition(|finding| finding.kind == FindingKind::Edge);
        let top_body = render_top_matches(&code_rest, &judge_labels, finding_authorized, &plan);
        push("Top matches", top_body, &mut tokens_used);
        // Call-graph edges render apart from code excerpts: the union
        // collapse in `EvidenceSet::dedupe` would otherwise fuse edge
        // rows into same-file excerpts and hide the caller list.
        const EDGE_ROW_CAP: usize = 12;
        if !edge_findings.is_empty() {
            let shown = edge_findings.len().min(EDGE_ROW_CAP);
            let edge_body = format!(
                "### Call graph\n{}",
                crate::read::render::render_code_findings(&edge_findings[..shown])
            );
            push("Call graph", edge_body, &mut tokens_used);
            if edge_findings.len() > EDGE_ROW_CAP {
                gaps.push(format!(
                    "call graph truncated to {EDGE_ROW_CAP} rows ({} recorded)",
                    edge_findings.len()
                ));
            }
        } else if edge_query_target(&request.query).is_some() {
            gaps.push(
                "graph has no recorded edges for this query (unknown symbol or unindexed calls)"
                    .to_string(),
            );
        }
        // The shown set is the admitted set: citations cover every
        // admitted row, including the extractive ones rendered in
        // `Answer`. Rendered sections below draw from the filtered
        // rows, so no excerpt appears twice.
        let mut code_findings = code_rest;
        code_findings.extend(edge_findings);
        // Freshness is labelled on the row, never hidden: findings
        // whose source moved since indexing cite with a ` (stale)`
        // marker, as do renamed ranges served from live bytes. Strict
        // callers refuse on marked citations; everyone else reads the
        // label and the degraded coverage beside it.
        let stale = stale_sources(ctx, &cited_code_findings);
        let mut citations: Vec<crate::source_ref::Citation> = cited_code_findings
            .iter()
            .map(|finding| {
                let row_stale =
                    stale.contains(finding.source.display()) || stale_ids.contains(&finding.id);
                finding
                    .source
                    .citation(finding.start_line, finding.end_line)
                    .with_status(row_stale, false)
                    .with_digest(common_core::hash::blake3_hex(finding.text.as_bytes()))
            })
            .collect();
        // Card paths with whole-card citations: verified docs and
        // capabilities plus the project sources below. The constrain
        // prefilter accepts these paths with no line-range check.
        let mut card_paths: Vec<String> = Vec::new();
        for hit in &cited_doc_hits {
            if hit.verified {
                let excerpt = hit.body.clone().unwrap_or_default();
                citations.push(
                    crate::source_ref::SourceRef::from_workspace(
                        ctx.workspace,
                        std::path::Path::new(&hit.source_path),
                    )
                    .citation(None, None)
                    .with_digest(common_core::hash::blake3_hex(excerpt.as_bytes())),
                );
                card_paths.push(hit.source_path.clone());
            }
        }
        for hit in &cap_hits {
            if hit.verified {
                let excerpt = hit
                    .live_excerpt
                    .clone()
                    .unwrap_or_else(|| hit.purpose.clone());
                citations.push(
                    crate::source_ref::SourceRef::from_workspace(
                        ctx.workspace,
                        std::path::Path::new(&hit.source_path),
                    )
                    .citation(None, None)
                    .with_digest(common_core::hash::blake3_hex(excerpt.as_bytes())),
                );
            }
        }
        if plan.intent == QueryIntent::Orientation {
            let mut project_citations = Vec::new();
            let manifest = ctx.workspace.join("Cargo.toml");
            if manifest.is_file() {
                let body = std::fs::read_to_string(&manifest).unwrap_or_default();
                project_citations.push(
                    crate::source_ref::SourceRef::from_workspace(
                        ctx.workspace,
                        std::path::Path::new("Cargo.toml"),
                    )
                    .citation(None, None)
                    .with_digest(common_core::hash::blake3_hex(body.as_bytes())),
                );
                card_paths.push("Cargo.toml".to_string());
            }
            if let Ok(project_cards) = ctx.db.project_cards_all() {
                for card in project_cards
                    .iter()
                    .filter(|card| matches!(card.kind.as_str(), "workspace" | "crate" | "domain"))
                {
                    if card.content_hash.is_empty() {
                        continue;
                    }
                    let source = card.source_path.split('#').next().unwrap_or("");
                    if source.is_empty() {
                        continue;
                    }
                    if !crate::query::rerank::scope_row_admitted(source, request.include_fixtures) {
                        continue;
                    }
                    if !ctx.workspace.join(source).is_file() {
                        continue;
                    }
                    project_citations.push(
                        crate::source_ref::SourceRef::from_workspace(
                            ctx.workspace,
                            std::path::Path::new(source),
                        )
                        .citation(None, None)
                        .with_digest(card.content_hash.clone()),
                    );
                    card_paths.push(source.to_string());
                }
            }
            citations.splice(0..0, project_citations);
        }
        // File-level collapse: one file cites once (first range wins),
        // so duplicate and spam rows never reach the envelope.
        {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            citations.retain(|cite| seen.insert(cite.source.display().to_string()));
        }
        push(
            "Citations",
            crate::read::render::render_capped_evidence_section(
                "Citations",
                &citations
                    .iter()
                    .map(|cite| (cite.text(), String::new()))
                    .collect::<Vec<_>>(),
                plan.budgets.code / 2,
            ),
            &mut tokens_used,
        );
        // One cache identity for both assembly paths: evidence bytes,
        // model, prompt version, and the assembly-mode discriminator.
        // The index generation invalidates rows computed over an older
        // marker before they are served.
        let assembly_mode = match request.answer_mode {
            AnswerMode::Auto => "auto",
            AnswerMode::Evidence => "evidence",
            AnswerMode::Synthesis => "synthesis",
        };
        let model_identity = synthesize.map_or("", Synthesizer::model_identity);
        let manifest_hash = crate::grounding::evidence_manifest_hash(
            &cited_code_findings,
            &cited_doc_hits,
            &cards,
            model_identity,
            crate::read::admission::ANSWER_PROMPT_VERSION,
            assembly_mode,
        );
        let cached = ctx
            .db
            .answer_cache_get(&manifest_hash, &self.generation)
            .ok()
            .flatten();
        // Answer assembly: extractive rows first for factual intents
        // in Auto mode (no model call), then the earned-generative
        // path, otherwise evidence-only. `Synthesis` keeps the legacy
        // prose rule exactly; `Evidence` never synthesizes.
        let map_grounds = plan.intent == QueryIntent::Orientation && !cards.is_empty();
        let mut claims = Vec::new();
        let mut answer_body: Option<String> = None;
        let mut cache_hit = false;
        if !extractive_rows.is_empty() {
            answer_body = Some(render_extractive(&extractive_rows));
        }
        if let Some(hit) = cached {
            claims = cached_claims(&hit.claims_json);
            answer_body = Some(hit.answer);
            cache_hit = true;
        }
        let legacy_evidence = !code_findings.is_empty()
            || doc_hits.iter().any(|hit| hit.verified)
            || (plan.intent == QueryIntent::Orientation && !cards.is_empty());
        let want_synthesis = match request.answer_mode {
            AnswerMode::Evidence => false,
            AnswerMode::Synthesis => !request.no_llm && legacy_evidence,
            AnswerMode::Auto => {
                !request.no_llm
                    && crate::read::admission::synthesis_allowed(
                        plan.intent,
                        doc_hits.len(),
                        code_findings.len(),
                        &[],
                    ) == crate::read::admission::SynthesisDecision::Synthesize
            }
        };
        if !cache_hit && want_synthesis {
            // Abstain through the sufficiency route: admitted evidence
            // that cannot answer skips generation (deterministic
            // fail-closed; no authorized sufficiency verdict exists
            // yet, so the rule is evidence presence).
            let grounded = legacy_evidence;
            let admitted = code_findings.len() + doc_hits.len();
            let (sufficiency, _) = crate::query::decision_points::route_sufficiency(
                admitted,
                grounded,
                None,
                &fluent_llm::decision::ThresholdSet {
                    act: 0.9,
                    review: 0.7,
                },
            );
            let sufficient =
                sufficiency != crate::query::decision_points::Sufficiency::Unanswerable;
            if !sufficient {
                gaps.push("insufficient evidence to answer: evidence only".to_string());
            }
            match (sufficient, synthesize) {
                (false, _) => {}
                (_, None) => gaps.push("synthesis unavailable: evidence only".to_string()),
                (_, Some(synth)) => {
                    let (prompt, evidence) =
                        synthesis_prompt(&request.query, &cards, &doc_hits, &code_findings);
                    let manifest: Vec<(String, String)> = evidence
                        .iter()
                        .map(|item| (item.token.clone(), item.canonical.clone()))
                        .collect();
                    let evidence_texts: Vec<(String, String)> = evidence
                        .iter()
                        .map(|item| (item.canonical.clone(), item.text.clone()))
                        .collect();
                    match synth.synthesize(&prompt) {
                        Ok(Some(text)) => {
                            let constrained =
                                crate::read::claims::constrain_answer_to_evidence_with_cards(
                                    &text,
                                    &code_findings
                                        .iter()
                                        .filter_map(|finding| {
                                            Some((
                                                String::new(),
                                                finding.source.display().to_string(),
                                                finding.start_line?,
                                                finding.end_line?,
                                                finding.text.clone(),
                                            ))
                                        })
                                        .collect::<Vec<_>>(),
                                    &card_paths,
                                );
                            claims = crate::read::claims::bind_claims(&constrained, &manifest);
                            // Deterministic entailment prefilter: a cited
                            // range without a shared identifier is
                            // insufficient with no model call.
                            crate::read::claims::prefilter_support(&mut claims, &evidence_texts);
                            if let Some(judge_ctx) = judge.as_ref() {
                                let support_judge = crate::read::decide::SupportJudge::new(
                                    judge_ctx.backend,
                                    judge_ctx.model.clone(),
                                    judge_ctx.timeout,
                                );
                                let judge_inputs: Vec<(usize, String)> = claims
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, claim)| {
                                        claim.citation.is_some()
                                            && claim.support
                                                == crate::read::claims::Support::Unverified
                                    })
                                    .map(|(index, claim)| {
                                        let excerpt = evidence_texts
                                            .iter()
                                            .find(|(canonical, _)| {
                                                Some(canonical.as_str())
                                                    == claim.citation.as_deref()
                                            })
                                            .map_or("", |(_, text)| text.as_str());
                                        (index, format!("{} | {}", claim.text, excerpt))
                                    })
                                    .collect();
                                let verdict_inputs: Vec<(String, String)> = judge_inputs
                                    .iter()
                                    .map(|(_, text)| (String::new(), text.clone()))
                                    .collect();
                                let verdicts = support_judge.judge(&request.query, &verdict_inputs);
                                for ((index, _), verdict) in
                                    judge_inputs.iter().zip(verdicts.iter())
                                {
                                    let _ = crate::query::shadow::support_shadow(
                                        &format!("claim-{index}"),
                                        verdict.label.as_str(),
                                        verdict.confidence,
                                        index + 1,
                                    );
                                    if !support_authorized {
                                        continue;
                                    }
                                    if let Some(claim) = claims.get_mut(*index) {
                                        use crate::read::decide::SupportLabel;
                                        match verdict.label {
                                            SupportLabel::Supported => {
                                                claim.support =
                                                    crate::read::claims::Support::Verified;
                                            }
                                            SupportLabel::Contradicted => {
                                                claim.support =
                                                    crate::read::claims::Support::Contradicted;
                                            }
                                            SupportLabel::Insufficient => {
                                                claim.support =
                                                    crate::read::claims::Support::Insufficient;
                                            }
                                        }
                                    }
                                }
                            }
                            match crate::read::claims::gate_answer(&claims) {
                                Some(gated) => {
                                    let bound = claims.iter().any(|claim| claim.citation.is_some());
                                    if claims_name_subject(&request.query, &claims, &evidence_texts)
                                        || (map_grounds && bound)
                                    {
                                        answer_body = Some(gated.clone());
                                    }
                                }
                                None => {
                                    gaps.push("no supported sentence: evidence only".to_string());
                                }
                            }
                        }
                        Ok(None) => {
                            gaps.push("synthesis unavailable: evidence only".to_string());
                        }
                        Err(error) => {
                            let detail = error.trim().chars().take(200).collect::<String>();
                            gaps.push(format!("synthesis unavailable: {detail}"));
                        }
                    }
                }
            }
        }
        // Floor-passing answers persist on every assembly path; the
        // negative-result contract is never an answer and never stores.
        if !cache_hit {
            if let Some(body) = answer_body.as_ref() {
                let _ = ctx
                    .db
                    .answer_cache_put(&crate::query::search_db::AnswerCacheRecord {
                        manifest_hash: manifest_hash.clone(),
                        answer: body.clone(),
                        claims_json: cached_claims_json(&claims),
                        citations_json: serde_json::to_string(
                            &citations
                                .iter()
                                .map(crate::source_ref::Citation::text)
                                .collect::<Vec<_>>(),
                        )
                        .unwrap_or_default(),
                        support: cached_support_status(&claims),
                        model: model_identity.to_string(),
                        prompt_version: crate::read::admission::ANSWER_PROMPT_VERSION as i64,
                        generation: self.generation.clone(),
                        created_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |elapsed| elapsed.as_millis() as i64),
                    });
            }
        }
        // Negative results name their covered scope once: either no
        // evidence at all, or evidence that never names the asked
        // subject. Grounded evidence without an Answer (evidence mode)
        // is not a negative result.
        let answer_material_grounded = map_grounds
            || cited_doc_hits.iter().filter(|hit| hit.verified).any(|hit| {
                hit.body.as_deref().is_some_and(|body| {
                    crate::read::vocabulary::evidence_names_subject(&request.query, body)
                })
            })
            || cited_code_findings.iter().any(|finding| {
                crate::read::vocabulary::evidence_names_subject(&request.query, &finding.text)
                    || stored_grounded.contains(&finding.id)
            })
            || claims_name_subject(&request.query, &claims, &[]);
        let evidence_present = !cited_code_findings.is_empty()
            || !cited_doc_hits.is_empty()
            || !cards.is_empty()
            || !cap_hits.is_empty();
        if answer_body.is_none() && evidence_present && !answer_material_grounded {
            push_covered_index_contract(&mut gaps, &plan, request.include_fixtures);
        }
        if cards.is_empty() && plan.intent == QueryIntent::Orientation {
            gaps.push("no project cards: orientation degrades to docs".to_string());
        }
        let degraded = !gaps.is_empty();
        let mut ordered = Vec::new();
        if let Some(body) = answer_body {
            let body = format!(
                "### Answer\n{}",
                crate::read::render::sanitize_for_markdown(&body)
            );
            tokens_used += tokens_for(&body);
            ordered.push(ReadSection {
                title: "Answer".to_string(),
                body,
            });
        }
        ordered.append(&mut sections);
        if !gaps.is_empty() {
            let body = crate::read::render::render_evidence_section(
                "Gaps",
                &gaps
                    .iter()
                    .map(|gap| (gap.clone(), String::new()))
                    .collect::<Vec<_>>(),
                plan.budgets.total,
            );
            tokens_used += tokens_for(&body);
            ordered.push(ReadSection {
                title: "Gaps".to_string(),
                body,
            });
        }
        // Per-candidate trace: deterministic rows over the admitted,
        // dropped, and budget-cut candidates. Judge columns read the
        // verdict maps collected above; unjudged rows never filtered.
        let code_anchor = scope_hypothesis
            .as_deref()
            .map_or("none".to_string(), |label| {
                format!("scope:{label} rerank-only")
            });
        let mut trace = Vec::new();
        if anchor_excluded > 0 {
            trace.push(TraceRow {
                plane: "docs".to_string(),
                candidate: "(anchor-filtered)".to_string(),
                anchor: anchor_decision.clone(),
                judge: "-".to_string(),
                outcome: format!("excluded:{anchor_excluded}"),
            });
        }
        for hit in &cited_doc_hits {
            let outcome = if used_doc_keys.contains(&hit.key) {
                "extractive".to_string()
            } else if hit.verified {
                "rendered".to_string()
            } else {
                "rendered:unverified".to_string()
            };
            trace.push(TraceRow {
                plane: "docs".to_string(),
                candidate: hit.key.clone(),
                anchor: anchor_decision.clone(),
                judge: doc_labels
                    .get(&hit.key)
                    .cloned()
                    .unwrap_or_else(|| "unjudged".to_string()),
                outcome,
            });
        }
        for (key, label) in &dropped_docs {
            trace.push(TraceRow {
                plane: "docs".to_string(),
                candidate: key.clone(),
                anchor: anchor_decision.clone(),
                judge: label.clone(),
                outcome: format!("dropped:{label}"),
            });
        }
        for id in &budget_cut {
            trace.push(TraceRow {
                plane: "code".to_string(),
                candidate: id.clone(),
                anchor: code_anchor.clone(),
                judge: "unjudged".to_string(),
                outcome: "budget-cap".to_string(),
            });
        }
        let dropped_by_id: std::collections::HashMap<&str, &str> = dropped_code
            .iter()
            .map(|(id, label)| (id.as_str(), label.as_str()))
            .collect();
        for finding in &cited_code_findings {
            let outcome = if used_code_ids.contains(&finding.id) {
                "extractive".to_string()
            } else if let Some(label) = dropped_by_id.get(finding.id.as_str()) {
                format!("dropped:{label}")
            } else {
                "rendered".to_string()
            };
            trace.push(TraceRow {
                plane: "code".to_string(),
                candidate: finding.id.clone(),
                anchor: code_anchor.clone(),
                judge: judge_labels
                    .get(&finding.id)
                    .cloned()
                    .unwrap_or_else(|| "unjudged".to_string()),
                outcome,
            });
        }
        ReadAnswer {
            query: request.query.clone(),
            intent: plan.intent,
            route: plan.route.clone(),
            sections: ordered,
            coverage: ctx.coverage.clone(),
            claims,
            citations: citations.clone(),
            gaps: gaps.clone(),
            degraded,
            tokens_used,
            subject,
            cache_hit,
            trace,
        }
    }
}

impl Default for ReadEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Coverage section body: intent, freshness, embeddings, briefs,
/// docs, model transport, budget.
fn coverage_section(
    coverage: Option<&crate::read::readiness::Coverage>,
    plan: &ReadPlan,
) -> String {
    use crate::read::readiness::{DecisionSupport, IndexState, MirrorState};
    use std::fmt::Write as _;
    let mut out = String::from("### Coverage\n");
    let _ = writeln!(out, "intent: {:?} (route {})", plan.intent, plan.route);
    let Some(coverage) = coverage else {
        return out + "coverage: unknown (no readiness snapshot)\n";
    };
    match &coverage.mirror {
        MirrorState::Fresh { total } => {
            let _ = writeln!(out, "mirror: fresh ({total} files)");
        }
        MirrorState::Stale { total, stale } => {
            let _ = writeln!(out, "mirror: stale ({stale}/{total})");
        }
        MirrorState::Missing => out.push_str("mirror: missing\n"),
    }
    match &coverage.index {
        IndexState::Ready => out.push_str("index: ready\n"),
        IndexState::Missing => out.push_str("index: missing\n"),
        IndexState::SchemaMismatch => out.push_str("index: schema-mismatch\n"),
    }
    if coverage.embeddings_off {
        out.push_str("embeddings: off (explicit policy; enable with --embed)\n");
    } else {
        let _ = writeln!(
            out,
            "embeddings: {}/{} ready",
            coverage.embeddings.ready, coverage.embeddings.total
        );
    }
    if coverage.capabilities.ready == 0 {
        out.push_str("capabilities: absent (optional corpus; not a read blocker)\n");
    } else {
        let _ = writeln!(out, "capabilities: {} ready", coverage.capabilities.ready);
    }
    let _ = writeln!(
        out,
        "docs: {} ({} doc cards)",
        if coverage.docs_present {
            "present"
        } else {
            "absent"
        },
        coverage.doc_cards
    );
    match &coverage.decision {
        DecisionSupport::Unknown => out.push_str("decision: unknown (shadow-only)\n"),
        DecisionSupport::Supported => out.push_str("decision: supported\n"),
        DecisionSupport::Unsupported { reason } => {
            let _ = writeln!(out, "decision: unsupported ({reason})");
        }
    }
    match &coverage.support {
        DecisionSupport::Unknown => out.push_str("support: unknown (unverified)\n"),
        DecisionSupport::Supported => out.push_str("support: supported\n"),
        DecisionSupport::Unsupported { reason } => {
            let _ = writeln!(out, "support: unsupported ({reason})");
        }
    }
    let _ = writeln!(
        out,
        "budget: {} tokens (project {} / evidence {} / code {})",
        plan.budgets.total, plan.budgets.project, plan.budgets.evidence, plan.budgets.code
    );
    if plan.scope_crates.is_empty() {
        out.push_str("scope: none (query names no crate; recall unscoped)\n");
    } else {
        let _ = writeln!(
            out,
            "scope: in=[{}] (70% of code budget; recall stays unscoped{})",
            plan.scope_crates.join(", "),
            if plan.scope_narrowed {
                "; graph narrowed to scope-owned definitions"
            } else {
                ""
            }
        );
    }
    out
}

/// Project-map cards for the plan intent: orientation serves the
/// workspace-first card order; ownership scopes by the exact
/// crate in the subject anchor, falling back to the symbol-term
/// scope only when no anchor resolves. A `Gaps` line names the
/// anchor when it removes every card.
/// Project-map trim for scoped queries: the workspace frame plus
/// members in scope; domain cards follow their code paths. The full
/// map survives only when nothing matches, so a bad scope never blanks
/// the frame.
fn scoped_project_cards(
    cards: Vec<crate::read::planes::orientation::CardFinding>,
    scope: &[String],
) -> Vec<crate::read::planes::orientation::CardFinding> {
    let scoped: Vec<crate::read::planes::orientation::CardFinding> = cards
        .iter()
        .filter(|card| match card.kind.as_str() {
            "workspace" => true,
            "crate" => scope.iter().any(|package| package == &card.title),
            _ => card.code_paths.iter().any(|path| {
                scope.iter().any(|package| {
                    crate::component_map::component_gate_for_path(path).package == *package
                })
            }),
        })
        .cloned()
        .collect();
    if scoped.is_empty() {
        cards
    } else {
        scoped
    }
}

fn orientation_cards(
    plan: &ReadPlan,
    query: &str,
    db: &crate::query::search_db::GuidanceSearchDb,
    anchor: Option<&crate::read::vocabulary::SubjectAnchor>,
) -> (
    Vec<crate::read::planes::orientation::CardFinding>,
    Option<String>,
) {
    let Ok(plane) = crate::read::planes::orientation::OrientationPlane::load(db) else {
        return (Vec::new(), None);
    };
    if plan.intent == QueryIntent::Ownership {
        if let Some(crate::read::vocabulary::SubjectAnchor {
            terms,
            source: crate::read::vocabulary::AnchorSource::Crate,
        }) = anchor
        {
            if let Some(package) = terms.first() {
                let scoped = plane.ownership_cards(package);
                if scoped.is_empty() {
                    return (
                        plane.orientation_cards(),
                        Some(format!(
                            "subject anchor {package} removed every card; ownership falls back"
                        )),
                    );
                }
                return (scoped, None);
            }
        }
        let package = crate::read::vocabulary::symbol_terms(query)
            .into_iter()
            .next()
            .unwrap_or_default();
        let scoped = plane.ownership_cards(&package);
        if scoped.is_empty() {
            (plane.orientation_cards(), None)
        } else {
            (scoped, None)
        }
    } else {
        (plane.orientation_cards(), None)
    }
}

/// Factual intents answer extractively: the top admitted rows
/// render verbatim with citations, never through the model.
fn is_factual_intent(intent: crate::query::strategy::QueryIntent) -> bool {
    use crate::query::strategy::QueryIntent;
    matches!(
        intent,
        QueryIntent::Ownership
            | QueryIntent::SingleIdentifier
            | QueryIntent::FilePath
            | QueryIntent::Graph
    )
}

/// Covered-index contract for negative results: names the scope the
/// index actually covers, including the fixture/eval exclusions.
fn covered_index_contract(plan: &ReadPlan, include_fixtures: bool) -> String {
    let scope = if plan.scope_crates.is_empty() {
        "whole workspace".to_string()
    } else {
        format!("crates: {}", plan.scope_crates.join(", "))
    };
    let exclusions = if include_fixtures {
        "eval corpora excluded"
    } else {
        "fixtures and eval corpora excluded"
    };
    format!("none found in covered index (scope: {scope}; {exclusions})")
}

/// Push the contract once: a negative result states its covered scope
/// exactly once, however many paths withheld answer material.
fn push_covered_index_contract(gaps: &mut Vec<String>, plan: &ReadPlan, include_fixtures: bool) {
    if gaps
        .iter()
        .any(|gap| gap.starts_with("none found in covered index"))
    {
        return;
    }
    gaps.push(covered_index_contract(plan, include_fixtures));
}

/// Whether any gated claim names the asked subject, in its own words
/// or in the excerpt it cites: a candidate-answer path never renders
/// prose the query did not ask about.
fn claims_name_subject(
    query: &str,
    claims: &[crate::read::claims::Claim],
    evidence: &[(String, String)],
) -> bool {
    claims.iter().any(|claim| {
        let Some(citation) = claim.citation.as_deref() else {
            return false;
        };
        crate::read::vocabulary::evidence_names_subject(query, &claim.text)
            || evidence.iter().any(|(canonical, text)| {
                canonical == citation
                    && crate::read::vocabulary::evidence_names_subject(query, text)
            })
    })
}

/// Extractive doc rows: at most two verified docs, in rank order.
/// Returns the rendered rows plus the consumed doc keys.
fn extractive_doc_rows(
    doc_hits: &[crate::read::planes::docs::DocHit],
) -> (Vec<(String, String)>, std::collections::HashSet<String>) {
    let mut rows = Vec::new();
    let mut used = std::collections::HashSet::new();
    for hit in doc_hits.iter().filter(|hit| hit.verified).take(2) {
        let canonical = if hit.anchor.is_empty() {
            hit.source_path.clone()
        } else {
            format!("{}#{}", hit.source_path, hit.anchor)
        };
        rows.push((canonical, hit.body.clone().unwrap_or_default()));
        used.insert(hit.key.clone());
    }
    (rows, used)
}

/// Extractive code rows: at most two admitted findings, in rank
/// order. Returns the rendered rows plus the consumed finding ids.
/// Renamed ranges render live bytes with the row marked stale, so
/// the answer never serves a silent wrong excerpt.
fn extractive_code_rows(
    findings: &[Finding],
    stale: &std::collections::HashSet<String>,
) -> (Vec<(String, String)>, std::collections::HashSet<String>) {
    let mut rows = Vec::new();
    let mut used = std::collections::HashSet::new();
    for finding in findings.iter().take(2) {
        rows.push((finding_citation(finding, stale), finding.text.clone()));
        used.insert(finding.id.clone());
    }
    (rows, used)
}

/// Display citation for one finding: `path:start-end`, range omitted
/// when unknown, ` (stale)` when the live bytes moved.
fn finding_citation(finding: &Finding, stale: &std::collections::HashSet<String>) -> String {
    let range = match (finding.start_line, finding.end_line) {
        (Some(start), Some(end)) => format!("{start}-{end}"),
        (Some(start), None) => start.to_string(),
        _ => String::new(),
    };
    let mut canonical = if range.is_empty() {
        finding.source.display().to_string()
    } else {
        format!("{}:{range}", finding.source.display())
    };
    if stale.contains(&finding.id) {
        canonical.push_str(" (stale)");
    }
    canonical
}

/// Deterministic owners of the retrieved code: one row per component
/// package, in evidence rank order, with an exact crate anchor first.
/// Multi-owner is the truth, never ambiguity to hide.
fn ownership_rows(
    findings: &[Finding],
    anchored: Option<&str>,
    stale: &std::collections::HashSet<String>,
) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    let citation = findings
        .first()
        .map(|finding| finding_citation(finding, stale));
    if let (Some(package), Some(citation)) = (anchored, citation.as_deref()) {
        if !package.is_empty() && seen.insert(package.to_string()) {
            rows.push((citation.to_string(), format!("owned by {package}")));
        }
    }
    for finding in findings {
        let gate = crate::component_map::component_gate_for_path(finding.source.display());
        if gate.package.is_empty() || !seen.insert(gate.package.clone()) {
            continue;
        }
        rows.push((
            finding_citation(finding, stale),
            format!("owned by {}", gate.package),
        ));
    }
    rows
}

/// Render extractive rows: one cited verbatim line each.
fn render_extractive(rows: &[(String, String)]) -> String {
    rows.iter()
        .map(|(citation, body)| {
            format!(
                "- {citation}: {}",
                crate::read::planes::code::clip_finding_text(body, RG_EXCERPT_CHAR_CAP)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Rebuild bound claims from a cached claims payload. The support
/// verdict stored with the answer rides along: a hit is
/// reproducibility, never a fresh correctness claim.
fn cached_claims(claims_json: &str) -> Vec<crate::read::claims::Claim> {
    use crate::read::claims::{Claim, Support};
    serde_json::from_str::<Vec<serde_json::Value>>(claims_json)
        .unwrap_or_default()
        .into_iter()
        .map(|value| {
            let support = match value
                .get("support")
                .and_then(|support| support.as_str())
                .unwrap_or("")
            {
                "verified" => Support::Verified,
                "contradicted" => Support::Contradicted,
                "insufficient" => Support::Insufficient,
                _ => Support::Unverified,
            };
            Claim {
                text: value
                    .get("text")
                    .and_then(|text| text.as_str())
                    .unwrap_or("")
                    .to_string(),
                citation: value
                    .get("citation")
                    .and_then(|citation| citation.as_str())
                    .map(str::to_string),
                support,
            }
        })
        .collect()
}

/// Serialize bound claims for the answer cache.
fn cached_claims_json(claims: &[crate::read::claims::Claim]) -> String {
    let rows: Vec<serde_json::Value> = claims
        .iter()
        .map(|claim| {
            serde_json::json!({
                "text": claim.text,
                "citation": claim.citation,
                "support": format!("{:?}", claim.support).to_lowercase(),
            })
        })
        .collect();
    serde_json::to_string(&rows).unwrap_or_default()
}

/// Row-level support status for a stored answer: verified only when
/// every bound claim verified, otherwise the unverified default.
fn cached_support_status(claims: &[crate::read::claims::Claim]) -> String {
    if !claims.is_empty()
        && claims
            .iter()
            .all(|claim| claim.support == crate::read::claims::Support::Verified)
    {
        "verified".to_string()
    } else {
        "unverified".to_string()
    }
}

/// Whether the admission point authorizes filtering: a `Calibrated`/// entry for the judge fingerprint and the admission question
/// version. Anything else reads shadow-only, exactly like today.
fn admission_authorized(
    judge: &crate::read::engine::JudgeCtx<'_>,
    registry: Option<&fluent_llm::decision::CalibrationRegistry>,
) -> bool {
    let Some(registry) = registry else {
        return false;
    };
    let Some(fingerprint) = judge.fingerprint.as_deref() else {
        return false;
    };
    matches!(
        crate::query::decision_points::authorize_point_with(
            registry,
            crate::read::calibration::ADMISSION_POINT,
            fingerprint,
            crate::read::calibration::ADMISSION_QUESTION_VERSION,
        ),
        fluent_llm::decision::CalibrationStatus::Calibrated(_)
    )
}

/// Resolve the query subject with the free-term selectivity cap: a
/// free term matching more than half of the doc cards reads as no
/// anchor (fail-open). Exact-crate anchors skip the check. Crate
/// names only anchor ownership questions: elsewhere the crate word
/// is context ("guidance sync", "router dispatch") and the
/// discriminative topic term is the subject.
fn subject_anchor_for(
    query: &str,
    db: &crate::query::search_db::GuidanceSearchDb,
    intent: crate::query::strategy::QueryIntent,
) -> Option<crate::read::vocabulary::SubjectAnchor> {
    use crate::query::strategy::QueryIntent;
    let known = if intent == QueryIntent::Ownership {
        crate::component_map::known_crate_names()
    } else {
        Vec::new()
    };
    let anchor = crate::read::vocabulary::subject_anchor(query, &known)?;
    if anchor.source == crate::read::vocabulary::AnchorSource::Crate {
        return Some(anchor);
    }
    let bodies: Vec<String> = db
        .project_cards_all()
        .map(|cards| {
            cards
                .into_iter()
                .filter(|card| card.kind == "doc")
                .map(|card| card.body)
                .collect()
        })
        .unwrap_or_default();
    let refs: Vec<&str> = bodies.iter().map(String::as_str).collect();
    if crate::read::vocabulary::anchor_selective(&anchor, &refs) {
        Some(anchor)
    } else {
        None
    }
}

/// Cap findings by token estimate, whole findings only: the first
/// finding always renders (an empty Top matches answers nothing),
/// then each further finding while it fits. Truncation cuts between
/// findings, never mid-fence, so every citation names a rendered
/// row. Pure and budget-exact for the perf tests to pin.
#[must_use]
pub fn cap_findings_by_budget(
    findings: &[crate::read::engine::Finding],
    budget_tokens: u64,
) -> Vec<crate::read::engine::Finding> {
    let mut kept = Vec::new();
    let mut used = 0u64;
    for finding in findings {
        let cost = crate::read::budget::tokens_for(&finding.text);
        if !kept.is_empty() && used + cost > budget_tokens {
            break;
        }
        used += cost;
        kept.push(finding.clone());
    }
    kept
}

/// Evidence prompt for the synthesizer: question plus budgeted
/// finding excerpts with self-describing citation tokens. Code rows
/// cite `[Cn] path:start-end`; doc and project-map rows cite
/// `[Dn] path#anchor`. Only shown ids may be cited; the evidence
/// beside the prompt maps every token to its canonical citation for
/// `bind_claims` and to its excerpt for the support prefilter.
/// Capability briefs stay out: they are generated commentary and
/// render `[considered]` until calibrated.
struct EvidenceItem {
    token: String,
    canonical: String,
    text: String,
}

fn synthesis_prompt(
    query: &str,
    cards: &[crate::read::planes::orientation::CardFinding],
    docs: &[crate::read::planes::docs::DocHit],
    findings: &[crate::read::engine::Finding],
) -> (String, Vec<EvidenceItem>) {
    use std::fmt::Write as _;
    let mut prompt = format!("Question: {query}\nEvidence:\n");
    let mut evidence: Vec<EvidenceItem> = Vec::new();
    let mut doc_token = 0;
    if !cards.is_empty() {
        prompt.push_str("### Project map\n");
        for card in cards {
            doc_token += 1;
            let token = format!("[D{doc_token}]");
            let _ = writeln!(
                prompt,
                "- {token} {}: {}: {}",
                card.source, card.title, card.body
            );
            evidence.push(EvidenceItem {
                token,
                canonical: card.source.clone(),
                text: format!("{}: {}", card.title, card.body),
            });
        }
    }
    let verified_docs: Vec<(&str, &str, &str)> = docs
        .iter()
        .filter(|hit| hit.verified)
        .filter_map(|hit| {
            hit.body
                .as_deref()
                .map(|body| (hit.source_path.as_str(), hit.anchor.as_str(), body))
        })
        .collect();
    if !verified_docs.is_empty() {
        prompt.push_str("### Docs\n");
        for (source, anchor, body) in &verified_docs {
            doc_token += 1;
            let token = format!("[D{doc_token}]");
            let canonical = if anchor.is_empty() {
                (*source).to_string()
            } else {
                format!("{source}#{anchor}")
            };
            let _ = writeln!(prompt, "- {token} {canonical}: {body}");
            evidence.push(EvidenceItem {
                token,
                canonical,
                text: (*body).to_string(),
            });
        }
    }
    if !findings.is_empty() {
        prompt.push_str("### Code\n");
        for (index, finding) in findings.iter().take(10).enumerate() {
            let token = format!("[C{}]", index + 1);
            let range = match (finding.start_line, finding.end_line) {
                (Some(start), Some(end)) => format!("{start}-{end}"),
                (Some(start), None) => start.to_string(),
                _ => String::new(),
            };
            let canonical = if range.is_empty() {
                finding.source.display().to_string()
            } else {
                format!("{}:{range}", finding.source.display())
            };
            let _ = writeln!(prompt, "- {token} {canonical}: {}", finding.text);
            evidence.push(EvidenceItem {
                token,
                canonical,
                text: finding.text.clone(),
            });
        }
    }
    prompt.push_str("Answer with cited facts only. Cite only shown ids.");
    (prompt, evidence)
}

/// Top-matches body with the soft crate scope applied: in-scope
/// findings render first with 70% of the code budget, out-of-scope
/// with the remainder. Rank order inside each half is untouched and
/// recall never constrains, so a wrong scope degrades gracefully.
/// No scope (or a one-sided scope) renders exactly as before.
fn render_top_matches(
    kept_code: &[Finding],
    judge_labels: &std::collections::HashMap<String, String>,
    finding_authorized: bool,
    plan: &ReadPlan,
) -> String {
    let render = |findings: &[Finding]| -> String {
        if judge_labels.is_empty() || !finding_authorized {
            crate::read::render::render_code_findings(findings)
        } else {
            render_code_findings_with_labels(findings, judge_labels)
        }
    };
    let scoped = !plan.scope_crates.is_empty();
    let (inside, outside): (Vec<Finding>, Vec<Finding>) = if scoped {
        let mut inside = Vec::new();
        let mut outside = Vec::new();
        for finding in kept_code {
            if plan
                .scope_crates
                .iter()
                .any(|scope| *scope == finding.crate_name())
            {
                inside.push(finding.clone());
            } else {
                outside.push(finding.clone());
            }
        }
        (inside, outside)
    } else {
        (kept_code.to_vec(), Vec::new())
    };
    if !scoped || inside.is_empty() || outside.is_empty() {
        return format!(
            "### Top matches\n{}",
            crate::read::budget::enforce_budget(&render(kept_code), plan.budgets.code)
        );
    }
    let in_budget = plan.budgets.code * 7 / 10;
    let out_budget = plan.budgets.code.saturating_sub(in_budget);
    format!(
        "### Top matches\nscope: in-scope first (70% of code budget for [{}])\n{}{}",
        plan.scope_crates.join(", "),
        crate::read::budget::enforce_budget(&render(&inside), in_budget),
        crate::read::budget::enforce_budget(&render(&outside), out_budget)
    )
}

/// Render code findings with shadow judge labels. Rank order is
/// unchanged; labels are annotations, never filters.
fn render_code_findings_with_labels(
    findings: &[crate::read::engine::Finding],
    labels: &std::collections::HashMap<String, String>,
) -> String {
    use crate::query::formatter::{Formatter, MarkdownFormatter};
    use crate::query::synthesize::Stage;
    use fluent_types::StageKind;
    let stages: Vec<Stage> = findings
        .iter()
        .filter(|finding| {
            matches!(
                finding.kind,
                crate::read::engine::FindingKind::Code
                    | crate::read::engine::FindingKind::Symbol
                    | crate::read::engine::FindingKind::Edge
            )
        })
        .map(|finding| {
            let label = labels.get(&finding.id).cloned().unwrap_or_default();
            let source = if label.is_empty() {
                finding.source.display().to_string()
            } else {
                format!("{} [{}]", finding.source.display(), label)
            };
            Stage {
                kind: StageKind::Code,
                content: finding.text.clone(),
                source,
                line: finding.start_line,
                end_line: finding.end_line,
                member_name: None,
                member_type: None,
                trace: None,
            }
        })
        .collect();
    MarkdownFormatter.format(&stages)
}

/// Ripgrep query text for the bounded arm: first symbol term, else
/// first content term, else the trimmed query. Deterministic.
fn rg_query_text(intent: QueryIntent, query: &str) -> Option<String> {
    let _ = intent;
    let mut terms = crate::read::vocabulary::symbol_terms(query);
    if terms.is_empty() {
        terms = crate::read::vocabulary::content_terms(query);
    }
    if let Some(first) = terms.into_iter().next() {
        return Some(first);
    }
    let trimmed = query.trim().trim_matches('"').trim_matches('\'').trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Bounded ripgrep run: literal search under the workspace, normalized
/// through `rg_hits_to_findings`. Caps files and excerpt chars (cost
/// caps, never relevance); truncation reports a gap.
fn run_rg_findings(
    workspace: &std::path::Path,
    query_text: &str,
) -> (Vec<crate::read::engine::Finding>, Option<String>) {
    use fluent_wvr::capability::{CapabilitySet, FsCapability};
    let caps = CapabilitySet::new().with(FsCapability::new());
    let backend = crate::query::rg_backend::RgBackend {
        root: workspace.to_path_buf(),
        exe_override: None,
        path_override: None,
    };
    let query_owned = query_text.to_string();
    let result = fluent_wvr::capability::run_gated_blocking(caps, move || {
        backend.search_with_options(
            &query_owned,
            RG_FILE_CAP + 1,
            &crate::query::rg_backend::RgOptions {
                context_before: 0,
                context_after: 0,
                mtime_after_ms: None,
                mtime_before_ms: None,
                include_globs: Vec::new(),
                exclude_globs: Vec::new(),
            },
        )
    });
    let hits = match result {
        Ok(Ok(hits)) => hits,
        Ok(Err(error)) => {
            return (Vec::new(), Some(format!("rg unavailable: {error}")));
        }
        Err(_) => {
            return (
                Vec::new(),
                Some("rg unavailable: worker failed".to_string()),
            );
        }
    };
    let truncated = hits.len() > RG_FILE_CAP;
    let capped: Vec<crate::query::rg_backend::RgHit> = hits.into_iter().take(RG_FILE_CAP).collect();
    let mut findings = crate::read::planes::code::rg_hits_to_findings(workspace, &capped);
    let mut gap: Option<String> = None;
    if truncated {
        gap = Some(format!("rg truncated to {RG_FILE_CAP} files"));
    }
    for finding in &mut findings {
        if finding.text.len() > RG_EXCERPT_CHAR_CAP {
            finding.text = common_core::string::truncate_utf8(&finding.text, RG_EXCERPT_CHAR_CAP);
            gap.get_or_insert_with(|| {
                format!("rg excerpt truncated to {RG_EXCERPT_CHAR_CAP} chars")
            });
        }
    }
    (findings, gap)
}

/// Sources whose live bytes moved since indexing: the committed
/// freshness snapshot (size + mtime, the same gate the ingest uses
/// for unchanged files) disagrees with the live stat, or the file is
/// gone. Keyed by workspace-relative source, so tempdir-absolute
/// file ids can never leak into the comparison.
fn stale_sources(
    ctx: &ExplainContext<'_>,
    findings: &[crate::read::engine::Finding],
) -> std::collections::HashSet<String> {
    use std::collections::HashMap;
    let mut stored: HashMap<String, (u64, i64)> = HashMap::new();
    if let Ok(files) = ctx.db.list_files() {
        for file in &files {
            stored
                .entry(file.relative_path.clone())
                .or_insert((file.size_bytes, file.last_modified_time));
        }
    }
    let mut stale = std::collections::HashSet::new();
    for finding in findings {
        if finding.start_line.is_none() {
            continue;
        }
        let live = std::fs::metadata(finding.source.live_path()).ok();
        let fresh = match (stored.get(finding.source.display()), live) {
            (Some((size, mtime)), Some(meta)) => {
                meta.len() == *size && file_mtime_ms(&meta) == Some(*mtime)
            }
            _ => false,
        };
        if !fresh {
            stale.insert(finding.source.display().to_string());
        }
    }
    stale
}

/// Live mtime in millis since the epoch (`None` when unavailable).
fn file_mtime_ms(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

/// Edge-query target for the honest-empty gap: the first identifier
/// after `callers of` / `callees of` / `dependents of`. Symbols are
/// single identifiers, so a trailing scope clause (`in <crate>`) or
/// stray words never pollute the lookup — the scope resolver owns
/// those tokens instead.
fn edge_query_target(query: &str) -> Option<String> {
    let trimmed = query.trim();
    let lowered = trimmed.to_lowercase();
    ["callers of", "callees of", "dependents of"]
        .iter()
        .find_map(|cue| {
            lowered.find(cue).and_then(|position| {
                trimmed.get(position + cue.len()..).map(|tail| {
                    tail.trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_string()
                })
            })
        })
        .filter(|target| !target.is_empty())
}

/// Exact-structure findings for Symbol/Graph intents: definitions,
/// call sites, and owners through the committed graph. Unknown
/// symbols yield no findings (the caller records the gap). Graph
/// edges resolve through scope-owned definitions first when prefixes
/// are given, falling back to the unscoped planes; the flag reports
/// which side answered.
fn structure_findings(
    query: &str,
    intent: QueryIntent,
    workspace: &std::path::Path,
    db: &crate::query::search_db::GuidanceSearchDb,
    scope_prefixes: &[String],
) -> (Vec<crate::read::engine::Finding>, bool) {
    use crate::read::engine::{Finding, FindingKind, FindingProducer};
    let mut out = Vec::new();
    let narrowed: bool;
    let lower = query.trim().to_lowercase();
    let after_of = |cue: &str| {
        let trimmed = query.trim();
        let lowered = trimmed.to_lowercase();
        lowered
            .find(cue)
            .and_then(|position| trimmed.get(position + cue.len()..))
            .map(|tail| {
                tail.trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .trim()
                    .to_string()
            })
            .filter(|target| !target.is_empty())
    };
    let source_of = |absolute: &str| {
        crate::source_ref::SourceRef::from_workspace(workspace, std::path::Path::new(absolute))
    };
    if intent == QueryIntent::SingleIdentifier {
        let store = crate::read::planes::symbol::GuidanceStructureStore::new(db);
        let target = query.trim().trim_matches('"').trim_matches('\'');
        for site in store.exact_symbols(target) {
            let (text, start, end) = symbol_excerpt(db, &site);
            out.push(Finding {
                id: format!("symbol:{}", site.symbol_id),
                producer: FindingProducer::Ast,
                kind: FindingKind::Symbol,
                source: source_of(&site.file),
                start_line: start,
                end_line: end,
                text,
                producer_score: 1.0,
                provenance: "symbol".to_string(),
            });
        }
        return (out, false);
    }
    if intent != QueryIntent::Graph {
        return (out, false);
    }
    let Ok(plane) = crate::read::planes::graph::GraphPlane::hydrate(
        db,
        &[workspace.to_string_lossy().into_owned()],
    ) else {
        return (out, false);
    };
    if lower.starts_with("owner of") {
        if let Some(path) = after_of("owner of") {
            let owner = plane.owner(&path);
            out.push(Finding {
                id: format!("owner:{path}"),
                producer: FindingProducer::Cards,
                kind: FindingKind::Project,
                source: source_of(&path),
                start_line: None,
                end_line: None,
                text: format!(
                    "Owner: package {} (component {}).",
                    owner.package,
                    owner.component.as_deref().unwrap_or("none")
                ),
                producer_score: 1.0,
                provenance: "ownership".to_string(),
            });
        }
        return (out, false);
    }
    let target = edge_query_target(query).unwrap_or_default();
    if target.is_empty() {
        return (out, false);
    }
    let scoped = !scope_prefixes.is_empty();
    if lower.starts_with("callees of") {
        let scoped_hits = if scoped {
            plane.callees_in_scope(&target, scope_prefixes)
        } else {
            Vec::new()
        };
        let (hits, narrow) = if scoped_hits.is_empty() {
            (plane.callees(&target), false)
        } else {
            (scoped_hits, true)
        };
        narrowed = narrow;
        for callee in hits {
            out.push(Finding {
                id: format!("callee:{}:{}", callee.file, callee.name),
                producer: FindingProducer::Ast,
                kind: FindingKind::Edge,
                source: source_of(&callee.file),
                start_line: None,
                end_line: None,
                text: format!("calls {}", callee.name),
                producer_score: 1.0,
                provenance: "graph".to_string(),
            });
        }
        return (out, narrowed);
    }
    if lower.starts_with("dependents of") {
        let scoped_hits = if scoped {
            plane.dependents_in_scope(&target, scope_prefixes)
        } else {
            Vec::new()
        };
        let (hits, narrow) = if scoped_hits.is_empty() {
            (plane.dependents(&target), false)
        } else {
            (scoped_hits, true)
        };
        narrowed = narrow;
        for dependent in hits {
            out.push(Finding {
                id: format!("dependent:{dependent}"),
                producer: FindingProducer::Ast,
                kind: FindingKind::Edge,
                source: source_of(&dependent),
                start_line: None,
                end_line: None,
                text: format!("depends on {target}"),
                producer_score: 1.0,
                provenance: "graph".to_string(),
            });
        }
        return (out, narrowed);
    }
    let scoped_callers = if scoped {
        plane.callers_in_scope(
            &target,
            crate::read::planes::graph::CallerFilter::default(),
            scope_prefixes,
        )
    } else {
        Vec::new()
    };
    let (callers, narrow) = if scoped_callers.is_empty() {
        (plane.callers(&target), false)
    } else {
        (scoped_callers, true)
    };
    narrowed = narrow;
    for caller in callers {
        let source = source_of(&caller.file);
        out.push(Finding {
            id: format!(
                "caller:{}:{}",
                source.display(),
                caller.symbol.as_deref().unwrap_or("")
            ),
            producer: FindingProducer::Ast,
            kind: FindingKind::Edge,
            source,
            start_line: None,
            end_line: None,
            text: format!(
                "calls {target} in {}{}",
                caller.symbol.as_deref().unwrap_or("unnamed scope"),
                caller
                    .resolution
                    .render_note()
                    .map(|note| format!(" ({note})"))
                    .unwrap_or_default()
            ),
            producer_score: 1.0,
            provenance: "graph".to_string(),
        });
    }
    (out, narrowed)
}

/// Live excerpt for one definition site: the owning fragment's
/// stored range over live bytes. Falls back to the symbol identity
/// when the fragment or range is gone.
fn symbol_excerpt(
    db: &crate::query::search_db::GuidanceSearchDb,
    site: &crate::read::planes::symbol::SymbolSite,
) -> (String, Option<u32>, Option<u32>) {
    let fragment = db.symbol_fragment(&site.file, &site.name, site.scope.as_deref());
    let Ok(Some((id, range_json))) = fragment else {
        return (
            format!(
                "symbol {} (scope {})",
                site.name,
                site.scope.as_deref().unwrap_or("-")
            ),
            None,
            None,
        );
    };
    let _ = id;
    let range: crate::search_types::FragmentSpan =
        serde_json::from_str(&range_json).unwrap_or(crate::search_types::FragmentSpan::File);
    let (start, end) = match range {
        crate::search_types::FragmentSpan::Text {
            start_line,
            end_line,
            ..
        } => (Some(start_line), Some(end_line)),
        _ => (None, None),
    };
    let (Some(start), Some(end)) = (start, end) else {
        return (format!("symbol {}", site.name), None, None);
    };
    let Ok(text) = std::fs::read_to_string(&site.file) else {
        return (format!("symbol {}", site.name), Some(start), Some(end));
    };
    let lines: Vec<&str> = text.lines().collect();
    let from = start.max(1) as usize - 1;
    let to = (end as usize).min(lines.len());
    if from >= to {
        return (format!("symbol {}", site.name), Some(start), Some(end));
    }
    let excerpt = lines[from..to].join("\n");
    if excerpt.trim().is_empty() {
        return (format!("symbol {}", site.name), Some(start), Some(end));
    }
    (excerpt, Some(start), Some(end))
}

#[cfg(test)]
#[path = "../../tests/read_engine.rs"]
mod read_engine_tests;

#[cfg(test)]
#[path = "../../tests/rg_backend.rs"]
mod rg_backend_tests;

#[cfg(test)]
#[path = "../../tests/graph_callers.rs"]
mod graph_callers_tests;

#[cfg(test)]
#[path = "../../tests/synthesis_evidence.rs"]
mod synthesis_evidence_tests;

#[cfg(test)]
#[path = "../../tests/read_trace.rs"]
mod read_trace_tests;

#[cfg(test)]
#[path = "../../tests/explain_golden.rs"]
mod explain_golden_tests;

#[cfg(test)]
#[path = "../../tests/answer_path.rs"]
mod answer_path_tests;

#[cfg(test)]
#[path = "../../tests/scope_filter.rs"]
mod scope_filter_tests;

#[cfg(test)]
#[path = "../../tests/answer_floor.rs"]
mod answer_floor_tests;

#[cfg(test)]
#[path = "../../tests/payload_trim.rs"]
mod payload_trim_tests;

#[cfg(test)]
#[path = "../../tests/ownership.rs"]
mod ownership_tests;

#[cfg(test)]
#[path = "../../tests/redteam.rs"]
mod redteam_tests;
