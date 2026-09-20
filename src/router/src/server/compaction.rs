//! Pre-dispatch context guard + LOD compaction (D).
//!
//! Long multi-turn sessions outgrow small role windows (log forensics: an
//! 8483-token prompt died on the 8192-token `quick` window). Instead of
//! discovering the overflow from the fork's 400, estimate the prompt first;
//! when it projects past the target window, rebuild the request from the
//! ledger: system messages + one summary message assembled from the
//! session's eager LOD5 per-turn labels + the newest turns verbatim.
//!
//! No model call: LOD5 labels are written eagerly at record time, so
//! compaction is pure local string work. Fail-open at every step —
//! anything missing (ledger, session, labels) yields `None` and the
//! request dispatches uncompacted down the usual chain (overflow →
//! fallbacks → ladder → honest 400).

use std::collections::HashMap;
use std::sync::Arc;

use fluent_llm::tokens::estimate_tokens;
use fluent_types::openai_wire::{RouterMessage, RouterMessageContent, RouterRequest};
use fluent_types::{LOD5_LABEL, OriginRole};

use crate::config::RoleEntry;
use crate::node_store::ContentNodeStore;
use crate::pipeline::RoutingTarget;

/// Completion headroom kept beside the prompt estimate: the fork counts
/// prompt tokens against the window, and the estimate itself is heuristic.
const COMPLETION_RESERVE_TOKENS: u64 = 1024;
/// LOD5 labels folded into one summary message (each ≤ ~64 chars, so 120
/// labels ≈ 8k chars ≈ 2k tokens — bounded by construction).
const MAX_SUMMARY_LABELS: usize = 120;
/// Ledger nodes scanned for labels (most recent; older turns matter less).
const MAX_SESSION_NODES: usize = 200;

/// The target window's context size in tokens: the role's `num_ctx` is what
/// sizes on-demand windows (model `max_ctx` only clamps). `None` for
/// role-less (direct-model) targets — the guard does not apply to them.
pub(crate) fn window_ctx_for_role(
    roles: &HashMap<String, RoleEntry>,
    target: &RoutingTarget,
) -> Option<u64> {
    let role = target.role.as_deref()?;
    roles.get(role).map(|entry| entry.params.num_ctx)
}

/// Heuristic prompt size: the same role-prefixed rendering the backend
/// serializes, measured with the workspace token estimator.
pub(crate) fn estimate_request_tokens(request: &RouterRequest) -> u64 {
    crate::server::dispatch::render_prompt(request)
        .split('\n')
        .map(estimate_tokens)
        .sum()
}

/// Rebuild an over-window request from the ledger, or `None` when the
/// request fits, the ledger/session is absent, or compaction cannot make it
/// fit (a single turn larger than the window compacts to nothing — the
/// normal chain then ends in the honest 400).
pub(crate) fn maybe_compact_request(
    request: &RouterRequest,
    window_ctx: u64,
    ledger: &Arc<ContentNodeStore>,
) -> Option<RouterRequest> {
    let estimate = estimate_request_tokens(request);
    if estimate.saturating_add(COMPLETION_RESERVE_TOKENS) <= window_ctx {
        return None;
    }
    let session_id = request.session_id.as_deref().filter(|s| !s.is_empty())?;

    // One shared budget: prompt headroom minus the completion reserve, split
    // between the verbatim tail and the label summary so the re-estimate
    // below can actually pass. The newest user turn is always kept — without
    // it the request is meaningless.
    let usable = window_ctx.saturating_sub(COMPLETION_RESERVE_TOKENS);
    let tail_allowance = usable / 2;
    let mut tail: Vec<&RouterMessage> = Vec::new();
    let mut tail_tokens: u64 = 0;
    let mut newest_user_kept = false;
    for msg in request.messages.iter().rev().filter(|m| m.role != "system") {
        let msg_tokens =
            estimate_tokens(&msg.role) + estimate_tokens(&msg.content.to_string_lossy());
        if !tail.is_empty() && tail_tokens.saturating_add(msg_tokens) > tail_allowance {
            break;
        }
        if msg.role == "user" {
            newest_user_kept = true;
        }
        tail_tokens = tail_tokens.saturating_add(msg_tokens);
        tail.push(msg);
    }
    if !newest_user_kept {
        return None;
    }
    tail.reverse();

    // Earlier turns as LOD5 labels, oldest first, skipping roughly the turns
    // the verbatim tail already covers (message/node alignment is
    // approximate — system messages and multi-part turns have no ledger
    // node — so the summary is marked approximate in its header). Labels
    // stop when the summary half of the budget runs out.
    let mut summary_allowance = usable.saturating_sub(tail_tokens);
    let system_tokens: u64 = request
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| estimate_tokens(&m.role) + estimate_tokens(&m.content.to_string_lossy()))
        .sum();
    summary_allowance = summary_allowance.saturating_sub(system_tokens);
    let nodes = ledger.get_session_nodes(session_id, MAX_SESSION_NODES).ok()?;
    let skip = tail.len().min(nodes.len());
    let mut labels: Vec<String> = Vec::new();
    for node in nodes
        .iter()
        .rev()
        .take(nodes.len().saturating_sub(skip))
        .take(MAX_SUMMARY_LABELS)
    {
        let label = node
            .lod
            .get(LOD5_LABEL as usize)
            .map(String::as_str)
            .unwrap_or_default()
            .trim();
        let text = if label.is_empty() {
            let full = node.lod.first().map(String::as_str).unwrap_or_default();
            let mut chars = full.chars();
            let snippet: String = chars.by_ref().take(80).collect();
            if chars.next().is_some() {
                format!("{snippet}…")
            } else {
                snippet
            }
        } else {
            label.to_string()
        };
        if text.trim().is_empty() {
            continue;
        }
            let role = node.role.as_ref().map_or("msg", role_name);
        let line = format!("[{role}] {text}");
        let line_tokens = estimate_tokens(&line);
        if !labels.is_empty() && summary_allowance < line_tokens {
            break;
        }
        summary_allowance = summary_allowance.saturating_sub(line_tokens);
        labels.push(line);
    }
    if labels.is_empty() {
        return None;
    }

    let mut messages: Vec<RouterMessage> = request
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .cloned()
        .collect();
    let dropped = request.messages.len().saturating_sub(tail.len() + messages.len());
    messages.push(RouterMessage {
        role: "system".into(),
        content: RouterMessageContent::Text(format!(
            "[Compacted history — {dropped} earlier message(s) replaced by per-turn ledger labels; \
             only the last {} turn(s) are verbatim. Approximate context: labels are first-sentence \
             summaries, not full text.]\n{}",
            tail.len(),
            labels.join("\n"),
        )),
        tool_calls: None,
        tool_call_id: None,
    });
    messages.extend(tail.into_iter().cloned());

    let mut compacted = request.clone();
    compacted.messages = messages;
    let compacted_estimate = estimate_request_tokens(&compacted);
    if compacted_estimate.saturating_add(COMPLETION_RESERVE_TOKENS) > window_ctx {
        return None;
    }
    compacted
        .metadata
        .insert("compacted".into(), serde_json::Value::Bool(true));
    compacted.metadata.insert(
        "compacted_estimate_tokens".into(),
        serde_json::Value::from(compacted_estimate),
    );
    tracing::info!(
        target: "router.server",
        session = %session_id,
        from_tokens = estimate,
        to_tokens = compacted_estimate,
        window_ctx = window_ctx,
        dropped_messages = dropped,
        "request compacted from LOD5 labels to fit the target window"
    );
    Some(compacted)
}

fn role_name(role: &OriginRole) -> &'static str {
    match role {
        OriginRole::User => "user",
        OriginRole::System => "system",
        OriginRole::Assistant => "assistant",
        OriginRole::Tool => "tool",
        OriginRole::Subagent => "subagent",
        OriginRole::SelfOrigin => "self",
        OriginRole::Other(_) => "msg",
    }
}
#[cfg(test)]
#[path = "../../tests/server_compaction.rs"]
mod tests;
