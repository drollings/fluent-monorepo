//! Assignment and ex-dividend protection: the controls, the trigger, the
//! boundary, and the calibration record.
//!
//! The calibration shape is the one every trigger in this phase carries: a
//! **control** that must not move (long-only with an ex-div date, a short leg
//! with a far ex-div, unknown events), a **trigger** that must move exactly
//! once (a short leg at the window edge), a **boundary** pinned on both
//! sides, and an **absence** case that is reported — never silent, never a
//! default. The corpus sweep then asserts the shipped default shows zero
//! false fires over generated positions.

use super::*;

use chrono::NaiveDate;
use protopyne_types::chain::LegSide;

/// A calendar day. Tests use `expect` (never in library code) because a
/// test date that fails to parse is a broken test, not a runtime refusal.
fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid test date")
}

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn marked(price: i64) -> MarkContext {
    MarkContext {
        basis: protopyne_types::MarkBasis::ChainMid,
        mark: Some(money(price)),
    }
}

fn absent() -> MarkContext {
    MarkContext {
        basis: protopyne_types::MarkBasis::Absent,
        mark: None,
    }
}

/// One leg over the test underlying, expiring `expiry`.
fn leg(side: LegSide, expiry: NaiveDate) -> LegView {
    LegView {
        side,
        expiry,
        strike: money(100),
        occ: "SPY   260620C00100000".to_owned(),
    }
}

/// A two-lot book over `legs` with `events`. The legs slice is borrowed —
/// the view never owns the book.
fn book(legs: Option<&[LegView]>, events: Option<PositionEvents>) -> PositionView<'_> {
    PositionView {
        underlying: "SPY".to_owned(),
        side: Direction::Long,
        quantity: 2,
        entry_avg: money(250),
        max_loss: money(600),
        dte_days: Some(30),
        legs,
        events,
    }
}

fn events(ex_dividend_on: Option<NaiveDate>, assignment_eligible: bool) -> PositionEvents {
    PositionEvents {
        ex_dividend_on,
        assignment_eligible,
    }
}

fn guard() -> AssignmentGuard {
    AssignmentGuard::new(2, true)
}

// ── Control: a long-only book with an ex-div date never acts ───────────────

#[test]
fn a_long_only_book_with_an_ex_div_date_holds_across_the_whole_window() {
    let expiry = day(2026, 6, 20);
    let long = [leg(LegSide::Long, expiry)];
    for days_out in 0..=2 {
        let ex_div = expiry - chrono::Duration::days(days_out);
        let position = book(Some(&long), Some(events(Some(ex_div), false)));
        assert!(guard().can_judge(&position), "legs and events are known");
        assert!(
            guard().review(&position, &marked(250)).is_empty(),
            "a long book cannot be assigned: ex-div {ex_div} must not move it"
        );
    }
    // Even a venue-flagged assignment window cannot move a long book: there
    // is no short leg to assign.
    let flagged = book(Some(&long), Some(events(Some(expiry), true)));
    assert!(
        guard().review(&flagged, &marked(250)).is_empty(),
        "assignment eligibility without a short leg is not an exit"
    );
}

// ── Control: a short leg with a far ex-div never acts ──────────────────────

#[test]
fn a_short_leg_with_a_far_ex_div_holds() {
    let expiry = day(2026, 6, 20);
    let short = [leg(LegSide::Short, expiry)];
    // A month before expiry: far outside any two-day window.
    let far = book(
        Some(&short),
        Some(events(Some(expiry - chrono::Duration::days(30)), false)),
    );
    assert!(guard().can_judge(&far));
    assert!(
        guard().review(&far, &marked(250)).is_empty(),
        "an ex-div a month from expiry is not this rule's business"
    );
    // After expiry the option is dead: an ex-div past expiry is no risk.
    let past = book(
        Some(&short),
        Some(events(Some(expiry + chrono::Duration::days(5)), false)),
    );
    assert!(
        guard().review(&past, &marked(250)).is_empty(),
        "an ex-div past expiry cannot assign a dead leg"
    );
}

// ── Control: unknown events are reported, never silent and never a default ─

#[test]
fn unknown_events_are_reported_not_silent_and_never_act() {
    let expiry = day(2026, 6, 20);
    let short = [leg(LegSide::Short, expiry)];
    // No calendar at all: the rule cannot judge and must say so.
    let dateless = book(None, None);
    assert!(
        guard().review(&dateless, &marked(250)).is_empty(),
        "without legs and events there is no action"
    );
    assert!(
        !guard().can_judge(&dateless),
        "without legs and events the rule reports it cannot judge"
    );
    // Legs without a calendar: still no judgment.
    let no_calendar = book(Some(&short), None);
    assert!(
        guard().review(&no_calendar, &marked(250)).is_empty(),
        "without events there is no action — never a default window"
    );
    assert!(
        !guard().can_judge(&no_calendar),
        "legs without a calendar are still unjudgeable"
    );
    // Legs and events known: judgeable, even when the verdict is hold.
    let judged = book(
        Some(&short),
        Some(events(Some(expiry - chrono::Duration::days(30)), false)),
    );
    assert!(guard().can_judge(&judged));
}

// ── Trigger: a short leg at the window edge fires once ────────────────────

#[test]
fn a_short_leg_at_the_window_edge_fires_once() {
    let expiry = day(2026, 6, 20);
    let short = [leg(LegSide::Short, expiry)];
    // Exactly two days before expiry: the inclusive outer edge fires.
    let at_edge = book(
        Some(&short),
        Some(events(Some(expiry - chrono::Duration::days(2)), false)),
    );
    let actions = guard().review(&at_edge, &marked(250));
    assert_eq!(actions.len(), 1, "the edge fires exactly once, not per leg");
    assert_eq!(actions[0].kind, "assignment-protection");
    assert!(
        actions[0].reason.contains("2026-06-18"),
        "the reason names the ex-div date: {}",
        actions[0].reason
    );
    // One day further out: outside the window, holds.
    let past_edge = book(
        Some(&short),
        Some(events(Some(expiry - chrono::Duration::days(3)), false)),
    );
    assert!(
        guard().review(&past_edge, &marked(250)).is_empty(),
        "one day past the edge is outside the window"
    );
}

// ── Boundary: the window edge is inclusive; pin both sides ─────────────────

#[test]
fn the_window_edge_is_inclusive_on_both_sides() {
    let expiry = day(2026, 6, 20);
    let short = [leg(LegSide::Short, expiry)];
    // Same-day: the option lives through the ex-div — fires.
    let same_day = book(Some(&short), Some(events(Some(expiry), false)));
    assert_eq!(
        guard().review(&same_day, &marked(250)).len(),
        1,
        "an ex-div on expiry day is inside the window"
    );
    // Outer edge: fires. One day past: holds.
    for (days_out, fires) in [(2, true), (3, false)] {
        let position = book(
            Some(&short),
            Some(events(
                Some(expiry - chrono::Duration::days(days_out)),
                false,
            )),
        );
        assert_eq!(
            guard().review(&position, &marked(250)).len(),
            usize::from(fires),
            "{days_out} days out must {}",
            if fires { "fire" } else { "hold" }
        );
    }
}

// ── The assignment window arm ───────────────────────────────────────────────

#[test]
fn the_assignment_window_arm_fires_only_when_included() {
    let expiry = day(2026, 6, 20);
    let short = [leg(LegSide::Short, expiry)];
    // No ex-div in sight, but the venue flags the assignment window.
    let flagged = book(
        Some(&short),
        Some(events(Some(expiry - chrono::Duration::days(30)), true)),
    );
    let armed = AssignmentGuard::new(2, true).review(&flagged, &marked(250));
    assert_eq!(armed.len(), 1);
    assert_eq!(armed[0].kind, "assignment-protection");
    let disarmed = AssignmentGuard::new(2, false).review(&flagged, &marked(250));
    assert!(
        disarmed.is_empty(),
        "without the assignment window the flag is not this rule's business"
    );
    // An unflagged book far from any ex-div holds under both settings.
    let quiet = book(
        Some(&short),
        Some(events(Some(expiry - chrono::Duration::days(30)), false)),
    );
    assert!(AssignmentGuard::new(2, true)
        .review(&quiet, &marked(250))
        .is_empty());
    assert!(AssignmentGuard::new(2, false)
        .review(&quiet, &marked(250))
        .is_empty());
}

// ── A dark chain prices no exit ─────────────────────────────────────────────

#[test]
fn a_dark_chain_holds_the_guard_like_every_rule() {
    let expiry = day(2026, 6, 20);
    let short = [leg(LegSide::Short, expiry)];
    let position = book(
        Some(&short),
        Some(events(Some(expiry - chrono::Duration::days(1)), false)),
    );
    assert!(
        guard().review(&position, &absent()).is_empty(),
        "even inside the window, an absent mark holds — a dark chain prices no exit"
    );
}

// ── Calibration: the shipped default shows zero false fires ────────────────

/// Deterministic generator: a small LCG so the corpus is reproducible
/// without a random-number dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

#[test]
fn the_default_shows_zero_false_fires_across_a_generated_corpus() {
    let shipped = AssignmentGuard::new(AssignmentGuard::DEFAULT_DAYS_BEFORE_EX_DIV, true);
    assert_eq!(
        AssignmentGuard::DEFAULT_DAYS_BEFORE_EX_DIV,
        2,
        "the default covers a weekend plus the session before"
    );
    let mut lcg = Lcg(0x9e37);
    let mut fired_inside = 0;
    for _ in 0..500 {
        let expiry = day(2026, 6, 20) + chrono::Duration::days((lcg.next() % 60) as i64);
        let long = [leg(LegSide::Long, expiry)];
        let short = [leg(LegSide::Short, expiry)];
        // Long-only across a ±10-day ex-div band: never an action.
        let offset = (lcg.next() % 21) as i64 - 10;
        let long_book = book(
            Some(&long),
            Some(events(Some(expiry + chrono::Duration::days(offset)), false)),
        );
        assert!(
            shipped.review(&long_book, &marked(250)).is_empty(),
            "long book with ex-div {offset:+}d from expiry must hold"
        );
        // Short with the ex-div far outside the window: never an action.
        let far = if lcg.next() % 2 == 0 {
            expiry - chrono::Duration::days(15 + (lcg.next() % 60) as i64)
        } else {
            expiry + chrono::Duration::days(1 + (lcg.next() % 60) as i64)
        };
        let short_far = book(Some(&short), Some(events(Some(far), false)));
        assert!(
            shipped.review(&short_far, &marked(250)).is_empty(),
            "short book with ex-div {far} must hold"
        );
        // Short with the ex-div inside the window: fires (the corpus is not
        // vacuous — the rule can move when it should).
        let inside = expiry - chrono::Duration::days((lcg.next() % 3) as i64);
        let short_inside = book(Some(&short), Some(events(Some(inside), false)));
        fired_inside += shipped.review(&short_inside, &marked(250)).len();
    }
    assert!(
        fired_inside > 0,
        "the inside-window corpus must move, or the zero-fire pins prove nothing"
    );
}
