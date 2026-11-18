//! The tool-call loop: at most five tool rounds per turn, then one
//! final tool-less answer. Calls and preamble travel together so the
//! spoken lead-in and the executed calls can never diverge; tool results
//! re-enter the prompt wrapped as untrusted data and taint the turn.

use crate::tools::parse::{parse_gbnf, parse_model_output, ParseError};
use crate::tools::policy::{Policy, PolicyCtx, Verdict};
use crate::tools::tool::{truncate_tool_output, ToolCall, ToolError, ToolRegistry, ToolTier};
use orator::untrusted::{wrap_tool_result, TRANSCRIPT_AS_DATA};

/// Dexter's bound: five tool rounds, then a final answer pass.
pub const MAX_TOOL_ROUNDS: u32 = 5;

/// Prompt budget in tokens for the 5-round accumulation. Newest turns
/// survive; the middle truncates with a loud marker.
pub const MAX_PROMPT_TOKENS: usize = 8192;

/// What the turn produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    /// Final speech: the carried preamble plus the closing answer.
    Answer {
        /// Spoken lead-in from the tool rounds (may be empty).
        preamble: String,
        /// Closing answer text.
        text: String,
    },
    /// The turn failed without answering.
    Error(String),
}

/// Answer a pending approval. Tests and the `approve` path drive this;
/// the model never does.
pub trait Approver {
    /// Whether `call` may run. Every T2 call arrives here; denials are
    /// ordinary, never errors.
    fn approve(&mut self, call: &ToolCall) -> bool;
}

/// Run one turn: `model` answers prompts, `approver` answers Asks.
/// `ctx` carries session, verification, and provenance; taint accrues
/// inside the loop as tool results land.
pub fn run_turn(
    registry: &ToolRegistry,
    policy: &Policy,
    user_text: &str,
    ctx: &PolicyCtx,
    model: &mut dyn FnMut(&str) -> Result<String, ToolError>,
    approver: &mut dyn Approver,
) -> TurnOutcome {
    let mut prompt = bound_prompt(&format!("{TRANSCRIPT_AS_DATA}\n\nUser: {user_text}"));
    record_prompt(&prompt);
    let mut tainted = ctx.tainted;
    let mut preamble = String::new();
    for _ in 0..MAX_TOOL_ROUNDS {
        let reply = match model(&prompt) {
            Ok(reply) => reply,
            Err(error) => return TurnOutcome::Error(format!("model failed: {error}")),
        };
        let Ok((calls, spoken)) = parse_model_output(&reply) else {
            record_prompt(&prompt);
            return TurnOutcome::Answer {
                preamble,
                text: reply,
            };
        };
        if !spoken.is_empty() && preamble.is_empty() {
            preamble = spoken;
        }
        let mut results = String::new();
        for call in &calls {
            results.push_str(&execute_call(
                registry, policy, ctx, tainted, call, approver,
            ));
            results.push('\n');
        }
        tainted = true;
        let capped = truncate_tool_output(&results);
        use std::fmt::Write as _;
        let mut next = prompt.clone();
        let _ = write!(next, "\n{}", wrap_tool_result("results", &capped));
        prompt = bound_prompt(&next);
        record_prompt(&prompt);
    }
    // Final tool-less pass: speech only. Calls here are an error, not a
    // sixth round.
    match model(&prompt) {
        Ok(reply) => match parse_gbnf(&reply) {
            Ok(_) => TurnOutcome::Error("tool call after round 5".to_string()),
            Err(ParseError::NoCalls | ParseError::BadJson(_)) => TurnOutcome::Answer {
                preamble,
                text: reply,
            },
            Err(error) => TurnOutcome::Error(format!("final pass failed: {error}")),
        },
        Err(error) => TurnOutcome::Error(format!("model failed: {error}")),
    }
}

/// Keep `prompt` under [`MAX_PROMPT_TOKENS`] with middle-truncation and
/// newest-turn protection. Small prompts pass through verbatim; large
/// ones keep a head prefix plus the newest tail with a loud marker.
/// The split (head prefix, marker, newest tail) assembles through the
/// shared `fluent_llm::tokens::truncate_middle`; the keep/head/tail
/// budgets and the marker text stay here (call-site policy, pinned by
/// `bound_prompt_pins_head_tail_split_and_marker`).
#[must_use]
pub fn bound_prompt(prompt: &str) -> String {
    use fluent_llm::tokens::{estimate_chars_for_tokens, estimate_tokens, truncate_middle};
    if estimate_tokens(prompt) as usize <= MAX_PROMPT_TOKENS {
        return prompt.to_string();
    }
    let total_chars = prompt.chars().count();
    let budget_chars = estimate_chars_for_tokens(MAX_PROMPT_TOKENS, 4).max(1024);
    let marker = format!("\n(truncated middle, {total_chars} total chars; newest kept)\n");
    let keep = budget_chars.saturating_sub(marker.chars().count()).max(512);
    let head_chars = 2048.min(keep / 4);
    let tail_chars = keep.saturating_sub(head_chars);
    truncate_middle(prompt, head_chars, tail_chars, &marker)
}

std::thread_local! {
    static LAST_PROMPT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn record_prompt(prompt: &str) {
    LAST_PROMPT.with(|slot| {
        *slot.borrow_mut() = Some(prompt.to_string());
    });
}

/// Last prompt seen by `run_turn` on this thread (test hook for the
/// budget golden; production callers ignore it).
#[must_use]
pub fn last_prompt_for_tests() -> Option<String> {
    LAST_PROMPT.with(|slot| slot.borrow().clone())
}

fn execute_call(
    registry: &ToolRegistry,
    policy: &Policy,
    ctx: &PolicyCtx,
    tainted: bool,
    call: &ToolCall,
    approver: &mut dyn Approver,
) -> String {
    let Some(tool) = registry.get(&call.name) else {
        return format!("[{}]: unknown tool", call.name);
    };
    let asking = PolicyCtx {
        tainted,
        ..ctx.clone()
    };
    match policy.decide(call, tool.tier(), &asking) {
        Verdict::Allow => run_tool(tool.as_ref(), call),
        Verdict::Deny(reason) => format!("[{}]: denied: {reason}", call.name),
        Verdict::Ask(_) => {
            if approver.approve(call) {
                run_tool(tool.as_ref(), call)
            } else {
                format!("[{}]: approval denied", call.name)
            }
        }
    }
}

fn run_tool(tool: &dyn crate::tools::tool::Tool, call: &ToolCall) -> String {
    match tool.execute(call.args.clone()) {
        Ok(output) => format!("[{}]: {}", call.name, truncate_tool_output(&output)),
        Err(error) => format!("[{}]: error: {error}", call.name),
    }
}

/// Tier lookup helper for policy call sites.
#[must_use]
pub fn tier_of(registry: &ToolRegistry, name: &str) -> Option<ToolTier> {
    registry.get(name).map(|tool| tool.tier())
}

#[cfg(test)]
#[path = "../../tests/tools_loop.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/tools_redteam.rs"]
mod redteam_tests;
