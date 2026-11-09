use super::identifier;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QueryIntent {
    IdentifierLookup,
    SingleIdentifier,
    CapabilityQuery,
    FilePath,
    HowTo,
    Conceptual,
    MultiKeyword,
    GeneralSearch,
}

impl QueryIntent {
    pub fn priority(&self) -> u8 {
        match self {
            Self::IdentifierLookup | Self::SingleIdentifier => 0,
            Self::FilePath => 1,
            Self::CapabilityQuery => 2,
            Self::HowTo | Self::Conceptual => 4,
            Self::MultiKeyword => 5,
            Self::GeneralSearch => 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FsmState {
    Intake,
    Classify,
    Route,
    Validate,
}

#[derive(Debug, Clone)]
pub struct QueryClass {
    pub intent: QueryIntent,
    pub domain: String,
    pub confidence: f32,
    pub tokens: Vec<String>,
    pub detected_file_paths: Vec<String>,
    pub detected_identifiers: Vec<String>,
}

pub struct FsmEngine {
    pub state: FsmState,
}

impl Default for FsmEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FsmEngine {
    pub fn new() -> Self {
        Self {
            state: FsmState::Intake,
        }
    }

    /// INTAKE state: tokenize, detect file paths, detect identifiers
    pub fn intake(&mut self, query: &str) -> QueryClass {
        let trimmed = query.trim().to_string();
        let tokens: Vec<String> = trimmed
            .split_whitespace()
            .map(ToString::to_string)
            .collect();

        let detected_identifiers: Vec<String> = tokens
            .iter()
            .filter(|t| identifier::detect_identifier_pattern(t).is_some())
            .cloned()
            .collect();

        let detected_file_paths: Vec<String> = tokens
            .iter()
            .filter(|t| {
                t.contains('/')
                    || t.contains('\\')
                    || (t.contains('.') && t.chars().any(|c| c.is_ascii_uppercase()))
            })
            .cloned()
            .collect();

        self.state = FsmState::Classify;
        QueryClass {
            intent: QueryIntent::GeneralSearch,
            domain: String::new(),
            confidence: 0.0,
            tokens,
            detected_file_paths,
            detected_identifiers,
        }
    }

    /// CLASSIFY state: determine intent + domain + confidence
    pub fn classify(&self, qc: &QueryClass) -> QueryClass {
        if let Some(decided) = intent_precheck(qc) {
            return decided;
        }

        let mut result = qc.clone();

        // How-to questions
        let how_to_words = [
            "how", "what", "why", "when", "where", "which", "can", "does", "do", "is", "are",
        ];
        let first_word = qc
            .tokens
            .first()
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        if how_to_words.contains(&first_word.as_str()) {
            result.intent = QueryIntent::HowTo;
            result.domain = "how_to".into();
            result.confidence = 0.85;
            return result;
        }

        let word_count = qc.tokens.len();

        // Multi-keyword (short phrases, 2-4 words)
        if (2..=4).contains(&word_count) {
            result.intent = QueryIntent::CapabilityQuery;
            result.domain = "capability".into();
            result.confidence = 0.75;
            return result;
        }

        // Conceptual (5+ words)
        if word_count >= 5 {
            result.intent = QueryIntent::Conceptual;
            result.domain = "conceptual".into();
            result.confidence = 0.7;
            return result;
        }

        // Multi-keyword fallback (2+ words shorter than capability)
        if word_count >= 2 {
            result.intent = QueryIntent::MultiKeyword;
            result.domain = "multi_keyword".into();
            result.confidence = 0.6;
        }

        result
    }

    /// Intent-Choice shadow: the deterministic rule verdict wins; the
    /// precomputed decision (from out-of-band async evaluation) is only
    /// logged via `super::shadow::intent_shadow`. Never alters the response.
    #[must_use]
    pub fn classify_with_shadow(
        &self,
        qc: &QueryClass,
        decision: Option<&fluent_llm::decision::Decision>,
    ) -> (QueryClass, Option<super::shadow::ShadowRecord>) {
        let verdict = self.classify(qc);
        let record = super::shadow::intent_shadow(
            &format!("{:?}", verdict.intent),
            decision,
            &fluent_llm::decision::ThresholdSet {
                act: 0.9,
                review: 0.7,
            },
        );
        (verdict, record)
    }

    /// Route classifier — determines which search primitive to use
    pub fn route(&self, qc: &QueryClass) -> &'static str {
        match qc.intent {
            QueryIntent::SingleIdentifier | QueryIntent::IdentifierLookup => "word_index",
            QueryIntent::FilePath => "anchor_lookup",
            QueryIntent::CapabilityQuery | QueryIntent::MultiKeyword => "fts",
            QueryIntent::HowTo => "hybrid",
            QueryIntent::Conceptual => "vector",
            QueryIntent::GeneralSearch => "keyword",
        }
    }

    /// VALIDATE state: check result quality
    pub fn validate(&self, qc: &QueryClass) -> bool {
        qc.confidence >= 0.5
    }

    /// Run the full FSM pipeline for a query
    pub fn run(&mut self, query: &str) -> QueryClass {
        let qc = self.intake(query);
        let qc = self.classify(&qc);
        self.state = FsmState::Route;
        let _route = self.route(&qc);
        self.state = FsmState::Validate;
        let _valid = self.validate(&qc);
        qc
    }
}

/// Deterministic pre-check: exact evidence decides without judgment.
/// Returns the verdict for the empty, file-path, and single-identifier
/// arms (identical to `classify`); `None` means ambiguous — the only
/// queries a decision model may ever see.
#[must_use]
pub fn intent_precheck(qc: &QueryClass) -> Option<QueryClass> {
    let mut result = qc.clone();
    let trimmed: String = qc.tokens.join(" ");
    if trimmed.trim().is_empty() {
        result.intent = QueryIntent::GeneralSearch;
        result.domain = "empty".into();
        result.confidence = 1.0;
        return Some(result);
    }
    if !qc.detected_file_paths.is_empty() {
        result.intent = QueryIntent::FilePath;
        result.domain = "file_path".into();
        result.confidence = 0.95;
        return Some(result);
    }
    if qc.tokens.len() == 1 && !qc.detected_identifiers.is_empty() {
        result.intent = QueryIntent::SingleIdentifier;
        result.domain = "identifier".into();
        result.confidence = 0.9;
        return Some(result);
    }
    None
}

#[cfg(test)]
#[path = "../../tests/intent_decision.rs"]
mod intent_decision_tests;

#[cfg(test)]
mod tests {
    use super::*;

    // ── Migrated classify tests (legacy `classify_query` removed in
    // favor of `FsmEngine`; same queries, expectations mapped to the
    // authoritative engine: single identifiers resolve to
    // `SingleIdentifier`, how-word questions to `HowTo`) ────────────

    #[test]
    fn test_classify_identifier() {
        let mut engine = FsmEngine::new();
        assert_eq!(
            engine.run("helloWorld").intent,
            QueryIntent::SingleIdentifier
        );
        assert_eq!(
            engine.run("snake_case_fn").intent,
            QueryIntent::SingleIdentifier
        );
        assert_eq!(
            engine.run("PascalCaseType").intent,
            QueryIntent::SingleIdentifier
        );
    }

    #[test]
    fn test_classify_capability() {
        let mut engine = FsmEngine::new();
        assert_eq!(
            engine.run("target registry").intent,
            QueryIntent::CapabilityQuery
        );
        assert_eq!(
            engine.run("ast parsing zig").intent,
            QueryIntent::CapabilityQuery
        );
    }

    #[test]
    fn test_classify_concept() {
        // The legacy word-count ladder read these as `Conceptual`; the
        // authoritative engine reads how-word questions as `HowTo`.
        let mut engine = FsmEngine::new();
        assert_eq!(
            engine.run("how do I parse a zig file").intent,
            QueryIntent::HowTo
        );
        assert_eq!(
            engine.run("what is the target registry used for").intent,
            QueryIntent::HowTo
        );
    }

    #[test]
    fn test_classify_empty() {
        let mut engine = FsmEngine::new();
        assert_eq!(engine.run("").intent, QueryIntent::GeneralSearch);
        assert_eq!(engine.run("   ").intent, QueryIntent::GeneralSearch);
    }

    #[test]
    fn test_matches() {
        // Replaced by the pre-check: exact identifiers decide locally.
        let mut engine = FsmEngine::new();
        let decided = intent_precheck(&engine.intake("helloWorld")).expect("decides");
        assert_eq!(decided.intent, QueryIntent::SingleIdentifier);
    }

    #[test]
    fn test_priority_order() {
        assert!(QueryIntent::IdentifierLookup.priority() < QueryIntent::CapabilityQuery.priority());
        assert!(QueryIntent::CapabilityQuery.priority() < QueryIntent::Conceptual.priority());
    }

    // ── FSM Engine tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_fsm_intake_tokenizes() {
        let mut engine = FsmEngine::new();
        let qc = engine.intake("hello world test");
        assert_eq!(qc.tokens.len(), 3);
        assert_eq!(engine.state, FsmState::Classify);
    }

    #[test]
    fn test_fsm_intake_detects_identifiers() {
        let mut engine = FsmEngine::new();
        let qc = engine.intake("helloWorld foo_bar");
        assert!(!qc.detected_identifiers.is_empty());
        assert!(qc.detected_identifiers.contains(&"helloWorld".to_string()));
    }

    #[test]
    fn test_fsm_intake_detects_file_paths() {
        let mut engine = FsmEngine::new();
        let qc = engine.intake("src/main.zig");
        assert!(!qc.detected_file_paths.is_empty());
    }

    #[test]
    fn test_fsm_classify_single_identifier() {
        let mut engine = FsmEngine::new();
        let qc = engine.intake("helloWorld");
        let qc = engine.classify(&qc);
        assert_eq!(qc.intent, QueryIntent::SingleIdentifier);
    }

    #[test]
    fn test_fsm_classify_how_to() {
        let mut engine = FsmEngine::new();
        let qc = engine.intake("how do I parse a file");
        let qc = engine.classify(&qc);
        assert_eq!(qc.intent, QueryIntent::HowTo);
    }

    #[test]
    fn test_fsm_classify_capability() {
        let mut engine = FsmEngine::new();
        let qc = engine.intake("ast parsing");
        let qc = engine.classify(&qc);
        assert_eq!(qc.intent, QueryIntent::CapabilityQuery);
    }

    #[test]
    fn test_fsm_classify_conceptual() {
        let mut engine = FsmEngine::new();
        let qc = engine.intake("the target registry resolves dependencies across modules");
        let qc = engine.classify(&qc);
        assert_eq!(qc.intent, QueryIntent::Conceptual);
    }

    #[test]
    fn test_fsm_classify_file_path() {
        let mut engine = FsmEngine::new();
        let qc = engine.intake("src/main.rs");
        let qc = engine.classify(&qc);
        assert_eq!(qc.intent, QueryIntent::FilePath);
    }

    #[test]
    fn test_fsm_route_returns_strategy() {
        let mut engine = FsmEngine::new();

        let qc1 = engine.intake("helloWorld");
        let qc1 = engine.classify(&qc1);
        assert_eq!(engine.route(&qc1), "word_index");

        let qc2 = engine.intake("what is a target used for");
        let qc2 = engine.classify(&qc2);
        assert_eq!(qc2.intent, QueryIntent::HowTo);
        assert_eq!(engine.route(&qc2), "hybrid");

        let qc3 = engine.intake("ast parsing zig");
        let qc3 = engine.classify(&qc3);
        assert_eq!(engine.route(&qc3), "fts");
    }

    #[test]
    fn test_fsm_full_run() {
        let mut engine = FsmEngine::new();
        let qc = engine.run("helloWorld");
        assert_eq!(qc.intent, QueryIntent::SingleIdentifier);
        assert!(qc.confidence >= 0.5);
    }

    #[test]
    fn test_fsm_confidence_threshold() {
        let mut engine = FsmEngine::new();
        let qc = engine.run("");
        assert_eq!(qc.intent, QueryIntent::GeneralSearch);
        assert!(engine.validate(&qc));
    }
}
