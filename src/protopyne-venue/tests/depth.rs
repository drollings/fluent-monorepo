use super::*;
use chrono::{TimeZone, Utc};
use fluent_wvr::validate::Validate;
use protopyne_types::{Money, SymbolIndex};
use rust_decimal::Decimal;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn level(price: &str, size: u64) -> PriceLevel {
    PriceLevel {
        price: money(price),
        size,
    }
}

fn ts() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn book() -> DepthBook {
    let mut index = SymbolIndex::new();
    DepthBook::new("SPY".to_owned(), &mut index).expect("intern SPY")
}

fn delta(bids: Vec<PriceLevel>, asks: Vec<PriceLevel>, sequence: u64) -> DepthDelta {
    DepthDelta {
        bids,
        asks,
        sequence,
        ts: ts(),
    }
}

// ── empty-side fixture ───────────────────────────────────────────────

#[test]
fn empty_book_has_no_touch_and_trips_no_detector() {
    let book = book();
    assert!(book.top().is_none(), "no touch without both sides");
    assert_eq!(book.sequence(), 0);
    assert_eq!(book.level_counts(), (0, 0));
    assert!(is_crossed(&book).is_none(), "empties are dark, not crossed");
    assert!(
        is_stale(&book, ts(), 60).is_none(),
        "a fresh empty is not stale"
    );
    assert!(Validate::validate(&book).is_ok());
}

// ── snapshot-replace fixture ─────────────────────────────────────────

#[test]
fn snapshot_replace_sets_best_first_ladders() {
    let mut book = book();
    book.snapshot_replace(
        &[level("600.00", 50), level("599.90", 10)],
        &[level("600.10", 40), level("600.20", 10)],
        1,
        ts(),
    )
    .expect("snapshot sets");
    assert_eq!(book.sequence(), 1);
    assert_eq!(book.level_counts(), (2, 2));
    let top = book.top().expect("touch");
    // Best-first is structural (B-tree order), not input order: the bids
    // arrived sorted here, the map guarantees it regardless.
    assert_eq!(top.bid, money("600.00"));
    assert_eq!(top.ask, money("600.10"));
    assert_eq!(top.bid_size, 50);
    assert_eq!(top.ask_size, 40);
}

#[test]
fn snapshot_refuses_stale_duplicates_and_bad_prices() {
    let mut book = book();
    book.snapshot_replace(&[level("600.00", 50)], &[level("600.10", 40)], 3, ts())
        .expect("first snapshot");
    assert!(
        book.snapshot_replace(&[level("600.00", 50)], &[level("600.10", 40)], 2, ts())
            .is_err(),
        "older snapshots never rewind the book"
    );
    assert!(
        book.snapshot_replace(
            &[level("600.00", 50), level("600.00", 10)],
            &[level("600.10", 40)],
            4,
            ts()
        )
        .is_err(),
        "duplicate prices in one snapshot refuse"
    );
    assert!(
        book.snapshot_replace(&[level("0.00", 50)], &[level("600.10", 40)], 4, ts())
            .is_err(),
        "non-positive prices refuse"
    );
    assert_eq!(book.sequence(), 3, "refused snapshots commit nothing");
}

// ── sequence-gap fixture ─────────────────────────────────────────────

#[test]
fn delta_gap_refuses_and_commits_nothing() {
    let mut book = book();
    book.snapshot_replace(&[level("600.00", 50)], &[level("600.10", 40)], 1, ts())
        .expect("snapshot");
    assert!(
        book.apply_delta(&delta(vec![level("600.05", 30)], vec![], 3))
            .is_err(),
        "a skipped sequence is a gap, not a fast-forward"
    );
    assert_eq!(book.sequence(), 1);
    assert_eq!(book.level_counts(), (1, 1), "refused deltas commit nothing");
    book.apply_delta(&delta(vec![level("600.05", 30)], vec![], 2))
        .expect("consecutive delta applies");
    assert_eq!(book.sequence(), 2);
    assert_eq!(book.level_counts(), (2, 1));
}

#[test]
fn delta_upsert_and_remove_semantics() {
    let mut book = book();
    book.snapshot_replace(&[level("600.00", 50)], &[level("600.10", 40)], 1, ts())
        .expect("snapshot");
    // Size zero removes the level; nonzero upserts.
    book.apply_delta(&delta(
        vec![level("600.00", 0)],
        vec![level("600.10", 99)],
        2,
    ))
    .expect("remove + upsert");
    let top = book.top();
    assert!(top.is_none(), "removing the only bid darkens the top");
    book.apply_delta(&delta(vec![level("599.95", 20)], vec![], 3))
        .expect("re-add bid");
    let top = book.top().expect("touch back");
    assert_eq!(top.bid, money("599.95"));
    assert_eq!(top.ask_size, 99);
}

// ── crossed-book fixture ─────────────────────────────────────────────

#[test]
fn crossed_top_reports_a_reason_while_clean_books_stay_silent() {
    let mut book = book();
    book.snapshot_replace(&[level("600.00", 50)], &[level("600.10", 40)], 1, ts())
        .expect("snapshot");
    assert!(is_crossed(&book).is_none());
    book.snapshot_replace(&[level("600.20", 50)], &[level("600.10", 40)], 2, ts())
        .expect("crossed snapshot");
    let reason = is_crossed(&book).expect("crossed trips");
    assert!(
        reason.contains("crossed"),
        "names the verdict, got: {reason}"
    );
    assert!(reason.contains("SPY"), "names the book, got: {reason}");
    // Locked books have no valid mid either.
    book.snapshot_replace(&[level("600.10", 50)], &[level("600.10", 40)], 3, ts())
        .expect("locked snapshot");
    assert!(is_crossed(&book).is_some(), "locked trips too");
}

// ── stale fixture ────────────────────────────────────────────────────

#[test]
fn stale_books_report_age_while_fresh_books_stay_silent() {
    let mut book = book();
    book.snapshot_replace(&[level("600.00", 50)], &[level("600.10", 40)], 1, ts())
        .expect("snapshot");
    assert!(is_stale(&book, ts(), 60).is_none());
    let late = ts() + chrono::Duration::seconds(120);
    let reason = is_stale(&book, late, 60).expect("stale trips");
    assert!(reason.contains("120"), "carries the age, got: {reason}");
    assert!(reason.contains("stale"), "names the verdict, got: {reason}");
}

// ── property test: delta-apply converges to the snapshot state ───────

/// Deterministic pseudo-random generator (xorshift64): no dev-dependency,
/// reproducible across runs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn random_levels(rng: &mut Rng, base: &str, count: usize) -> Vec<PriceLevel> {
    let base: Decimal = base.parse().expect("decimal");
    let mut prices: Vec<Decimal> = (0..count as u64)
        .map(|i| base + Decimal::from((rng.next() % 100) + i * 100))
        .collect();
    prices.sort_unstable();
    prices.dedup();
    prices
        .into_iter()
        .map(|price| PriceLevel {
            price: Money::new(price / Decimal::from(100)),
            size: (rng.next() % 50) + 1,
        })
        .collect()
}

#[test]
fn delta_streams_converge_to_the_snapshot_state() {
    // Two construction paths, one state: a book built level-by-level
    // through consecutive deltas converges to a book snapshot-replaced
    // with the same levels at the same sequence. Delta-apply is a pure
    // function of (state, deltas) — never of history or timing — so both
    // paths agree on tops, counts, and sequence.
    for seed in [7_u64, 99, 1_335] {
        let mut rng = Rng(seed);
        let bids = random_levels(&mut rng, "60000", 6);
        let asks = random_levels(&mut rng, "60100", 6);
        let target = bids.len() + asks.len();
        assert!(target > 0, "seed {seed} must generate levels");

        // Path one: one level per delta, sequences 1..=target.
        let mut streamed = book();
        let mut sequence = 0_u64;
        for level in bids
            .iter()
            .map(|l| (vec![*l], Vec::new()))
            .chain(asks.iter().map(|l| (Vec::new(), vec![*l])))
        {
            sequence += 1;
            streamed
                .apply_delta(&delta(level.0, level.1, sequence))
                .expect("stream applies");
        }
        // Path two: one wholesale snapshot at the same sequence.
        let mut snapshotted = book();
        snapshotted
            .snapshot_replace(&bids, &asks, sequence, ts())
            .expect("snapshot sets");
        assert_eq!(streamed.sequence(), snapshotted.sequence());
        assert_eq!(
            streamed.top(),
            snapshotted.top(),
            "seed {seed}: tops converge"
        );
        assert_eq!(
            streamed.level_counts(),
            snapshotted.level_counts(),
            "seed {seed}: ladders converge"
        );
    }
}

// ── BookManager ──────────────────────────────────────────────────────

#[test]
fn manager_holds_one_book_per_key_with_scoped_isolation() {
    let mut manager = BookManager::new();
    assert!(manager.is_empty());
    assert!(manager.book("SPY").is_none(), "reads never insert");
    let key = manager
        .apply_snapshot(
            "SPY",
            &[level("600.00", 50)],
            &[level("600.10", 40)],
            1,
            ts(),
        )
        .expect("snapshot opens the book");
    assert_eq!(manager.len(), 1);
    assert!(manager.book("SPY").is_some());
    assert!(
        manager.book("AAPL").is_none(),
        "no cross-talk between symbols"
    );
    assert_eq!(manager.book("SPY").expect("book").key(), key);
}

#[test]
fn manager_events_advance_consecutive_markers_only() {
    use crate::market::MarketEvent;
    let mut manager = BookManager::new();
    assert!(
        manager
            .apply_event(&MarketEvent::BookDelta {
                underlying: "SPY".to_owned(),
                sequence: 1
            })
            .is_err(),
        "books open on snapshot, never on a mid-stream delta"
    );
    manager
        .apply_snapshot(
            "SPY",
            &[level("600.00", 50)],
            &[level("600.10", 40)],
            1,
            ts(),
        )
        .expect("snapshot opens");
    manager
        .apply_event(&MarketEvent::BookDelta {
            underlying: "SPY".to_owned(),
            sequence: 2,
        })
        .expect("consecutive heartbeat advances");
    assert_eq!(manager.book("SPY").expect("book").sequence(), 2);
    assert!(
        manager
            .apply_event(&MarketEvent::BookDelta {
                underlying: "SPY".to_owned(),
                sequence: 4
            })
            .is_err(),
        "gaps refuse so the book re-snapshots"
    );
    assert!(
        manager
            .apply_event(&MarketEvent::Quote {
                underlying: "SPY".to_owned(),
                quote: manager.book("SPY").expect("book").top().expect("touch")
            })
            .is_err(),
        "misrouted kinds refuse instead of skipping silently"
    );
}

#[test]
fn scan_is_record_only_and_sorted() {
    let mut manager = BookManager::new();
    manager
        .apply_snapshot(
            "SPY",
            &[level("600.20", 50)],
            &[level("600.10", 40)],
            1,
            ts(),
        )
        .expect("crossed SPY");
    manager
        .apply_snapshot(
            "AAPL",
            &[level("100.00", 10)],
            &[level("100.05", 10)],
            1,
            ts() - chrono::Duration::seconds(500),
        )
        .expect("stale AAPL");
    manager
        .apply_snapshot(
            "MSFT",
            &[level("200.00", 10)],
            &[level("200.05", 10)],
            1,
            ts(),
        )
        .expect("clean MSFT");
    let findings = manager.scan(ts(), 60);
    assert_eq!(findings.len(), 2, "one crossed, one stale, none for MSFT");
    assert_eq!(findings[0].symbol, "AAPL");
    assert_eq!(findings[1].symbol, "SPY");
    assert!(findings[0].reason.contains("stale"));
    assert!(findings[1].reason.contains("crossed"));
    // Record-only: sweeping changes nothing observable.
    assert_eq!(manager.len(), 3);
    assert!(manager.book("MSFT").expect("book").top().is_some());
    // Findings serialize for the journal; keys never appear in them.
    let json = serde_json::to_string(&findings).expect("findings serialize");
    assert!(!json.contains("key"), "no key material in journal rows");
}

#[test]
fn book_key_round_trips_through_index() {
    let mut index = SymbolIndex::new();
    let book = DepthBook::new("SPY".to_owned(), &mut index).expect("intern");
    assert_eq!(book.symbol_key(&mut index).expect("re-intern"), book.key());
    assert_eq!(index.resolve(book.key()), Some("SPY"));
}

#[test]
fn validate_bounds() {
    assert!(Validate::validate(&book()).is_ok());
    assert!(Validate::validate(&BookManager::new()).is_ok());
    let narrow = DepthBook::new("SPY".to_owned(), &mut SymbolIndex::new())
        .expect("intern")
        .with_max_levels(0);
    assert!(Validate::validate(&narrow).is_err());
}
