//! Single-token logprobs decoder + text fallback tests (tests-first).

#[path = "decision_spike_fixtures.rs"]
mod fixtures;

use fluent_llm::decision::Verdict;
use fluent_llm::decision::{decode_choice, decode_noul, decode_score};

#[test]
fn choice_golden_decodes_b_with_summed_variants() {
    let body = fixtures::choice_b_response();
    let d = decode_choice(&body, &["A", "B", "C"], "fixture", 1).expect("decode B");
    assert_eq!(d.label, "B");
    // P(B) + P( B) summed, normalized over the expected labels.
    let mass_b = (-0.022f64).exp() + (-3.5f64).exp();
    let total = mass_b + (-4.2f64).exp() + (-5.1f64).exp();
    let want = mass_b / total;
    let got: f64 = d
        .distribution
        .iter()
        .find(|(l, _)| l == "B")
        .map_or(0.0, |(_, p)| *p);
    assert!((got - want).abs() < 1e-9, "got={got} want={want}");
    assert!(!d.cached);
}

#[test]
fn noul_golden_decodes_yes() {
    let body = fixtures::noul_yes_response();
    let d = decode_noul(&body, "fixture", 1).expect("decode Yes");
    assert_eq!(d.label, "Yes");
    assert!(d.confidence > 0.5);
}

#[test]
fn variant_split_sums_rather_than_picks() {
    let body = serde_json::json!({
        "choices": [{
            "message": {"role": "assistant", "content": "B"},
            "logprobs": {"content": [{
                "token": "B", "logprob": -0.7,
                "top_logprobs": [
                    {"token": "B", "logprob": -0.7},
                    {"token": " B", "logprob": -0.7},
                    {"token": "A", "logprob": -0.7}
                ]
            }]}
        }]
    });
    let d = decode_choice(&body, &["A", "B"], "m", 1).expect("decode");
    assert_eq!(d.label, "B");
    let total: f64 = d.distribution.iter().map(|(_, p)| p).sum();
    assert!((total - 1.0).abs() < 1e-6, "total={total}");
}

#[test]
fn reasoning_hijack_falls_back_instead_of_answering_the() {
    let body = serde_json::json!({
        "choices": [{
            "message": {"role": "assistant", "content": "",
                "reasoning_content": "The user wants B"},
            "logprobs": {"content": [{
                "token": "The", "logprob": -0.1,
                "top_logprobs": [{"token": "The", "logprob": -0.1}]
            }]}
        }]
    });
    let err = decode_choice(&body, &["A", "B"], "m", 1).expect_err("must not answer The");
    assert!(!err.is_unavailable());
}

#[test]
fn hostile_echo_of_non_candidate_never_a_label() {
    // The model parroting an injected token ("Z") must not decode when Z
    // is not a candidate — whole-string match only, no substring search.
    let body = serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": "Z"}}]
    });
    let err = decode_choice(&body, &["A", "B"], "m", 1).expect_err("no match");
    assert!(!err.is_unavailable());
}

#[test]
fn missing_logprobs_uses_text_fallback_with_low_confidence() {
    let body = serde_json::json!({
        "choices": [{
            "message": {"role": "assistant", "content": "B"}
        }]
    });
    let d = decode_choice(&body, &["A", "B", "C"], "m", 1).expect("fallback");
    assert_eq!(d.label, "B");
    assert!(d.confidence < 0.6, "fallback confidence downgraded");
}

#[test]
fn randomized_variant_splits_still_sum_to_one() {
    // Deterministic LCG: split P(B)=0.7 across two variants in 50 ways.
    let mut seed = 0x2026_1108u64;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        seed >> 33
    };
    for _ in 0..50 {
        let split = f64::from((next() % 1000) as u32) / 1000.0 * 0.7;
        let (b1, b2) = (split, 0.7 - split);
        let body = serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "B"},
                "logprobs": {"content": [{
                    "token": "B", "logprob": 0.0,
                    "top_logprobs": [
                        {"token": "B", "logprob": b1.ln()},
                        {"token": " B", "logprob": b2.ln()},
                        {"token": "A", "logprob": 0.3f64.ln()}
                    ]
                }]}
            }]
        });
        let d = decode_choice(&body, &["A", "B"], "m", 1).expect("decode");
        assert_eq!(d.label, "B");
        let total: f64 = d.distribution.iter().map(|(_, p)| p).sum();
        assert!((total - 1.0).abs() < 1e-9, "total={total}");
    }
}

#[test]
fn corrupt_entry_skipped_rest_still_decode() {
    // One NaN entry must not discard nine good ones: skip it, decode rest.
    let body = serde_json::json!({
        "choices": [{
            "message": {"role": "assistant", "content": "B"},
            "logprobs": {"content": [{
                "token": "B", "logprob": -0.02,
                "top_logprobs": [
                    {"token": "B", "logprob": -0.02},
                    {"token": "BOGUS", "logprob": f64::NAN},
                    {"token": " B", "logprob": -3.0},
                    {"token": "A", "logprob": -4.0}
                ]
            }]}
        }]
    });
    // serde_json cannot hold NaN: inject it as a string logprob instead,
    // which the decoder must also skip rather than abort on.
    let mut body = body;
    body["choices"][0]["logprobs"]["content"][0]["top_logprobs"][1]["logprob"] =
        serde_json::json!("nan");
    let d = decode_choice(&body, &["A", "B"], "m", 1).expect("rest decode");
    assert_eq!(d.label, "B");
    assert!(!d.degraded);
}

#[test]
fn prefix_labels_discriminate_exactly() {
    let body = serde_json::json!({
        "choices": [{
            "message": {"role": "assistant", "content": "AB"},
            "logprobs": {"content": [{
                "token": "AB", "logprob": -0.02,
                "top_logprobs": [
                    {"token": "AB", "logprob": -0.02},
                    {"token": "A", "logprob": -3.0}
                ]
            }]}
        }]
    });
    let d = decode_choice(&body, &["A", "AB"], "m", 1).expect("exact");
    assert_eq!(d.label, "AB");
}

#[test]
fn tiny_truncated_masses_normalize_safely() {
    // Truncated top lists with small absolute mass still decode; the
    // normalized confidence stays modest so policy reviews, never acts.
    let body = serde_json::json!({
        "choices": [{
            "message": {"role": "assistant", "content": "B"},
            "logprobs": {"content": [{
                "token": "B", "logprob": -4.5,
                "top_logprobs": [
                    {"token": "B", "logprob": -4.5},
                    {"token": "A", "logprob": -4.7}
                ]
            }]}
        }]
    });
    let d = decode_choice(&body, &["A", "B"], "m", 1).expect("decode");
    assert!(
        (d.confidence - 0.55).abs() < 0.01,
        "confidence={}",
        d.confidence
    );
    let t = fluent_llm::decision::ThresholdSet {
        act: 0.9,
        review: 0.7,
    };
    assert_eq!(
        fluent_llm::decision::decide_verdict(&d, &t),
        fluent_llm::decision::Verdict::Escalate
    );
}

fn ordered_scale_body() -> serde_json::Value {
    // Mass concentrated on "4" (index 3 of 1..5) with an ordinal tail.
    let entries: Vec<serde_json::Value> = [
        ("1", -6.0),
        ("2", -4.5),
        ("3", -2.2),
        ("4", -0.15),
        ("5", -3.1),
    ]
    .iter()
    .map(|(t, lp)| serde_json::json!({"token": t, "logprob": lp}))
    .collect();
    serde_json::json!({
        "choices": [{
            "message": {"role": "assistant", "content": "4"},
            "logprobs": {"content": [{
                "token": "4", "logprob": -0.15, "top_logprobs": entries
            }]}
        }]
    })
}

#[test]
fn score_decodes_ordered_scale_with_moment() {
    use fluent_llm::decision::{decide_score_verdict, score_moment};
    let body = ordered_scale_body();
    let d = decode_score(&body, &["1", "2", "3", "4", "5"], "m", 1).expect("score");
    assert_eq!(d.label, "4");
    let moment = score_moment(&d).expect("moment");
    // E ≈ 2.91 index units, tight spread around "4".
    assert!(
        (moment.expected - 2.91).abs() < 0.1,
        "E={}",
        moment.expected
    );
    assert!(moment.std < 0.8, "std={}", moment.std);
    // Directional verdict on the expected value, Noul-style.
    assert_eq!(decide_score_verdict(&d, 3.5, 0.5), Verdict::Review);
    assert_eq!(decide_score_verdict(&d, 2.5, 0.5), Verdict::Act);
}

#[test]
fn score_rejects_degenerate_scale() {
    let body = ordered_scale_body();
    let err = decode_score(&body, &["only"], "m", 1).expect_err("one rung is no scale");
    assert!(!err.is_unavailable());
}
