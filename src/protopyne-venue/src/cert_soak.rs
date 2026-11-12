//! Cert-sandbox soak harness: one deterministic fault schedule, paper or live.
//!
//! The schedule below is the whole storm: a reconnect burst that must trip
//! the stream breaker on its third consecutive error, a journal-first submit,
//! three injected venue faults (two refusals plus one lost acknowledgement
//! that resolves by query-by-ID), and a reconcile drift that must halt
//! entries, page exactly once, then clear on the next clean pass.
//!
//! The hermetic suite drives [`run_paper_soak`] (no credentials, no network)
//! and asserts every number exactly; the credential-gated live run drives the
//! same schedule shape against the certification sandbox and asserts the same
//! pass criteria. One schedule, two transports — the soak's logic is provable
//! without credentials before it is ever trusted with them.
//!
//! Each run writes four artifacts under its run directory: the durable
//! journal bytes, the reconcile report history, the breaker transition log,
//! and the alert file. The operator runbook names these files; the
//! recorded-artifact test asserts every one exists and parses.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use tokio::sync::mpsc;

use fluent_concurrency::stream::StreamAbort;
use protopyne_types::Money;

use crate::alert::{
    poll_alerts, AlertCursor, AlertSink, FanoutSink, FileSink, GateWatch, MemorySink,
};
use crate::error::VenueError;
use crate::journal::{Journal, VenueStorage};
use crate::order::{OrderIntent, OrderSide, OrderStatus, Position};
use crate::paper::{Fault, PaperConfig, PaperVenue};
use crate::quote::Quote;
use crate::reconcile::{reconcile, JournalBook, ReconcileHandle, ReconcileSource, ReconcileUnit};
use crate::stream::{StreamConfig, SupervisedStream};
use crate::submit::{submit_guarded, Recorder};
use crate::tastytrade::{TastytradeConfig, TastytradeTransport, TransportError};
use crate::venue::{AccountEvent, Venue};

/// Journal bytes artifact: the durable intent/status log.
pub const JOURNAL_FILE: &str = "journal.db";
/// Reconcile report history artifact: one JSON object per pass.
pub const RECONCILE_FILE: &str = "reconcile.jsonl";
/// Breaker transition log artifact: one JSON object per breaker event.
pub const BREAKER_FILE: &str = "breaker.jsonl";
/// Alert file artifact: one JSON object per delivered alert.
pub const ALERTS_FILE: &str = "alerts.jsonl";

fn money(value: &str) -> Result<Money, VenueError> {
    value
        .parse::<Decimal>()
        .map(Money::new)
        .map_err(|_| VenueError::Invalid("soak harness money literal".to_owned()))
}

fn soak_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5)
        .single()
        .unwrap_or_else(Utc::now)
}

fn soak_intent(id: &str) -> Result<OrderIntent, VenueError> {
    Ok(OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("soak-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50")?)
        .build())
}

fn soak_quote() -> Result<Quote, VenueError> {
    Ok(Quote::new()
        .bid(money("600.00")?)
        .ask(money("600.10")?)
        .bid_size(50)
        .ask_size(50)
        .ts(soak_now())
        .build())
}

fn approved_for(intent: &OrderIntent) -> Result<protopyne_strategy::RiskApproved, VenueError> {
    use protopyne_strategy::{
        Direction, Proposal, ProposalState, RiskEngine, RiskLimits, RiskState,
    };
    use protopyne_types::Qty;
    let proposal = Proposal::new()
        .id(intent.proposal_id.clone())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(protopyne_strategy::ProposalSide::Entry)
        .structure(protopyne_strategy::StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(2)))
        .limit_price(money("600.50")?)
        .max_loss(money("300")?)
        .notional(money("1_000")?)
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .state(ProposalState::Proposed)
        .created_at(soak_now())
        .build();
    let engine = RiskEngine::new(RiskLimits::default());
    engine
        .check(
            &proposal,
            protopyne_strategy::OrderShape::new(true, true),
            &RiskState {
                quote_age_secs: 5,
                clock_skew_secs: 1,
                reconciliation_healthy: true,
                facts: protopyne_strategy::PortfolioFacts::unobserved(),
            },
        )
        .map_err(|refused| {
            VenueError::Invalid(format!(
                "soak harness proposal must be healthy: {refused:?}"
            ))
        })
}

/// The deterministic fault schedule every soak run executes, in order.
///
/// Three fault episodes behind the existing [`Fault`] queue, one per
/// submitted intent: a dropped connection the protocol retries into a fill,
/// four 5xx answers that exhaust the retry budget into a deterministic
/// refusal recording nothing, and a lost acknowledgement whose banked order
/// resolves by query-by-ID with no second order. No second injector: the
/// stream half of the storm drives [`SupervisedStream`] directly, and the
/// reconcile half observes a scripted drift.
#[must_use]
/// The deterministic fault schedule every soak run executes, in order.
/// 
/// Three fault episodes behind the existing [`Fault`] queue, one per
/// submitted intent: a dropped connection the protocol retries into a fill,
/// four 5xx answers that exhaust the retry budget into a deterministic
/// refusal recording nothing, and a lost acknowledgement whose banked order
/// resolves by query-by-ID with no second order. No second injector: the
/// stream half of the storm drives [`SupervisedStream`] directly, and the
/// reconcile half observes a scripted drift.
pub fn fault_schedule() -> Vec<Fault> {
    vec![
        Fault::DropConnection,
        Fault::Status500,
        Fault::Status500,
        Fault::Status500,
        Fault::Status500,
        Fault::LoseAck,
    ]
}

/// Scripted connector: pops canned connect outcomes and counts attempts.
struct Script {
    outcomes: std::sync::Mutex<VecDeque<Result<mpsc::Receiver<String>, TransportError>>>,
    attempts: AtomicUsize,
}

impl Script {
    fn errors(count: usize) -> Self {
        Self {
            outcomes: std::sync::Mutex::new(
                (0..count)
                    .map(|n| Err(TransportError::Transient(format!("soak storm drop {n}"))))
                    .collect(),
            ),
            attempts: AtomicUsize::new(0),
        }
    }

    fn connect(&self) -> Result<mpsc::Receiver<String>, TransportError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        common_core::sync::lock(&self.outcomes)
            .pop_front()
            .unwrap_or_else(|| Err(TransportError::Transient("soak storm exhausted".to_owned())))
    }
}

/// Scripted drift: the journal implies two contracts the venue never reports.
struct Drifted;

impl ReconcileSource for Drifted {
    fn journal(&self) -> JournalBook {
        JournalBook {
            net: HashMap::from([("SPY".to_owned(), 2)]),
            legs: HashMap::new(),
        }
    }

    fn venue_positions(&self) -> Vec<Position> {
        Vec::new()
    }

    fn venue_legs(&self) -> HashMap<String, i64> {
        HashMap::new()
    }
}

/// Scripted agreement: both sides report nothing, so the pass matches.
struct Agreed;

impl ReconcileSource for Agreed {
    fn journal(&self) -> JournalBook {
        JournalBook {
            net: HashMap::new(),
            legs: HashMap::new(),
        }
    }

    fn venue_positions(&self) -> Vec<Position> {
        Vec::new()
    }

    fn venue_legs(&self) -> HashMap<String, i64> {
        HashMap::new()
    }
}

/// What one soak run proved, as numbers the pass criteria assert directly.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
/// What one soak run proved, as numbers the pass criteria assert directly.
pub struct SoakOutcome {
    /// Intent rows the journal holds at the end of the run.
    pub intents_journaled: usize,
    /// Orders the venue holds at the end of the run.
    pub orders: usize,
    /// Submitted client IDs with no journal intent row (must be zero).
    pub unjournaled_intents: usize,
    /// Venue positions the journal cannot explain (must be zero: the only
    /// drift in the run is the scheduled mismatch, which the journal
    /// explains and which clears on the next clean pass).
    pub unknown_positions: usize,
    /// Stream connect attempts before the trip (must equal the schedule).
    pub stream_attempts: usize,
    /// Whether the scheduled storm tripped the breaker.
    pub breaker_tripped: bool,
    /// The banked trip reason.
    pub breaker_reason: String,
    /// Whether a fresh breaker over the same config starts untripped
    /// (recovery is a new run, never an in-place re-arm).
    pub fresh_breaker_untripped: bool,
    /// Whether the opening reconcile pass matched.
    pub open_matched: bool,
    /// Mismatching underlyings in the scheduled drift pass.
    pub drift_mismatches: usize,
    /// Whether entries halted while the drift stood.
    pub halted_during_drift: bool,
    /// Whether the closing clean pass cleared the halt.
    pub cleared_after: bool,
    /// Alerts delivered out of band across the whole run.
    pub alerts_delivered: usize,
    /// Alerts delivered by a repeat poll with no new bank (must be zero:
    /// delivery drains, edges never re-fire).
    pub repoll_delivered: usize,
    /// Run directory holding the four artifacts.
    pub run_dir: PathBuf,
}

fn append_line(path: &Path, value: &serde_json::Value) -> Result<(), VenueError> {
    let mut line = serde_json::to_string(value)
        .map_err(|e| VenueError::Journal(format!("soak artifact is not JSON: {e}")))?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| {
            use std::io::Write as _;
            file.write_all(line.as_bytes())
        })
        .map_err(|e| VenueError::Journal(format!("{}: {e}", path.display())))
}

/// Execute the deterministic schedule against the paper venue, writing the
/// four runbook artifacts under `run_dir`. No credentials, no network: the
/// stream storm never dials (its connector returns canned errors) and the
/// venue answers from memory.
pub async fn run_paper_soak(run_dir: &Path) -> Result<SoakOutcome, VenueError> {
    std::fs::create_dir_all(run_dir)
        .map_err(|e| VenueError::Journal(format!("{}: {e}", run_dir.display())))?;
    let journal_path = run_dir.join(JOURNAL_FILE);
    let reconcile_path = run_dir.join(RECONCILE_FILE);
    let breaker_path = run_dir.join(BREAKER_FILE);
    let alerts_path = run_dir.join(ALERTS_FILE);

    let permit = VenueStorage::new(journal_path.clone());
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            run_paper_soak_inner(
                run_dir,
                &journal_path,
                &reconcile_path,
                &breaker_path,
                &alerts_path,
                &permit,
            )
            .await
        })
        .await
}

#[allow(clippy::too_many_lines)]
async fn run_paper_soak_inner(
    run_dir: &Path,
    journal_path: &Path,
    reconcile_path: &Path,
    breaker_path: &Path,
    alerts_path: &Path,
    permit: &VenueStorage,
) -> Result<SoakOutcome, VenueError> {
    let now = soak_now();
    let quote = soak_quote()?;

    // ── journal-first submit over a clean venue ──────────────────────
    let journal = Arc::new(Journal::open(journal_path)?);
    let clean = PaperVenue::new(PaperConfig::new().build());
    let recorder = Recorder::durable(&journal, permit);
    let intent = soak_intent("soak-clean#0")?;
    let approved = approved_for(&intent)?;
    let record = submit_guarded(&recorder, &clean, &approved, intent.clone(), &quote, now)
        .await
        .map_err(|e| VenueError::Server(format!("clean submit must succeed: {e}")))?;
    if record.status != OrderStatus::Filled {
        return Err(VenueError::Server(format!(
            "clean submit must fill, got {:?}",
            record.status
        )));
    }

    // ── the fault schedule, every intent through the protocol ──────
    // Journal-before-venue holds for the faults too: each episode submits
    // through `submit_guarded`, so the intent row lands before the first
    // fault can fire and the run's unjournaled count is structural.
    let schedule = fault_schedule();
    let faulted = PaperVenue::new(PaperConfig::new().build());
    let recorder = Recorder::durable(&journal, permit);
    // A dropped connection retries into a fill: one fault, one order.
    faulted.inject(Fault::DropConnection);
    let dropped = soak_intent("soak-drop#0")?;
    let retry_fill = submit_guarded(
        &recorder,
        &faulted,
        &approved_for(&dropped)?,
        dropped,
        &quote,
        now,
    )
    .await
    .map_err(|e| VenueError::Server(format!("the retry must fill: {e}")))?;
    if retry_fill.status != OrderStatus::Filled {
        return Err(VenueError::Server(format!(
            "the retry must fill, got {:?}",
            retry_fill.status
        )));
    }
    // Four 5xx answers exhaust the retry budget into a deterministic
    // refusal: the intent row stands, no order banks.
    for _ in 0..4 {
        faulted.inject(Fault::Status500);
    }
    let refused = soak_intent("soak-500#0")?;
    let refusal = submit_guarded(
        &recorder,
        &faulted,
        &approved_for(&refused)?,
        refused,
        &quote,
        now,
    )
    .await;
    if !matches!(refusal, Err(VenueError::Server(_))) {
        return Err(VenueError::Server(format!(
            "the exhausted budget must refuse as server, got {refusal:?}"
        )));
    }
    // The lost acknowledgement banks the order, then loses the verdict: the
    // protocol's query-by-ID resolves it, never a second order.
    faulted.inject(Fault::LoseAck);
    let lost = soak_intent("soak-lost#0")?;
    let resolved = submit_guarded(
        &recorder,
        &faulted,
        &approved_for(&lost)?,
        lost,
        &quote,
        now,
    )
    .await
    .map_err(|e| VenueError::Server(format!("the lost ack must resolve: {e}")))?;
    if resolved.status != OrderStatus::Filled && resolved.status != OrderStatus::Acknowledged {
        return Err(VenueError::Server(format!(
            "the lost ack must resolve to its banked order, got {:?}",
            resolved.status
        )));
    }
    if faulted.pending_faults() != 0 {
        return Err(VenueError::Server(format!(
            "the schedule must drain exactly, {} left",
            faulted.pending_faults()
        )));
    }
    if faulted.orders().len() != 2 {
        return Err(VenueError::Server(format!(
            "the faults bank exactly two orders, got {}",
            faulted.orders().len()
        )));
    }

    // ── reconnect storm: trips on the third consecutive error ────────
    let stream = SupervisedStream::new(
        StreamConfig::new()
            .backoff_base_ms(1)
            .backoff_jitter_pct(0)
            .max_consecutive_errors(2)
            .restart_window_secs(60)
            .max_restarts_per_window(5)
            .build(),
    );
    let script = Script::errors(10);
    let abort = StreamAbort::new();
    let (out_tx, _out_rx) = mpsc::channel::<AccountEvent>(8);
    let vehicle = Arc::new(TastytradeTransport::new(
        TastytradeConfig::new()
            .base_url("http://127.0.0.1:9".to_owned())
            .build(),
    ));
    append_line(
        breaker_path,
        &serde_json::json!({"errors": 0, "tripped": false}),
    )?;
    let outcome = stream
        .run(|| async { script.connect() }, &abort, out_tx, &vehicle)
        .await;
    let stream_attempts = script.attempts.load(Ordering::SeqCst);
    let breaker_tripped = stream.breaker().tripped();
    let breaker_reason = stream
        .breaker()
        .trip_reason()
        .unwrap_or_else(|| "stream breaker silent".to_owned());
    append_line(
        breaker_path,
        &serde_json::json!({
            "errors": stream_attempts,
            "tripped": breaker_tripped,
            "reason": breaker_reason,
        }),
    )?;
    if !matches!(outcome, crate::stream::StreamOutcome::BreakerTripped { .. }) {
        return Err(VenueError::Server(format!(
            "the storm must trip the breaker, got {outcome:?}"
        )));
    }
    // Recovery is a fresh run: a new breaker over the same config starts
    // untripped with cold counters. No reset exists by design.
    let fresh = SupervisedStream::new(
        StreamConfig::new()
            .backoff_base_ms(1)
            .backoff_jitter_pct(0)
            .max_consecutive_errors(2)
            .restart_window_secs(60)
            .max_restarts_per_window(5)
            .build(),
    );
    let fresh_breaker_untripped =
        !fresh.breaker().tripped() && fresh.breaker().consecutive_errors() == 0;
    append_line(
        breaker_path,
        &serde_json::json!({"fresh": true, "tripped": false}),
    )?;
    // ── reconcile: clean pass, scheduled drift, clean pass ───────────
    let gate = ReconcileHandle::new();
    let open = ReconcileUnit::new(Arc::new(Agreed), gate.clone()).observe();
    append_line(
        reconcile_path,
        &serde_json::json!({"matched": open.matched, "mismatches": open.mismatches.len()}),
    )?;
    if !open.matched {
        return Err(VenueError::Server("the opening pass must match".to_owned()));
    }
    let drifted = ReconcileUnit::new(Arc::new(Drifted), gate.clone()).observe();
    append_line(
        reconcile_path,
        &serde_json::json!({"matched": drifted.matched, "mismatches": drifted.mismatches.len()}),
    )?;
    let halted_during_drift = gate.entries_halted();

    // ── out-of-band pages: one per trigger, never one per poll ────
    // The breaker trip and the drift mismatch page on separate polls so the
    // count per trigger is exact: one trip, one page; one mismatch, one page.
    let memory = MemorySink::new();
    let file: Arc<dyn AlertSink> = Arc::new(FileSink::new(alerts_path.to_owned()));
    let fanout = FanoutSink::new(vec![Arc::new(memory.clone()) as Arc<dyn AlertSink>, file]);
    let mut breaker_cursor = AlertCursor::new();
    let breaker_pages = poll_alerts(
        &mut breaker_cursor,
        &GateWatch {
            breaker: Some(stream.breaker()),
            reconcile: None,
            backfill: None,
            state: None,
        },
        &fanout,
        now,
    )?;
    let mut drift_cursor = AlertCursor::new();
    let drift_pages = poll_alerts(
        &mut drift_cursor,
        &GateWatch {
            breaker: None,
            reconcile: Some(&gate),
            backfill: None,
            state: None,
        },
        &fanout,
        now,
    )?;
    // A re-poll pages nothing: delivery drains, edges never re-fire.
    let repeat_pages = poll_alerts(
        &mut drift_cursor,
        &GateWatch {
            breaker: None,
            reconcile: Some(&gate),
            backfill: None,
            state: None,
        },
        &fanout,
        now,
    )?;
    // The file artifact carries exactly what memory saw: no lost pages.
    let file_lines = std::fs::read_to_string(alerts_path)
        .map_err(|e| VenueError::Journal(format!("{}: {e}", alerts_path.display())))?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    if file_lines != memory.alerts().len() {
        return Err(VenueError::Server(format!(
            "alert file holds {file_lines} lines, memory holds {}",
            memory.alerts().len()
        )));
    }
    // The closing clean pass clears the halt; paging never clears it.
    let cleared = ReconcileUnit::new(Arc::new(Agreed), gate.clone()).observe();
    append_line(
        reconcile_path,
        &serde_json::json!({"matched": cleared.matched, "mismatches": cleared.mismatches.len()}),
    )?;
    let cleared_after = !gate.entries_halted() && cleared.matched;

    // ── journal census: every submitted ID has its intent row ────────
    let replayed = journal.replay(permit)?;
    let intent_rows = replayed
        .events
        .iter()
        .filter(|event| event.kind == crate::journal::INTENT_KIND)
        .count();
    let submitted = ["soak-clean#0", "soak-drop#0", "soak-500#0", "soak-lost#0"];
    let mut unjournaled = 0_usize;
    for id in submitted {
        if !replayed
            .events
            .iter()
            .any(|event| event.kind == crate::journal::INTENT_KIND && event.client_id == id)
        {
            unjournaled += 1;
        }
    }
    // Unknown positions: venue truth the journal cannot explain. The closing
    // census folds the replayed fills the same way the live loop does (last
    // status wins, filled intents contribute signed quantities) and compares
    // against the clean book; the only drift in the run is the scheduled
    // mismatch, which the journal explains and which cleared above.
    let mut last_status: HashMap<&str, &str> = HashMap::new();
    for event in &replayed.events {
        if matches!(
            event.kind.as_str(),
            "pending"
                | "submitted"
                | "acknowledged"
                | "partial"
                | "filled"
                | "cancelled"
                | "rejected"
                | "expired"
                | "open_failed"
        ) {
            last_status.insert(event.client_id.as_str(), event.kind.as_str());
        }
    }
    let mut fills: Vec<(String, i64)> = Vec::new();
    for intent_event in replayed
        .events
        .iter()
        .filter(|event| event.kind == crate::journal::INTENT_KIND)
    {
        if last_status.get(intent_event.client_id.as_str()) != Some(&"filled") {
            continue;
        }
        let Ok(intent): Result<OrderIntent, _> = serde_json::from_str(&intent_event.detail) else {
            return Err(VenueError::Journal(
                "soak journal intent row is not JSON".to_owned(),
            ));
        };
        let signed = match intent.side {
            OrderSide::Buy => i64::from(intent.quantity),
            OrderSide::Sell => -i64::from(intent.quantity),
        };
        fills.push((intent.underlying, signed));
    }
    let journal_net = crate::reconcile::journal_net(&fills);
    // Both paper books are venue truth: the clean fill and the two fills
    // behind the fault schedule.
    let mut paper_positions = clean.positions();
    paper_positions.extend(faulted.positions());
    let closing = reconcile(&journal_net, &paper_positions);
    let unknown_positions = if closing.matched {
        0
    } else {
        closing.mismatches.len()
    };
    let _ = schedule.len();

    Ok(SoakOutcome {
        intents_journaled: intent_rows,
        orders: clean.orders().len() + faulted.orders().len(),

        unjournaled_intents: unjournaled,
        unknown_positions,
        stream_attempts,
        breaker_tripped,
        breaker_reason,
        fresh_breaker_untripped,
        open_matched: open.matched,
        drift_mismatches: drifted.mismatches.len(),
        halted_during_drift,
        cleared_after,
        alerts_delivered: breaker_pages + drift_pages,
        repoll_delivered: repeat_pages,
        run_dir: run_dir.to_owned(),
    })
}

#[cfg(test)]
#[path = "../tests/cert_soak.rs"]
mod tests;