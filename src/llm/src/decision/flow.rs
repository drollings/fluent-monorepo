//! Low-boilerplate logical flow over decision models (M7.2).
//!
//! The `bulk`/`ladder`/`question` modules own the mechanisms (fan-out,
//! tournaments, specs); this module owns the call shapes an
//! orchestrator actually writes: a semantic if, a multi-arm match, a
//! one-call rerank, and a per-node tree descent. Specs are built
//! internally from one [`FlowAsk`]; failures resolve to the
//! caller's fail-closed default, never to an exception-shaped
//! verdict.

use crate::decision::{
    decide_all, match_text_label, rank_score, Decision, Question, QuestionSpec,
    MAX_DECISION_OPTIONS,
};
use crate::LlmClient;

/// Shared ask parameters for the flow helpers: question identity,
/// act gate, and per-call timeout. Thresholds stay explicit —
/// there is no silent default gate.
#[derive(Debug, Clone)]
/// Shared ask parameters for the flow helpers: question identity,
/// act gate, and per-call timeout. Thresholds stay explicit —
/// there is no silent default gate.
pub struct FlowAsk {
    /// Question id base (`:if`, `:match`, … suffixed per helper).
    pub id: String,
    /// Question version.
    pub version: u64,
    /// Act gate on producer confidence.
    pub act: f64,
    /// Per-call timeout in ms.
    pub timeout_ms: u64,
}

impl FlowAsk {
    /// Minimal ask: gate at `act`, default identity and timeout.
    #[must_use]
    pub fn ask(act: f64) -> Self {
        Self {
            id: "flow".to_string(),
            version: 1,
            act,
            timeout_ms: 2000,
        }
    }
}

impl Default for FlowAsk {
    fn default() -> Self {
        Self::ask(0.5)
    }
}

fn choice_spec(id: &str, version: u64, template: &str, options: &str, ask: &FlowAsk) -> QuestionSpec {
    QuestionSpec::new()
        .id(id.to_string())
        .version(version)
        .kind("choice".to_string())
        .prompt_template(template.to_string())
        .options_csv(options.to_string())
        .act_threshold(ask.act)
        .review_threshold(0.0)
        .timeout_ms(ask.timeout_ms)
        .cacheable(false)
        .build()
}

/// Semantic if: ask a yes/no question, take the `then` branch only
/// when the winner is "yes" at or above `ask.act`. Anything else —
/// "no", an unknown label, a below-gate "yes" — is false; transport
/// and decoding failures return `default` (fail-closed).
pub async fn semantic_if(
    client: &LlmClient,
    state: &str,
    question: &str,
    ask: &FlowAsk,
    default: bool,
) -> bool {
    let spec = choice_spec(
        &format!("{}:if", ask.id),
        ask.version,
        "Answer yes or no.",
        "yes,no",
        ask,
    );
    if spec.validate().is_err() {
        return default;
    }
    let questions = [Question {
        spec: &spec,
        state,
        question,
    }];
    let mut outcomes = decide_all(client, &questions, 1, 2, None).await;
    let Some(outcome) = outcomes.pop() else {
        return default;
    };
    match outcome {
        Ok(decision)
            if match_text_label(&decision.label, &["yes", "no"]) == Some("yes")
                && decision.confidence >= ask.act =>
        {
            true
        }
        Ok(_) => false,
        Err(_) => default,
    }
}

/// Multi-arm match: one choice question over 2..=MAX options, return
/// the winning arm index plus the decision. An unknown winner label
/// falls back to `default` (kept with its decision); transport and
/// validation failures return `(default, None)`.
pub async fn match_arm(
    client: &LlmClient,
    state: &str,
    question: &str,
    arms: &[String],
    ask: &FlowAsk,
    default: usize,
) -> (usize, Option<Decision>) {
    if arms.len() < 2 || arms.len() > MAX_DECISION_OPTIONS {
        return (default, None);
    }
    let options = arms.join(",");
    let spec = choice_spec(
        &format!("{}:match", ask.id),
        ask.version,
        "Pick one.",
        &options,
        ask,
    );
    if spec.validate().is_err() {
        return (default, None);
    }
    let questions = [Question {
        spec: &spec,
        state,
        question,
    }];
    let mut outcomes = decide_all(client, &questions, 1, 2, None).await;
    let Some(outcome) = outcomes.pop() else {
        return (default, None);
    };
    match outcome {
        Ok(decision) => {
            let options: Vec<&str> = arms.iter().map(String::as_str).collect();
            let index = match_text_label(&decision.label, &options)
                .and_then(|label| arms.iter().position(|arm| arm == label))
                .unwrap_or(default);
            (index, Some(decision))
        }
        Err(_) => (default, None),
    }
}

/// One item to rank: id plus the text the Score question reads.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One item to rank: id plus the text the Score question reads.
pub struct RerankItem {
    /// Stable id (returned positions index the input slice).
    pub id: String,
    /// Text the Score question grades.
    pub text: String,
}

/// One-call rerank: score every item text on a 1–5 rung scale, order
/// by expected rung descending, return input positions. Transport
/// failure fails open to input order (rank preserved, never
/// reordered on no signal).
pub async fn rerank_items(
    client: &LlmClient,
    state: &str,
    items: &[RerankItem],
    ask: &FlowAsk,
) -> Vec<usize> {
    if items.is_empty() {
        return Vec::new();
    }
    let spec = QuestionSpec::new()
        .id(format!("{}:rerank", ask.id))
        .version(ask.version)
        .kind("score".to_string())
        .prompt_template("Rate relevance from 1 to 5.".to_string())
        .options_csv("1,2,3,4,5".to_string())
        .act_threshold(ask.act)
        .review_threshold(0.0)
        .timeout_ms(ask.timeout_ms)
        .cacheable(false)
        .build();
    if spec.validate().is_err() {
        return (0..items.len()).collect();
    }
    let texts: Vec<String> = items.iter().map(|item| item.text.clone()).collect();
    match rank_score(client, &spec, state, &texts, None).await {
        Ok(ranked) => {
            let mut order: Vec<usize> = ranked.into_iter().map(|(index, _)| index).collect();
            // `rank_score` drops degraded/unscored items; keep every
            // input exactly once, unscored tail in input order.
            for index in 0..items.len() {
                if !order.contains(&index) {
                    order.push(index);
                }
            }
            order
        }
        Err(_) => (0..items.len()).collect(),
    }
}

/// One tree level: its own question, options, and act gate.
#[derive(Debug, Clone, PartialEq)]
/// One tree level: its own question, options, and act gate.
pub struct TreeLevel {
    /// State block for this level.
    pub state: String,
    /// Question for this level.
    pub question: String,
    /// Closed option labels for this level.
    pub options: Vec<String>,
    /// Act gate: descend while winners stay below it.
    pub act: f64,
}

/// Descent outcome: the (level, winner) path walked, the settling
/// decision if any level cleared its gate, and whether the walk
/// settled or ran out/failed.
#[derive(Debug, Clone, Default)]
/// Descent outcome: the (level, winner) path walked, the settling
/// decision if any level clears its gate, and whether the walk
/// settled or ran out/failed.
pub struct DescentOutcome {
    /// `(level index, winner label)` for every level asked.
    pub path: Vec<(usize, String)>,
    /// Settling decision (`None` when no level cleared its gate or
    /// a transport failure stopped the walk).
    pub decision: Option<Decision>,
    /// True when a level cleared its gate.
    pub settled: bool,
}

/// Tree descent: ask each level in order with its own options and
/// act gate; stop at the first winner at or above its gate. Covers
/// logical trees (per-node closed options) and percent-matched
/// trees (per-node act bars). An empty level list or a transport
/// failure yields an unsettled outcome with the path walked so far.
pub async fn descend_tree(
    client: &LlmClient,
    ask: &FlowAsk,
    levels: &[TreeLevel],
) -> DescentOutcome {
    let mut outcome = DescentOutcome::default();
    for (index, level) in levels.iter().enumerate() {
        if level.options.len() < 2 || level.options.len() > MAX_DECISION_OPTIONS {
            continue;
        }
        let options = level.options.join(",");
        let spec = choice_spec(
            &format!("{}:tree:{index}", ask.id),
            ask.version,
            "Pick one.",
            &options,
            &FlowAsk {
                act: level.act,
                ..ask.clone()
            },
        );
        if spec.validate().is_err() {
            continue;
        }
        let questions = [Question {
            spec: &spec,
            state: &level.state,
            question: &level.question,
        }];
        let mut outcomes = decide_all(client, &questions, 1, 2, None).await;
        let Some(Ok(decision)) = outcomes.pop() else {
            return outcome;
        };
        let options: Vec<&str> = level.options.iter().map(String::as_str).collect();
        let label = match_text_label(&decision.label, &options)
            .unwrap_or_default()
            .to_string();
        outcome.path.push((index, label));
        if decision.confidence >= level.act {
            outcome.decision = Some(decision);
            outcome.settled = true;
            return outcome;
        }
    }
    outcome
}
