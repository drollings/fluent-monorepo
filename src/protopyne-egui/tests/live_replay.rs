//! Interactive replay controls: start resolution, prefix truncation, and the
//! gateway-driven open/step/stop wiring.
//!
//! Every assertion reads headless state (no paint): the truncation helpers
//! the canvas reads, the recompute the steps drive, and the cached session
//! the dock panes render.

use super::*;

use std::sync::Mutex;

use protopyne_chart_model::ChartRow;
use protopyne_view::{
    GatewayError, ReplayDecisionView, ReplayPositionView, ReplayStartParams, ReplayStateView,
};

fn ladder_rows(count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|index| {
            let close = 100.0 + index as f64;
            ChartRow {
                time: 1_700_000_000 + index as i64 * 900,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 1_000,
            }
        })
        .collect()
}

fn idle_state(cursor: u64, total: u64) -> ReplayStateView {
    ReplayStateView {
        session_id: "terminal".to_owned(),
        active: true,
        cursor,
        total_bars: total,
        decisions: Vec::new(),
        positions: Vec::new(),
        filled_orders: 0,
        total_fees: Money::from(0),
        journal: Vec::new(),
    }
}

fn seat_live(workspace: &mut ChartWorkspace, cursor: usize) {
    let total = workspace.rows.len() as u64;
    workspace.live_replay = Some(LiveReplay::new(
        "terminal".to_owned(),
        idle_state(cursor as u64, total),
    ));
}

/// A script replay gateway: opens at the requested start bar over a fixed
/// window, steps the cursor, and records every verb.
struct ScriptReplay {
    total: u64,
    cursor: Mutex<u64>,
    calls: Mutex<Vec<String>>,
    structures: Mutex<Vec<String>>,
}

impl ScriptReplay {
    fn new(total: u64) -> Self {
        Self {
            total,
            cursor: Mutex::new(0),
            calls: Mutex::new(Vec::new()),
            structures: Mutex::new(Vec::new()),
        }
    }

    fn state_of(&self, session: &str) -> ReplayStateView {
        idle_state(*self.cursor.lock().expect("cursor locks"), self.total).with_session(session)
    }
}

trait WithSession {
    fn with_session(self, session: &str) -> Self;
}

impl WithSession for ReplayStateView {
    fn with_session(mut self, session: &str) -> Self {
        self.session_id = session.to_owned();
        self
    }
}

impl TerminalGateway for ScriptReplay {
    fn health(&self) -> Result<protopyne_view::HealthView, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn proposals(&self) -> Result<Vec<protopyne_view::ProposalView>, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn portfolio(&self) -> Result<protopyne_view::PortfolioView, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn journal(&self) -> Result<Vec<protopyne_view::JournalRowView>, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn chart_bars(&self) -> Result<protopyne_view::ChartBars, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn chart_overlays(
        &self,
        _kind: &str,
        _period: i64,
    ) -> Result<protopyne_view::OverlaySeries, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn replay(&self) -> Result<protopyne_view::ReplayView, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn preview_arm(
        &self,
        _patch: serde_json::Value,
    ) -> Result<protopyne_view::WorstCase, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn grant_tiers(&self) -> Result<std::collections::BTreeMap<String, String>, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn arm(&self, _grant: serde_json::Value) -> Result<protopyne_view::ArmResponse, GatewayError> {
        Err(GatewayError::Unavailable("script replay".to_owned()))
    }

    fn replay_start(&self, params: ReplayStartParams) -> Result<ReplayStateView, GatewayError> {
        self.calls
            .lock()
            .expect("call log locks")
            .push(format!("start:{}", params.start_bar));
        self.structures
            .lock()
            .expect("structure log locks")
            .push(params.structure.clone());
        if params.start_bar > self.total {
            return Err(GatewayError::Malformed("start past the end".to_owned()));
        }
        *self.cursor.lock().expect("cursor locks") = params.start_bar;
        Ok(self.state_of(&params.session_id))
    }

    fn replay_step(&self, session_id: &str, bars: u64) -> Result<ReplayStateView, GatewayError> {
        self.calls
            .lock()
            .expect("call log locks")
            .push(format!("step:{bars}"));
        let mut cursor = self.cursor.lock().expect("cursor locks");
        *cursor = (*cursor + bars).min(self.total);
        drop(cursor);
        Ok(self.state_of(session_id))
    }

    fn replay_state(&self, session_id: &str) -> Result<ReplayStateView, GatewayError> {
        Ok(self.state_of(session_id))
    }

    fn replay_stop(&self, session_id: &str) -> Result<(), GatewayError> {
        self.calls
            .lock()
            .expect("call log locks")
            .push(format!("stop:{session_id}"));
        Ok(())
    }
}

#[test]
fn replay_start_resolves_index_date_and_refusals() {
    let workspace = ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    assert_eq!(
        workspace
            .resolve_replay_start("")
            .expect("empty is the head"),
        0
    );
    assert_eq!(
        workspace
            .resolve_replay_start("7")
            .expect("an index resolves"),
        7
    );
    assert_eq!(
        workspace
            .resolve_replay_start("40")
            .expect("the end resolves"),
        40
    );
    assert!(
        workspace.resolve_replay_start("41").is_err(),
        "an index past the last bar refuses"
    );
    assert!(
        workspace.resolve_replay_start("someday").is_err(),
        "garbage refuses with the accepted shapes named"
    );
    // Row 0 sits at 2023-11-14T22:13:20Z; a date before it clamps to the head.
    assert_eq!(
        workspace
            .resolve_replay_start("2023-01-01")
            .expect("an early date clamps"),
        0
    );
    assert!(
        workspace.resolve_replay_start("2030-01-01").is_err(),
        "a date past the last bar refuses"
    );
}

#[test]
fn replay_prefix_truncates_the_view_never_the_store() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    assert_eq!(
        workspace.replay_prefix_len(),
        None,
        "no run is the live chart"
    );
    assert_eq!(workspace.replay_view_rows(&workspace.rows).len(), 40);
    seat_live(&mut workspace, 12);
    assert_eq!(workspace.replay_prefix_len(), Some(12));
    let view = workspace.replay_view_rows(&workspace.rows);
    assert_eq!(view.len(), 12, "painters read the decided prefix only");
    assert_eq!(view.last().map(|row| row.close), Some(111.0));
    assert_eq!(workspace.rows.len(), 40, "the store keeps the full history");
    // A cursor past the store clamps instead of panicking: the gateway, not
    // the painter, owns cursor validity.
    seat_live(&mut workspace, 400);
    assert_eq!(workspace.replay_view_rows(&workspace.rows).len(), 40);
}

#[test]
fn recompute_over_a_prefix_holds_holes_past_the_cursor() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    seat_live(&mut workspace, 10);
    workspace.recompute().expect("a prefix recomputes");
    // Every plugin series keeps full slot length with holes past the cursor:
    // painters cull the same slot space, and a hole paints as a gap.
    for (id, evaluation) in &workspace.evaluations {
        for series in &evaluation.series {
            assert_eq!(
                series.values.len(),
                40,
                "{id} keeps full slot length under a live replay"
            );
            assert!(
                series.values[10..].iter().all(Option::is_none),
                "{id} carries no values past the cursor (no lookahead)"
            );
        }
    }
    // The sub-pane folds decided bars only: its legs end at the cursor.
    let pane = workspace
        .sub_pane
        .as_ref()
        .expect("the default pane builds");
    assert_eq!(pane.ribbon.upper.len(), 10, "ribbon legs end at the cursor");
    assert_eq!(pane.ribbon.lower.len(), 10);
    assert_eq!(pane.ribbon.bullish.len(), 10);
    assert_eq!(
        workspace.momentum_keys.len(),
        10,
        "tint keys end at the cursor"
    );
    // Stopping returns to the full evaluation without a re-pull.
    workspace.live_replay = None;
    workspace.recompute().expect("the live chart recomputes");
    let pane = workspace.sub_pane.as_ref().expect("the pane rebuilds");
    assert_eq!(
        pane.ribbon.upper.len(),
        40,
        "exiting restores the full fold"
    );
}

#[test]
fn open_step_and_stop_drive_the_cached_session() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let gateway = ScriptReplay::new(40);
    workspace.open_live_replay(&gateway, 5, 7, None, "Single".to_owned());
    let live = workspace.live_replay.as_ref().expect("open seats a run");
    assert_eq!(live.state.cursor, 5, "the run opens at the requested bar");
    assert_eq!(live.state.total_bars, 40);
    assert_eq!(workspace.replay_prefix_len(), Some(5));
    assert!(live.error.is_none());

    workspace.step_live_replay(&gateway, 3);
    let live = workspace.live_replay.as_ref().expect("the run stays live");
    assert_eq!(live.state.cursor, 8, "a step advances the cached cursor");

    workspace.stop_live_replay(&gateway);
    assert!(workspace.live_replay.is_none(), "stop clears the run");
    assert_eq!(
        workspace.replay_prefix_len(),
        None,
        "the chart is live again"
    );
    assert_eq!(workspace.rows.len(), 40, "no re-pull was needed");
    let calls = gateway.calls.lock().expect("call log locks").clone();
    assert_eq!(calls, vec!["start:5", "step:3", "stop:terminal"]);
}

#[test]
fn a_refused_step_stops_play_with_the_reason_kept() {
    struct RefusingStart(ScriptReplay);
    impl TerminalGateway for RefusingStart {
        fn health(&self) -> Result<protopyne_view::HealthView, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn proposals(&self) -> Result<Vec<protopyne_view::ProposalView>, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn portfolio(&self) -> Result<protopyne_view::PortfolioView, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn journal(&self) -> Result<Vec<protopyne_view::JournalRowView>, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn chart_bars(&self) -> Result<protopyne_view::ChartBars, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn chart_overlays(
            &self,
            _kind: &str,
            _period: i64,
        ) -> Result<protopyne_view::OverlaySeries, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn replay(&self) -> Result<protopyne_view::ReplayView, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn preview_arm(
            &self,
            _patch: serde_json::Value,
        ) -> Result<protopyne_view::WorstCase, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn grant_tiers(&self) -> Result<std::collections::BTreeMap<String, String>, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn arm(
            &self,
            _grant: serde_json::Value,
        ) -> Result<protopyne_view::ArmResponse, GatewayError> {
            Err(GatewayError::Unavailable("refusing".to_owned()))
        }

        fn replay_start(&self, params: ReplayStartParams) -> Result<ReplayStateView, GatewayError> {
            self.0.replay_start(params)
        }

        fn replay_step(
            &self,
            _session_id: &str,
            _bars: u64,
        ) -> Result<ReplayStateView, GatewayError> {
            Err(GatewayError::Unavailable("venue halted mid-run".to_owned()))
        }

        fn replay_state(&self, session_id: &str) -> Result<ReplayStateView, GatewayError> {
            self.0.replay_state(session_id)
        }

        fn replay_stop(&self, session_id: &str) -> Result<(), GatewayError> {
            self.0.replay_stop(session_id)
        }
    }

    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    let gateway = RefusingStart(ScriptReplay::new(40));
    workspace.open_live_replay(&gateway, 0, 7, None, "Single".to_owned());
    let live = workspace.live_replay.as_mut().expect("open seats a run");
    live.playing = true;
    workspace.step_live_replay(&gateway, 1);
    let live = workspace
        .live_replay
        .as_ref()
        .expect("the session stays live for retry");
    assert!(!live.playing, "a refused step stops the play loop");
    assert_eq!(live.state.cursor, 0, "a refused step decides nothing");
    assert!(
        live.error
            .as_deref()
            .unwrap_or_default()
            .contains("venue halted"),
        "the refusal reason stays on the section"
    );
}

#[test]
fn chain_gated_verdicts_stay_distinct_from_proposals() {
    // The dock renders direction-bearing verdicts as proposals and counts
    // the chain-gated remainder beside them; the flag (never reason text)
    // is what separates the two.
    let gated = ReplayDecisionView {
        bar_index: 2,
        proposal_id: "run-7-2".to_owned(),
        ts: "2023-11-14T22:13:20+00:00".to_owned(),
        direction: None,
        structure: None,
        state: "options spreads need an option-chain feed (none configured)".to_owned(),
        conviction: 0.0,
        submitted: false,
        filled_quantity: 0,
        fill_price: None,
        skipped: Some("options spreads need an option-chain feed (none configured)".to_owned()),
        chain_gated: true,
        legs: Vec::new(),
    };
    let filled = ReplayDecisionView {
        bar_index: 3,
        proposal_id: "run-7-3".to_owned(),
        ts: "2023-11-14T22:28:20+00:00".to_owned(),
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
    };
    let state = ReplayStateView {
        decisions: vec![gated, filled],
        positions: vec![ReplayPositionView {
            underlying: "SPY".to_owned(),
            quantity: 1,
            avg_price: Money::from(100),
        }],
        ..idle_state(4, 40)
    };
    let proposed: Vec<_> = state
        .decisions
        .iter()
        .filter(|decision| decision.direction.is_some())
        .collect();
    assert_eq!(proposed.len(), 1, "only proposal-bearing verdicts propose");
    assert_eq!(
        state
            .decisions
            .iter()
            .filter(|decision| decision.chain_gated)
            .count(),
        1,
        "the gate count rides the flag, never reason text"
    );
}

#[test]
fn replay_grant_draft_resolves_empty_json_and_refusals() {
    assert_eq!(
        ChartWorkspace::resolve_replay_grant("").expect("empty is the rehearsal grant"),
        None
    );
    assert_eq!(
        ChartWorkspace::resolve_replay_grant("   ").expect("whitespace is the rehearsal grant"),
        None
    );
    let document =
        ChartWorkspace::resolve_replay_grant(r#"{"grant_id": "desk-1"}"#).expect("JSON parses");
    assert_eq!(
        document,
        Some(serde_json::json!({"grant_id": "desk-1"})),
        "a document forwards to the core unparsed-beyond-JSON"
    );
    assert!(
        ChartWorkspace::resolve_replay_grant("not json {").is_err(),
        "garbage refuses with the reason on the section"
    );
}

#[test]
fn open_forwards_an_explicit_grant_document() {
    struct GrantRecording(ScriptReplay);
    impl TerminalGateway for GrantRecording {
        fn health(&self) -> Result<protopyne_view::HealthView, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn proposals(&self) -> Result<Vec<protopyne_view::ProposalView>, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn portfolio(&self) -> Result<protopyne_view::PortfolioView, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn journal(&self) -> Result<Vec<protopyne_view::JournalRowView>, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn chart_bars(&self) -> Result<protopyne_view::ChartBars, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn chart_overlays(
            &self,
            _kind: &str,
            _period: i64,
        ) -> Result<protopyne_view::OverlaySeries, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn replay(&self) -> Result<protopyne_view::ReplayView, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn preview_arm(
            &self,
            _patch: serde_json::Value,
        ) -> Result<protopyne_view::WorstCase, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn grant_tiers(&self) -> Result<std::collections::BTreeMap<String, String>, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn arm(
            &self,
            _grant: serde_json::Value,
        ) -> Result<protopyne_view::ArmResponse, GatewayError> {
            Err(GatewayError::Unavailable("recording".to_owned()))
        }
        fn replay_start(&self, params: ReplayStartParams) -> Result<ReplayStateView, GatewayError> {
            assert!(
                params.grant == Some(serde_json::json!({"grant_id": "desk-1"})),
                "the explicit document reaches the core untouched"
            );
            self.0.replay_start(params)
        }
        fn replay_step(
            &self,
            session_id: &str,
            bars: u64,
        ) -> Result<ReplayStateView, GatewayError> {
            self.0.replay_step(session_id, bars)
        }
        fn replay_state(&self, session_id: &str) -> Result<ReplayStateView, GatewayError> {
            self.0.replay_state(session_id)
        }
        fn replay_stop(&self, session_id: &str) -> Result<(), GatewayError> {
            self.0.replay_stop(session_id)
        }
    }

    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(20)).expect("seeded workspace builds");
    let gateway = GrantRecording(ScriptReplay::new(20));
    workspace.open_live_replay(
        &gateway,
        2,
        7,
        Some(serde_json::json!({"grant_id": "desk-1"})),
        "Single".to_owned(),
    );
    assert!(
        workspace.live_replay.is_some(),
        "open with a grant document seats a run"
    );
    assert!(
        workspace.replay_error.is_none(),
        "a well-formed document opens cleanly"
    );
}

#[test]
fn structure_picker_defaults_single_and_forwards_the_tag() {
    let mut workspace =
        ChartWorkspace::with_rows(ladder_rows(40)).expect("seeded workspace builds");
    assert_eq!(
        workspace.replay_structure_draft, "Single",
        "the picker opens on the legacy shape"
    );
    let gateway = ScriptReplay::new(40);
    workspace.open_live_replay(
        &gateway,
        5,
        7,
        None,
        workspace.replay_structure_draft.clone(),
    );
    assert_eq!(
        gateway
            .structures
            .lock()
            .expect("structure log locks")
            .as_slice(),
        ["Single"],
        "the default open forwards singles"
    );
    workspace.replay_structure_draft = "Vertical".to_owned();
    workspace.open_live_replay(
        &gateway,
        5,
        7,
        None,
        workspace.replay_structure_draft.clone(),
    );
    assert_eq!(
        gateway
            .structures
            .lock()
            .expect("structure log locks")
            .as_slice(),
        ["Single", "Vertical"],
        "the picker tag reaches the core as a plain string"
    );
}

#[test]
fn spread_decision_formats_one_row_per_leg_and_singles_format_none() {
    use protopyne_view::{ReplayDecisionView, ReplayLegView};
    let legged = ReplayDecisionView {
        bar_index: 3,
        proposal_id: "run-7-3".to_owned(),
        ts: "2026-01-09T22:00:00Z".to_owned(),
        direction: Some("Long".to_owned()),
        structure: Some("Vertical".to_owned()),
        state: "Filled".to_owned(),
        conviction: 0.8,
        submitted: true,
        filled_quantity: 2,
        fill_price: None,
        skipped: None,
        chain_gated: false,
        legs: vec![
            ReplayLegView {
                contract: "SPY   260717C00690000".to_owned(),
                side: "buy".to_owned(),
                quantity: 1,
                price: protopyne_types::Money::from(400),
                fee: protopyne_types::Money::from(65),
            },
            ReplayLegView {
                contract: "SPY   260717C00695000".to_owned(),
                side: "sell".to_owned(),
                quantity: 1,
                price: protopyne_types::Money::from(199),
                fee: protopyne_types::Money::from(65),
            },
        ],
    };
    assert_eq!(
        ChartWorkspace::replay_leg_lines(&legged),
        vec![
            "SPY   260717C00690000 buy 1 @ 400 (fee 65)".to_owned(),
            "SPY   260717C00695000 sell 1 @ 199 (fee 65)".to_owned(),
        ],
        "each executed leg reads as one display row"
    );
    let single = ReplayDecisionView {
        legs: Vec::new(),
        ..legged
    };
    assert!(
        ChartWorkspace::replay_leg_lines(&single).is_empty(),
        "a fill with no leg detail adds no rows"
    );
}
