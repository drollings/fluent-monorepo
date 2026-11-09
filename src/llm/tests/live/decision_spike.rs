//! Throwaway live spike: Choice + Noul from single-token logprobs.
//!
//! Never merged into production paths. Compiled only with the `live-ai`
//! feature; every test is `#[ignore]`d and skips cleanly when
//! `LLM_BASE_URL`/`LLM_MODEL` are absent. Run via `make llm-test-live`.
//!
//! Env contract follows the existing live tests: `LLM_BASE_URL` is the
//! OpenAI-compatible base URL, `LLM_MODEL` the model name.

use common_core::http::shared_http_client;
use fluent_llm::block_on;
use fluent_llm::openai::build_openai_chat_body;
use fluent_llm::url::chat_completions_url;

fn live_env() -> Option<(String, String)> {
    let base = std::env::var("LLM_BASE_URL").ok()?;
    let model = std::env::var("LLM_MODEL").ok()?;
    Some((base, model))
}

fn decision_extras() -> serde_json::Value {
    serde_json::json!({
        "max_tokens": 1,
        "temperature": 0,
        "logprobs": true,
        "top_logprobs": 10,
    })
}

async fn post_full_response(
    base: &str,
    model: &str,
    system: &str,
    user: &str,
    extras: Option<&serde_json::Value>,
    think: Option<bool>,
) -> serde_json::Value {
    let url = chat_completions_url(base);
    let messages = serde_json::json!([
        {"role": "system", "content": system},
        {"role": "user", "content": user},
    ]);
    let body = build_openai_chat_body(model, &messages, extras, false, think);
    let body_str = serde_json::to_string(&body).expect("serialize spike body");
    let resp = shared_http_client()
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body_str)
        .send()
        .await
        .expect("live spike HTTP send");
    let text = resp.text().await.expect("live spike body text");
    serde_json::from_str(&text).expect("live spike JSON parse")
}

fn top_tokens(body: &serde_json::Value) -> Vec<(String, f64)> {
    body.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("logprobs"))
        .and_then(|l| l.get("content"))
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("top_logprobs"))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let tok = e.get("token")?.as_str()?.to_string();
                    let lp = e.get("logprob")?.as_f64()?;
                    Some((tok, lp))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn content_of(body: &serde_json::Value) -> String {
    body.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string()
}

#[test]
#[ignore = "live-AI: requires LLM_BASE_URL + LLM_MODEL; run via `make llm-test-live`"]
fn spike_choice_single_token_logprobs() {
    let Some((base, model)) = live_env() else {
        eprintln!("LLM_BASE_URL/LLM_MODEL not set; skipping choice spike");
        return;
    };
    let extras = decision_extras();
    let body = block_on(post_full_response(
        &base,
        &model,
        "Reply with exactly one of the single characters A, B, or C. No other text.",
        "Pick the second letter of the alphabet. Reply with only that letter.",
        Some(&extras),
        None,
    ));
    let content = content_of(&body);
    let tops = top_tokens(&body);
    eprintln!("choice content={content:?} tops={tops:?}");
    assert!(
        !content.trim().is_empty(),
        "choice response must be non-empty"
    );
    assert!(
        !tops.is_empty(),
        "choice response should carry top_logprobs when the endpoint honors logprobs"
    );
    let has_b = tops
        .iter()
        .any(|(t, _)| t.trim() == "B" || t.trim() == "A" || t.trim() == "C");
    assert!(has_b, "top_logprobs should contain a choice variant");
}

#[test]
#[ignore = "live-AI: requires LLM_BASE_URL + LLM_MODEL; run via `make llm-test-live`"]
fn spike_noul_single_token_logprobs() {
    let Some((base, model)) = live_env() else {
        eprintln!("LLM_BASE_URL/LLM_MODEL not set; skipping noul spike");
        return;
    };
    let extras = decision_extras();
    let body = block_on(post_full_response(
        &base,
        &model,
        "Reply with exactly Yes or No. No other text.",
        "Is water wet? Reply with only Yes or No.",
        Some(&extras),
        None,
    ));
    let content = content_of(&body);
    let tops = top_tokens(&body);
    eprintln!("noul content={content:?} tops={tops:?}");
    assert!(
        !content.trim().is_empty(),
        "noul response must be non-empty"
    );
    assert!(
        !tops.is_empty(),
        "noul response should carry top_logprobs when the endpoint honors logprobs"
    );
    let has_yes_no = tops
        .iter()
        .any(|(t, _)| t.trim().eq_ignore_ascii_case("yes") || t.trim().eq_ignore_ascii_case("no"));
    assert!(has_yes_no, "top_logprobs should contain a yes/no variant");
}

#[test]
#[ignore = "live-AI: requires LLM_BASE_URL + LLM_MODEL; run via `make llm-test-live`"]
fn spike_negative_probes() {
    let Some((base, model)) = live_env() else {
        eprintln!("LLM_BASE_URL/LLM_MODEL not set; skipping negative probes");
        return;
    };
    // Without the no-think default, reasoning models may spend the single
    // token on reasoning_content (empty content, first token "The").
    let no_think_off = block_on(post_full_response(
        &base,
        &model,
        "Reply with only the letter B.",
        "Reply with only the letter B.",
        Some(&decision_extras()),
        Some(true),
    ));
    let content = content_of(&no_think_off);
    let reasoning = no_think_off
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("reasoning_content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    eprintln!(
        "think-on content={content:?} reasoning_len={}",
        reasoning.len()
    );

    // logit_bias is silently ignored by llama-server: a maximal bias against
    // every token still returns prose.
    let biased = block_on(post_full_response(
        &base,
        &model,
        "Reply with Hello.",
        "Say hello.",
        Some(&serde_json::json!({"max_tokens": 1, "logit_bias": {"0": -100}})),
        None,
    ));
    eprintln!("logit_bias probe content={:?}", content_of(&biased));

    // response_format schemas are unenforced: a truncated object may return.
    let schema = block_on(post_full_response(
        &base,
        &model,
        "Reply with JSON.",
        "Reply with JSON.",
        Some(&serde_json::json!({"max_tokens": 4,
            "response_format": {"type": "json_object", "schema": {"type": "object"}}})),
        None,
    ));
    eprintln!("schema probe content={:?}", content_of(&schema));
}

#[test]
#[ignore = "live-AI: requires LLM_BASE_URL + LLM_MODEL; run via `make llm-test-live`"]
fn spike_shared_state_concurrency() {
    let Some((base, model)) = live_env() else {
        eprintln!("LLM_BASE_URL/LLM_MODEL not set; skipping concurrency spike");
        return;
    };
    for n in [1usize, 4, 16] {
        let start = std::time::Instant::now();
        std::thread::scope(|s| {
            let mut handles = Vec::new();
            for _ in 0..n {
                let (b, m) = (base.clone(), model.clone());
                handles.push(s.spawn(move || {
                    block_on(post_full_response(
                        &b,
                        &m,
                        "Reply with only the letter A.",
                        "Shared state: the sky is blue. Reply with only the letter A.",
                        Some(&decision_extras()),
                        None,
                    ))
                }));
            }
            for h in handles {
                let body = h.join().expect("spike thread");
                let usage = body.get("usage").cloned().unwrap_or_default();
                eprintln!("n={n} usage={usage}");
            }
        });
        let wall = start.elapsed();
        eprintln!(
            "n={n} wall_ms={} avg_ms={}",
            wall.as_millis(),
            wall.as_millis() / n as u128
        );
    }
}

#[test]
#[ignore = "live-AI: requires LLM_BASE_URL + LLM_MODEL; run via `make llm-test-live`"]
fn spike_option_order_bias() {
    // Same question, rotated option orders: reports whether the top label
    // follows the position or the content. Structural only (all decode).
    let Some((base, model)) = live_env() else {
        eprintln!("LLM_BASE_URL/LLM_MODEL not set; skipping bias spike");
        return;
    };
    let mut decoded = Vec::new();
    for order in [
        "A) bark  B) meow  C) moo",
        "A) moo  B) bark  C) meow",
        "A) meow  B) moo  C) bark",
    ] {
        let body = block_on(post_full_response(
            &base,
            &model,
            "Reply with exactly one of A, B, or C. No other text.",
            &format!("Which animal barks? Options: {order}. Reply with only the letter."),
            Some(&decision_extras()),
            None,
        ));
        eprintln!(
            "order={order:?} content={:?} tops={:?}",
            content_of(&body),
            top_tokens(&body)
        );
        if let Ok(d) = fluent_llm::decision::decode_choice(&body, &["A", "B", "C"], &model, 1) {
            decoded.push(d);
        }
    }
    eprintln!(
        "bias agreement={} over {} rotations",
        fluent_llm::decision::rotation_agreement(&decoded),
        decoded.len()
    );
}

#[test]
#[ignore = "live-AI: requires LLM_BASE_URL + LLM_MODEL; run via `make llm-test-live`"]
fn spike_score_shape() {
    // Numeric 1-5 scale through the single-token path: documents the live
    // ordinal shape (top label, expected rung, spread) for calibration.
    let Some((base, model)) = live_env() else {
        eprintln!("LLM_BASE_URL/LLM_MODEL not set; skipping score spike");
        return;
    };
    let body = block_on(post_full_response(
        &base,
        &model,
        "Reply with exactly one of 1, 2, 3, 4, 5. No other text.",
        "Rate relevance 1-5: a dictionary definition of photosynthesis vs \
         'how do leaves make food'. Reply with only the digit.",
        Some(&decision_extras()),
        None,
    ));
    eprintln!(
        "score content={:?} tops={:?}",
        content_of(&body),
        top_tokens(&body)
    );
    assert!(!content_of(&body).trim().is_empty());
}

#[test]
#[ignore = "live-AI: requires LLM_BASE_URL + LLM_MODEL; run via `make llm-test-live`"]
fn spike_canaries() {
    // Fixed canary suite against the live endpoint: verify each decision
    // and fingerprint the set. A drift here means the alias moved weights.
    let Some((base, model)) = live_env() else {
        eprintln!("LLM_BASE_URL/LLM_MODEL not set; skipping canary spike");
        return;
    };
    use fluent_llm::decision::{canary_suite, verify_canary};
    let mut verified = Vec::new();
    for case in canary_suite() {
        let body = block_on(post_full_response(
            &base,
            &model,
            &case.prompt,
            &case.prompt,
            Some(&decision_extras()),
            None,
        ));
        let refs: Vec<&str> = case.options.iter().map(String::as_str).collect();
        match fluent_llm::decision::decode_choice(&body, &refs, &model, 1) {
            Ok(decision) => {
                let drift = verify_canary(&decision, &case);
                eprintln!(
                    "canary {} drift={drift:?} label={}",
                    case.id, decision.label
                );
                if drift.is_ok() {
                    verified.push(decision);
                }
            }
            Err(error) => eprintln!("canary {} undecodable: {error}", case.id),
        }
    }
    if !verified.is_empty() {
        eprintln!(
            "canary fingerprint={}",
            fluent_llm::decision::canary_signature(&verified)
        );
    }
}
