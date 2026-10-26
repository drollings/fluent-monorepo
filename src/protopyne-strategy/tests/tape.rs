//! Bounds and cursor behaviour of the point-in-time tape.

use chrono::{TimeZone as _, Utc};
use protopyne_kernels::lite::{lite_legs, LiteLegs, LiteParams};
use protopyne_types::Bar;
use rust_decimal::Decimal;

use crate::tape::Tape;

fn bars(len: usize) -> Vec<Bar> {
    (0..len)
        .map(|index| {
            let close = 100.0 + index as f64;
            let value = Decimal::from_f64_retain(close).expect("finite");
            Bar::new()
                .ts(Utc
                    .with_ymd_and_hms(2026, 1, 5, 15, 0, 0)
                    .single()
                    .expect("valid time")
                    + chrono::Duration::minutes(index as i64))
                .o(value)
                .h(Decimal::from_f64_retain(close + 0.5).expect("finite"))
                .l(Decimal::from_f64_retain(close - 0.5).expect("finite"))
                .c(value)
                .volume(10_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn closes(len: usize) -> Vec<f64> {
    (0..len).map(|index| 100.0 + index as f64).collect()
}

fn batch(bars: &[Bar]) -> LiteLegs {
    lite_legs(bars, &LiteParams::default(), 14).expect("legs batch")
}

#[test]
fn a_bar_past_the_cursor_is_invisible() {
    let bars = bars(6);
    let legs = batch(&bars);
    let closes = closes(6);
    let tape = Tape::new(&bars, &legs, &closes, 3).expect("bounded tape");
    assert_eq!(tape.at(), 3);
    assert!(tape.bar(3).is_some());
    assert!(
        tape.bar(4).is_none(),
        "the next bar is not visible through the cursor"
    );
    assert!(tape.bar(usize::MAX).is_none());
    assert_eq!(tape.close(3), Some(103.0));
    assert_eq!(tape.close(4), None);
}

#[test]
fn the_window_is_exactly_the_prefix() {
    let bars = bars(5);
    let legs = batch(&bars);
    let closes = closes(5);
    let tape = Tape::new(&bars, &legs, &closes, 2).expect("bounded tape");
    assert_eq!(tape.window(), &bars[..=2]);
    assert_eq!(tape.closes(), &closes[..=2]);
}

#[test]
fn advancing_moves_one_bar_without_re_slicing() {
    let bars = bars(4);
    let legs = batch(&bars);
    let closes = closes(4);
    let mut tape = Tape::new(&bars, &legs, &closes, 0).expect("bounded tape");
    let mut seen = 0;
    loop {
        assert_eq!(tape.window().len(), tape.at() + 1);
        seen += 1;
        match tape.next() {
            Some(next) => tape = next,
            None => break,
        }
    }
    assert_eq!(seen, bars.len(), "the cursor walks every bar exactly once");
    assert!(tape.next().is_none(), "the end refuses to advance");
}

#[test]
fn construction_refuses_a_window_that_cannot_line_up() {
    let bars = bars(4);
    let legs = batch(&bars);
    let closes = closes(4);
    assert!(
        Tape::new(&bars, &legs, &closes, 4).is_none(),
        "an index past the last bar refuses"
    );
    let short = batch(&bars[..2]);
    assert!(
        Tape::new(&bars, &short, &closes, 1).is_none(),
        "a legs batch shorter than the bars refuses"
    );
    assert!(
        Tape::new(&bars, &legs, &closes[..2], 1).is_none(),
        "a closes window shorter than the bars refuses"
    );
}

#[test]
fn the_legs_are_the_batch_the_caller_supplied() {
    let bars = bars(4);
    let legs = batch(&bars);
    let closes = closes(4);
    let tape = Tape::new(&bars, &legs, &closes, 2).expect("bounded tape");
    assert_eq!(tape.legs().at(2), legs.at(2));
}
