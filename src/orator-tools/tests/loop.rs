//! Loop tests: round bound, preamble carriage, denial paths, and the
//! scripted-model harness shared with the red-team suite.

use std::sync::Arc;

use orator_core::SessionKind;

use crate::builtins::default_registry;
use crate::loop_::{run_turn, TurnOutcome, MAX_TOOL_ROUNDS};
use crate::policy::{CallOrigin, Policy, PolicyCtx};
use crate::sandbox::{Guarded, GuardedConfig};
use crate::tool::{ToolCall, ToolError};

pub struct Script {
    replies: Vec<String>,
}

impl Script {
    pub fn new(replies: Vec<String>) -> Self {
        Self { replies }
    }

    pub fn model(&mut self) -> impl FnMut(&str) -> Result<String, ToolError> + '_ {
        move |_| {
            if self.replies.is_empty() {
                return Err(ToolError::Failed("model".to_string(), "script empty".to_string()));
            }
            Ok(self.replies.remove(0))
        }
    }
}

pub struct AllowNone;

impl crate::loop_::Approver for AllowNone {
    fn approve(&mut self, _call: &ToolCall) -> bool {
        false
    }
}

pub struct AllowAll;

impl crate::loop_::Approver for AllowAll {
    fn approve(&mut self, _call: &ToolCall) -> bool {
        true
    }
}

fn harness() -> (crate::tool::ToolRegistry, Policy, PolicyCtx) {
    let dir = tempfile::tempdir().expect("tempdir");
    let guarded = Arc::new(
        Guarded::new(GuardedConfig {
            root: dir.path().to_path_buf(),
            ..GuardedConfig::default()
        })
        .expect("jail builds"),
    );
    // Leak the jail dir: tools run after this returns.
    let _ = dir.keep();
    let registry = default_registry(&guarded, Arc::new(|_| {}));
    let ctx = PolicyCtx {
        session: SessionKind::Assistant,
        verified: true,
        tainted: false,
        origin: CallOrigin::Direct,
    };
    (registry, Policy::new(), ctx)
}

#[test]
fn plain_answer_needs_no_tools() {
    let (registry, policy, ctx) = harness();
    let mut script = Script::new(vec!["Just chatting here.".to_string()]);
    let mut approver = AllowNone;
    let outcome = run_turn(
        &registry,
        &policy,
        "hello",
        &ctx,
        &mut script.model(),
        &mut approver,
    );
    assert_eq!(
        outcome,
        TurnOutcome::Answer {
            preamble: String::new(),
            text: "Just chatting here.".to_string(),
        }
    );
}

#[test]
fn tool_round_carries_preamble_and_answers() {
    let (registry, policy, ctx) = harness();
    let mut script = Script::new(vec![
        r#"{"preamble": "Checking.", "tool_calls": [{"name": "time_now", "arguments": {}}]}"#.to_string(),
        "It is daytime.".to_string(),
    ]);
    let mut approver = AllowNone;
    let outcome = run_turn(
        &registry,
        &policy,
        "what time is it",
        &ctx,
        &mut script.model(),
        &mut approver,
    );
    assert_eq!(
        outcome,
        TurnOutcome::Answer {
            preamble: "Checking.".to_string(),
            text: "It is daytime.".to_string(),
        }
    );
}

#[test]
fn sixth_round_call_is_an_error() {
    let (registry, policy, ctx) = harness();
    let round = r#"{"preamble": "", "tool_calls": [{"name": "time_now", "arguments": {}}]}"#.to_string();
    let mut replies = vec![round; MAX_TOOL_ROUNDS as usize + 1];
    replies.push("done".to_string());
    let mut script = Script::new(replies);
    let mut approver = AllowNone;
    let outcome = run_turn(
        &registry,
        &policy,
        "go",
        &ctx,
        &mut script.model(),
        &mut approver,
    );
    // Five tool rounds run; the final pass still calls → error.
    assert_eq!(outcome, TurnOutcome::Error("tool call after round 5".to_string()));
}

#[test]
fn denied_calls_become_results_not_runs() {
    let (registry, policy, mut ctx) = harness();
    ctx.session = SessionKind::Meeting;
    let mut script = Script::new(vec![
        r#"{"preamble": "", "tool_calls": [{"name": "time_now", "arguments": {}}]}"#.to_string(),
        "No tools here.".to_string(),
    ]);
    let mut approver = AllowAll;
    let outcome = run_turn(
        &registry,
        &policy,
        "hello",
        &ctx,
        &mut script.model(),
        &mut approver,
    );
    assert_eq!(
        outcome,
        TurnOutcome::Answer {
            preamble: String::new(),
            text: "No tools here.".to_string(),
        }
    );
}

#[test]
fn large_tool_output_truncates_with_marker() {
    use crate::tool::truncate_tool_output;
    let big = "x".repeat(20_000);
    let capped = truncate_tool_output(&big);
    assert!(capped.chars().count() < big.chars().count());
    assert!(capped.contains("truncated"), "{capped:.100}");
    let small = "hi";
    assert_eq!(truncate_tool_output(small), "hi");
}

#[test]
fn five_round_prompt_stays_under_budget() {
    use crate::loop_::{bound_prompt, MAX_PROMPT_TOKENS};
    use fluent_llm::tokens::estimate_tokens;
    let (registry, policy, ctx) = harness();
    let round = "{\"preamble\": \"\", \"tool_calls\": [{\"name\": \"time_now\", \"arguments\": {}}]}"
        .to_string();
    let mut replies = vec![round; MAX_TOOL_ROUNDS as usize + 1];
    replies.push("done".to_string());
    let mut script = Script::new(replies);
    let mut approver = AllowNone;
    let outcome = run_turn(
        &registry,
        &policy,
        &"q".repeat(1000),
        &ctx,
        &mut script.model(),
        &mut approver,
    );
    assert!(matches!(
        outcome,
        TurnOutcome::Answer { .. } | TurnOutcome::Error(_)
    ));
    let prompt = crate::loop_::last_prompt_for_tests().expect("prompt recorded");
    assert!(
        estimate_tokens(&prompt) as usize <= MAX_PROMPT_TOKENS + 64,
        "prompt under budget: {} tokens",
        estimate_tokens(&prompt)
    );
    // Over-budget inputs truncate the middle and keep the newest tail.
    let huge = "z".repeat(60_000);
    let capped = bound_prompt(&huge);
    assert!(capped.contains("truncated"), "large prompts mark truncation");
    assert!(capped.contains("newest"), "marker keeps newest turn");
    assert!(
        estimate_tokens(&capped) as usize <= MAX_PROMPT_TOKENS + 64,
        "capped prompt under budget"
    );
    assert!(capped.len() < huge.len());
}
