//! Route-payload goldens: every read route plus the arm submit, byte-exact.
//!
//! The files under `tests/data/views/` were generated BEFORE the
//! `protopyne-view` swap with the old local view structs; the swap must
//! reproduce them byte-for-byte (enum string tags and key order
//! preserved). Regenerate only by running with `OVERWRITE_GOLDENS=1` on
//! reviewed code — the overwrite path writes and passes, the default path
//! compares and fails loudly on any drift.

use super::*;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};

fn history() -> Vec<Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
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

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join("views").join(name)
}

/// Byte-exact compare against the checked-in golden; with
/// `OVERWRITE_GOLDENS=1`, rewrite the file instead (reviewed code only).
fn check_golden(name: &str, body: &str) {
    let path = golden_path(name);
    if std::env::var("OVERWRITE_GOLDENS").is_ok() {
        std::fs::create_dir_all(path.parent().expect("golden dir")).expect("golden dir creates");
        std::fs::write(&path, body).expect("golden writes");
        return;
    }
    let expected = std::fs::read_to_string(&path).expect("golden checked in");
    assert_eq!(body, expected, "{name} route bytes drifted from the golden");
}

async fn get(client: &reqwest::Client, base: &str, route: &str) -> String {
    client
        .get(format!("{base}{route}"))
        .send()
        .await
        .expect("route reachable")
        .text()
        .await
        .expect("body reads")
}

#[tokio::test]
async fn route_payloads_match_checked_in_goldens() {
    let handle = boot().await;
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();

    check_golden("health.json", &get(&client, &base, "/health").await);
    check_golden("portfolio_fresh.json", &get(&client, &base, "/portfolio").await);

    let preview = client
        .post(format!("{base}/proposals/preview"))
        .json(&serde_json::json!({"seed": 11}))
        .send()
        .await
        .expect("preview reachable")
        .text()
        .await
        .expect("preview reads");
    check_golden("preview.json", &preview);
    check_golden("proposals.json", &get(&client, &base, "/proposals").await);
    check_golden("portfolio.json", &get(&client, &base, "/portfolio").await);
    check_golden("chart_bars.json", &get(&client, &base, "/chart/bars").await);
    check_golden(
        "overlay_sma_14.json",
        &get(&client, &base, "/chart/overlays?kind=sma&period=14").await,
    );
    check_golden("replay.json", &get(&client, &base, "/replay").await);

    let arm = client
        .post(format!("{base}/grants/arm"))
        .json(&test_grant("golden-grant", 5))
        .send()
        .await
        .expect("arm reachable")
        .text()
        .await
        .expect("arm reads");
    check_golden("arm.json", &arm);

    handle.shutdown().await;
}
