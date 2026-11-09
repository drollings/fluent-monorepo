//! Bulk helpers over concrete types with shared-state layout.
//!
//! `decide_all` fans `&[Question]` out over the existing `ResultPool` (no new
//! pool, no bare spawn, no per-call runtime): the state renders once per
//! question from the shared borrow, submissions run concurrently under the
//! caller's cap, and outputs preserve input order via `submit_all`.
//! `bulk_filter` / `bulk_rerank` / `bulk_route` are pure post-processors.

use fluent_concurrency::pool::{ResultPool, ResultPoolError};

use crate::client::LlmClient;
use crate::decision::{decide_verdict, Decision, DecisionError, Question, QuestionSpec};

/// Owned bulk job: the rendered prompt plus everything decoding needs.
/// Borrowed `Question`s render into these before fan-out so the pool owns
/// `'static` work.
struct OwnedJob {
    prompt: String,
    candidates: Vec<String>,
    kind: crate::decision::DecisionKind,
    version: u64,
    timeout_ms: u64,
}

fn pool_error(error: ResultPoolError<DecisionError>) -> DecisionError {
    match error {
        ResultPoolError::Inner(inner) => inner,
        ResultPoolError::Canceled => DecisionError::Unavailable("bulk submit canceled".to_string()),
        ResultPoolError::Pool(pool) => DecisionError::Unavailable(pool.to_string()),
    }
}

/// Decide every question concurrently, preserving input order in the outputs.
///
/// With a configured backend the batch goes through its multi-question
/// path; otherwise each question fans out over the pool. Either way an
/// invalid spec is a loud per-index `Rejected`, never a send.
///
/// ```rust
/// use std::sync::Arc;
/// use fluent_llm::client::{block_on, LlmClient};
/// use fluent_llm::decision::{decide_all, Decision, Question, QuestionSpec, ScriptedDecideBackend};
///
/// let backend = Arc::new(ScriptedDecideBackend::canned(
///     Decision::from_distribution(
///         "B",
///         vec![("B".to_string(), 0.9), ("A".to_string(), 0.1)],
///         "stub".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned"),
/// ));
/// let mut client = LlmClient::new("http://127.0.0.1:9", "stub");
/// client.decide_backend = Some(backend);
/// # let spec = QuestionSpec::new()
/// #     .id("q".to_string()).version(1).kind("choice".to_string())
/// #     .prompt_template("Pick.".to_string()).options_csv("A,B".to_string())
/// #     .act_threshold(0.9).review_threshold(0.7)
/// #     .timeout_ms(1000).cacheable(false).build();
/// let questions = [Question { spec: &spec, state: "st", question: "qq?" }];
/// let out = block_on(decide_all(&client, &questions, 2, 4, None));
/// assert_eq!(out.len(), 1);
/// ```
pub async fn decide_all(
    client: &LlmClient,
    questions: &[Question<'_>],
    cap: usize,
    queue_capacity: usize,
    abort: Option<fluent_concurrency::stream::StreamAbort>,
) -> Vec<Result<Decision, DecisionError>> {
    use crate::decision::{DecideRequest, DecisionOption};
    // Validate before any send: an invalid spec is a loud per-index
    // `Rejected`, never a silently coerced kind or threshold.
    let mut outcomes: Vec<Option<Result<Decision, DecisionError>>> = Vec::new();
    outcomes.resize_with(questions.len(), || None);
    let mut order: Vec<usize> = Vec::new();
    let mut requests: Vec<DecideRequest> = Vec::new();
    for (index, q) in questions.iter().enumerate() {
        match q.spec.validate() {
            Ok(()) => {
                order.push(index);
                requests.push(DecideRequest {
                    prompt: q.render(),
                    state: q.state.to_string(),
                    question: q.question.to_string(),
                    options: q
                        .spec
                        .options()
                        .into_iter()
                        .map(|label| DecisionOption {
                            label,
                            description: String::new(),
                        })
                        .collect(),
                    kind: q
                        .spec
                        .kind_enum()
                        .unwrap_or(crate::decision::DecisionKind::Choice),
                    version: q.spec.version,
                    timeout_ms: q.spec.timeout_ms,
                    model: client.model.clone(),
                });
            }
            Err(detail) => {
                outcomes[index] = Some(Err(DecisionError::Rejected(detail)));
            }
        }
    }
    if !requests.is_empty() {
        if client.decide_backend.is_some() {
            for (slot, result) in order
                .into_iter()
                .zip(client.decide_batch_requests(requests, abort).await)
            {
                outcomes[slot] = Some(result);
            }
        } else {
            let jobs: Vec<OwnedJob> = requests
                .into_iter()
                .map(|r| OwnedJob {
                    prompt: r.prompt,
                    candidates: r.options.into_iter().map(|o| o.label).collect(),
                    kind: r.kind,
                    version: r.version,
                    timeout_ms: r.timeout_ms,
                })
                .collect();
            // Degenerate bounds would wedge or fail every submit; clamp to 1.
            let pool = ResultPool::new(
                fluent_concurrency::tokio_runtime(),
                cap.max(1),
                queue_capacity.max(1),
                {
                    let client = client.clone();
                    move |job: OwnedJob| {
                        let client = client.clone();
                        async move {
                            client
                                .decide_prompt(
                                    job.prompt,
                                    job.candidates,
                                    job.kind,
                                    job.version,
                                    job.timeout_ms,
                                )
                                .await
                        }
                    }
                },
            );
            let mut slots = order.into_iter();
            let results = pool.submit_all(jobs, abort).await;
            pool.shutdown().await;
            for result in results {
                outcomes[slots.next().expect("slot per result")] = Some(result.map_err(pool_error));
            }
        }
    }
    outcomes
        .into_iter()
        .map(|slot| slot.expect("every index filled"))
        .collect()
}

/// Merge per-rotation decisions by summing label masses and renormalizing.
/// The label is the merged argmax; latency sums (total spend); the
/// aggregate is degraded when any input is (mixed evidence is never
/// presented as a clean majority). `None` on empty input.
///
/// ```rust
/// use fluent_llm::decision::{aggregate_rotations, Decision};
///
/// let mk = |top: f64| {
///     Decision::from_distribution(
///         "A",
///         vec![("A".to_string(), top), ("B".to_string(), 1.0 - top)],
///         "m".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned")
/// };
/// let merged = aggregate_rotations(&[mk(0.7), mk(0.8)]).expect("merge");
/// assert_eq!(merged.label, "A");
/// ```
#[must_use]
pub fn aggregate_rotations(decisions: &[Decision]) -> Option<Decision> {
    if decisions.is_empty() {
        return None;
    }
    let mut labels: Vec<String> = Vec::new();
    for decision in decisions {
        for (label, _) in &decision.distribution {
            if !labels.contains(label) {
                labels.push(label.clone());
            }
        }
    }
    let mut masses = vec![0.0f64; labels.len()];
    for decision in decisions {
        let total: f64 = decision.distribution.iter().map(|(_, p)| p).sum();
        if total > 0.0 {
            for (label, mass) in &decision.distribution {
                if let Some(i) = labels.iter().position(|l| l == label) {
                    masses[i] += mass / total;
                }
            }
        }
    }
    let total: f64 = masses.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let distribution: Vec<(String, f64)> = labels
        .into_iter()
        .zip(masses.iter())
        .map(|(label, mass)| (label, mass / total))
        .collect();
    let mut order: Vec<usize> = (0..distribution.len()).collect();
    order.sort_by(|a, b| distribution[*b].1.total_cmp(&distribution[*a].1));
    let first = &decisions[0];
    let mut merged = Decision::from_distribution(
        &distribution[order[0]].0.clone(),
        distribution,
        first.model.clone(),
        first.question_version,
        decisions.iter().map(|d| d.latency_ms).sum(),
        false,
    )
    .ok()?;
    merged.degraded = decisions.iter().any(|d| d.degraded);
    Some(merged)
}

/// True when a non-empty rotation set names one label throughout.
///
/// ```rust
/// use fluent_llm::decision::{rotation_agreement, Decision};
///
/// let mk = |label: &str| {
///     Decision::from_distribution(
///         label,
///         vec![(label.to_string(), 0.9), ("other".to_string(), 0.1)],
///         "m".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned")
/// };
/// assert!(rotation_agreement(&[mk("A"), mk("A")]));
/// assert!(!rotation_agreement(&[mk("A"), mk("B")]));
/// ```
#[must_use]
pub fn rotation_agreement(decisions: &[Decision]) -> bool {
    match decisions.first() {
        None => false,
        Some(first) => decisions.iter().all(|d| d.label == first.label),
    }
}
/// Shuffle-and-aggregate mitigation: `rotations` renderings of one question
/// with seeds `base_seed..`, decided through the batch path and merged by
/// `aggregate_rotations`. Cost is `rotations`× model calls and tokens —
/// spend it where position bias was measured, not by default. Any rotation
/// error fails the whole aggregate loudly; a zero count is rejected.
///
/// ```rust
/// use std::sync::Arc;
/// use fluent_llm::client::{block_on, LlmClient};
/// use fluent_llm::decision::{decide_aggregated, Decision, QuestionSpec, ScriptedDecideBackend};
///
/// let backend = Arc::new(ScriptedDecideBackend::canned(
///     Decision::from_distribution(
///         "B",
///         vec![("B".to_string(), 0.8), ("A".to_string(), 0.2)],
///         "stub".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned"),
/// ));
/// let mut client = LlmClient::new("http://127.0.0.1:9", "stub");
/// client.decide_backend = Some(backend);
/// # let spec = QuestionSpec::new()
/// #     .id("q".to_string()).version(1).kind("choice".to_string())
/// #     .prompt_template("Pick.".to_string()).options_csv("A,B".to_string())
/// #     .act_threshold(0.9).review_threshold(0.7)
/// #     .timeout_ms(1000).cacheable(false).build();
/// let merged = block_on(decide_aggregated(&client, &spec, "st", "qq?", 2, 7, None))
///     .expect("aggregate");
/// assert_eq!(merged.label, "B");
/// ```
pub async fn decide_aggregated(
    client: &LlmClient,
    spec: &QuestionSpec,
    state: &str,
    question: &str,
    rotations: usize,
    base_seed: u64,
    abort: Option<fluent_concurrency::stream::StreamAbort>,
) -> Result<Decision, DecisionError> {
    spec.validate().map_err(DecisionError::Rejected)?;
    if rotations == 0 {
        return Err(DecisionError::Rejected(
            "rotations must be >= 1".to_string(),
        ));
    }
    let specs: Vec<QuestionSpec> = (0..rotations)
        .map(|i| {
            let mut owned = spec.clone();
            owned.seed = base_seed.wrapping_add(i as u64);
            owned
        })
        .collect();
    let questions: Vec<Question<'_>> = specs
        .iter()
        .map(|owned| Question {
            spec: owned,
            state,
            question,
        })
        .collect();
    let outcomes = decide_all(
        client,
        &questions,
        rotations.max(1),
        rotations.max(1).saturating_mul(2),
        abort,
    )
    .await;
    let mut merged: Vec<Decision> = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        merged.push(outcome?);
    }
    aggregate_rotations(&merged)
        .ok_or_else(|| DecisionError::Decode("rotation aggregate found no mass".to_string()))
}
/// Hierarchical routing for option sets beyond the single-token cap:
/// chunks of at most `MAX_DECISION_OPTIONS` decide in one batch round, each
/// chunk's top label advances, and the winners meet in a final round.
/// Best-effort ordering (a weak chunk winner can eliminate a strong
/// runner-up); calibrate per chunk where it matters. Any round error fails
/// the tournament loudly.
///
/// ```rust
/// use std::sync::Arc;
/// use fluent_llm::client::{block_on, LlmClient};
/// use fluent_llm::decision::{route_hierarchical, Decision, QuestionSpec, ScriptedDecideBackend};
///
/// let backend = Arc::new(ScriptedDecideBackend::canned(
///     Decision::from_distribution(
///         "B",
///         vec![("B".to_string(), 0.9), ("A".to_string(), 0.1)],
///         "stub".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned"),
/// ));
/// let mut client = LlmClient::new("http://127.0.0.1:9", "stub");
/// client.decide_backend = Some(backend);
/// # let spec = QuestionSpec::new()
/// #     .id("q".to_string()).version(1).kind("choice".to_string())
/// #     .prompt_template("Pick.".to_string()).options_csv("A,B".to_string())
/// #     .act_threshold(0.9).review_threshold(0.7)
/// #     .timeout_ms(1000).cacheable(false).build();
/// let winner = block_on(route_hierarchical(&client, &spec, "st", "qq?", None))
///     .expect("tournament");
/// assert_eq!(winner.label, "B");
/// ```
pub async fn route_hierarchical(
    client: &LlmClient,
    spec: &QuestionSpec,
    state: &str,
    question: &str,
    abort: Option<fluent_concurrency::stream::StreamAbort>,
) -> Result<Decision, DecisionError> {
    use crate::decision::MAX_DECISION_OPTIONS;
    if spec.options().len() < 2 {
        return Err(DecisionError::Rejected(
            "hierarchical routing needs at least two options".to_string(),
        ));
    }
    let mut round: Vec<String> = spec.options();
    loop {
        let chunks: Vec<Vec<String>> = round
            .chunks(MAX_DECISION_OPTIONS)
            .map(<[String]>::to_vec)
            .collect();
        let subs: Vec<QuestionSpec> = chunks
            .into_iter()
            .map(|chunk| {
                let mut owned = spec.clone();
                owned.options_csv = chunk.join(",");
                owned
            })
            .collect();
        for sub in &subs {
            sub.validate().map_err(DecisionError::Rejected)?;
        }
        let questions: Vec<Question<'_>> = subs
            .iter()
            .map(|owned| Question {
                spec: owned,
                state,
                question,
            })
            .collect();
        let outcomes = decide_all(
            client,
            &questions,
            subs.len().max(1),
            subs.len().max(1).saturating_mul(2),
            abort.clone(),
        )
        .await;
        let mut winners: Vec<Decision> = Vec::with_capacity(outcomes.len());
        for outcome in outcomes {
            winners.push(outcome?);
        }
        if winners.len() == 1 {
            // Single chunk: the round decision stands (also the small-set
            // path — one send, no extra round).
            return Ok(winners.into_iter().next().expect("one chunk, one winner"));
        }
        round = winners.into_iter().map(|w| w.label).collect();
    }
}

/// One-call Noul filter over statements: per statement, `true` where the
/// answer is `Yes` and the spec's own verdict acts. Built on `decide_all`;
/// anything else (errors included) surfaces per statement.
///
/// ```rust
/// use std::sync::Arc;
/// use fluent_llm::client::{block_on, LlmClient};
/// use fluent_llm::decision::{filter_noul, Decision, QuestionSpec, ScriptedDecideBackend};
///
/// let backend = Arc::new(ScriptedDecideBackend::canned(
///     Decision::from_distribution(
///         "Yes",
///         vec![("Yes".to_string(), 0.97), ("No".to_string(), 0.03)],
///         "stub".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned"),
/// ));
/// let mut client = LlmClient::new("http://127.0.0.1:9", "stub");
/// client.decide_backend = Some(backend);
/// # let spec = QuestionSpec::new()
/// #     .id("q".to_string()).version(1).kind("noul".to_string())
/// #     .prompt_template("True?".to_string()).options_csv("Yes,No".to_string())
/// #     .act_threshold(0.9).review_threshold(0.7)
/// #     .timeout_ms(1000).cacheable(false).build();
/// let kept = block_on(filter_noul(&client, &spec, "st", &["water is wet".to_string()], None));
/// assert_eq!(kept.len(), 1);
/// ```
pub async fn filter_noul(
    client: &LlmClient,
    spec: &QuestionSpec,
    state: &str,
    statements: &[String],
    abort: Option<fluent_concurrency::stream::StreamAbort>,
) -> Vec<Result<bool, DecisionError>> {
    if spec.validate().is_err()
        || spec
            .kind_enum()
            .is_ok_and(|k| k != crate::decision::DecisionKind::Noul)
    {
        return statements
            .iter()
            .map(|_| {
                Err(DecisionError::Rejected(
                    "filter_noul needs a valid Noul spec".to_string(),
                ))
            })
            .collect();
    }
    let thresholds = spec.thresholds();
    let questions: Vec<Question<'_>> = statements
        .iter()
        .map(|statement| Question {
            spec,
            state,
            question: statement,
        })
        .collect();
    decide_all(
        client,
        &questions,
        statements.len().max(1),
        statements.len().max(1).saturating_mul(2),
        abort,
    )
    .await
    .into_iter()
    .map(|outcome| {
        outcome.map(|d| {
            d.label == "Yes" && decide_verdict(&d, &thresholds) == crate::decision::Verdict::Act
        })
    })
    .collect()
}

/// One-call Choice route: the winning label. Built on `decide_all`.
///
/// ```rust
/// use std::sync::Arc;
/// use fluent_llm::client::{block_on, LlmClient};
/// use fluent_llm::decision::{route_choice, Decision, QuestionSpec, ScriptedDecideBackend};
///
/// let backend = Arc::new(ScriptedDecideBackend::canned(
///     Decision::from_distribution(
///         "B",
///         vec![("B".to_string(), 0.9), ("A".to_string(), 0.1)],
///         "stub".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned"),
/// ));
/// let mut client = LlmClient::new("http://127.0.0.1:9", "stub");
/// client.decide_backend = Some(backend);
/// # let spec = QuestionSpec::new()
/// #     .id("q".to_string()).version(1).kind("choice".to_string())
/// #     .prompt_template("Pick.".to_string()).options_csv("A,B".to_string())
/// #     .act_threshold(0.9).review_threshold(0.7)
/// #     .timeout_ms(1000).cacheable(false).build();
/// assert_eq!(block_on(route_choice(&client, &spec, "st", "qq?", None)).expect("route"), "B");
/// ```
pub async fn route_choice(
    client: &LlmClient,
    spec: &QuestionSpec,
    state: &str,
    question: &str,
    abort: Option<fluent_concurrency::stream::StreamAbort>,
) -> Result<String, DecisionError> {
    if spec.validate().is_err()
        || spec
            .kind_enum()
            .is_ok_and(|k| k != crate::decision::DecisionKind::Choice)
    {
        return Err(DecisionError::Rejected(
            "route_choice needs a valid Choice spec".to_string(),
        ));
    }
    let questions = [Question {
        spec,
        state,
        question,
    }];
    decide_all(client, &questions, 1, 2, abort)
        .await
        .into_iter()
        .next()
        .expect("one question in, one outcome out")
        .map(|d| d.label)
}

/// One-call Score rank over items: `(index, expected rung)` descending.
/// Degraded moments are uncalibrated and excluded; the first transport
/// error fails the whole rank loudly.
///
/// ```rust
/// use std::sync::Arc;
/// use fluent_llm::client::{block_on, LlmClient};
/// use fluent_llm::decision::{rank_score, Decision, QuestionSpec, ScriptedDecideBackend};
///
/// let backend = Arc::new(ScriptedDecideBackend::canned(
///     Decision::from_distribution(
///         "5",
///         vec![("4".to_string(), 0.1), ("5".to_string(), 0.9)],
///         "stub".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned"),
/// ));
/// let mut client = LlmClient::new("http://127.0.0.1:9", "stub");
/// client.decide_backend = Some(backend);
/// # let spec = QuestionSpec::new()
/// #     .id("q".to_string()).version(1).kind("score".to_string())
/// #     .prompt_template("Rate.".to_string()).options_csv("4,5".to_string())
/// #     .act_threshold(0.9).review_threshold(0.7)
/// #     .timeout_ms(1000).cacheable(false).build();
/// let ranked = block_on(rank_score(&client, &spec, "st", &["item".to_string()], None))
///     .expect("rank");
/// assert_eq!(ranked.len(), 1);
/// ```
pub async fn rank_score(
    client: &LlmClient,
    spec: &QuestionSpec,
    state: &str,
    items: &[String],
    abort: Option<fluent_concurrency::stream::StreamAbort>,
) -> Result<Vec<(usize, f64)>, DecisionError> {
    if spec.validate().is_err()
        || spec
            .kind_enum()
            .is_ok_and(|k| k != crate::decision::DecisionKind::Score)
    {
        return Err(DecisionError::Rejected(
            "rank_score needs a valid Score spec".to_string(),
        ));
    }
    let questions: Vec<Question<'_>> = items
        .iter()
        .map(|item| Question {
            spec,
            state,
            question: item,
        })
        .collect();
    let outcomes = decide_all(
        client,
        &questions,
        items.len().max(1),
        items.len().max(1).saturating_mul(2),
        abort,
    )
    .await;
    let mut ranked: Vec<(usize, f64)> = Vec::new();
    for (index, outcome) in outcomes.into_iter().enumerate() {
        let decision = outcome?;
        if decision.degraded {
            continue;
        }
        if let Some(moment) = crate::decision::score_moment(&decision) {
            ranked.push((index, moment.expected));
        }
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(ranked)
}
/// Indices whose verdict is `Act` or `Review` at the spec's own thresholds.
/// Invalid thresholds fail closed to empty: no verdict is trustworthy.
///
/// ```rust
/// use fluent_llm::decision::{bulk_filter, Decision, QuestionSpec};
///
/// # let spec = QuestionSpec::new()
/// #     .id("q".to_string()).version(1).kind("choice".to_string())
/// #     .prompt_template("Pick.".to_string()).options_csv("A,B".to_string())
/// #     .act_threshold(0.9).review_threshold(0.7)
/// #     .timeout_ms(1000).cacheable(false).build();
/// let decisions = vec![
///     Decision::from_distribution(
///         "A",
///         vec![("A".to_string(), 0.95), ("B".to_string(), 0.05)],
///         "m".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned"),
/// ];
/// assert_eq!(bulk_filter(&spec, &decisions), vec![0]);
/// ```
#[must_use]
pub fn bulk_filter(spec: &QuestionSpec, decisions: &[Decision]) -> Vec<usize> {
    let Ok(()) = spec.validate() else {
        return Vec::new();
    };
    let thresholds = spec.thresholds();
    decisions
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            !matches!(
                decide_verdict(d, &thresholds),
                crate::decision::Verdict::Escalate
            )
        })
        .map(|(i, _)| i)
        .collect()
}

/// Indices ordered by confidence descending (stable).
///
/// ```rust
/// use fluent_llm::decision::{bulk_rerank, Decision};
///
/// let mk = |top: f64| {
///     Decision::from_distribution(
///         "A",
///         vec![("A".to_string(), top), ("B".to_string(), 1.0 - top)],
///         "m".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned")
/// };
/// assert_eq!(bulk_rerank(&[mk(0.6), mk(0.9)]), vec![1, 0]);
/// ```
#[must_use]
pub fn bulk_rerank(decisions: &[Decision]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..decisions.len()).collect();
    order.sort_by(|a, b| {
        decisions[*b]
            .confidence
            .total_cmp(&decisions[*a].confidence)
    });
    order
}

/// Highest-confidence non-escalated decision with its index, if any.
///
/// ```rust
/// use fluent_llm::decision::{bulk_route, Decision, QuestionSpec};
///
/// # let spec = QuestionSpec::new()
/// #     .id("q".to_string()).version(1).kind("choice".to_string())
/// #     .prompt_template("Pick.".to_string()).options_csv("A,B".to_string())
/// #     .act_threshold(0.9).review_threshold(0.7)
/// #     .timeout_ms(1000).cacheable(false).build();
/// let decisions = vec![
///     Decision::from_distribution(
///         "A",
///         vec![("A".to_string(), 0.95), ("B".to_string(), 0.05)],
///         "m".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned"),
/// ];
/// assert!(bulk_route(&spec, &decisions).is_some());
/// ```
/// Invalid thresholds fail closed to `None`.
#[must_use]
pub fn bulk_route(spec: &QuestionSpec, decisions: &[Decision]) -> Option<(usize, Decision)> {
    if spec.validate().is_err() {
        return None;
    }
    let thresholds = spec.thresholds();
    decisions
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            !matches!(
                decide_verdict(d, &thresholds),
                crate::decision::Verdict::Escalate
            )
        })
        .max_by(|(_, a), (_, b)| a.confidence.total_cmp(&b.confidence))
        .map(|(i, d)| (i, d.clone()))
}
