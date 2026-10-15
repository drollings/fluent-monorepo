//! Read-plan resolution: what was asked against what is stored.
//!
//! One table over `(catalog, coverage, request)` proving every direction
//! `resolve` can answer. The control group comes first: a request fully
//! inside coverage resolves to exactly the wanted span with no repair
//! implied. Then every refusal — each `PlanError` variant with its case —
//! and the two honest non-errors: a tail past the tape's end resolves with
//! the spans reporting the shortfall, and a coarser spacing resolves to
//! stored-spacing spans for the fold.

use super::*;
use protopyne_types::{Bar, Timeframe};

use crate::commit::test_tapes::{bar, stamp};
use crate::fold::FoldPolicy;
use crate::sqlite_tape::test_setup::{VENUE, memory_tape, seeded, with_memory_caps};
use crate::store::CandleStore;

/// Which tape a case plans against.
#[derive(Clone, Copy)]
enum Tape {
    /// Ten contiguous minute bars, steps 0–9.
    Full,
    /// Steps 0–3 and 6–9: a two-bar interior hole at steps 4–5.
    Holed,
    /// A registered market with no rows.
    Empty,
    /// A three-minute tape, for the non-multiple spacing refusal.
    ThreeMinute,
}

/// What a case must produce.
enum Expect {
    /// Stored spans to fetch, each with its origin and completeness.
    Spans(Vec<(TimeRange, SpanOrigin, bool)>),
    /// A refusal, compared by value.
    Err(PlanError),
}

/// One scripted resolution.
struct Case {
    /// What the case proves.
    name: &'static str,
    /// Which tape to plan against.
    tape: Tape,
    /// Requested symbol.
    symbol: &'static str,
    /// Requested spacing.
    timeframe: Timeframe,
    /// Requested window.
    range: TimeRange,
    /// Bar cap, if the read is open-ended.
    limit: Option<usize>,
    /// Edge treatment for folded reads.
    policy: FoldPolicy,
    /// What resolution must produce.
    expect: Expect,
}

fn ms(step: i64) -> i64 {
    stamp(step).timestamp_millis()
}

fn bounded(start: i64, end: i64) -> TimeRange {
    TimeRange::bounded(ms(start), ms(end)).expect("ordered fixture bounds")
}

fn cases() -> Vec<Case> {
    let full_window = bounded(0, 10);
    vec![
        Case {
            name: "a request fully inside coverage resolves to exactly the wanted span",
            tape: Tape::Full,
            symbol: "SPY",
            timeframe: Timeframe::M1,
            range: bounded(2, 8),
            limit: None,
            policy: FoldPolicy::ExtendToFullBuckets,
            expect: Expect::Spans(vec![(bounded(2, 8), SpanOrigin::Stored, true)]),
        },
        Case {
            name: "no symbol named is no plan",
            tape: Tape::Full,
            symbol: "",
            timeframe: Timeframe::M1,
            range: full_window,
            limit: None,
            policy: FoldPolicy::ExtendToFullBuckets,
            expect: Expect::Err(PlanError::BlankSymbol),
        },
        Case {
            name: "an unregistered symbol refuses with its spelling",
            tape: Tape::Full,
            symbol: "NOPE",
            timeframe: Timeframe::M1,
            range: full_window,
            limit: None,
            policy: FoldPolicy::ExtendToFullBuckets,
            expect: Expect::Err(PlanError::UnknownSymbol("NOPE".to_owned())),
        },
        Case {
            name: "an inverted window refuses with both bounds",
            tape: Tape::Full,
            symbol: "SPY",
            timeframe: Timeframe::M1,
            range: TimeRange {
                start_ms: Some(ms(8)),
                end_ms: Some(ms(2)),
            },
            limit: None,
            policy: FoldPolicy::ExtendToFullBuckets,
            expect: Expect::Err(PlanError::InvertedRange {
                start: ms(8),
                end: ms(2),
            }),
        },
        Case {
            name: "a finer spacing than the tape refuses: unfolding is not a fold",
            tape: Tape::Full,
            symbol: "SPY",
            timeframe: Timeframe::S5,
            range: full_window,
            limit: None,
            policy: FoldPolicy::ExtendToFullBuckets,
            expect: Expect::Err(PlanError::UnsupportedTimeframe {
                symbol: "SPY".to_owned(),
                timeframe: Timeframe::S5,
            }),
        },
        Case {
            name: "a non-multiple spacing refuses: a source bar must nest in exactly one bucket",
            tape: Tape::ThreeMinute,
            symbol: "M3SYM",
            timeframe: Timeframe::M5,
            range: full_window,
            limit: None,
            policy: FoldPolicy::ExtendToFullBuckets,
            expect: Expect::Err(PlanError::UnsupportedTimeframe {
                symbol: "M3SYM".to_owned(),
                timeframe: Timeframe::M5,
            }),
        },
        Case {
            name: "an interior hole refuses with the hole's span, not the request's",
            tape: Tape::Holed,
            symbol: "SPY",
            timeframe: Timeframe::M1,
            range: full_window,
            limit: None,
            policy: FoldPolicy::ExtendToFullBuckets,
            expect: Expect::Err(PlanError::UncoveredRange {
                span: bounded(4, 6),
                want: Timeframe::M1,
            }),
        },
        Case {
            name: "an empty tape refuses: there is no coverage to plan against",
            tape: Tape::Empty,
            symbol: "SPY",
            timeframe: Timeframe::M1,
            range: full_window,
            limit: None,
            policy: FoldPolicy::ExtendToFullBuckets,
            expect: Expect::Err(PlanError::UncoveredRange {
                span: full_window,
                want: Timeframe::M1,
            }),
        },
        Case {
            name: "one bar past the end resolves: the spans end where the coverage ends",
            tape: Tape::Full,
            symbol: "SPY",
            timeframe: Timeframe::M1,
            range: bounded(0, 11),
            limit: None,
            policy: FoldPolicy::ExtendToFullBuckets,
            expect: Expect::Spans(vec![(bounded(0, 10), SpanOrigin::Stored, true)]),
        },
        Case {
            name: "a coarser spacing resolves to stored-spacing spans for the fold",
            tape: Tape::Full,
            symbol: "SPY",
            timeframe: Timeframe::M5,
            range: full_window,
            limit: None,
            policy: FoldPolicy::ExtendToFullBuckets,
            expect: Expect::Spans(vec![(bounded(0, 10), SpanOrigin::Stored, true)]),
        },
        Case {
            name: "diagnostics under truncate-edges report incomplete spans",
            tape: Tape::Full,
            symbol: "SPY",
            timeframe: Timeframe::M5,
            range: full_window,
            limit: None,
            policy: FoldPolicy::TruncateEdges,
            expect: Expect::Spans(vec![(bounded(0, 10), SpanOrigin::Stored, false)]),
        },
    ]
}

#[tokio::test]
async fn resolve_answers_every_case() {
    with_memory_caps(async {
        let full_bars: Vec<Bar> = (0..10).map(|step| bar(step, "100")).collect();
        let (full_tape, full_key) = seeded("SPY", &full_bars).await;
        let full_coverage = full_tape.coverage(full_key).expect("coverage reads");

        let mut holed_bars: Vec<Bar> = (0..4).map(|step| bar(step, "100")).collect();
        holed_bars.extend((6..10).map(|step| bar(step, "100")));
        let (holed_tape, holed_key) = seeded("SPY", &holed_bars).await;
        let holed_coverage = holed_tape.coverage(holed_key).expect("coverage reads");

        let (empty_tape, empty_key) = seeded("SPY", &[]).await;
        let empty_coverage = empty_tape.coverage(empty_key).expect("coverage reads");
        assert_eq!(empty_coverage, None, "the empty tape covers nothing");

        let m3_tape = memory_tape();
        m3_tape
            .register(
                "M3SYM",
                VENUE,
                crate::catalog::AssetClass::Stock,
                crate::catalog::PriceScale::of(2).expect("valid scale"),
                "UTC",
                Timeframe::M3,
            )
            .expect("three-minute market registers");

        for case in cases() {
            let (catalog, coverage) = match case.tape {
                Tape::Full => (full_tape.catalog(), full_coverage.clone()),
                Tape::Holed => (holed_tape.catalog(), holed_coverage.clone()),
                Tape::Empty => (empty_tape.catalog(), empty_coverage.clone()),
                Tape::ThreeMinute => (m3_tape.catalog(), None),
            };
            let request = ReadRequest {
                symbol: case.symbol.to_owned(),
                venue: VENUE.to_owned(),
                timeframe: case.timeframe,
                range: case.range,
                limit: case.limit,
                policy: case.policy,
            };
            match case.expect {
                Expect::Err(expected) => assert_eq!(
                    ReadPlan::resolve(catalog, coverage, &request),
                    Err(expected),
                    "{}",
                    case.name
                ),
                Expect::Spans(expected) => {
                    let plan = ReadPlan::resolve(catalog, coverage, &request).unwrap_or_else(|e| {
                        panic!("{}: expected spans, refused: {e:?}", case.name)
                    });
                    let got: Vec<(TimeRange, SpanOrigin, bool)> = plan
                        .spans()
                        .iter()
                        .map(|span| (span.span(), span.origin(), span.complete()))
                        .collect();
                    assert_eq!(got, expected, "{}", case.name);
                    assert_eq!(plan.timeframe(), case.timeframe, "{}", case.name);
                }
            }
        }
    })
    .await;
}
