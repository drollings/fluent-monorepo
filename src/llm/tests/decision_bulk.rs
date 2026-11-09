//! Bulk helpers tests (tests-first).

use fluent_llm::client::LlmClient;
use fluent_llm::decision::{
    bulk_filter, bulk_rerank, bulk_route, decide_all, Decision, Question, QuestionSpec,
};

fn spec(id: &str, options: &str) -> QuestionSpec {
    QuestionSpec::new()
        .id(id.to_string())
        .version(1)
        .kind("choice".to_string())
        .prompt_template("Pick.".to_string())
        .options_csv(options.to_string())
        .act_threshold(0.9)
        .review_threshold(0.7)
        .timeout_ms(5000)
        .cacheable(false)
        .build()
}

fn choice_body(top: &str, options: &[&str]) -> serde_json::Value {
    let tops: Vec<serde_json::Value> = options
        .iter()
        .map(|o| {
            let lp = if *o == top { -0.02 } else { -4.0 };
            serde_json::json!({"token": o, "logprob": lp})
        })
        .collect();
    serde_json::json!({
        "id": "cmpl-1", "object": "chat.completion", "created": 0, "model": "m",
        "choices": [{
            "index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": top},
            "logprobs": {"content": [{
                "token": top, "logprob": -0.02, "top_logprobs": tops
            }]}
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

fn canned(label: &str, confidence: f64) -> Decision {
    // Genuine top-1 confidence: the label holds `confidence` only when it is
    // the majority; the low case below inverts so top-1 stays low.
    let (top, top_p, rest_p) = if confidence >= 0.5 {
        (label.to_string(), confidence, 1.0 - confidence)
    } else {
        ("other".to_string(), 1.0 - confidence, confidence)
    };
    let other_label = if top == label {
        "other".to_string()
    } else {
        label.to_string()
    };
    Decision::from_distribution(
        label,
        vec![(top, top_p), (other_label, rest_p)],
        "m".to_string(),
        1,
        5,
        false,
    )
    .expect("canned")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn order_preserved_under_shuffled_completion() {
    let server = httpmock::MockServer::start();
    // Slowest first: completion order differs from submission order.
    for (q, top, options, delay) in [
        ("qq-0", "B", vec!["A", "B"], 150),
        ("qq-1", "D", vec!["C", "D"], 50),
        ("qq-2", "F", vec!["E", "F"], 0),
    ] {
        server.mock(|when, then| {
            when.method("POST")
                .path("/chat/completions")
                .body_contains(q);
            then.status(200)
                .delay(std::time::Duration::from_millis(delay))
                .json_body(choice_body(top, &options));
        });
    }
    let client = LlmClient::new(&server.base_url(), "m");
    let specs = [spec("q0", "A,B"), spec("q1", "C,D"), spec("q2", "E,F")];
    let questions = [
        Question {
            spec: &specs[0],
            state: "shared state",
            question: "qq-0?",
        },
        Question {
            spec: &specs[1],
            state: "shared state",
            question: "qq-1?",
        },
        Question {
            spec: &specs[2],
            state: "shared state",
            question: "qq-2?",
        },
    ];
    let out = decide_all(&client, &questions, 4, 16, None).await;
    assert_eq!(out.len(), 3);
    let labels: Vec<String> = out.into_iter().map(|r| r.expect("decide").label).collect();
    assert_eq!(labels, vec!["B", "D", "F"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_cancels_in_flight() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(200)
            .delay(std::time::Duration::from_millis(300))
            .json_body(choice_body("B", &["A", "B"]));
    });
    let client = LlmClient::new(&server.base_url(), "m");
    let specs = [spec("q0", "A,B")];
    let questions = [Question {
        spec: &specs[0],
        state: "st",
        question: "q?",
    }];
    let abort = fluent_concurrency::stream::StreamAbort::new();
    abort.cancel();
    let out = decide_all(&client, &questions, 2, 8, Some(abort)).await;
    assert_eq!(out.len(), 1);
    let err = out.into_iter().next().unwrap().expect_err("canceled");
    assert!(err.is_unavailable());
}

#[test]
fn shared_prefix_golden() {
    // One shared state, N questions: every render carries the identical
    // state prefix (prefix-cache reuse), each ending with its own question.
    let specs = [spec("q0", "A,B"), spec("q1", "C,D")];
    let questions = [
        Question {
            spec: &specs[0],
            state: "SHARED-STATE",
            question: "first?",
        },
        Question {
            spec: &specs[1],
            state: "SHARED-STATE",
            question: "second?",
        },
    ];
    let rendered: Vec<String> = questions.iter().map(Question::render).collect();
    for r in &rendered {
        assert!(r.starts_with("SHARED-STATE"));
    }
    assert!(rendered[0].ends_with("first?"));
    assert!(rendered[1].ends_with("second?"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_spec_never_sends() {
    use fluent_wvr::FieldAccess;
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(200).json_body(choice_body("B", &["A", "B"]));
    });
    let client = LlmClient::new(&server.base_url(), "m");
    // Unknown kind smuggled past the builder via runtime field access.
    let mut bad_kind = spec("bad", "A,B");
    bad_kind.set_field("kind", "nonsense").expect("set kind");
    // Cross-field breakage the derive cannot see: act below review.
    let mut bad_thresholds = spec("bad", "A,B");
    bad_thresholds
        .set_field("act_threshold", "0.5")
        .expect("set act");
    bad_thresholds
        .set_field("review_threshold", "0.8")
        .expect("set review");
    for bad in [&bad_kind, &bad_thresholds] {
        let questions = [Question {
            spec: bad,
            state: "st",
            question: "q?",
        }];
        let out = decide_all(&client, &questions, 2, 8, None).await;
        assert_eq!(out.len(), 1);
        let err = out.into_iter().next().unwrap().expect_err("invalid spec");
        assert!(matches!(
            err,
            fluent_llm::decision::DecisionError::Rejected(_)
        ));
    }
    mock.assert_hits(0);
}

#[test]
fn filter_route_fail_closed_on_invalid_thresholds() {
    use fluent_wvr::FieldAccess;
    let mut bad = spec("bad", "A,B");
    bad.set_field("act_threshold", "0.5").expect("set act");
    bad.set_field("review_threshold", "0.8")
        .expect("set review");
    let decisions = vec![Decision::from_distribution(
        "A",
        vec![("A".to_string(), 0.99), ("B".to_string(), 0.01)],
        "m".to_string(),
        1,
        0,
        false,
    )
    .expect("canned")];
    assert!(bulk_filter(&bad, &decisions).is_empty());
    assert!(bulk_route(&bad, &decisions).is_none());
}

#[test]
fn filter_rerank_route_are_pure_post_processors() {
    let specs = [spec("q0", "A,B")];
    let decisions = vec![canned("A", 0.95), canned("B", 0.8), canned("A", 0.6)];
    let kept = bulk_filter(&specs[0], &decisions);
    assert_eq!(kept, vec![0, 1]);
    let ranked = bulk_rerank(&decisions);
    assert_eq!(ranked, vec![0, 1, 2]);
    let best = bulk_route(&specs[0], &decisions).expect("route");
    assert_eq!(best.0, 0);
    let low = vec![canned("A", 0.6)];
    assert!(bulk_route(&specs[0], &low).is_none());
}

#[test]
fn rotation_aggregation_merges_masses_by_label() {
    use fluent_llm::decision::{aggregate_rotations, rotation_agreement};
    let mk = |label: &str, top: f64| {
        Decision::from_distribution(
            label,
            vec![(label.to_string(), top), ("other".to_string(), 1.0 - top)],
            "m".to_string(),
            1,
            0,
            false,
        )
        .expect("canned")
    };
    // Same label across rotations strengthens; agreement holds.
    let same = vec![mk("A", 0.7), mk("A", 0.8)];
    let merged = aggregate_rotations(&same).expect("merge");
    assert_eq!(merged.label, "A");
    assert!(rotation_agreement(&same));
    // Split rotations disagree and stay review-grade, never silently won.
    let split = vec![mk("A", 0.9), mk("B", 0.9)];
    assert!(!rotation_agreement(&split));
    let merged = aggregate_rotations(&split).expect("merge");
    assert!(merged.confidence < 0.9, "split must not stay extreme");
    // Any degraded input degrades the aggregate (fail-closed evidence).
    let mut bad = mk("A", 0.7);
    bad.degraded = true;
    assert!(
        aggregate_rotations(&[mk("A", 0.8), bad])
            .expect("merge")
            .degraded
    );
    assert!(aggregate_rotations(&[]).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aggregated_choice_fans_rotations_and_merges() {
    use fluent_llm::decision::{decide_aggregated, ScriptedDecideBackend};
    use std::sync::Arc;
    let mk = |top: f64| {
        Decision::from_distribution(
            "A",
            vec![("A".to_string(), top), ("B".to_string(), 1.0 - top)],
            "stub".to_string(),
            1,
            0,
            false,
        )
        .expect("canned")
    };
    let backend = Arc::new(ScriptedDecideBackend::new(vec![
        Ok(mk(0.7)),
        Ok(mk(0.8)),
        Ok(mk(0.75)),
    ]));
    let mut client = LlmClient::new("http://127.0.0.1:9", "stub");
    client.decide_backend = Some(backend.clone());
    let mut rotated = spec("q0", "A,B,C");
    rotated.rotation = "rotate".to_string();
    let merged = decide_aggregated(&client, &rotated, "st", "qq?", 3, 7, None)
        .await
        .expect("aggregate");
    assert_eq!(merged.label, "A");
    // Three sends, three distinct option orders.
    let prompts = backend.prompts();
    assert_eq!(prompts.len(), 3);
    let orders: Vec<String> = prompts
        .iter()
        .map(|p| {
            p.lines()
                .find(|l| l.starts_with("Options: "))
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_ne!(orders[0], orders[1]);
    assert_ne!(orders[1], orders[2]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hierarchical_route_tournaments_large_sets() {
    use fluent_llm::decision::{route_hierarchical, ScriptedDecideBackend};
    use std::sync::Arc;
    let win = |label: &str, others: &[&str]| {
        let mut dist = vec![(label.to_string(), 0.9)];
        for o in others {
            dist.push((o.to_string(), 0.1 / others.len() as f64));
        }
        Decision::from_distribution(label, dist, "stub".to_string(), 1, 0, false).expect("canned")
    };
    let backend = Arc::new(ScriptedDecideBackend::new(vec![
        Ok(win("O3", &["O0", "O1"])),
        Ok(win("O11", &["O10"])),
        Ok(win("O11", &["O3"])),
    ]));
    let mut client = LlmClient::new("http://127.0.0.1:9", "stub");
    client.decide_backend = Some(backend.clone());
    let mut big = spec("big", "A,B");
    big.options_csv = (0..12)
        .map(|i| format!("O{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let winner = route_hierarchical(&client, &big, "st", "qq?", None)
        .await
        .expect("tournament");
    assert_eq!(winner.label, "O11");
    assert_eq!(backend.prompts().len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pool_healthy_after_abort() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/chat/completions");
        then.status(200)
            .delay(std::time::Duration::from_millis(200))
            .json_body(choice_body("B", &["A", "B"]));
    });
    let client = LlmClient::new(&server.base_url(), "m");
    let specs = [spec("q0", "A,B")];
    let questions = [Question {
        spec: &specs[0],
        state: "st",
        question: "q?",
    }];
    let abort = fluent_concurrency::stream::StreamAbort::new();
    abort.cancel();
    let cancelled = decide_all(&client, &questions, 2, 8, Some(abort)).await;
    assert!(cancelled.into_iter().next().unwrap().is_err());
    // A fresh pool per call: the next run is unaffected by the abort.
    let recovered = decide_all(&client, &questions, 2, 8, None).await;
    assert_eq!(
        recovered
            .into_iter()
            .next()
            .unwrap()
            .expect("healthy")
            .label,
        "B"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_call_helpers_cover_common_cases() {
    use fluent_llm::decision::{filter_noul, rank_score, route_choice, ScriptedDecideBackend};
    use std::sync::Arc;
    let yes = || {
        Decision::from_distribution(
            "Yes",
            vec![("Yes".to_string(), 0.97), ("No".to_string(), 0.03)],
            "stub".to_string(),
            1,
            0,
            false,
        )
        .expect("yes")
    };
    let backend = Arc::new(ScriptedDecideBackend::new(vec![
        Ok(yes()),
        Ok(yes()),
        Ok(Decision::from_distribution(
            "B",
            vec![("B".to_string(), 0.9), ("A".to_string(), 0.1)],
            "stub".to_string(),
            1,
            0,
            false,
        )
        .expect("choice")),
        Ok(Decision::from_distribution(
            "4",
            vec![
                ("1".to_string(), 0.02),
                ("2".to_string(), 0.03),
                ("3".to_string(), 0.10),
                ("4".to_string(), 0.80),
                ("5".to_string(), 0.05),
            ],
            "stub".to_string(),
            1,
            0,
            false,
        )
        .expect("score")),
        Ok(Decision::from_distribution(
            "2",
            vec![
                ("1".to_string(), 0.10),
                ("2".to_string(), 0.70),
                ("3".to_string(), 0.10),
                ("4".to_string(), 0.05),
                ("5".to_string(), 0.05),
            ],
            "stub".to_string(),
            1,
            0,
            false,
        )
        .expect("score")),
    ]));
    let mut client = LlmClient::new("http://127.0.0.1:9", "stub");
    client.decide_backend = Some(backend);
    let mut noul = spec("noul", "Yes,No");
    noul.kind = "noul".to_string();
    let kept = filter_noul(
        &client,
        &noul,
        "water is wet; fire is cold",
        &["water is wet".to_string(), "fire is cold".to_string()],
        None,
    )
    .await;
    assert_eq!(kept.len(), 2);
    let choice = route_choice(&client, &spec("c", "A,B"), "st", "qq?", None)
        .await
        .expect("route");
    assert_eq!(choice, "B");
    let mut scale = spec("s", "1,2,3,4,5");
    scale.kind = "score".to_string();
    let ranked = rank_score(
        &client,
        &scale,
        "st",
        &["first".to_string(), "second".to_string()],
        None,
    )
    .await
    .expect("rank");
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].0, 0, "higher expected value ranks first");
    assert!(ranked[0].1 > ranked[1].1);
}
