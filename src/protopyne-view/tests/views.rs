use super::*;
use protopyne_types::Money;

fn money(value: i64) -> Money {
    Money::from(value)
}

#[test]
fn proposal_view_round_trips_with_stable_tags() {
    let view = ProposalView {
        id: "prop-1".to_owned(),
        strategy: "ribbon".to_owned(),
        underlying: "SPY".to_owned(),
        direction: "Long".to_owned(),
        structure: "Vertical".to_owned(),
        limit_price: money(400),
        conviction: 0.8,
        state: "Proposed".to_owned(),
    };
    let json = serde_json::to_string(&view).expect("proposal serializes");
    assert!(json.contains("\"direction\":\"Long\""), "direction tag stable: {json}");
    assert!(json.contains("\"structure\":\"Vertical\""), "structure tag stable: {json}");
    assert!(json.contains("\"state\":\"Proposed\""), "state tag stable: {json}");
    let back: ProposalView = serde_json::from_str(&json).expect("proposal deserializes");
    assert_eq!(back, view);
}

#[test]
fn portfolio_view_round_trips_with_positions() {
    let view = PortfolioView {
        positions: vec![PositionView {
            underlying: "SPY".to_owned(),
            quantity: 2,
            avg_price: money(100),
        }],
        filled_orders: 3,
        total_fees: money(6),
        open_contracts: 2,
        trading_state: "Enabled".to_owned(),
    };
    let json = serde_json::to_string(&view).expect("portfolio serializes");
    assert!(json.contains("\"trading_state\":\"Enabled\""), "trading tag stable: {json}");
    let back: PortfolioView = serde_json::from_str(&json).expect("portfolio deserializes");
    assert_eq!(back, view);
    let empty = PortfolioView::default();
    assert!(empty.positions.is_empty() && empty.trading_state == "Enabled");
}

#[test]
fn health_view_round_trips_with_command_echo() {
    let view = HealthView {
        status: "ok".to_owned(),
        version: "0.2.0".to_owned(),
        trading_state: "ExitOnly".to_owned(),
        allows_entries: false,
        allows_exits: true,
        last_command: Some(CommandStatus {
            kind: "halt".to_owned(),
            detail: "operator halt".to_owned(),
            at_ms: 1_700_000_000_000,
        }),
    };
    let json = serde_json::to_string(&view).expect("health serializes");
    let back: HealthView = serde_json::from_str(&json).expect("health deserializes");
    assert_eq!(back, view);
    assert_eq!(back.last_command.expect("command echoes").kind, "halt");
}

#[test]
fn health_view_wire_order_is_alphabetical() {
    // The health route historically serialized a `serde_json::Map`
    // (BTreeMap: sorted keys). The DTO keeps that byte order so the
    // server-side swap is invisible on the wire — pinned here, proved by
    // the server golden.
    let view = HealthView {
        status: "ok".to_owned(),
        version: "0.2.0".to_owned(),
        trading_state: "Enabled".to_owned(),
        allows_entries: true,
        allows_exits: true,
        last_command: None,
    };
    let json = serde_json::to_string(&view).expect("health serializes");
    let positions = [
        "\"allows_entries\"",
        "\"allows_exits\"",
        "\"last_command\"",
        "\"status\"",
        "\"trading_state\"",
        "\"version\"",
    ]
    .map(|key| json.find(key).expect("health carries every key"));
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "health keys serialize alphabetically: {json}"
    );
}

#[test]
fn arm_response_round_trips_with_worst_case() {
    let response = ArmResponse {
        grant_id: "grant-7".to_owned(),
        worst_case: WorstCase {
            max_trades: 10,
            max_contracts: 20,
            max_loss: money(2_000),
            max_notional: money(50_000),
        },
        armed: true,
    };
    let json = serde_json::to_string(&response).expect("arm response serializes");
    let back: ArmResponse = serde_json::from_str(&json).expect("arm response deserializes");
    assert_eq!(back, response);
}

#[test]
fn preview_response_round_trips_with_opaque_run() {
    let response = PreviewResponse {
        run: serde_json::json!({"manifest": {"data_version": "test"}, "decisions": []}),
        deny_histogram: vec![DenyCount { reason: "grant deny".to_owned(), bars: 4 }],
    };
    let json = serde_json::to_string(&response).expect("preview serializes");
    let back: PreviewResponse = serde_json::from_str(&json).expect("preview deserializes");
    assert_eq!(back, response);
    assert_eq!(back.run["decisions"], serde_json::json!([]), "opaque run survives byte-stable");
}

#[test]
fn journal_rows_and_chart_payloads_round_trip() {
    let row = JournalRowView {
        seq: 12,
        client_id: "order-9".to_owned(),
        kind: "filled".to_owned(),
        detail: "{\"price\":\"100.50\"}".to_owned(),
        at_ms: 1_700_000_000_000,
    };
    let json = serde_json::to_string(&row).expect("journal row serializes");
    let back: JournalRowView = serde_json::from_str(&json).expect("journal row deserializes");
    assert_eq!(back, row);

    let bars = ChartBars {
        rows: vec![ChartBar {
            time: 1_700_000_000,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 10,
        }],
        dropped: 1,
    };
    let json = serde_json::to_string(&bars).expect("bars serialize");
    let back: ChartBars = serde_json::from_str(&json).expect("bars deserialize");
    assert_eq!(back, bars);

    let overlay = OverlaySeries {
        kind: "sma".to_owned(),
        period: 14,
        values: vec![None, Some(100.25)],
        dropped: 0,
    };
    let json = serde_json::to_string(&overlay).expect("overlay serializes");
    let back: OverlaySeries = serde_json::from_str(&json).expect("overlay deserializes");
    assert_eq!(back, overlay);
}
