//! End-to-end fault soak: reconnect storm, kill-mid-submit, and reconcile
//! mismatch on one rig, with out-of-band alert delivery at the end.
//!
//! Protocol: every fault is scripted in memory — no sockets beyond
//! loopback-free stubs, no sleeps beyond millisecond backoffs — and every
//! number is asserted exactly (attempt counts, order counts, alert counts).
//! The storm trips the breaker on the third consecutive error; the
//! accept-then-silence submit resolves by retry with exactly one order; the
//! injected drift halts entries and pages out of band. Nothing here judges
//! a trade on merit.

use super::*;
use crate::journal::{Journal, VenueStorage};
use crate::submit::Recorder;
use crate::order::{OrderIntent, OrderRecord, OrderSide, OrderStatus, Position};
use crate::reconcile::{JournalBook, ReconcileHandle, ReconcileSource, ReconcileUnit};
use crate::stream::{StreamOutcome, SupervisedStream};
use crate::tastytrade::TransportError;
use crate::venue::{DryRunOutcome, Venue};
use chrono::{TimeZone, Utc};
use fluent_concurrency::stream::StreamAbort;
use protopyne_types::Money;
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::mpsc;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn intent(id: &str) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build()
}

fn quote() -> Quote {
    Quote::new()
        .bid(money("600.00"))
        .ask(money("600.10"))
        .bid_size(50)
        .ask_size(50)
        .ts(now())
        .build()
}

// ── reconnect storm ────────────────────────────────────────────────

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
                    .map(|n| Err(TransportError::Transient(format!("storm drop {n}"))))
                    .collect(),
            ),
            attempts: AtomicUsize::new(0),
        }
    }

    fn connect(&self) -> Result<mpsc::Receiver<String>, TransportError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.outcomes.lock().expect("outcomes").pop_front().expect("scripted outcome")
    }
}

/// A ten-drop storm trips the breaker on the third consecutive error:
/// exactly three connect attempts, no hidden extra tries, and the trip
/// names the streak.
#[tokio::test]
async fn reconnect_storm_trips_on_the_third_attempt() {
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
        TastytradeConfig::new().base_url("http://127.0.0.1:9".to_owned()).build(),
    ));
    let outcome = stream.run(|| async { script.connect() }, &abort, out_tx, &vehicle).await;
    assert!(
        matches!(outcome, StreamOutcome::BreakerTripped { .. }),
        "the storm trips, got: {outcome:?}"
    );
    assert_eq!(script.attempts.load(Ordering::SeqCst), 3, "trip on the third error, then stop");
    assert!(stream.breaker().tripped());
    assert!(
        stream.breaker().trip_reason().unwrap_or_default().contains("3 consecutive"),
        "got: {:?}",
        stream.breaker().trip_reason()
    );
}

// ── kill-mid-submit ────────────────────────────────────────────────

/// Accept-then-silence venue: the first submit for an ID banks the order
/// but reports a transport failure (the kill window); resubmits for a
/// known ID return the banked record, never a second order.
struct FlakyOnce {
    orders: std::sync::Mutex<HashMap<String, OrderRecord>>,
    submits: AtomicUsize,
    silence: AtomicUsize,
}

impl FlakyOnce {
    fn new() -> Self {
        Self {
            orders: std::sync::Mutex::new(HashMap::new()),
            submits: AtomicUsize::new(0),
            silence: AtomicUsize::new(1),
        }
    }
}

impl Venue for FlakyOnce {
    fn dry_run(&self, _intent: &OrderIntent, _quote: &Quote) -> Result<DryRunOutcome, VenueError> {
        Ok(DryRunOutcome { allowed: true, reserved_notional: Money::zero() })
    }

    fn submit<'a>(
        &'a self,
        intent: OrderIntent,
        _quote: &'a Quote,
        now: chrono::DateTime<Utc>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            self.submits.fetch_add(1, Ordering::SeqCst);
            let mut orders = self.orders.lock().expect("orders");
            if let Some(known) = orders.get(&intent.client_id) {
                return Ok(known.clone());
            }
            let record = OrderRecord::new()
                .intent(intent.clone())
                .status(OrderStatus::Acknowledged)
                .filled_quantity(0)
                .maybe_avg_price(None)
                .fees_paid(Money::zero())
                .updated_at(now)
                .build();
            orders.insert(intent.client_id.clone(), record.clone());
            if self.silence.fetch_sub(1, Ordering::SeqCst) > 0 {
                return Err(VenueError::Transport("kill window: ack lost".to_owned()));
            }
            Ok(record)
        })
    }

    fn query<'a>(
        &'a self,
        client_id: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Option<OrderRecord>> + Send + 'a>> {
        Box::pin(async move { self.orders.lock().expect("orders").get(client_id).cloned() })
    }

    fn cancel<'a>(
        &'a self,
        client_id: &'a str,
        _now: chrono::DateTime<Utc>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<OrderRecord, VenueError>> + Send + 'a>> {
        Box::pin(async move {
            self.orders
                .lock()
                .expect("orders")
                .get(client_id)
                .cloned()
                .ok_or_else(|| VenueError::Invalid(format!("unknown order {client_id}")))
        })
    }

    fn positions(&self) -> Vec<Position> {
        Vec::new()
    }

    fn orders(&self) -> Vec<OrderRecord> {
        self.orders.lock().expect("orders").values().cloned().collect()
    }

    fn apply_corporate_event(
        &self,
        _underlying: &str,
        _event: CorporateEvent,
        _now: chrono::DateTime<Utc>,
    ) -> Result<String, VenueError> {
        Err(VenueError::Invalid("no corporate surface on the stub".to_owned()))
    }
}

fn approved_for(intent: &OrderIntent) -> protopyne_strategy::RiskApproved {
    let engine = protopyne_strategy::RiskEngine::new(protopyne_strategy::RiskLimits::default());
    let proposal = protopyne_strategy::Proposal::new()
        .id(intent.proposal_id.clone())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(protopyne_strategy::Direction::Long)
        .side(protopyne_strategy::ProposalSide::Entry)
        .structure(protopyne_strategy::StructureKind::Vertical)
        .quantity(protopyne_types::Qty::new(Decimal::from(2)))
        .limit_price(money("600.50"))
        .max_loss(money("300"))
        .notional(money("1_000"))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .state(protopyne_strategy::ProposalState::Proposed)
        .created_at(now())
        .build();
    engine
        .check(
            &proposal,
            &protopyne_strategy::RiskState {
                quote_age_secs: 5,
                clock_skew_secs: 1,
                reconciliation_healthy: true,
                is_limit_order: true,
                is_spread: true,
            },
        )
        .expect("healthy entry")
}

/// Kill-mid-submit, two halves. First the durable half: an intent journaled
/// to a file survives a dropped handle — reopen replays it pending, with
/// no fill and no duplicate. Then the verdict half: the accept-then-silence
/// submit resolves by retry into exactly one order.
#[tokio::test]
async fn kill_mid_submit_leaves_no_unknown_position_and_no_duplicate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("soak.db");
    let permit = VenueStorage::new(path.clone());
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            let journal = Journal::open(&path).expect("open");
            journal.record_intent(&permit, &intent("soak#0"), now()).expect("journal the intent");
        })
        .await;
    // The crash: the handle above dropped. Reopen replays the intent.
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    let replayed = fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            Journal::open(&path).expect("reopen").replay(&permit).expect("replay")
        })
        .await;
    assert_eq!(replayed.intents.len(), 1, "the intent survives the crash");
    assert_eq!(replayed.intents[0].client_id, "soak#0");
    assert!(
        replayed.events.iter().all(|event| event.kind != "filled"),
        "no fill without a verdict"
    );

    let venue = FlakyOnce::new();
    let journal = Journal::ephemeral();
    let recorder = Recorder::ephemeral(&journal);
    let record = crate::submit::submit_guarded(&recorder, &venue, &approved_for(&intent("soak#0")), intent("soak#0"), &quote(), now())
        .await
        .expect("the retry resolves");
    assert_eq!(record.status, OrderStatus::Acknowledged);
    assert_eq!(venue.submits.load(Ordering::SeqCst), 2, "one lost ack, one resolving retry");
    assert_eq!(venue.orders().len(), 1, "the retry was a lookup, never a second order");
    let permit = VenueStorage::new(std::path::PathBuf::from("ephemeral-test"));
    assert_eq!(
        journal
            .replay(&permit)
            .expect("replay")
            .events
            .iter()
            .filter(|event| event.kind == crate::journal::INTENT_KIND)
            .count(),
        1,
        "one intent row for one order"
    );
}

// ── reconcile mismatch + paging ────────────────────────────────────

/// Scripted drift: the journal implies two contracts the venue never
/// reports.
struct Drifted;

impl ReconcileSource for Drifted {
    fn journal(&self) -> JournalBook {
        JournalBook { net: HashMap::from([("SPY".to_owned(), 2)]), legs: HashMap::new() }
    }

    fn venue_positions(&self) -> Vec<Position> {
        Vec::new()
    }

    fn venue_legs(&self) -> HashMap<String, i64> {
        HashMap::new()
    }
}

/// Injected drift halts entries, fails the health verdict, and pages out
/// of band: one correctness alert through a memory-plus-file fan-out, with
/// the halt still set afterwards.
#[test]
fn reconcile_mismatch_halts_entries_and_pages_out_of_band() {
    let gate = ReconcileHandle::new();
    let unit = ReconcileUnit::new(Arc::new(Drifted), gate.clone());
    let report = unit.observe();
    assert!(!report.matched, "the drift fires");
    assert!(gate.entries_halted(), "entries halt on disagreement");
    assert!(gate.check_entries().is_err());

    let state = HealthState {
        quote_age_secs: 5,
        clock_skew_secs: 1,
        stream_connected: true,
        store: protopyne_store::StoreHealth::default(),
        reconciled: gate.reconciled(),
        backfilled: true,
    };
    assert!(!may_trade(&state, &HealthLimits::new().build()).may_trade(), "health fails closed");

    let dir = tempfile::tempdir().expect("tempdir");
    let memory = MemorySink::new();
    let file: Arc<dyn AlertSink> =
        Arc::new(FileSink::new(dir.path().join("soak-alerts.jsonl")));
    let fanout = FanoutSink::new(vec![Arc::new(memory.clone()) as Arc<dyn AlertSink>, file]);
    let mut cursor = AlertCursor::new();
    let sent = poll_alerts(
        &mut cursor,
        &GateWatch { breaker: None, reconcile: Some(&gate), backfill: None, state: None },
        &fanout,
        now(),
    )
    .expect("delivery");
    assert_eq!(sent, 1, "one mismatch, one page");
    assert_eq!(memory.alerts().len(), 1);
    assert_eq!(memory.alerts()[0].source, AlertSource::ReconcileMismatch);
    assert_eq!(memory.alerts()[0].class, AlertClass::Correctness);
    assert!(gate.entries_halted(), "paging never clears the halt");
}
