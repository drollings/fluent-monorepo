//! Manual ticket surface: the gateway verb and the HTTP route journal the
//! identical issued/confirmed rows through the tracker's journal-first
//! path, a mismatched fill flags drift, the journal rebuilds the waiting
//! set, and a dark import reports without touching ticket state.

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_types::Money;
use protopyne_venue::{
    ConfirmedFill, ImportedPosition, ManualTicket, OrderIntent, OrderSide, TicketRead,
    TicketTracker, TICKET_CONFIRMED_KIND, TICKET_ISSUED_KIND,
};
use protopyne_view::TerminalGateway;

use crate::service::{serve, ServerConfig};

fn bars() -> Vec<protopyne_types::Bar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}

fn base(handle: &crate::service::ServerHandle) -> String {
    format!("http://{}", handle.addr)
}

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn intent(id: &str) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-rh".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build()
}

fn intent_value(id: &str) -> serde_json::Value {
    serde_json::to_value(intent(id)).expect("intent serializes")
}

fn fill_value(quantity: u32, price: &str) -> serde_json::Value {
    serde_json::to_value(ConfirmedFill {
        quantity,
        price: money(price),
    })
    .expect("fill serializes")
}

/// An issue through the gateway equals an issue through the route: both
/// journal the identical issued row (same ticket client id and kind), so the
/// waiting read cannot tell which surface ordered it.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_issue_equals_route_issue() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, bars()).await.expect("test server boots");
    let gateway = handle.gateway();
    gateway
        .issue_ticket(intent_value("rh-1"))
        .expect("gateway issues");
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/tickets/issue", base(&handle)))
        .json(&intent_value("rh-2"))
        .send()
        .await
        .expect("route posts");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let rows = gateway.journal().expect("journal reads");
    let issued: Vec<_> = rows
        .iter()
        .filter(|row| row.kind == TICKET_ISSUED_KIND)
        .collect();
    assert_eq!(issued.len(), 2, "one row per surface: {rows:?}");
    let waiting = gateway.tickets_waiting().expect("waiting reads");
    assert_eq!(waiting.len(), 2, "both tickets wait: {waiting:?}");
    assert_eq!(waiting[0].ticket_id, "rh-1");
    assert_eq!(waiting[1].ticket_id, "rh-2");
    for row in &issued {
        let ticket: ManualTicket =
            serde_json::from_str(&row.detail).expect("issued detail is a ticket");
        assert_eq!(row.client_id, ticket.ticket_id());
    }
    handle.shutdown().await;
}

/// Issue then confirm journals both rows and clears the wait; a mismatched
/// fill flags drift on the drifts read.
#[tokio::test(flavor = "multi_thread")]
async fn confirm_clears_wait_and_flags_drift() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, bars()).await.expect("test server boots");
    let gateway = handle.gateway();
    gateway
        .issue_ticket(intent_value("rh-1"))
        .expect("first ticket issues");
    gateway
        .issue_ticket(intent_value("rh-2"))
        .expect("second ticket issues");
    gateway
        .confirm_ticket("rh-1", fill_value(2, "600.40"))
        .expect("matching fill confirms");
    let waiting = gateway.tickets_waiting().expect("waiting reads");
    assert_eq!(waiting.len(), 1, "one ticket still waits");
    assert_eq!(waiting[0].ticket_id, "rh-2");
    assert!(gateway.ticket_drifts().expect("drifts read").is_empty());
    gateway
        .confirm_ticket("rh-2", fill_value(1, "600.40"))
        .expect("short fill still confirms");
    let waiting = gateway.tickets_waiting().expect("waiting reads");
    assert!(waiting.is_empty(), "nothing waits after both confirms");
    let drifts = gateway.ticket_drifts().expect("drifts read");
    assert_eq!(drifts.len(), 1, "the short fill flags drift: {drifts:?}");
    assert_eq!(drifts[0].ticket_id, "rh-2");
    assert_eq!(drifts[0].kind, "QuantityMismatch");
    let rows = gateway.journal().expect("journal reads");
    assert!(
        rows.iter()
            .any(|row| row.kind == TICKET_CONFIRMED_KIND && row.client_id == "rh-2"),
        "the confirm journals its row: {rows:?}"
    );
    handle.shutdown().await;
}

/// The waiting set rebuilds from the journal alone: a fresh tracker folded
/// over the journaled rows matches the live read, so a restart loses no
/// wait. Confirming an unknown ticket refuses without journaling.
#[tokio::test(flavor = "multi_thread")]
async fn restart_replays_waiting_set_and_unknown_confirm_refuses() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, bars()).await.expect("test server boots");
    let gateway = handle.gateway();
    gateway
        .issue_ticket(intent_value("rh-1"))
        .expect("ticket issues");
    let rows = gateway.journal().expect("journal reads");
    let events: Vec<protopyne_venue::JournalEvent> = rows
        .iter()
        .map(|row| protopyne_venue::JournalEvent {
            seq: row.seq,
            client_id: row.client_id.clone(),
            kind: row.kind.clone(),
            detail: row.detail.clone(),
            at_ms: row.at_ms,
        })
        .collect();
    let rebuilt = TicketTracker::replay(&events);
    let live = gateway.tickets_waiting().expect("waiting reads");
    assert_eq!(
        rebuilt
            .unconfirmed()
            .iter()
            .map(|ticket| ticket.ticket_id().to_owned())
            .collect::<Vec<_>>(),
        live.iter()
            .map(|ticket| ticket.ticket_id.clone())
            .collect::<Vec<_>>(),
        "replay rebuilds exactly the live wait"
    );
    let before = rows.len();
    let refused = gateway.confirm_ticket("no-such-ticket", fill_value(2, "600.40"));
    assert!(refused.is_err(), "an unknown ticket refuses");
    let rows = gateway.journal().expect("journal reads");
    assert_eq!(rows.len(), before, "a refused confirm journals nothing");
    handle.shutdown().await;
}

/// A failing read-only import reports and changes nothing: the waiting set
/// and the last good snapshot survive, and no drift is banked.
#[test]
fn dark_import_reports_and_changes_nothing() {
    struct Dark;
    impl TicketRead for Dark {
        fn snapshot(&self) -> Result<Vec<ImportedPosition>, protopyne_venue::VenueError> {
            Err(protopyne_venue::VenueError::Invalid(
                "read-only import is dark".to_owned(),
            ))
        }
    }
    let mut tracker = TicketTracker::new();
    tracker
        .issue(
            intent("rh-1"),
            Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap(),
        )
        .expect("issue");
    let refused = tracker.apply_import(&Dark);
    assert!(refused.is_err(), "a dark import reports its failure");
    assert_eq!(tracker.unconfirmed().len(), 1, "the wait survives");
    assert!(tracker.imported().is_empty(), "no snapshot on failure");
    assert!(tracker.drifts().is_empty(), "no drift banked on failure");
}

/// A read-only import that disagrees with the confirmed book flags import
/// drift through the route, and a corrected import clears its own flag.
#[tokio::test(flavor = "multi_thread")]
async fn import_flags_drift_and_a_corrected_import_clears_it() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, bars()).await.expect("test server boots");
    let gateway = handle.gateway();
    gateway
        .issue_ticket(intent_value("rh-1"))
        .expect("ticket issues");
    gateway
        .confirm_ticket("rh-1", fill_value(2, "600.40"))
        .expect("fill confirms");
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/tickets/import", base(&handle)))
        .json(&serde_json::json!([{"underlying": "SPY", "quantity": 5}]))
        .send()
        .await
        .expect("route posts");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let drifts = gateway.ticket_drifts().expect("drifts read");
    assert_eq!(drifts.len(), 1, "the import disagrees: {drifts:?}");
    assert_eq!(drifts[0].kind, "ImportMismatch");
    gateway
        .import_tickets(serde_json::json!([{"underlying": "SPY", "quantity": 2}]))
        .expect("corrected import applies");
    assert!(
        gateway.ticket_drifts().expect("drifts read").is_empty(),
        "a corrected import clears its own flag"
    );
    let malformed = client
        .post(format!("{}/tickets/import", base(&handle)))
        .json(&serde_json::json!([{"underlying": "SPY"}]))
        .send()
        .await
        .expect("route posts");
    assert_eq!(malformed.status(), reqwest::StatusCode::BAD_REQUEST);
    handle.shutdown().await;
}
