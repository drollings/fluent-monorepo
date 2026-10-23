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
use protopyne_view::{TerminalGateway, TicketDraft, TicketSide};

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

/// One draft, as the terminal's form fills it.
fn draft() -> TicketDraft {
    TicketDraft {
        side: TicketSide::Buy,
        quantity: 2,
        limit_price: "600.50".to_owned(),
        symbol: "SPY".to_owned(),
    }
}

/// The manual-ticket intent that same draft describes on the wire: the shape
/// the HTTP route's parser accepts.
fn manual_intent(id: &str) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("manual".to_owned())
        .strategy("manual".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build()
}

fn fill_value(quantity: u32, price: &str) -> serde_json::Value {
    serde_json::to_value(ConfirmedFill {
        quantity,
        price: money(price),
    })
    .expect("fill serializes")
}

/// The draft conversion and the route's wire parse are one schema: the same
/// draft and id produce the same intent, so the rendered ticket cannot differ
/// by entry point.
#[test]
fn draft_intent_equals_the_route_document() {
    let converted = crate::views::intent_of_draft(&draft(), "manual-1".to_owned())
        .expect("a well-formed draft converts");
    let parsed: OrderIntent = serde_json::from_value(
        serde_json::to_value(manual_intent("manual-1")).expect("document serializes"),
    )
    .expect("the wire document parses");
    assert_eq!(converted, parsed, "one schema, two entry points");
    let at = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap();
    assert_eq!(
        ManualTicket::from_intent(converted, at).render(),
        ManualTicket::from_intent(parsed, at).render(),
        "the human places the identical ticket from either surface"
    );
}

/// An issue through the gateway equals an issue through the route: the draft
/// path mints the identity, and a route issue of the same document reads back
/// as the identical waiting ticket, so the dock cannot tell which surface
/// ordered it.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_issue_of_a_draft_equals_the_route_issue() {
    let boot = || ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let drafts = serve(boot(), bars()).await.expect("draft server boots");
    let route = serve(boot(), bars()).await.expect("route server boots");
    drafts
        .gateway()
        .issue_ticket(draft())
        .expect("the draft issues");
    let waiting = drafts.gateway().tickets_waiting().expect("draft waits");
    assert_eq!(waiting.len(), 1, "one ticket waits");
    let minted = waiting[0].ticket_id.clone();
    let response = reqwest::Client::new()
        .post(format!("{}/tickets/issue", base(&route)))
        .json(&serde_json::to_value(manual_intent(&minted)).expect("document serializes"))
        .send()
        .await
        .expect("route posts");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        waiting,
        route.gateway().tickets_waiting().expect("route waits"),
        "the same draft waits identically on both surfaces"
    );
    let issued: Vec<_> = drafts
        .gateway()
        .journal()
        .expect("journal reads")
        .into_iter()
        .filter(|row| row.kind == TICKET_ISSUED_KIND)
        .collect();
    assert_eq!(issued.len(), 1, "the draft journals one issued row");
    let ticket: ManualTicket =
        serde_json::from_str(&issued[0].detail).expect("issued detail is a ticket");
    assert_eq!(issued[0].client_id, ticket.ticket_id());
    drafts.shutdown().await;
    route.shutdown().await;
}

/// A draft the core cannot parse refuses there, naming the field, and
/// journals nothing: the terminal runs no second validator, so a malformed
/// price or a blank symbol has exactly one authority and one message.
#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_draft_is_refused_by_the_core_and_journals_nothing() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, bars()).await.expect("test server boots");
    let gateway = handle.gateway();
    let malformed = TicketDraft {
        limit_price: "not-a-price".to_owned(),
        ..draft()
    };
    let refused = gateway
        .issue_ticket(malformed)
        .expect_err("a malformed price refuses");
    assert!(
        refused.to_string().contains("limit_price"),
        "the refusal names the field: {refused}"
    );
    let blank = TicketDraft {
        symbol: "   ".to_owned(),
        ..draft()
    };
    let refused = gateway
        .issue_ticket(blank)
        .expect_err("a blank symbol refuses");
    assert!(
        refused.to_string().contains("symbol"),
        "the refusal names the field: {refused}"
    );
    assert!(
        gateway.tickets_waiting().expect("waiting reads").is_empty(),
        "a refused draft leaves no wait"
    );
    assert!(
        !gateway
            .journal()
            .expect("journal reads")
            .iter()
            .any(|row| row.kind == TICKET_ISSUED_KIND),
        "a refused draft journals nothing"
    );
    handle.shutdown().await;
}

/// Issue then confirm journals both rows and clears the wait; a mismatched
/// fill flags drift on the drifts read.
#[tokio::test(flavor = "multi_thread")]
async fn confirm_clears_wait_and_flags_drift() {
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    let handle = serve(config, bars()).await.expect("test server boots");
    let gateway = handle.gateway();
    gateway.issue_ticket(draft()).expect("first ticket issues");
    gateway.issue_ticket(draft()).expect("second ticket issues");
    gateway
        .confirm_ticket("manual-1", fill_value(2, "600.40"))
        .expect("matching fill confirms");
    let waiting = gateway.tickets_waiting().expect("waiting reads");
    assert_eq!(waiting.len(), 1, "one ticket still waits");
    assert_eq!(waiting[0].ticket_id, "manual-2");
    assert!(gateway.ticket_drifts().expect("drifts read").is_empty());
    gateway
        .confirm_ticket("manual-2", fill_value(1, "600.40"))
        .expect("short fill still confirms");
    let waiting = gateway.tickets_waiting().expect("waiting reads");
    assert!(waiting.is_empty(), "nothing waits after both confirms");
    let drifts = gateway.ticket_drifts().expect("drifts read");
    assert_eq!(drifts.len(), 1, "the short fill flags drift: {drifts:?}");
    assert_eq!(drifts[0].ticket_id, "manual-2");
    assert_eq!(drifts[0].kind, "QuantityMismatch");
    let rows = gateway.journal().expect("journal reads");
    assert!(
        rows.iter()
            .any(|row| row.kind == TICKET_CONFIRMED_KIND && row.client_id == "manual-2"),
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
    gateway.issue_ticket(draft()).expect("ticket issues");
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
    gateway.issue_ticket(draft()).expect("ticket issues");
    gateway
        .confirm_ticket("manual-1", fill_value(2, "600.40"))
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
