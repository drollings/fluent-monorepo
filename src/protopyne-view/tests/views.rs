use super::*;
use protopyne_types::Money;

fn money(value: i64) -> Money {
    Money::from(value)
}

/// Serialize, deserialize, and demand the value back unchanged.
///
/// The wire goldens pin *serialization* only — nothing else in this crate
/// exercises the decode half, so a field that stops being decodable is
/// invisible to them. Every DTO goes through here.
fn round_trips<T>(value: &T) -> String
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("dto serializes");
    let back: T =
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("dto deserializes from {json}: {e}"));
    assert_eq!(&back, value, "round trip must be value-preserving: {json}");
    json
}

fn sample_proposal() -> ProposalView {
    ProposalView {
        id: "prop-1".to_owned(),
        strategy: "ribbon".to_owned(),
        underlying: "SPY".to_owned(),
        direction: "Long".to_owned(),
        structure: "Vertical".to_owned(),
        limit_price: money(400),
        conviction: 0.8,
        state: "Proposed".to_owned(),
    }
}

fn sample_position() -> PositionView {
    PositionView {
        underlying: "SPY".to_owned(),
        quantity: -2,
        avg_price: money(100),
    }
}

fn sample_portfolio() -> PortfolioView {
    PortfolioView {
        positions: vec![sample_position()],
        filled_orders: 3,
        total_fees: money(6),
        open_contracts: -2,
        trading_state: "ExitOnly".to_owned(),
        source: "rehearsal".to_owned(),
    }
}

fn sample_command() -> CommandStatus {
    CommandStatus {
        kind: "close_all".to_owned(),
        detail: "closed 4 legs".to_owned(),
        at_ms: 1_700_000_000_000,
    }
}

fn sample_health() -> HealthView {
    HealthView {
        allows_entries: false,
        allows_exits: true,
        last_command: Some(sample_command()),
        status: "ok".to_owned(),
        store_open_gaps: None,
        store_reachable: None,
        store_watermark_age_secs: None,
        ingest_open_gaps: None,
        ingest_watermark_age_secs: None,
        ingest_last_repair: None,
        tape_denied: None,
        tape_last_miss: None,
        tape_no_tape: None,
        tape_served: None,
        tape_store_error: None,
        tape_unknown_symbol: None,
        tape_unsupported_timeframe: None,
        trading_state: "ExitOnly".to_owned(),
        version: "0.2.0".to_owned(),
    }
}

fn sample_worst_case() -> WorstCase {
    WorstCase {
        max_trades: 10,
        max_contracts: 20,
        max_loss: money(2_000),
        max_notional: money(50_000),
        basis: "BudgetEcho".to_owned(),
    }
}

fn sample_arm() -> ArmResponse {
    ArmResponse {
        grant_id: "grant-7".to_owned(),
        worst_case: sample_worst_case(),
        armed: true,
    }
}

fn sample_preview() -> PreviewResponse {
    PreviewResponse {
        run: serde_json::json!({"manifest": {"data_version": "test"}, "decisions": []}),
        deny_histogram: vec![DenyCount {
            reason: "grant deny".to_owned(),
            bars: 4,
        }],
    }
}

fn sample_journal_row() -> JournalRowView {
    JournalRowView {
        seq: 12,
        client_id: "order-9".to_owned(),
        kind: "filled".to_owned(),
        detail: "{\"price\":\"100.50\"}".to_owned(),
        at_ms: 1_700_000_000_000,
    }
}

fn sample_bar() -> ChartBar {
    ChartBar {
        time: 1_700_000_000,
        open: 100.0,
        high: 101.0,
        low: 99.0,
        close: 100.5,
        volume: 10,
    }
}

fn sample_bars() -> ChartBars {
    ChartBars {
        rows: vec![sample_bar()],
        dropped: 1,
    }
}

fn sample_overlay() -> OverlaySeries {
    OverlaySeries {
        kind: "sma".to_owned(),
        period: 14,
        values: vec![None, Some(100.25)],
        dropped: 0,
    }
}

fn sample_step() -> ReplayStepView {
    ReplayStepView {
        bar_index: 41,
        ts: "2023-11-14T22:13:20+00:00".to_owned(),
        done: true,
        checkpoint: true,
    }
}

fn sample_replay() -> ReplayView {
    ReplayView {
        cursor: 2,
        total_bars: 2,
        steps: vec![
            sample_step(),
            ReplayStepView {
                bar_index: 42,
                ts: "2023-11-14T22:28:20+00:00".to_owned(),
                done: false,
                checkpoint: false,
            },
        ],
    }
}

fn sample_start_params() -> ReplayStartParams {
    ReplayStartParams {
        session_id: "terminal".to_owned(),
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

fn sample_decision() -> ReplayDecisionView {
    ReplayDecisionView {
        bar_index: 3,
        proposal_id: "run-7-3".to_owned(),
        ts: "2023-11-14T22:43:20+00:00".to_owned(),
        direction: Some("Long".to_owned()),
        structure: Some("Single".to_owned()),
        state: "Filled".to_owned(),
        conviction: 0.8,
        submitted: true,
        filled_quantity: 1,
        fill_price: Some(Money::from(100)),
        skipped: None,
        chain_gated: false,
        legs: Vec::new(),
    }
}

fn sample_replay_state() -> ReplayStateView {
    ReplayStateView {
        session_id: "terminal".to_owned(),
        active: true,
        cursor: 4,
        total_bars: 9,
        decisions: vec![sample_decision()],
        positions: vec![ReplayPositionView {
            underlying: "SPY".to_owned(),
            quantity: 1,
            avg_price: Money::from(100),
        }],
        filled_orders: 1,
        total_fees: Money::from(2),
        journal: vec![sample_journal_row()],
    }
}

#[test]
fn serialization_round_trips_through_deserialization() {
    // Every DTO the crate exports rides the same decode path a terminal
    // client would use; one omission here is a broken wire contract.
    round_trips(&sample_proposal());
    round_trips(&sample_position());
    round_trips(&sample_portfolio());
    round_trips(&sample_command());
    round_trips(&sample_health());
    round_trips(&sample_worst_case());
    round_trips(&sample_arm());
    round_trips(&DenyCount {
        reason: "risk".to_owned(),
        bars: 9,
    });
    round_trips(&sample_preview());
    round_trips(&sample_journal_row());
    round_trips(&sample_bar());
    round_trips(&sample_bars());
    round_trips(&sample_overlay());
    round_trips(&sample_step());
    round_trips(&sample_replay());
    round_trips(&sample_start_params());
    round_trips(&sample_decision());
    round_trips(&sample_replay_state());
    // An absent optional and an empty collection must survive too: both
    // are the shapes a fresh service actually publishes.
    round_trips(&HealthView {
        last_command: None,
        ..sample_health()
    });
    round_trips(&PortfolioView::default());
    round_trips(&ChartBars {
        rows: Vec::new(),
        dropped: 0,
    });
    round_trips(&OverlaySeries {
        values: Vec::new(),
        ..sample_overlay()
    });
    round_trips(&ReplayView {
        steps: Vec::new(),
        ..sample_replay()
    });
}

#[test]
fn decimals_and_negative_display_floats_survive_the_boundary() {
    // Exact money must not become a float on the way through the wire.
    let negative = sample_portfolio();
    let json = round_trips(&negative);
    assert!(
        json.contains("\"quantity\":-2"),
        "signed contracts stay signed: {json}"
    );
    let zero = ProposalView {
        conviction: 0.0,
        ..sample_proposal()
    };
    assert!(round_trips(&zero).contains("\"conviction\":0.0"));
}

#[test]
fn payload_missing_dropped_still_deserializes() {
    for body in [
        serde_json::json!({"rows": [sample_bar()]}),
        serde_json::json!({"rows": []}),
    ] {
        let back: ChartBars = serde_json::from_value(body.clone())
            .unwrap_or_else(|e| panic!("ChartBars decodes without `dropped`: {body} / {e}"));
        assert_eq!(back.dropped, 0);
    }
    for body in [
        serde_json::json!({"kind": "sma", "period": 14, "values": [null, 100.25]}),
        serde_json::json!({"kind": "bb", "period": 20, "values": []}),
    ] {
        let back: OverlaySeries = serde_json::from_value(body.clone())
            .unwrap_or_else(|e| panic!("OverlaySeries decodes without `dropped`: {body} / {e}"));
        assert_eq!(back.dropped, 0);
    }
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
    assert!(
        json.contains("\"direction\":\"Long\""),
        "direction tag stable: {json}"
    );
    assert!(
        json.contains("\"structure\":\"Vertical\""),
        "structure tag stable: {json}"
    );
    assert!(
        json.contains("\"state\":\"Proposed\""),
        "state tag stable: {json}"
    );
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
        source: "booked".to_owned(),
    };
    let json = serde_json::to_string(&view).expect("portfolio serializes");
    assert!(
        json.contains("\"trading_state\":\"Enabled\""),
        "trading tag stable: {json}"
    );
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
        store_open_gaps: None,
        store_reachable: None,
        store_watermark_age_secs: None,
        ingest_open_gaps: None,
        ingest_watermark_age_secs: None,
        ingest_last_repair: None,
        tape_denied: None,
        tape_last_miss: None,
        tape_no_tape: None,
        tape_served: None,
        tape_store_error: None,
        tape_unknown_symbol: None,
        tape_unsupported_timeframe: None,
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
        store_open_gaps: None,
        store_reachable: None,
        store_watermark_age_secs: None,
        ingest_open_gaps: None,
        ingest_watermark_age_secs: None,
        ingest_last_repair: None,
        tape_denied: None,
        tape_last_miss: None,
        tape_no_tape: None,
        tape_served: None,
        tape_store_error: None,
        tape_unknown_symbol: None,
        tape_unsupported_timeframe: None,
        last_command: None,
    };
    let json = serde_json::to_string(&view).expect("health serializes");
    // No store reading on record: the payload is byte-identical to the
    // form without the store fields (the server golden pins these bytes).
    assert_eq!(
        json,
        r#"{"allows_entries":true,"allows_exits":true,"last_command":null,"status":"ok","trading_state":"Enabled","version":"0.2.0"}"#
    );
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

    // A recorded reading surfaces all three facts, still alphabetical.
    let observed = HealthView {
        store_open_gaps: Some(2),
        store_reachable: Some(true),
        store_watermark_age_secs: Some(90),
        ..view
    };
    let observed_json = serde_json::to_string(&observed).expect("health serializes");
    let observed_positions = [
        "\"allows_entries\"",
        "\"allows_exits\"",
        "\"last_command\"",
        "\"status\"",
        "\"store_open_gaps\"",
        "\"store_reachable\"",
        "\"store_watermark_age_secs\"",
        "\"trading_state\"",
        "\"version\"",
    ]
    .map(|key| observed_json.find(key).expect("health carries every key"));
    assert!(
        observed_positions.windows(2).all(|pair| pair[0] < pair[1]),
        "health keys serialize alphabetically: {observed_json}"
    );

    // Observed tape reads surface all seven facts, still alphabetical.
    let taped = HealthView {
        tape_denied: Some(0),
        tape_last_miss: Some("Denied".to_owned()),
        tape_no_tape: Some(0),
        tape_served: Some(3),
        tape_store_error: Some(0),
        tape_unknown_symbol: Some(1),
        tape_unsupported_timeframe: Some(0),
        ..observed
    };
    let taped_json = serde_json::to_string(&taped).expect("health serializes");
    let taped_positions = [
        "\"store_watermark_age_secs\"",
        "\"tape_denied\"",
        "\"tape_last_miss\"",
        "\"tape_no_tape\"",
        "\"tape_served\"",
        "\"tape_store_error\"",
        "\"tape_unknown_symbol\"",
        "\"tape_unsupported_timeframe\"",
        "\"trading_state\"",
    ]
    .map(|key| taped_json.find(key).expect("health carries every key"));
    assert!(
        taped_positions.windows(2).all(|pair| pair[0] < pair[1]),
        "health keys serialize alphabetically: {taped_json}"
    );
    let back: HealthView = serde_json::from_str(&taped_json).expect("health deserializes");
    assert_eq!(back, taped);
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
            basis: "BudgetEcho".to_owned(),
        },
        armed: true,
    };
    let json = serde_json::to_string(&response).expect("arm response serializes");
    let back: ArmResponse = serde_json::from_str(&json).expect("arm response deserializes");
    assert_eq!(back, response);
}

#[test]
fn a_worst_case_without_a_basis_reads_as_unknown_provenance() {
    // The field is additive: an older arm payload still loads, and an absent
    // basis never reads as a computed exposure.
    let older = r#"{"max_trades":1,"max_contracts":2,"max_loss":"100","max_notional":"1000"}"#;
    let worst: WorstCase = serde_json::from_str(older).expect("older payload reads");
    assert_eq!(worst.basis, "");
}

#[test]
fn preview_response_round_trips_with_opaque_run() {
    let response = PreviewResponse {
        run: serde_json::json!({"manifest": {"data_version": "test"}, "decisions": []}),
        deny_histogram: vec![DenyCount {
            reason: "grant deny".to_owned(),
            bars: 4,
        }],
    };
    let json = serde_json::to_string(&response).expect("preview serializes");
    let back: PreviewResponse = serde_json::from_str(&json).expect("preview deserializes");
    assert_eq!(back, response);
    assert_eq!(
        back.run["decisions"],
        serde_json::json!([]),
        "opaque run survives byte-stable"
    );
}

#[test]
fn replay_view_round_trips_with_its_cursor_and_anchors() {
    // The scrub decision rides these three fields: the cursor is the
    // task-value bar index, `done` says how far the run got, and
    // `checkpoint` marks the positions an operator scrubs to. A silent
    // default on any of them would move the cursor without the core
    // saying so, so they are decoded explicitly here.
    let view = sample_replay();
    let json = round_trips(&view);
    let back: ReplayView = serde_json::from_str(&json).expect("replay view deserializes");
    assert_eq!(back.cursor, 2);
    assert_eq!(back.total_bars, 2);
    assert_eq!(back.steps[0].bar_index, 41);
    assert!(
        back.steps[0].checkpoint,
        "a recorded anchor survives the wire"
    );
    assert!(back.steps[0].done);
    assert!(
        !back.steps[1].checkpoint && !back.steps[1].done,
        "an unreached step reads as unreached, never as reached"
    );
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

#[test]
fn replay_structure_round_trips_and_old_payloads_default_to_single() {
    let vertical = ReplayStartParams {
        structure: "Vertical".to_owned(),
        ..sample_start_params()
    };
    round_trips(&vertical);
    // A payload written before the picker names no structure: it decodes,
    // and replays exactly as before (singles).
    let json = serde_json::to_value(sample_start_params()).expect("sample encodes");
    let mut old = json.as_object().expect("object").clone();
    old.remove("structure");
    let back: ReplayStartParams =
        serde_json::from_value(serde_json::Value::Object(old)).expect("old payload decodes");
    assert_eq!(
        back.structure, "Single",
        "absence backfills the legacy shape"
    );
    assert_eq!(back, sample_start_params());
}

#[test]
fn replay_legs_round_trip_and_old_decisions_default_to_no_legs() {
    let leg = ReplayLegView {
        contract: "SPY   260717C00690000".to_owned(),
        side: "buy".to_owned(),
        quantity: 1,
        price: Money::from(400),
        fee: Money::from(65),
    };
    round_trips(&leg);
    let mut decision = sample_decision();
    decision.legs = vec![leg];
    round_trips(&decision);
    // A verdict written before leg rows names none: it decodes, and reads
    // as a fill with no per-leg detail rather than refusing.
    let json = serde_json::to_value(&decision).expect("decision encodes");
    let mut old = json.as_object().expect("object").clone();
    old.remove("legs");
    let back: ReplayDecisionView =
        serde_json::from_value(serde_json::Value::Object(old)).expect("old verdict decodes");
    assert!(back.legs.is_empty(), "absence backfills to no leg rows");
}

fn sample_backtest_run() -> BacktestRunView {
    BacktestRunView {
        run_id: "backtest-01".to_owned(),
        manifest_json: "{\"bars\":116}".to_owned(),
        metrics: vec![
            BacktestMetricView {
                label: "fills".to_owned(),
                value: Some("3".to_owned()),
            },
            BacktestMetricView {
                label: "sharpe".to_owned(),
                value: None,
            },
        ],
        equity: vec![EquityPointView {
            at: "2026-01-05T15:00:00+00:00".to_owned(),
            equity: "100000".to_owned(),
        }],
        trades: vec![BacktestTradeView {
            long: true,
            quantity: 10,
            entry_at: "2026-01-05T15:00:00+00:00".to_owned(),
            entry_price: "500.00".to_owned(),
            exit_at: "2026-01-05T15:05:00+00:00".to_owned(),
            exit_price: "505.00".to_owned(),
            pnl: "50.00".to_owned(),
            bars_held: 6,
            exit_reason: "take-profit".to_owned(),
        }],
        markers: vec![BacktestMarkerView {
            at: "2026-01-05T15:00:00+00:00".to_owned(),
            price: "500.00".to_owned(),
            kind: "entry".to_owned(),
        }],
        status: Some(BacktestStatusView {
            at: "2026-01-05T15:05:00+00:00".to_owned(),
            position: "Flat".to_owned(),
            day_pnl_pct: "0.05%".to_owned(),
            trades_today: "1".to_owned(),
            loss_streak: 0,
            cooldown: "Ready".to_owned(),
            status: "Active".to_owned(),
            win_rate: "100.0%".to_owned(),
            net_pnl: "50.00".to_owned(),
        }),
        warnings: vec!["single symbol".to_owned()],
    }
}

/// The run view carries display vocabulary only: no strategy, venue, or
/// server type crosses the boundary, and every money value rides decimal
/// text. The round trip pins the shape; the string assertions pin the
/// exactness contract a float could silently break.
#[test]
fn the_run_view_carries_no_trading_types_and_money_as_text() {
    let json = round_trips(&sample_backtest_run());
    let value: serde_json::Value = serde_json::from_str(&json).expect("view is JSON");
    assert_eq!(value["run_id"], "backtest-01");
    // Money rides text, never a JSON number.
    assert_eq!(value["trades"][0]["pnl"], "50.00");
    assert!(value["trades"][0]["pnl"].is_string());
    assert_eq!(value["equity"][0]["equity"], "100000");
    assert!(value["equity"][0]["equity"].is_string());
    assert!(value["status"]["net_pnl"].is_string());
    // An unmeasurable metric reads null, never an invented zero.
    assert!(value["metrics"][1]["value"].is_null());
}
