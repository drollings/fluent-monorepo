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
    let run: BacktestRun = client
        .post(format!("{}/proposals/preview", base(&handle)))
        .json(&PreviewParams { seed: 3 })
        .send()
        .await
        .expect("preview reachable")
        .json()
        .await
        .expect("preview is a run");
    assert_eq!(run.decisions.len(), 60);
    assert!(run.report.fills > 0);

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

#[tokio::test]
async fn chart_contract_matches_kernel_output() {
    let handle = boot().await;
    let client = reqwest::Client::new();
    let bars: Vec<CandleRow> = client
        .get(format!("{}/chart/bars", base(&handle)))
        .send()
        .await
        .expect("bars reachable")
        .json()
        .await
        .expect("bars are JSON");
    assert_eq!(bars.len(), 60);
    assert_eq!(bars[0].time, history()[0].ts.timestamp());

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
                frames += 1;
            }
            () = &mut deadline => break,
        }
    }
    assert_eq!(frames, 2, "feed must stream bar frames promptly");
    handle.shutdown().await;
}
