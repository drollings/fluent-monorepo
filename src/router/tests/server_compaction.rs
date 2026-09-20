use super::*;
use std::collections::HashMap;

use fluent_types::openai_wire::{RouterMessage, RouterMessageContent, RouterRequest};

fn text_msg(role: &str, content: &str) -> RouterMessage {
    RouterMessage {
        role: role.into(),
        content: RouterMessageContent::Text(content.into()),
        tool_calls: None,
        tool_call_id: None,
    }
}

fn request_with_history(session: Option<&str>, turns: usize, pad: usize) -> RouterRequest {
    let mut messages = vec![text_msg("system", "you are helpful")];
    for i in 0..turns {
        messages.push(text_msg("user", &format!("question {i} {}", "w".repeat(pad))));
        messages.push(text_msg(
            "assistant",
            &format!("answer {i} {}", "v".repeat(pad)),
        ));
    }
    RouterRequest {
        model: "local".into(),
        messages,
        temperature: None,
        max_tokens: None,
        stream: None,
        tools: None,
        tool_choice: None,
        session_id: session.map(str::to_string),
        agent_id: None,
        adapter: None,
        instance: None,
        snapshot: None,
        id_slot: None,
        metadata: Default::default(),
    }
}

/// Seed an in-memory ledger the way the serving path does: one user node +
/// one accepted assistant node per turn (eager LOD5 labels ride along).
fn seeded_ledger(session: &str, turns: usize) -> Arc<ContentNodeStore> {
    let store = Arc::new(
        ContentNodeStore::open_in_memory().expect("in-memory ledger opens"),
    );
    for i in 0..turns {
        let node = store
            .record_request(session, &format!("r{i}"), &format!("question {i} about dragons"))
            .expect("record request");
        store
            .record_result(node, true, Some(1.0), &format!("answer {i} the mood is calm"))
            .expect("record result");
    }
    store
}

fn quick_roles(num_ctx: u64) -> HashMap<String, RoleEntry> {
    let role: RoleEntry = serde_json::from_value(serde_json::json!({
        "params": {"num_ctx": num_ctx},
        "models": ["last"],
    }))
    .expect("role parses");
    HashMap::from([("quick".to_string(), role)])
}

fn quick_target() -> RoutingTarget {
    serde_json::from_value(serde_json::json!({
        "url": "http://127.0.0.1:9/v1/chat/completions",
        "model": "qwen3.5-9b:quick",
        "role": "quick",
    }))
    .expect("target parses")
}

#[test]
fn window_ctx_comes_from_role_params() {
    let roles = quick_roles(32768);
    assert_eq!(window_ctx_for_role(&roles, &quick_target()), Some(32768));
    let mut bare = quick_target();
    bare.role = None;
    assert_eq!(
        window_ctx_for_role(&roles, &bare),
        None,
        "role-less targets skip the guard"
    );
}

#[test]
fn fitting_request_passes_through() {
    let req = request_with_history(Some("s1"), 2, 10);
    let ledger = seeded_ledger("s1", 2);
    assert!(
        maybe_compact_request(&req, 32768, &ledger).is_none(),
        "a fitting request dispatches uncompacted"
    );
}

#[test]
fn overflow_compacts_from_lod5_labels() {
    let session = "dragon-chat";
    let req = request_with_history(Some(session), 10, 400);
    let ledger = seeded_ledger(session, 10);
    let window_ctx = 2000;
    assert!(
        estimate_request_tokens(&req) + 1024 > window_ctx,
        "fixture must actually overflow the small window"
    );
    let compacted = maybe_compact_request(&req, window_ctx, &ledger)
        .expect("overflow compacts instead of dying on the fork's 400");

    // Newest user turn survives verbatim.
    let last_user = compacted
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .expect("compacted request keeps a user turn");
    assert!(
        last_user.content.to_string_lossy().contains("question 9"),
        "newest turn verbatim, got: {}",
        last_user.content.to_string_lossy()
    );
    // Earlier turns survive as ledger labels in one summary message.
    let summary = compacted
        .messages
        .iter()
        .find(|m| m.role == "system" && m.content.to_string_lossy().contains("Compacted history"))
        .expect("compacted request carries the label summary");
    let summary_text = summary.content.to_string_lossy();
    assert!(
        summary_text.contains("dragons"),
        "summary carries ledger labels, got: {summary_text}"
    );
    // The compacted prompt fits with headroom, and is marked.
    assert!(
        estimate_request_tokens(&compacted) + 1024 <= window_ctx,
        "compacted request fits the window"
    );
    assert_eq!(
        compacted.metadata.get("compacted"),
        Some(&serde_json::Value::Bool(true)),
        "compaction is auditable on the request"
    );
}

#[test]
fn single_huge_turn_cannot_compact() {
    // One user turn larger than the window compacts to nothing — the normal
    // chain then ends in the honest 400 rather than a mangled request.
    let mut req = request_with_history(Some("s2"), 0, 0);
    req.messages.push(text_msg("user", &"z".repeat(9000)));
    let ledger = seeded_ledger("s2", 0);
    assert!(
        maybe_compact_request(&req, 2000, &ledger).is_none(),
        "an uncompactable prompt falls through to the honest error path"
    );
}

#[test]
fn missing_session_or_labels_fails_open() {
    // No session id: no ledger lookup possible — dispatch uncompacted.
    let req = request_with_history(None, 10, 400);
    let ledger = seeded_ledger("other", 10);
    assert!(
        maybe_compact_request(&req, 2000, &ledger).is_none(),
        "session-less requests skip the guard"
    );
    // Session with no recorded turns: no labels — dispatch uncompacted.
    let req = request_with_history(Some("empty"), 10, 400);
    let ledger = seeded_ledger("elsewhere", 10);
    assert!(
        maybe_compact_request(&req, 2000, &ledger).is_none(),
        "label-less sessions skip the guard"
    );
}
