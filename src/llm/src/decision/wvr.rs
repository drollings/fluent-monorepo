//! Pure render/decode/policy `WorkUnit`s (no I/O).
//!
//! Each unit is a synchronous, side-effect-free transform over the typed
//! `WorkContext` channel: render reads `state`/`question` and returns the
//! prompt; decode reads `raw_response` and returns the `Decision`; policy
//! reads `decision` and returns the `Verdict`. Network I/O lives only in
//! `LlmClient::decide_async`, never in `execute`.

use fluent_wvr::{ArcIntern, WorkContext, WorkError, WorkOutput, WorkUnit};

use crate::decision::{Question, QuestionSpec, ThresholdSet};

/// Pure prompt-render unit holding its spec.
///
/// ```rust
/// use fluent_llm::decision::{QuestionSpec, RenderQuestionUnit};
/// use fluent_wvr::{WorkContext, WorkOutput, WorkUnit};
///
/// # let spec = QuestionSpec::new()
/// #     .id("q".to_string()).version(1).kind("choice".to_string())
/// #     .prompt_template("Pick.".to_string()).options_csv("A,B".to_string())
/// #     .act_threshold(0.9).review_threshold(0.7)
/// #     .timeout_ms(1000).cacheable(false).build();
/// let mut ctx = WorkContext::default();
/// ctx.set("state".to_string(), "ST".to_string());
/// ctx.set("question".to_string(), "Q?".to_string());
/// let out = RenderQuestionUnit::new(spec).execute(&ctx).expect("render");
/// let text: String = out.data_as().expect("prompt");
/// assert!(text.ends_with("Q?"));
/// ```
pub struct RenderQuestionUnit {
    spec: QuestionSpec,
}

impl RenderQuestionUnit {
    #[must_use]
    pub fn new(spec: QuestionSpec) -> Self {
        Self { spec }
    }
}

impl WorkUnit for RenderQuestionUnit {
    fn name(&self) -> &str {
        "decision.render"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let state = ctx.get::<String>("state").ok_or_else(|| {
            WorkError::Execution("decision.render: missing typed `state`".to_string())
        })?;
        let question = ctx.get::<String>("question").ok_or_else(|| {
            WorkError::Execution("decision.render: missing typed `question`".to_string())
        })?;
        let q = Question {
            spec: &self.spec,
            state,
            question,
        };
        WorkOutput::typed("rendered", &q.render())
    }
}

fluent_wvr::impl_fieldless!(RenderQuestionUnit);

impl fluent_wvr::Describable for RenderQuestionUnit {
    fn describe(&self) -> serde_json::Value {
        fluent_wvr::describe_work_unit(self.name(), self.depends(), self.provides(), Some("pure"))
    }
}

fluent_wvr::impl_component!(RenderQuestionUnit);

/// Pure logprobs-decode unit over fixed candidates.
///
/// ```rust
/// use fluent_llm::decision::DecodeDecisionUnit;
/// use fluent_wvr::{WorkContext, WorkUnit};
///
/// let unit = DecodeDecisionUnit::new(vec!["A".to_string(), "B".to_string()], "m".to_string(), 1);
/// let mut ctx = WorkContext::default();
/// ctx.set(
///     "raw_response".to_string(),
///     serde_json::json!({"choices": [{"message": {"role": "assistant", "content": "B"}}]}),
/// );
/// let out = unit.execute(&ctx).expect("decode");
/// let d: fluent_llm::decision::Decision = out.data_as().expect("decision");
/// assert_eq!(d.label, "B");
/// ```
pub struct DecodeDecisionUnit {
    candidates: Vec<String>,
    model: String,
    version: u64,
}

impl DecodeDecisionUnit {
    #[must_use]
    pub fn new(candidates: Vec<String>, model: String, version: u64) -> Self {
        Self {
            candidates,
            model,
            version,
        }
    }
}

impl WorkUnit for DecodeDecisionUnit {
    fn name(&self) -> &str {
        "decision.decode"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let raw = ctx
            .get::<serde_json::Value>("raw_response")
            .ok_or_else(|| {
                WorkError::Execution("decision.decode: missing typed `raw_response`".to_string())
            })?;
        let refs: Vec<&str> = self.candidates.iter().map(String::as_str).collect();
        match crate::decision::decode_choice(raw, &refs, &self.model, self.version) {
            Ok(d) => WorkOutput::typed("decoded", &d),
            Err(e) => Err(WorkError::Execution(e.to_string())),
        }
    }
}

fluent_wvr::impl_fieldless!(DecodeDecisionUnit);

impl fluent_wvr::Describable for DecodeDecisionUnit {
    fn describe(&self) -> serde_json::Value {
        fluent_wvr::describe_work_unit(self.name(), self.depends(), self.provides(), Some("pure"))
    }
}

fluent_wvr::impl_component!(DecodeDecisionUnit);

/// Pure threshold-policy unit mapping a `Decision` to its `Verdict`.
///
/// ```rust
/// use fluent_llm::decision::{Decision, PolicyVerdictUnit, ThresholdSet, Verdict};
/// use fluent_wvr::{WorkContext, WorkUnit};
///
/// let unit = PolicyVerdictUnit::new(ThresholdSet { act: 0.9, review: 0.7 });
/// let decision = Decision::from_distribution(
///     "B",
///     vec![("B".to_string(), 0.95), ("A".to_string(), 0.05)],
///     "m".to_string(),
///     1,
///     0,
///     false,
/// )
/// .expect("decision");
/// let mut ctx = WorkContext::default();
/// ctx.set("decision".to_string(), decision);
/// let out = unit.execute(&ctx).expect("policy");
/// assert_eq!(out.data_as::<Verdict>().expect("verdict"), Verdict::Act);
/// ```
pub struct PolicyVerdictUnit {
    thresholds: ThresholdSet,
}

impl PolicyVerdictUnit {
    #[must_use]
    pub fn new(thresholds: ThresholdSet) -> Self {
        Self { thresholds }
    }
}

impl WorkUnit for PolicyVerdictUnit {
    fn name(&self) -> &str {
        "decision.policy"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let decision = ctx
            .get::<crate::decision::Decision>("decision")
            .ok_or_else(|| {
                WorkError::Execution("decision.policy: missing typed `decision`".to_string())
            })?;
        let verdict = crate::decision::decide_verdict(decision, &self.thresholds);
        WorkOutput::typed("verdict", &verdict)
    }
}

fluent_wvr::impl_fieldless!(PolicyVerdictUnit);

impl fluent_wvr::Describable for PolicyVerdictUnit {
    fn describe(&self) -> serde_json::Value {
        fluent_wvr::describe_work_unit(self.name(), self.depends(), self.provides(), Some("pure"))
    }
}

fluent_wvr::impl_component!(PolicyVerdictUnit);
