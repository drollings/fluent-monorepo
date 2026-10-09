use super::*;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeZone, Utc};

fn history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    let bars = crate::load_bars_csv(&path).expect("vendored history loads");
    bars[..60].to_vec()
}

fn test_grant(id: &str, max_trades: u32) -> Grant {
    Grant::new()
        .id(id.to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical])
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission { entries: true, exits: true, rolls: false })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(max_trades)
                .max_contracts(10_000)
                .max_loss(Money::from(1_000_000))
                .max_notional(Money::from(100_000_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.0)
                .min_activity_score(0.0)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(Money::from(1_000))
                .max_spread_width(Money::from(50))
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(Money::from(10_000_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

async fn boot() -> ServerHandle {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    serve(config, history()).await.expect("test server boots")
}

fn base(handle: &ServerHandle) -> String {
    format!("http://{}", handle.addr)
}

#[test]
fn bar_frames_carry_feed_cycle() {
    let bar = &history()[0];
    for cycle in [0u64, 3] {
        let text = bar_frame(cycle, 7, bar).expect("chartable bar frames");
        let frame: serde_json::Value = serde_json::from_str(&text).expect("frame is JSON");
        assert_eq!(frame["type"], "bar");
        assert_eq!(frame["index"], 7);
        assert_eq!(frame["cycle"], cycle);
    }
}

#[tokio::test]
async fn config_validation_is_fail_closed() {
    assert!(ServerConfig::default().validate().is_ok());
    let bad = ServerConfig::new().bind("not-an-address".to_owned()).build();
    assert!(bad.validate().is_err());
    let zero = ServerConfig::new().max_concurrent_requests(0).build();
    assert!(zero.validate().is_err());
    assert!(serve(ServerConfig::default(), Vec::new()).await.is_err());
}

#[tokio::test]
async fn health_reports_version() {
    let handle = boot().await;
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/health", base(&handle)))
        .send()
        .await
        .expect("health reachable")
        .json()
        .await
        .expect("health is JSON");
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    handle.shutdown().await;
}

#[tokio::test]
async fn preview_populates_inbox_and_portfolio() {
    let handle = boot().await;
    let client = reqwest::Client::new();
    let response: PreviewResponse = client
        .post(format!("{}/proposals/preview", base(&handle)))
        .json(&PreviewParams { seed: 3 })
        .send()
        .await
        .expect("preview reachable")
        .json()
        .await
        .expect("preview is a run");
    let run = response.run;
    assert_eq!(run.decisions.len(), 60);
    assert!(run.report.fills > 0);
    for entry in &response.deny_histogram {
        assert!(!entry.reason.is_empty());
        assert!(entry.bars > 0);
    }
    let tallied: u64 = response.deny_histogram.iter().map(|entry| entry.bars).sum();
    assert!(tallied <= 3 * run.decisions.len() as u64);

    let inbox: Vec<ProposalView> = client
        .get(format!("{}/proposals", base(&handle)))
        .send()
        .await
        .expect("inbox reachable")
        .json()
        .await
        .expect("inbox is JSON");
    assert!(!inbox.is_empty());
    assert!(inbox.iter().all(|row| row.strategy == "ribbon"));
    assert!(inbox.iter().any(|row| row.state == ProposalState::Filled));

    let portfolio: PortfolioView = client
        .get(format!("{}/portfolio", base(&handle)))
        .send()
        .await
        .expect("portfolio reachable")
        .json()
        .await
        .expect("portfolio is JSON");
    assert_eq!(portfolio.filled_orders, run.report.fills);
    assert!(!portfolio.positions.is_empty());
    handle.shutdown().await;
}

#[tokio::test]
async fn unfilled_inbox_rows_carry_bar_close_limits() {
    use rust_decimal::prelude::ToPrimitive as _;
    let handle = boot().await;
    let client = reqwest::Client::new();
    // A strict grant denies every proposal, so the whole inbox stays proposed.
    let mut strict = test_grant("strict-inbox", 10);
    strict.signal.min_conviction = 0.95;
    assert!(client
        .post(format!("{}/grants/arm", base(&handle)))
        .json(&strict)
        .send()
        .await
        .expect("arm reachable")
        .status()
        .is_success());
    let preview = client
        .post(format!("{}/proposals/preview", base(&handle)))
        .json(&PreviewParams { seed: 3 })
        .send()
        .await
        .expect("preview reachable");
    assert!(preview.status().is_success());
    let inbox: Vec<ProposalView> = client
        .get(format!("{}/proposals", base(&handle)))
        .send()
        .await
        .expect("inbox reachable")
        .json()
        .await
        .expect("inbox is JSON");
    assert!(!inbox.is_empty());
    let bars = history();
    let mut saw_proposed = false;
    for row in &inbox {
        if row.state == ProposalState::Proposed {
            saw_proposed = true;
            let index: usize = row
                .id
                .rsplit('-')
                .next()
                .expect("proposal id ends with bar index")
                .parse()
                .expect("bar index is numeric");
            assert_ne!(row.limit_price, Money::from(0), "unfilled rows must not fabricate a zero limit");
            assert_eq!(row.limit_price, Money::new(bars[index].c), "unfilled limit must be the bar close");
            // Sanity: the close must be representable or the bar would have skipped.
            assert!(bars[index].c.to_f64().is_some());
        }
    }
    assert!(saw_proposed, "a fully denied preview must leave proposed rows");
    handle.shutdown().await;
}

#[tokio::test]
async fn portfolio_positions_are_stable_across_previews() {
    let handle = boot().await;
    let client = reqwest::Client::new();
    let mut snapshots = Vec::new();
    for _ in 0..2 {
        let preview = client
            .post(format!("{}/proposals/preview", base(&handle)))
            .json(&PreviewParams { seed: 3 })
            .send()
            .await
            .expect("preview reachable");
        assert!(preview.status().is_success());
        let portfolio: PortfolioView = client
            .get(format!("{}/portfolio", base(&handle)))
            .send()
            .await
            .expect("portfolio reachable")
            .json()
            .await
            .expect("portfolio is JSON");
        snapshots.push(serde_json::to_string(&portfolio.positions).expect("serializable"));
    }
    assert_eq!(snapshots[0], snapshots[1], "position order must be deterministic");
    handle.shutdown().await;
}

#[tokio::test]
async fn out_of_window_grant_preview_explains_empty_fills() {
    let handle = boot().await;
    let client = reqwest::Client::new();
    // Live at wall-clock time but missing the January-2026 history window:
    // every bar denies on the window and nothing fills.
    let mut grant = test_grant("missed-window", 10);
    grant.valid_from = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    grant.valid_until = Utc.with_ymd_and_hms(2027, 6, 1, 0, 0, 0).unwrap();
    assert!(client
        .post(format!("{}/grants/arm", base(&handle)))
        .json(&grant)
        .send()
        .await
        .expect("arm reachable")
        .status()
        .is_success());
    let response: PreviewResponse = client
        .post(format!("{}/proposals/preview", base(&handle)))
        .json(&PreviewParams { seed: 3 })
        .send()
        .await
        .expect("preview reachable")
        .json()
        .await
        .expect("preview is a response");
    assert_eq!(response.run.report.fills, 0);
    let top = response
        .deny_histogram
        .first()
        .expect("an empty preview must still explain itself");
    assert_eq!(top.reason, "grant window not live");
    assert!(top.bars > 0);
    for entry in &response.deny_histogram {
        assert!(entry.bars <= top.bars, "histogram must sort most frequent first");
    }
    handle.shutdown().await;
}

#[tokio::test]
async fn arming_accepts_then_refuses_loosening() {
    let handle = boot().await;
    let client = reqwest::Client::new();
    let armed: ArmResponse = client
        .post(format!("{}/grants/arm", base(&handle)))
        .json(&test_grant("tight", 10))
        .send()
        .await
        .expect("arm reachable")
        .json()
        .await
        .expect("arm responds");
    assert!(armed.armed);
    assert_eq!(armed.grant_id, "tight");
    assert_eq!(armed.worst_case.max_trades, 10);

    let loose = client
        .post(format!("{}/grants/arm", base(&handle)))
        .json(&test_grant("loose", 20))
        .send()
        .await
        .expect("re-arm reachable");
    assert_eq!(loose.status(), reqwest::StatusCode::CONFLICT);

    let current: Option<Grant> = client
        .get(format!("{}/grants/current", base(&handle)))
        .send()
        .await
        .expect("current reachable")
        .json()
        .await
        .expect("current is JSON");
    assert_eq!(current.expect("grant stays armed").id, "tight");
    handle.shutdown().await;
}

#[tokio::test]
async fn arming_rejects_empty_window() {
    let handle = boot().await;
    let mut grant = test_grant("empty-window", 10);
    grant.valid_until = grant.valid_from;
    let response = reqwest::Client::new()
        .post(format!("{}/grants/arm", base(&handle)))
        .json(&grant)
        .send()
        .await
        .expect("arm reachable");
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    handle.shutdown().await;
}

fn mid_grant(id: &str) -> Grant {
    // Interior values on every tier so each tier can move either way.
    Grant::new()
        .id(id.to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical])
                .directions(vec![Direction::Long])
                .permissions(GrantPermission { entries: true, exits: true, rolls: false })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(10)
                .max_contracts(100)
                .max_loss(Money::from(1_000))
                .max_notional(Money::from(10_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.5)
                .min_activity_score(0.5)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(Money::from(500))
                .max_spread_width(Money::from(25))
                .min_dte_days(7)
                .max_dte_days(60)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(Money::from(1_000_000))
                .quote_max_age_secs(30)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(2).build())
        .valid_from(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

#[tokio::test]
async fn arming_refuses_every_loosened_tier() {
    let handle = boot().await;
    let client = reqwest::Client::new();
    let armed: ArmResponse = client
        .post(format!("{}/grants/arm", base(&handle)))
        .json(&mid_grant("mid"))
        .send()
        .await
        .expect("arm reachable")
        .json()
        .await
        .expect("arm responds");
    assert!(armed.armed);

    let mut loosened: Vec<(String, Grant)> = Vec::new();
    let mut widen = |label: &str, f: fn(&mut Grant)| {
        let mut grant = mid_grant("wider");
        f(&mut grant);
        loosened.push((label.to_owned(), grant));
    };
    widen("budgets", |g| g.budgets.max_trades = 11);
    widen("signal-conviction", |g| g.signal.min_conviction = 0.4);
    widen("signal-activity", |g| g.signal.min_activity_score = 0.4);
    widen("trade-loss", |g| g.trade.max_loss_per_trade = Money::from(501));
    widen("trade-spread", |g| g.trade.max_spread_width = Money::from(26));
    widen("trade-min-dte", |g| g.trade.min_dte_days = 6);
    widen("trade-max-dte", |g| g.trade.max_dte_days = 61);
    widen("portfolio-risk", |g| g.portfolio.max_open_risk = Money::from(1_000_001));
    widen("portfolio-age", |g| g.portfolio.quote_max_age_secs = 31);
    widen("breakers", |g| g.breakers.max_consecutive_errors = 3);
    widen("scope", |g| g.scope.directions.push(Direction::Short));
    widen("window-end", |g| {
        g.valid_until += chrono::Duration::days(1);
    });
    widen("window-start", |g| {
        g.valid_from -= chrono::Duration::days(1);
    });
    assert_eq!(loosened.len(), 13);
    for (label, grant) in &loosened {
        let response = client
            .post(format!("{}/grants/arm", base(&handle)))
            .json(grant)
            .send()
            .await
            .expect("re-arm reachable");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::CONFLICT,
            "loosened {label} tier must be refused"
        );
    }
    let current: Option<Grant> = client
        .get(format!("{}/grants/current", base(&handle)))
        .send()
        .await
        .expect("current reachable")
        .json()
        .await
        .expect("current is JSON");
    assert_eq!(current.expect("refusals keep the armed grant").id, "mid");

    let mut tighter = mid_grant("tighter");
    tighter.budgets.max_trades = 9;
    tighter.signal.min_conviction = 0.6;
    tighter.valid_until -= chrono::Duration::days(1);
    let accepted = client
        .post(format!("{}/grants/arm", base(&handle)))
        .json(&tighter)
        .send()
        .await
        .expect("tighten reachable");
    assert!(accepted.status().is_success(), "every-tier-tighter must arm");
    handle.shutdown().await;
}

#[tokio::test]
async fn chart_contract_matches_kernel_output() {
    let handle = boot().await;
    let client = reqwest::Client::new();
    let bars: ChartBars = client
        .get(format!("{}/chart/bars", base(&handle)))
        .send()
        .await
        .expect("bars reachable")
        .json()
        .await
        .expect("bars are JSON");
    assert_eq!(bars.rows.len(), 60);
    assert_eq!(bars.dropped, 0, "vendored history must be fully chartable");
    assert_eq!(bars.rows[0].time, history()[0].ts.timestamp());

    let overlay: OverlaySeries = client
        .get(format!("{}/chart/overlays?kind=sma&period=14", base(&handle)))
        .send()
        .await
        .expect("overlay reachable")
        .json()
        .await
        .expect("overlay is JSON");
    assert_eq!(overlay.values.len(), 60);
    let closes: Vec<f64> = history()
        .iter()
        .filter_map(|bar| {
            use rust_decimal::prelude::ToPrimitive as _;
            bar.c.to_f64()
        })
        .collect();
    let expected = SmaIndicator::new()
        .batch(
            &Ohlcv::close_only(&closes),
            &ParamSet::new().with_int("length", 14),
        )
        .expect("kernel runs")
        .values;
    assert_eq!(overlay.values, expected);

    assert_eq!(overlay.dropped, 0, "clean history drops nothing from the overlay");
    assert_eq!(bars.dropped, 0);
    assert_eq!(
        overlay.values.len(),
        bars.rows.len(),
        "rows and overlays agree positionally on clean history"
    );

    let bad_kind = client
        .get(format!("{}/chart/overlays?kind=wma&period=14", base(&handle)))
        .send()
        .await
        .expect("bad kind reachable");
    assert_eq!(bad_kind.status(), reqwest::StatusCode::BAD_REQUEST);
    let bad_period = client
        .get(format!("{}/chart/overlays?kind=sma&period=0", base(&handle)))
        .send()
        .await
        .expect("bad period reachable");
    assert_eq!(bad_period.status(), reqwest::StatusCode::BAD_REQUEST);
    handle.shutdown().await;
}

#[test]
fn overlay_scatter_holds_bar_alignment() {
    // A bar whose close has no chart float keeps its index with `None`:
    // values are never dropped, so later bars never shift.
    let computed = vec![Some(1.0), Some(2.0), None, Some(4.0)];
    let values = align_overlay(5, &[0, 1, 3, 4], &computed);
    assert_eq!(values.len(), 5);
    assert_eq!(values, vec![Some(1.0), Some(2.0), None, None, Some(4.0)]);
    assert_eq!(values[2], None, "the unrepresentable bar holds its index");
    assert_eq!(values[3], None, "kernel warmup Nones survive the scatter");
    assert_eq!(values[0], Some(1.0));
    assert_eq!(values[4], Some(4.0), "compacted slots map back to their bars");
    assert_eq!(align_overlay(0, &[], &[]).len(), 0);
}

#[tokio::test]
async fn feed_streams_bar_frames() {
    use futures_util::StreamExt as _;
    let handle = boot().await;
    let url = format!("ws://{}/feed", handle.addr);
    let (mut socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("feed connects");
    let mut frames = 0;
    let deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(deadline);
    while frames < 2 {
        tokio::select! {
            message = socket.next() => {
                let message = message.expect("frame arrives").expect("socket open");
                let text = message.into_text().expect("text frame");
                let frame: serde_json::Value = serde_json::from_str(&text).expect("frame is JSON");
                assert_eq!(frame["type"], "bar");
                assert!(frame.get("close").is_some());
                assert_eq!(frame["cycle"], 0, "first pass over history is cycle 0");
                frames += 1;
            }
            () = &mut deadline => break,
        }
    }
    assert_eq!(frames, 2, "feed must stream bar frames promptly");
    handle.shutdown().await;
}
