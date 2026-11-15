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
/// Which producer surfaced a finding. `database` is FTS/vector/lemma
/// recall, `ast` is symbol/edge extraction, `ripgrep` is the
/// exhaustive L0 arm, and `cards` is project/capability/doc metadata.
/// Every producer emits the same `Finding` through the same filter.
pub enum FindingProducer {
    Cards,
    Database,
    Ast,
    Ripgrep,
}

/// What a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// What a finding is.
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
/// One candidate from any producer: the model judges findings, it
/// never selects retrieval, so every producer funnels through this
/// one shape.
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

/// One rendered answer section.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One rendered answer section.
pub struct ReadSection {
    /// Section title (`From the project map`, `Top matches`, …).
    pub title: String,
    /// Markdown body, budget-truncated at assembly.
    pub body: String,
}

/// One read request.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One read request.
pub struct ReadRequest {
    /// Natural-language question, run verbatim.
    pub query: String,
    /// Section budget override (tokens); per-intent table otherwise.
    pub budget_override: Option<usize>,
}

/// One explain request: evidence assembly over one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One explain request: evidence assembly over one workspace.
pub struct ExplainRequest {
    /// Natural-language question, run verbatim.
    pub query: String,
    /// Code finding limit.
    pub limit: usize,
    /// Section budget override (tokens); per-intent table otherwise.
    pub budget_override: Option<usize>,
    /// Extractive evidence only: no synthesis call on any path.
    pub no_llm: bool,
}

/// Retrieval context for one explain call (borrowed, no clones).
pub struct ExplainContext<'a> {
    /// Workspace root (live reads resolve under it).
    pub workspace: &'a std::path::Path,
    /// Committed index.
    pub db: &'a search_vector::db::GuidanceDb,
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
}

/// Judge input for shadow labelling: backend plus fingerprint. `None`
/// fingerprint means shadow-only (M7 authorizes filtering).
pub struct JudgeCtx<'a> {
    /// Decision backend (SystemOne).
    pub backend: &'a dyn fluent_llm::decision::DecideBackend,
    /// Model name for the backend.
    pub model: String,
    /// Timeout per question.
    pub timeout_ms: u64,
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
        fluent_llm::decision::DecisionCache::new(256).expect("judge cache capacity")
    })
}

/// The execution plan for one request: intent plus the producers and
/// budgets each plane gets. Pure and testable without any retrieval.
#[derive(Debug, Clone, PartialEq, Eq)]
/// The execution plan for one request: intent plus the producers and
/// budgets each plane gets. Pure and testable without any retrieval.
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
}

/// Primary plane route for one intent.
#[must_use]
/// Primary plane route for one intent.
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

/// One read answer: intent, budgeted sections, and coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One read answer: intent, budgeted sections, and coverage.
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
}

/// The read engine. Planes are not yet implemented: `plan` resolves
/// intent and budgets, `answer` assembles (currently empty) sections
/// with the caller's coverage.
pub struct ReadEngine;

impl ReadEngine {
    /// Build an engine. Configuration rides the project config
    /// Construction is cheap; callers own the workspace and index.
    #[must_use]
    pub fn new() -> Self {
        Self
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
        use crate::read::budget::tokens_for;
        let plan_request = ReadRequest {
            query: request.query.clone(),
            budget_override: request.budget_override,
        };
        let plan = self.plan(&plan_request);
        let mut sections = Vec::new();
        let mut tokens_used = 0u64;
        let mut push = |title: &str, body: String, tokens_used: &mut u64| {
            *tokens_used += tokens_for(&body);
            sections.push(ReadSection {
                title: title.to_string(),
                body,
            });
        };
        push(
            "Coverage",
            coverage_section(ctx.coverage.as_ref(), &plan),
            &mut tokens_used,
        );
        let cards = orientation_cards(&plan, &request.query, ctx.db);
        push(
            "From the project map",
            crate::read::render::render_card_section(
                "From the project map",
                &cards,
                plan.budgets.project,
            ),
            &mut tokens_used,
        );
        let cap_hits = crate::read::planes::docs::retrieve_cards(
            &request.query,
            Some(ctx.workspace),
            ctx.db,
            ctx.nlp,
            None,
        );
        let doc_hits = if matches!(
            plan.intent,
            QueryIntent::SingleIdentifier | QueryIntent::FilePath | QueryIntent::Graph
        ) {
            Vec::new()
        } else {
            crate::read::planes::docs::retrieve_doc_sections(
                &request.query,
                Some(ctx.workspace),
                ctx.db,
                // Bounded doc funnel: cost cap, not relevance. Fewer live
                // verifications keep p50/p95 within budget; recall is held
                // by the budget-capped evidence sections.
                plan.budgets.evidence.max(8) / 32,
            )
        };
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
        push(
            "From the capabilities",
            crate::read::render::render_evidence_section(
                "From the capabilities",
                &cap_entries,
                plan.budgets.evidence.saturating_sub(half_evidence),
            ),
            &mut tokens_used,
        );
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
        // Symbol/Graph intents resolve through the structure/graph
        // planes first; exact sites head Top matches, code recall
        // follows. Unknown symbols read as honest gaps.
        let plane_findings = structure_findings(&request.query, plan.intent, ctx.workspace, ctx.db);
        // Bounded ripgrep arm: exact/symbol/graph/path intents only when
        // the index yields no product code, so a stale index cannot hide
        // a literal. Broad language stays index-only. Cost caps, never
        // silent truncation.
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
        let code_findings = evidence.into_findings();
        gaps.extend(rg_gaps);
        // Negative-evidence floor: exact symbol/edge, product code, or a
        // verified doc/card satisfies it; project cards satisfy it only for
        // Orientation (they are context elsewhere). Otherwise suppress with
        // an honest gap. Lexical scores alone never satisfy it.
        let floor_met = !code_findings.is_empty()
            || doc_hits.iter().any(|hit| hit.verified)
            || cap_hits.iter().any(|hit| hit.verified)
            || (plan.intent == QueryIntent::Orientation && !cards.is_empty());
        if !floor_met {
            gaps.push("none found in covered index".to_string());
        }
        // Whole findings only: truncation cuts between findings, never
        // mid-fence, so citations always name a rendered row.
        let mut kept_code = cap_findings_by_budget(&code_findings, plan.budgets.code);
        // Shadow judge: labels render, nothing filters, rank order kept.
        // `None` judge is byte-identical to the extractive path. Authorized
        // filtering (M7) drops `Irrelevant`, gaps `Insufficient`, and keeps
        // `Relevant` plus deterministic exact evidence; support gating needs
        // its own point. Caches stay cold until authorized.
        let mut judge_labels: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut finding_authorized = false;
        let mut support_authorized = false;
        if let Some(judge_ctx) = judge.as_ref() {
            // Authorization through the loaded registry, never a model
            // verdict: only `Calibrated` filters.
            let registry_path = ctx.workspace.join(".guidance/state/calibration.json");
            let registry = std::fs::read(&registry_path)
                .ok()
                .and_then(|bytes| {
                    serde_json::from_slice::<fluent_llm::decision::CalibrationRegistry>(&bytes).ok()
                })
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
                    judge_ctx.timeout_ms,
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
            if judge_ctx.fingerprint.is_none() {
                gaps.push("decision fingerprint absent: shadow-only".to_string());
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
                        }
                        FindingLabel::Irrelevant => {
                            // Deterministic exact evidence is never dropped:
                            // symbol/graph structure is authoritative.
                            if finding.producer == FindingProducer::Ast {
                                filtered.push(finding);
                            }
                        }
                    }
                }
                kept_code = filtered;
            }
        }
        let top_body = if judge_labels.is_empty() {
            format!(
                "### Top matches\n{}",
                crate::read::render::render_code_findings(&kept_code)
            )
        } else {
            format!(
                "### Top matches\n{}",
                render_code_findings_with_labels(&kept_code, &judge_labels)
            )
        };
        push("Top matches", top_body, &mut tokens_used);
        // The shown set is the evidence set: citations and synthesis
        // draw from capped findings, never from trimmed rows.
        // Live bytes win over stored ranges: edits shift lines without
        // re-syncing, so excerpts re-resolve before they render.
        let (code_findings, live_gaps) =
            crate::read::planes::code::resolve_live_findings(ctx.workspace, kept_code);
        gaps.extend(live_gaps);
        // Freshness is labelled on the row, never hidden: findings
        // whose source moved since indexing cite with a ` (stale)`
        // marker. Strict callers refuse on marked citations; everyone
        // else reads the label and the degraded coverage beside it.
        let stale = stale_sources(ctx, &code_findings);
        let mut citations: Vec<crate::source_ref::Citation> = code_findings
            .iter()
            .map(|finding| {
                finding
                    .source
                    .citation(finding.start_line, finding.end_line)
                    .with_status(stale.contains(finding.source.display()), false)
            })
            .collect();
        for hit in &doc_hits {
            if hit.verified {
                citations.push(
                    crate::source_ref::SourceRef::from_workspace(
                        ctx.workspace,
                        std::path::Path::new(&hit.source_path),
                    )
                    .citation(None, None),
                );
            }
        }
        for hit in &cap_hits {
            if hit.verified {
                citations.push(
                    crate::source_ref::SourceRef::from_workspace(
                        ctx.workspace,
                        std::path::Path::new(&hit.source_path),
                    )
                    .citation(None, None),
                );
            }
        }
        if plan.intent == QueryIntent::Orientation {
            let mut project_citations = Vec::new();
            let manifest = ctx.workspace.join("Cargo.toml");
            if manifest.is_file() {
                project_citations.push(
                    crate::source_ref::SourceRef::from_workspace(
                        ctx.workspace,
                        std::path::Path::new("Cargo.toml"),
                    )
                    .citation(None, None),
                );
            }
            if let Ok(project_cards) = ctx.db.project_cards_all() {
                for card in project_cards
                    .iter()
                    .filter(|card| matches!(card.kind.as_str(), "workspace" | "crate" | "domain"))
                {
                    let source = card.source_path.split('#').next().unwrap_or("");
                    if !source.is_empty() {
                        project_citations.push(
                            crate::source_ref::SourceRef::from_workspace(
                                ctx.workspace,
                                std::path::Path::new(source),
                            )
                            .citation(None, None),
                        );
                    }
                }
            }
            citations.splice(0..0, project_citations);
        }
        push(
            "Citations",
            crate::read::render::render_evidence_section(
                "Citations",
                &citations
                    .iter()
                    .map(|cite| (cite.text(), String::new()))
                    .collect::<Vec<_>>(),
                plan.budgets.code / 2,
            ),
            &mut tokens_used,
        );
        // Synthesis over evidence, support-gated: unsupported sentences
        // drop, and an empty gate means evidence-only (no answer
        // section). Until claim support calibrates, kept sentences are
        // `[unverified]` and the evidence sections stay authoritative.
        let mut claims = Vec::new();
        let mut answer_body: Option<String> = None;
        if !request.no_llm && !code_findings.is_empty() {
            match synthesize {
                None => gaps.push("synthesis unavailable: evidence only".to_string()),
                Some(synth) => {
                    let prompt = synthesis_prompt(&request.query, &code_findings);
                    match synth.synthesize(&prompt) {
                        Ok(Some(text)) => {
                            let constrained = crate::read::claims::constrain_answer_to_evidence(
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
                            );
                            claims = crate::read::claims::bind_claims(
                                &constrained,
                                &citations
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect::<Vec<_>>(),
                            );
                            if support_authorized {
                                for claim in &mut claims {
                                    if claim.citation.is_some()
                                        && claim.support == crate::read::claims::Support::Unverified
                                    {
                                        claim.support = crate::read::claims::Support::Verified;
                                    }
                                }
                            }
                            match crate::read::claims::gate_answer(&claims) {
                                Some(gated) => answer_body = Some(gated),
                                None => {
                                    gaps.push("no supported sentence: evidence only".to_string());
                                }
                            }
                        }
                        Ok(None) => {
                            answer_body = Some("(unavailable: empty answer)".to_string());
                        }
                        Err(error) => {
                            let detail = error.trim().chars().take(200).collect::<String>();
                            answer_body = Some(format!("(unavailable: {detail})"));
                        }
                    }
                }
            }
        }
        if cards.is_empty() && plan.intent == QueryIntent::Orientation {
            gaps.push("no project cards: orientation degrades to docs".to_string());
        }
        let degraded = !gaps.is_empty();
        let mut ordered = Vec::new();
        if let Some(body) = answer_body {
            let body = format!("### Answer\n{body}");
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
    out
}

/// Project-map cards for the plan intent: orientation serves the
/// workspace-first card order; ownership scopes by the first
/// identifier-shaped term, falling back to the full order.
fn orientation_cards(
    plan: &ReadPlan,
    query: &str,
    db: &search_vector::db::GuidanceDb,
) -> Vec<(String, String)> {
    let Ok(plane) = crate::read::planes::orientation::OrientationPlane::load(db) else {
        return Vec::new();
    };
    let cards = if plan.intent == QueryIntent::Ownership {
        let package = crate::read::vocabulary::symbol_terms(query)
            .into_iter()
            .next()
            .unwrap_or_default();
        let scoped = plane.ownership_cards(&package);
        if scoped.is_empty() {
            plane.orientation_cards()
        } else {
            scoped
        }
    } else {
        plane.orientation_cards()
    };
    cards
        .into_iter()
        .map(|card| (card.title, card.body))
        .collect()
}

/// Cap findings by token estimate, whole findings only: the first
/// finding always renders (an empty Top matches answers nothing),
/// then each further finding while it fits. Truncation cuts between
/// findings, never mid-fence, so every citation names a rendered
/// row. Pure and budget-exact for the perf tests to pin.
#[must_use]
/// Cap findings by token estimate, whole findings only: the first
/// finding always renders (an empty Top matches answers nothing),
/// then each further finding while it fits. Truncation cuts between
/// findings, never mid-fence, so every citation names a rendered
/// row. Pure and budget-exact for the perf tests to pin.
pub fn cap_findings_by_budget(
    findings: &[crate::read::engine::Finding],
    budget_tokens: usize,
) -> Vec<crate::read::engine::Finding> {
    let mut kept = Vec::new();
    let mut used = 0u64;
    for finding in findings {
        let cost = crate::read::budget::tokens_for(&finding.text);
        if !kept.is_empty() && used + cost > budget_tokens as u64 {
            break;
        }
        used += cost;
        kept.push(finding.clone());
    }
    kept
}

/// Evidence prompt for the synthesizer: question plus budgeted
/// finding excerpts with ids, source anchors, and verbatim text. Only
/// shown ids may be cited; `constrain_answer_to_evidence` enforces it.
fn synthesis_prompt(query: &str, findings: &[crate::read::engine::Finding]) -> String {
    use std::fmt::Write as _;
    let mut prompt = format!("Question: {query}\nEvidence:\n");
    for finding in findings.iter().take(10) {
        let _ = writeln!(
            prompt,
            "- [{}] {}: {}",
            finding.id,
            finding.source.display(),
            finding.text
        );
    }
    prompt.push_str("Answer with cited facts only. Cite only shown ids.");
    prompt
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

/// Exact-structure findings for Symbol/Graph intents: definitions,
/// call sites, and owners through the committed graph. Unknown
/// symbols yield no findings (the caller records the gap).
fn structure_findings(
    query: &str,
    intent: QueryIntent,
    workspace: &std::path::Path,
    db: &search_vector::db::GuidanceDb,
) -> Vec<crate::read::engine::Finding> {
    use crate::read::engine::{Finding, FindingKind, FindingProducer};
    let mut out = Vec::new();
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
        return out;
    }
    if intent != QueryIntent::Graph {
        return out;
    }
    let Ok(plane) = crate::read::planes::graph::GraphPlane::hydrate(
        db,
        &[workspace.to_string_lossy().into_owned()],
    ) else {
        return out;
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
        return out;
    }
    let target = after_of("callers of")
        .or_else(|| after_of("callees of"))
        .or_else(|| after_of("dependents of"))
        .unwrap_or_default();
    if target.is_empty() {
        return out;
    }
    if lower.starts_with("callees of") {
        for callee in plane.callees(&target) {
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
        return out;
    }
    if lower.starts_with("dependents of") {
        for dependent in plane.dependents(&target) {
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
        return out;
    }
    for caller in plane.callers(&target) {
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
    out
}

/// Live excerpt for one definition site: the owning fragment's
/// stored range over live bytes. Falls back to the symbol identity
/// when the fragment or range is gone.
fn symbol_excerpt(
    db: &search_vector::db::GuidanceDb,
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
