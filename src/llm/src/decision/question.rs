//! Two-layer question API: spec Component + concrete hot-loop type.
//!
//! `QuestionSpec` is the control-plane shape (runtime field access, schema,
//! builder construction, loadable from config/DB through the same surface).
//! `Question` is the tight-loop shape (concrete borrows, no `dyn` per
//! element). The renderer keeps the shared state first so endpoint
//! prefix-cache reuse can engage.

use fluent_wvr::{ArcIntern, WorkContext, WorkError, WorkOutput, WorkUnit};

/// Control-plane question spec: id, version, kind, prompt template,
/// single-token option list, per-point thresholds, timeout, cache flag.
///
/// ```rust
/// use fluent_llm::decision::QuestionSpec;
///
/// let spec = QuestionSpec::new()
///     .id("intent".to_string())
///     .version(1)
///     .kind("choice".to_string())
///     .prompt_template("Pick.".to_string())
///     .options_csv("A,B".to_string())
///     .act_threshold(0.9)
///     .review_threshold(0.7)
///     .timeout_ms(2000)
///     .cacheable(true)
///     .build();
/// assert!(spec.validate().is_ok());
/// ```
#[derive(Debug, Clone, fluent_wvr::FieldAccess, fluent_wvr::Describable, bon::Builder)]
#[builder(start_fn = new)]
pub struct QuestionSpec {
    #[field(desc = "Question id")]
    pub id: String,
    #[field(desc = "Question version", min = 1)]
    pub version: u64,
    #[field(desc = "Decision kind (choice|score|noul)")]
    pub kind: String,
    #[field(desc = "Prompt template rendered between state and question")]
    pub prompt_template: String,
    #[field(desc = "Comma-separated single-token option labels")]
    pub options_csv: String,
    #[field(desc = "Act threshold (producer confidence)", min = 0, max = 1)]
    pub act_threshold: f64,
    #[field(desc = "Review threshold (producer confidence)", min = 0, max = 1)]
    pub review_threshold: f64,
    #[field(desc = "Per-call timeout in ms", min = 1)]
    pub timeout_ms: u64,
    #[field(desc = "Whether answers may be cached")]
    pub cacheable: bool,
    #[field(desc = "Option-order mitigation (none|rotate|shuffle)")]
    #[builder(default = "none".to_string())]
    pub rotation: String,
    #[field(desc = "Deterministic seed for rotation/shuffle")]
    #[builder(default = 0)]
    pub seed: u64,
}

impl QuestionSpec {
    /// Fallible validation: kind spelling, kind-shaped options, thresholds.
    pub fn validate(&self) -> Result<(), String> {
        let kind = crate::decision::DecisionKind::parse(&self.kind)?;
        let options = self.options();
        // Single-token labels always: the single generated token must name
        // one of them for logprobs decoding to discriminate.
        for label in &options {
            if label.is_empty() || label.chars().any(char::is_whitespace) {
                return Err(format!("option label must be a single token: {label:?}"));
            }
        }
        // Variant normalization merges case/space twins, so duplicates are
        // checked canonicalized: two labels decoding to one are one label
        // too many (silent vote-splitting otherwise).
        let mut seen: Vec<String> = Vec::new();
        for label in &options {
            let canonical = label.trim_start().to_lowercase();
            if seen.contains(&canonical) {
                return Err(format!("duplicate option label: {label:?}"));
            }
            seen.push(canonical);
        }
        if !matches!(self.rotation.as_str(), "none" | "rotate" | "shuffle") {
            return Err(format!(
                "unknown rotation: {:?} (expected none|rotate|shuffle)",
                self.rotation
            ));
        }
        // One distribution carries 10 top logprobs; larger sets cannot all
        // appear and would always degrade. Route them hierarchically.
        if options.len() > crate::decision::MAX_DECISION_OPTIONS {
            return Err(format!(
                "options exceed the single-token distribution cap ({}); split via route_hierarchical",
                crate::decision::MAX_DECISION_OPTIONS
            ));
        }
        match kind {
            crate::decision::DecisionKind::Choice => {
                if options.is_empty() {
                    return Err("options_csv must list at least one label".to_string());
                }
            }
            crate::decision::DecisionKind::Score => {
                if options.len() < 2 {
                    return Err("score needs at least two rungs".to_string());
                }
            }
            // The Noul decoder answers only Yes/No; anything else smuggles
            // a label the decoder will never emit.
            crate::decision::DecisionKind::Noul => {
                if options.len() != 2
                    || !options.iter().any(|o| o.eq_ignore_ascii_case("yes"))
                    || !options.iter().any(|o| o.eq_ignore_ascii_case("no"))
                {
                    return Err("noul options must be exactly Yes,No".to_string());
                }
            }
        }
        crate::decision::ThresholdSet {
            act: self.act_threshold,
            review: self.review_threshold,
        }
        .validate()?;
        Ok(())
    }

    /// Typed kind for the held spelling.
    pub fn kind_enum(&self) -> Result<crate::decision::DecisionKind, String> {
        crate::decision::DecisionKind::parse(&self.kind)
    }

    /// Canonical option labels in declared order.
    #[must_use]
    pub fn options(&self) -> Vec<String> {
        self.options_csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Per-point thresholds for the held fields.
    #[must_use]
    pub fn thresholds(&self) -> crate::decision::ThresholdSet {
        crate::decision::ThresholdSet {
            act: self.act_threshold,
            review: self.review_threshold,
        }
    }
}

fluent_wvr::impl_component!(QuestionSpec);

impl WorkUnit for QuestionSpec {
    fn name(&self) -> &str {
        &self.id
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        match self.validate() {
            Ok(()) => Ok(WorkOutput::ok(format!("question {}", self.id))),
            Err(detail) => Err(WorkError::Execution(detail)),
        }
    }
}

/// Concrete hot-loop question: borrows its spec plus state (no `dyn`).
#[derive(Debug, Clone, Copy)]
pub struct Question<'a> {
    pub spec: &'a QuestionSpec,
    pub state: &'a str,
    pub question: &'a str,
}

impl Question<'_> {
    /// Render state-first/question-last for prefix-cache sharing. The spec's
    /// rotation applies with a salt from the question text, so different
    /// questions spread position bias while one question stays stable.
    ///
    /// ```rust
    /// use fluent_llm::decision::{Question, QuestionSpec};
    ///
    /// # let spec = QuestionSpec::new()
    /// #     .id("q".to_string()).version(1).kind("choice".to_string())
    /// #     .prompt_template("Pick.".to_string()).options_csv("A,B".to_string())
    /// #     .act_threshold(0.9).review_threshold(0.7)
    /// #     .timeout_ms(2000).cacheable(false).build();
    /// let rendered = Question { spec: &spec, state: "ST", question: "Q?" }.render();
    /// assert!(rendered.starts_with("ST") && rendered.ends_with("Q?"));
    /// ```
    #[must_use]
    pub fn render(&self) -> String {
        let salt = common_core::hash::fnv1a64(self.question.as_bytes());
        let rotated = rotated_options(
            &self.spec.options(),
            &self.spec.rotation,
            self.spec.seed,
            salt,
        );
        let csv: Vec<&str> = rotated.iter().map(String::as_str).collect();
        render_prompt_full(
            self.state,
            &self.spec.prompt_template,
            &csv.join(","),
            self.question,
        )
    }
}

/// `MAX_DECISION_PROMPT_BYTES` transport guard: prompts never exceed this size. Over-budget state is
/// truncated at a character boundary (tail cut, prefix kept for endpoint
/// prefix-cache reuse); template, options, and question are caller-shaped
/// and stay intact.
pub const MAX_DECISION_PROMPT_BYTES: usize = 8192;

/// Truncate to at most `max` bytes without splitting a character.
fn truncate_to_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Single-token distribution cap: the decide path requests 10 top logprobs,
/// so more options can never all appear in one distribution. Larger sets
/// must route hierarchically (`route_hierarchical`), not widen the call.
pub const MAX_DECISION_OPTIONS: usize = 10;

/// Order `options` for one rendering under `mode` (`none`/`rotate`/
/// `shuffle`), deterministically from `(seed, salt)`. Unknown modes pass
/// through declared order (fail-open rendering; `validate` refuses them).
///
/// ```rust
/// use fluent_llm::decision::rotated_options;
///
/// let options = vec!["A".to_string(), "B".to_string(), "C".to_string()];
/// assert_eq!(rotated_options(&options, "none", 7, 3), options);
/// assert_eq!(
///     rotated_options(&options, "rotate", 7, 3),
///     rotated_options(&options, "rotate", 7, 3)
/// );
/// ```
#[must_use]
pub fn rotated_options(options: &[String], mode: &str, seed: u64, salt: u64) -> Vec<String> {
    if options.len() < 2 {
        return options.to_vec();
    }
    match mode {
        "rotate" => {
            let mut out = options.to_vec();
            let shift = ((seed.wrapping_add(salt)) % out.len() as u64) as usize;
            out.rotate_left(shift);
            out
        }
        "shuffle" => {
            let mut out = options.to_vec();
            // Deterministic LCG Fisher-Yates (no RNG dependency): same
            // (seed, salt) shuffles identically on every run.
            let mut state = seed
                .wrapping_add(salt)
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let mut next = move || {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as usize
            };
            for i in (1..out.len()).rev() {
                let j = next() % (i + 1);
                out.swap(i, j);
            }
            out
        }
        _ => options.to_vec(),
    }
}

/// Render state-first/question-last without a spec (shared layout core).
///
/// ```rust
/// use fluent_llm::decision::render_prompt;
///
/// let out = render_prompt("ST", "Q?", &["A", "B"]);
/// assert!(out.starts_with("ST") && out.ends_with("Q?"));
/// ```
#[must_use]
pub fn render_prompt(state: &str, question: &str, options: &[&str]) -> String {
    render_prompt_full(state, "", &options.join(","), question)
}

fn render_prompt_full(state: &str, template: &str, options_csv: &str, question: &str) -> String {
    // Fixed overhead around the state: separators, template, options line.
    let mut fixed = 2 + question.len() + 2;
    if !template.is_empty() {
        fixed += 2 + template.len();
    }
    if !options_csv.is_empty() {
        fixed += "\nOptions: ".len() + options_csv.len();
    }
    let state_budget = MAX_DECISION_PROMPT_BYTES.saturating_sub(fixed);
    let state = truncate_to_bytes(state, state_budget);
    let mut out = String::with_capacity(state.len() + fixed);
    out.push_str(state);
    if !template.is_empty() {
        out.push_str("\n\n");
        out.push_str(template);
    }
    if !options_csv.is_empty() {
        out.push_str("\nOptions: ");
        out.push_str(options_csv);
    }
    out.push_str("\n\n");
    out.push_str(question);
    out
}

/// Caller-enum to canonical-label mapping for compile-time checking.
///
/// Implement for the caller's enum; the labels must satisfy the spec's
/// single-token validation or `validate` refuses them.
///
/// ```rust
/// use fluent_llm::decision::DecisionOptions;
///
/// #[allow(dead_code)]
/// enum Intent { A, B }
///
/// impl DecisionOptions for Intent {
///     fn labels() -> Vec<String> {
///         vec!["A".to_string(), "B".to_string()]
///     }
/// }
///
/// assert_eq!(Intent::labels().len(), 2);
/// ```
pub trait DecisionOptions {
    fn labels() -> Vec<String>;
}
