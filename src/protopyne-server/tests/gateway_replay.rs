//! Interactive replay through the gateway: open, step, read, close.
//!
//! The gateway owns no decision logic — it validates the open, holds the
//! driver in the registry, and bridges each step onto the shared runtime.
//! These tests pin the verb contract (fail-closed opens, bounded steps,
//! unknown-session refusals) and one end-to-end property: stepping a session
//! to completion decides every bar from the start bar on.

use std::path::PathBuf;

use protopyne_types::{Bar, Money};
use protopyne_view::{ReplayStartParams, TerminalGateway};

use crate::service::{serve, ServerConfig};

/// Step a replay session the way the terminal does: the gateway is sync,
/// so the step runs on a blocking thread with no runtime — the shared
/// bridge drives the async driver there. Calling the sync seam straight
/// from an async test would nest runtimes and refuse.
async fn step(
    gateway: &crate::gateway::ServerGateway,
    session: &str,
    bars: u64,
) -> Result<protopyne_view::ReplayStateView, protopyne_view::GatewayError> {
    let gateway = gateway.clone();
    let session = session.to_owned();
    tokio::task::spawn_blocking(move || gateway.replay_step(&session, bars))
        .await
        .expect("blocking step joins")
}

fn history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}

fn params(session: &str) -> ReplayStartParams {
    ReplayStartParams {
        session_id: session.to_owned(),
        underlying: "SPY".to_owned(),
        strategy_name: "ribbon".to_owned(),
        max_loss: Money::from(400),
        notional: Money::from(2_000),
        dte_days: 30,
        warmup_bars: 0,
        seed: 7,
        start_bar: 0,
        structure: "Single".to_owned(),
        grant: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_opens_steps_reads_and_closes() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, history()).await.expect("test server boots");
    let gateway = handle.gateway();
    let total = history().len() as u64;

    let opened = gateway
        .replay_start(params("terminal"))
        .expect("replay opens");
    assert!(opened.active);
    assert_eq!(opened.session_id, "terminal");
    assert_eq!(opened.cursor, 0, "a zero start opens at the head");
    assert_eq!(opened.total_bars, total);
    assert!(opened.decisions.is_empty(), "opening decides nothing");

    let stepped = step(&gateway, "terminal", 3)
        .await
        .expect("three bars step");
    assert_eq!(stepped.cursor, 3);
    assert_eq!(stepped.decisions.len(), 3, "one verdict per decided bar");
    assert!(
        stepped
            .decisions
            .iter()
            .enumerate()
            .all(|(offset, decision)| decision.bar_index == offset as u64),
        "verdicts arrive in replay order"
    );

    let read = gateway.replay_state("terminal").expect("state reads");
    assert_eq!(read, stepped, "a read without stepping changes nothing");

    gateway.replay_stop("terminal").expect("the session closes");
    assert!(
        gateway.replay_state("terminal").is_err(),
        "a closed session reads as refused, never empty"
    );
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_steps_to_completion_from_a_start_bar() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, history()).await.expect("test server boots");
    let gateway = handle.gateway();
    let total = history().len() as u64;

    let opened = gateway
        .replay_start(ReplayStartParams {
            start_bar: 10,
            ..params("from-ten")
        })
        .expect("a mid-history replay opens");
    assert_eq!(opened.cursor, 10);
    // Step in chunks to the end: the last call decides the tail and holds.
    let mut state = opened;
    while state.cursor < total {
        state = step(&gateway, "from-ten", 64).await.expect("a chunk steps");
    }
    assert_eq!(state.cursor, total, "stepping reaches the last bar");
    assert_eq!(state.decisions.len() as u64, total - 10);
    let tail = step(&gateway, "from-ten", 64)
        .await
        .expect("stepping past the end holds");
    assert_eq!(tail.cursor, total);
    assert_eq!(tail.decisions.len(), state.decisions.len());
    // Fills book through the same paper fills as batch: singles under the
    // rehearsal grant fill honestly from bar-derived quotes.
    assert!(
        state.filled_orders > 0,
        "a full replay over liquid history fills something"
    );
    assert!(
        !state.journal.is_empty(),
        "submitted proposals journal their lifecycles"
    );
    assert!(
        state
            .journal
            .windows(2)
            .all(|pair| pair[0].seq < pair[1].seq),
        "journal rows read in sequence order"
    );
    let proposed: std::collections::HashSet<&str> = state
        .decisions
        .iter()
        .map(|decision| decision.proposal_id.as_str())
        .collect();
    assert!(
        state
            .journal
            .iter()
            .all(|row| proposed.contains(row.client_id.as_str())),
        "every journal row belongs to a decided bar's proposal"
    );
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_start_replaces_the_slot() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, history()).await.expect("test server boots");
    let gateway = handle.gateway();

    gateway
        .replay_start(params("terminal"))
        .expect("replay opens");
    step(&gateway, "terminal", 5).await.expect("five bars step");
    let reopened = gateway
        .replay_start(params("terminal"))
        .expect("reopen replaces");
    assert_eq!(
        reopened.cursor, 0,
        "a restart returns to the head, never merges"
    );
    assert!(reopened.decisions.is_empty());
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_verbs_fail_closed() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, history()).await.expect("test server boots");
    let gateway = handle.gateway();

    assert!(
        gateway
            .replay_start(ReplayStartParams {
                session_id: "  ".to_owned(),
                ..params("x")
            })
            .is_err(),
        "a blank slot refuses"
    );
    assert!(
        gateway
            .replay_start(ReplayStartParams {
                underlying: String::new(),
                ..params("x")
            })
            .is_err(),
        "a blank underlying refuses"
    );
    assert!(
        gateway
            .replay_start(ReplayStartParams {
                strategy_name: String::new(),
                ..params("x")
            })
            .is_err(),
        "a blank strategy refuses"
    );
    assert!(
        gateway
            .replay_start(ReplayStartParams {
                start_bar: u64::MAX,
                ..params("x")
            })
            .is_err(),
        "a start past the last bar refuses"
    );
    assert!(
        gateway
            .replay_start(ReplayStartParams {
                grant: Some(serde_json::json!({"grant_id": 7})),
                ..params("x")
            })
            .is_err(),
        "a malformed grant document refuses without arming"
    );
    gateway
        .replay_start(params("live"))
        .expect("a session opens");
    assert!(
        step(&gateway, "live", 0).await.is_err(),
        "a zero step refuses"
    );
    assert!(
        step(&gateway, "live", 257).await.is_err(),
        "an oversize step refuses"
    );
    assert!(
        step(&gateway, "live", u64::MAX).await.is_err(),
        "an unaddressable step refuses"
    );
    assert!(
        step(&gateway, "ghost", 1).await.is_err(),
        "an unknown session refuses"
    );
    assert!(
        gateway.replay_state("ghost").is_err(),
        "an unknown session reads as refused"
    );
    assert!(
        gateway.replay_stop("ghost").is_err(),
        "stopping a ghost refuses"
    );
    assert!(
        step(&gateway, "  ", 1).await.is_err(),
        "a blank slot refuses on step too"
    );
    handle.shutdown().await;
}

/// Boot with a chain feed config: the chain slice installs at open and the
/// gate screens every bar — covered bars book two-leg fills with no gate
/// note while uncovered bars refuse with the verbatim note, under one
/// Vertical session.
#[tokio::test(flavor = "multi_thread")]
async fn replay_boot_with_chain_feed_opens_and_keeps_the_gate() {
    let bars = history();
    // Two covered bars amid an uncovered history: the split below exercises
    // both arms of the screen in one session.
    let (handle, _dir) = chained_server(bars.clone(), &covering_rows(&bars, 4..6));
    let gateway = handle.gateway();
    let opened = gateway
        .replay_start(vertical_params("chained"))
        .expect("replay opens with chains");
    assert_eq!(opened.total_bars, history().len() as u64);
    let stepped = step(&gateway, "chained", 6).await.expect("steps proceed");
    assert_eq!(stepped.cursor, 6);
    assert_eq!(stepped.decisions.len(), 6);
    for decision in &stepped.decisions {
        if decision.bar_index < 4 {
            assert!(
                !decision.submitted,
                "an uncovered bar submits nothing: {decision:?}"
            );
            assert_eq!(
                decision.skipped.as_deref(),
                Some(crate::replay_session::CHAIN_GATE_NOTE),
                "the refusal names the missing feed verbatim"
            );
            assert!(decision.chain_gated, "the uncovered bar is gate-noted");
        } else {
            assert!(
                decision.submitted,
                "a covered bar submits: {:?}",
                decision.skipped
            );
            assert_eq!(
                decision.legs.len(),
                2,
                "the covered fill is a two-leg package"
            );
            assert!(!decision.chain_gated, "no gate note on a covered bar");
        }
    }
    handle.shutdown().await;
}

/// Fail closed at boot: a chain feed naming a missing or malformed chain
/// file refuses to bind rather than serving replays against half-loaded
/// chains.
#[tokio::test(flavor = "multi_thread")]
async fn replay_boot_with_unreadable_chains_refuses_to_bind() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Missing chain file.
    let feed_path = dir.path().join("chain-feed.json");
    std::fs::write(
        &feed_path,
        r#"{"chain_data_path": "/nonexistent/chain.csv"}"#,
    )
    .expect("feed config writes");
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .maybe_chain_feed_path(Some(feed_path.to_string_lossy().into_owned()))
        .build();
    assert!(
        serve(config, history()).await.is_err(),
        "a missing chain file refuses to bind"
    );
    // Malformed chain file.
    let chain_path = dir.path().join("bad-chain.csv");
    std::fs::write(&chain_path, "ts,underlying,bid\n1,SPY,3\n").expect("bad chain writes");
    let feed_path = dir.path().join("bad-feed.json");
    std::fs::write(
        &feed_path,
        format!(
            "{{\"chain_data_path\": {}}}",
            serde_json::to_string(&chain_path.to_string_lossy()).expect("path encodes")
        ),
    )
    .expect("feed config writes");
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .maybe_chain_feed_path(Some(feed_path.to_string_lossy().into_owned()))
        .build();
    assert!(
        serve(config, history()).await.is_err(),
        "a malformed chain file refuses to bind"
    );
}

/// Chain CSV rows for `bars` at the given indices: the 690/695 call pair
/// around the ~694 close on one expiry inside the replay grant's DTE
/// window, quoted one tick wide at each bar's own timestamp so leg fills
/// pass the age gate and the package clears its tolerance.
fn covering_rows(bars: &[Bar], indices: std::ops::Range<usize>) -> String {
    let mut csv = "ts,underlying,expiry,strike,kind,bid,ask,bid_size,ask_size\n".to_owned();
    for bar in &bars[indices] {
        let ts = bar.ts.timestamp();
        use std::fmt::Write as _;
        let _ = writeln!(csv, "{ts},SPY,2026-02-20,690,call,2.499,2.501,50,50");
        let _ = writeln!(csv, "{ts},SPY,2026-02-20,695,call,1.499,1.501,50,50");
    }
    csv
}

fn chained_server(
    bars: Vec<Bar>,
    chain_csv: &str,
) -> (crate::service::ServerHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let chain_path = dir.path().join("chain.csv");
    std::fs::write(&chain_path, chain_csv).expect("chain csv writes");
    let feed_path = dir.path().join("chain-feed.json");
    std::fs::write(
        &feed_path,
        format!(
            "{{\"chain_data_path\": {}}}",
            serde_json::to_string(&chain_path.to_string_lossy()).expect("path encodes")
        ),
    )
    .expect("feed config writes");
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .maybe_chain_feed_path(Some(feed_path.to_string_lossy().into_owned()))
        .build();
    let handle = common_core::runtime::block_on(serve(config, bars)).expect("server boots");
    (handle, dir)
}

fn vertical_params(session: &str) -> ReplayStartParams {
    ReplayStartParams {
        structure: "Vertical".to_owned(),
        ..params(session)
    }
}

/// A vertical open with covering chains fills spreads end to end: every
/// decided bar submits two-leg fills with journaled transitions, and no
/// verdict carries the gate note.
#[tokio::test(flavor = "multi_thread")]
async fn vertical_replay_with_covering_chains_fills_spreads() {
    let bars = history();
    let csv = covering_rows(&bars, 0..6);
    let (handle, _dir) = chained_server(bars, &csv);
    let gateway = handle.gateway();
    gateway
        .replay_start(vertical_params("vert"))
        .expect("vertical opens");
    let stepped = step(&gateway, "vert", 6).await.expect("steps proceed");
    assert_eq!(stepped.cursor, 6);
    assert_eq!(stepped.decisions.len(), 6);
    for decision in &stepped.decisions {
        assert!(
            decision.submitted,
            "covered bar submits: {:?}",
            decision.skipped
        );
        assert_eq!(decision.filled_quantity, 1, "the package is one unit");
        assert_eq!(decision.legs.len(), 2, "both legs are recorded");
        assert!(!decision.chain_gated, "no gate note on a covered bar");
    }
    assert!(
        !stepped.journal.is_empty(),
        "the fills journal their lifecycle"
    );
    assert!(
        !stepped.positions.is_empty(),
        "spread fills fold into positions"
    );
    assert!(
        stepped.total_fees.as_decimal() > rust_decimal::Decimal::ZERO,
        "fills pay fees"
    );
    handle.shutdown().await;
}

/// Bars without coverage refuse with the byte-identical gate note while
/// covered bars around them still fill: the run continues past the gap.
#[tokio::test(flavor = "multi_thread")]
async fn gap_bars_refuse_with_the_unchanged_note_and_continue() {
    let bars = history();
    // Quotes only at bars 4 and 5: bars 0..4 see an empty snapshot.
    let csv = covering_rows(&bars, 4..6);
    let (handle, _dir) = chained_server(bars, &csv);
    let gateway = handle.gateway();
    gateway
        .replay_start(vertical_params("gaps"))
        .expect("vertical opens");
    let early = step(&gateway, "gaps", 2)
        .await
        .expect("early steps proceed");
    assert_eq!(early.decisions.len(), 2);
    for decision in &early.decisions {
        assert!(!decision.submitted, "an uncovered bar submits nothing");
        assert_eq!(
            decision.skipped.as_deref(),
            Some(crate::replay_session::CHAIN_GATE_NOTE),
            "the refusal names the missing feed verbatim"
        );
        assert!(decision.chain_gated);
    }
    let late = step(&gateway, "gaps", 4)
        .await
        .expect("later steps proceed");
    assert_eq!(late.cursor, 6);
    let filled = late
        .decisions
        .iter()
        .filter(|decision| decision.submitted)
        .count();
    assert_eq!(filled, 2, "covered bars fill after the gap");
    handle.shutdown().await;
}

/// Structure tags outside the defined-risk envelope refuse at open, before
/// any driver constructs: no half-built session lingers in the registry.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_and_naked_structures_refuse_at_open() {
    let bars = history();
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = common_core::runtime::block_on(serve(config, bars)).expect("server boots");
    let gateway = handle.gateway();
    for (tag, session) in [("Sideways", "bad-tag"), ("NakedShort", "naked")] {
        let outcome = gateway.replay_start(ReplayStartParams {
            structure: tag.to_owned(),
            ..params(session)
        });
        assert!(outcome.is_err(), "{tag} refuses at open");
        assert!(
            gateway.replay_state(session).is_err(),
            "a refused open leaves no session behind"
        );
    }
    handle.shutdown().await;
}

/// Vendored chain fixture over the `SPY_15M` range: the 690/695 call pair,
/// quoted tight at every bar's own timestamp.
fn vendored_chain_csv() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_CHAIN_15M.csv")
}

fn vendored_server(bars: Vec<Bar>) -> (crate::service::ServerHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let feed_path = dir.path().join("chain-feed.json");
    std::fs::write(
        &feed_path,
        format!(
            "{{\"chain_data_path\": {}}}",
            serde_json::to_string(&vendored_chain_csv().to_string_lossy()).expect("path encodes")
        ),
    )
    .expect("feed config writes");
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .maybe_chain_feed_path(Some(feed_path.to_string_lossy().into_owned()))
        .build();
    let handle = common_core::runtime::block_on(serve(config, bars)).expect("server boots");
    (handle, dir)
}

/// Production trust gate: the full vertical replay over vendored history is
/// frozen decision-for-decision. Any later move to selector policy,
/// slippage, or grant tiers re-earns this tape first.
#[tokio::test(flavor = "multi_thread")]
async fn vertical_replay_economics_are_frozen() {
    let bars = history();
    let (handle, _dir) = vendored_server(bars);
    let gateway = handle.gateway();
    gateway
        .replay_start(vertical_params("frozen"))
        .expect("vertical opens");
    let done = step(&gateway, "frozen", 256)
        .await
        .expect("steps to completion");
    assert_eq!(done.cursor, done.total_bars);
    assert_eq!(done.decisions.len(), done.total_bars as usize);
    assert_eq!(done.filled_orders, 11, "every bar fills one spread");
    assert_eq!(
        done.total_fees,
        Money::new(rust_decimal::Decimal::new(1430, 2)),
        "eleven fills at 1.30 per two-leg unit"
    );
    assert_eq!(done.journal.len(), 55, "five lifecycle rows per fill");
    // Net debit 1.006, bar after bar: long 690 at 2.501 plus 10 bps
    // (2.504) minus short 695 at 1.499 less 10 bps (1.498).
    let net = Money::new(rust_decimal::Decimal::new(1006, 3));
    for (index, decision) in done.decisions.iter().enumerate() {
        assert_eq!(
            decision.bar_index, index as u64,
            "verdicts stay in replay order"
        );
        assert!(decision.submitted);
        assert_eq!(decision.filled_quantity, 1, "the package is one unit");
        assert_eq!(decision.legs.len(), 2, "both legs are recorded");
        assert_eq!(decision.fill_price, Some(net), "frozen net debit");
        assert_eq!(decision.skipped, None);
        assert!(!decision.chain_gated);
        assert_eq!(decision.structure.as_deref(), Some("Vertical"));
    }
    handle.shutdown().await;
}

/// Backward-compat proof: with chains absent, the same open reproduces the
/// all-refused tape exactly — every bar names the missing feed, nothing
/// submits, no lifecycle journals.
#[tokio::test(flavor = "multi_thread")]
async fn chains_absent_replay_reproduces_the_all_refused_tape() {
    let bars = history();
    let total = bars.len();
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = common_core::runtime::block_on(serve(config, bars)).expect("server boots");
    let gateway = handle.gateway();
    gateway
        .replay_start(vertical_params("refused"))
        .expect("vertical opens");
    let done = step(&gateway, "refused", 256)
        .await
        .expect("steps to completion");
    assert_eq!(done.decisions.len(), total);
    for decision in &done.decisions {
        assert!(!decision.submitted);
        assert_eq!(
            decision.skipped.as_deref(),
            Some(crate::replay_session::CHAIN_GATE_NOTE)
        );
        assert!(decision.chain_gated);
    }
    assert!(done.journal.is_empty(), "refusals journal nothing");
    assert!(done.positions.is_empty(), "refusals fold no positions");
    handle.shutdown().await;
}

/// Fetch acceptance: a loopback tastytrade snapshot carrying the M5
/// economics lands a file that replays the frozen tape. The fetch output
/// and the hand-written fixture are interchangeable by construction — this
/// test proves it end to end rather than asserting it.
#[tokio::test(flavor = "multi_thread")]
async fn fetched_fixture_reproduces_the_frozen_tape() {
    use fluent_concurrency::io::net::NetCapability;
    use fluent_wvr::{CapabilitySet, CURRENT_CAPS};
    use protopyne_venue::{TastytradeConfig, TastytradeTransport};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let bars = history();
    // One 690/695 call pair per bar timestamp, in the chain-wire shape.
    let mut wires = Vec::new();
    for bar in &bars {
        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(bar.ts.timestamp(), 0)
            .expect("bar ts valid")
            .to_rfc3339();
        for (strike, bid, ask) in [("690", "2.499", "2.501"), ("695", "1.499", "1.501")] {
            wires.push(format!(
                r#"{{"underlying":"SPY","expiry":"2026-02-20","strike":"{strike}","kind":"call","bid":"{bid}","ask":"{ask}","bid_size":50,"ask_size":50,"open_interest":500,"iv":0.25,"ts":"{ts}"}}"#
            ));
        }
    }
    let body = format!(r#"{{"quotes":[{}]}}"#, wires.join(","));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = vec![0_u8; 65536];
            let mut head = Vec::new();
            loop {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        head.extend_from_slice(&buf[..n]);
                        if head.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });

    let dir = tempfile::tempdir().expect("tempdir");
    let creds = dir.path().join("tasty-creds.json");
    std::fs::write(&creds, r#"{"token":"acceptance"}"#).expect("creds write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    let out = dir.path().join("fetched-chain.csv");
    let transport = TastytradeTransport::new(TastytradeConfig::new().base_url(base).build());
    let net = NetCapability::new();
    let caps = CapabilitySet::new().with(NetCapability::new());
    let fetched = CURRENT_CAPS
        .scope(
            caps,
            crate::chain_history::fetch_and_write_chain_history(
                &net,
                &transport,
                creds.to_str().expect("utf8"),
                "SPY",
                &out,
            ),
        )
        .await
        .expect("loopback fetch lands");
    assert_eq!(fetched, bars.len() * 2, "one row per wire entry");

    // Boot over the fetched file and replay the frozen vertical run.
    let feed_path = dir.path().join("chain-feed.json");
    std::fs::write(
        &feed_path,
        format!(
            "{{\"chain_data_path\": {}}}",
            serde_json::to_string(&out.to_string_lossy()).expect("path encodes")
        ),
    )
    .expect("feed config writes");
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .maybe_chain_feed_path(Some(feed_path.to_string_lossy().into_owned()))
        .build();
    let handle = common_core::runtime::block_on(serve(config, bars)).expect("server boots");
    let gateway = handle.gateway();
    gateway
        .replay_start(vertical_params("fetched"))
        .expect("vertical opens");
    let done = step(&gateway, "fetched", 256)
        .await
        .expect("steps to completion");
    assert_eq!(done.cursor, done.total_bars);
    assert_eq!(done.filled_orders, 11, "the fetched tape fills every bar");
    assert_eq!(
        done.total_fees,
        Money::new(rust_decimal::Decimal::new(1430, 2)),
        "the fetched tape pays the frozen fees"
    );
    assert_eq!(
        done.journal.len(),
        55,
        "the fetched tape journals the frozen lifecycle"
    );
    let net = Money::new(rust_decimal::Decimal::new(1006, 3));
    for decision in &done.decisions {
        assert!(decision.submitted);
        assert_eq!(
            decision.fill_price,
            Some(net),
            "the fetched tape holds the frozen debit"
        );
    }
    handle.shutdown().await;
}

/// Terminal truth: a filled spread verdict carries one row per executed
/// leg — contract, side, quantity, slipped price, and fee — read off the
/// venue's own executions, so the terminal shows what the venue filled.
#[tokio::test(flavor = "multi_thread")]
async fn vertical_decisions_carry_per_leg_execution_rows() {
    let bars = history();
    let csv = covering_rows(&bars, 0..6);
    let (handle, _dir) = chained_server(bars, &csv);
    let gateway = handle.gateway();
    gateway
        .replay_start(vertical_params("legs"))
        .expect("vertical opens");
    let stepped = step(&gateway, "legs", 2).await.expect("steps proceed");
    assert_eq!(stepped.decisions.len(), 2);
    for decision in &stepped.decisions {
        assert!(decision.submitted);
        assert_eq!(decision.legs.len(), 2, "one row per executed leg");
        assert_eq!(decision.legs[0].contract, "SPY   260220C00690000");
        assert_eq!(decision.legs[0].side, "buy");
        assert_eq!(decision.legs[1].contract, "SPY   260220C00695000");
        assert_eq!(decision.legs[1].side, "sell");
        assert_eq!(
            (decision.legs[0].quantity, decision.legs[1].quantity),
            (1, 1)
        );
        // Slipped per-leg prices behind the frozen 1.006 net debit (2.503501
        // long minus 1.497501 short), 0.65 commission per leg.
        assert_eq!(
            decision.legs[0].price,
            Money::new(rust_decimal::Decimal::new(2503501, 6))
        );
        assert_eq!(
            decision.legs[1].price,
            Money::new(rust_decimal::Decimal::new(1497501, 6))
        );
        assert_eq!(
            (decision.legs[0].fee, decision.legs[1].fee),
            (
                Money::new(rust_decimal::Decimal::new(65, 2)),
                Money::new(rust_decimal::Decimal::new(65, 2))
            )
        );
    }
    handle.shutdown().await;
}
