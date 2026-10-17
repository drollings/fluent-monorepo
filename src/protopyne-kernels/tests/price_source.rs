use crate::indicator::{BarTick, OwnedOhlcv};
use crate::price_source::PriceSource;
use crate::{InputDef, InputSet, PineValue};

fn frame_fixture() -> OwnedOhlcv {
    OwnedOhlcv {
        open: vec![10.0, 11.0, 12.0, 13.0, 14.0],
        high: vec![12.0, 13.0, 14.0, 15.0, 16.0],
        low: vec![8.0, 9.0, 10.0, 11.0, 12.0],
        close: vec![11.0, 12.0, 11.5, 14.0, 13.0],
        volume: vec![100.0; 5],
    }
}

#[test]
fn registry_covers_documented_formulas_byte_exact() {
    let frame = frame_fixture();
    let close = PriceSource::Close.column_of(&frame);
    assert_eq!(close, frame.close);
    let open = PriceSource::Open.column_of(&frame);
    assert_eq!(open, frame.open);
    let high = PriceSource::High.column_of(&frame);
    assert_eq!(high, frame.high);
    let low = PriceSource::Low.column_of(&frame);
    assert_eq!(low, frame.low);
    let hl2 = PriceSource::Hl2.column_of(&frame);
    let want_hl2: Vec<f64> = vec![10.0, 11.0, 12.0, 13.0, 14.0];
    assert_eq!(hl2, want_hl2);
    let hlc3 = PriceSource::Hlc3.column_of(&frame);
    let want_hlc3: Vec<f64> = vec![
        (12.0 + 8.0 + 11.0) / 3.0,
        (13.0 + 9.0 + 12.0) / 3.0,
        (14.0 + 10.0 + 11.5) / 3.0,
        (15.0 + 11.0 + 14.0) / 3.0,
        (16.0 + 12.0 + 13.0) / 3.0,
    ];
    assert_eq!(hlc3, want_hlc3);
    let ohlc4 = PriceSource::Ohlc4.column_of(&frame);
    let want_ohlc4: Vec<f64> = vec![
        (10.0 + 12.0 + 8.0 + 11.0) / 4.0,
        (11.0 + 13.0 + 9.0 + 12.0) / 4.0,
        (12.0 + 14.0 + 10.0 + 11.5) / 4.0,
        (13.0 + 15.0 + 11.0 + 14.0) / 4.0,
        (14.0 + 16.0 + 12.0 + 13.0) / 4.0,
    ];
    assert_eq!(ohlc4, want_ohlc4);
    let wcl = PriceSource::Wcl.column_of(&frame);
    let want_wcl: Vec<f64> = vec![
        (12.0 + 8.0 + 2.0 * 11.0) / 4.0,
        (13.0 + 9.0 + 2.0 * 12.0) / 4.0,
        (14.0 + 10.0 + 2.0 * 11.5) / 4.0,
        (15.0 + 11.0 + 2.0 * 14.0) / 4.0,
        (16.0 + 12.0 + 2.0 * 13.0) / 4.0,
    ];
    assert_eq!(wcl, want_wcl);
    // `avgprice` kernel spelling is the same average as `ohlc4`.
    assert_eq!(ohlc4, want_ohlc4);
}

#[test]
fn per_bar_scalars_match_column_cells() {
    let frame = frame_fixture();
    for source in PriceSource::all() {
        let column = source.column_of(&frame);
        assert_eq!(column.len(), 5);
        for (i, cell) in column.iter().enumerate() {
            let bar = BarTick {
                open: frame.open[i],
                high: frame.high[i],
                low: frame.low[i],
                close: frame.close[i],
                volume: frame.volume[i],
            };
            let scalar = source.compute_bar(bar);
            if cell.is_nan() {
                assert!(scalar.is_nan(), "{source:?} row {i} hole preserved");
            } else {
                assert!(
                    cell.to_bits() == scalar.to_bits(),
                    "{source:?} row {i} scalar agreement"
                );
            }
        }
    }
}

#[test]
fn columns_are_length_and_hole_preserving() {
    // Deterministic pseudo-random frames with injected NaN holes.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state as f64) / (u64::MAX as f64)
    };
    for len in [0, 1, 7, 64] {
        let mut frame = OwnedOhlcv {
            open: (0..len).map(|_| next() * 100.0).collect(),
            high: (0..len).map(|_| next() * 100.0).collect(),
            low: (0..len).map(|_| next() * 100.0).collect(),
            close: (0..len).map(|_| next() * 100.0).collect(),
            volume: vec![1.0; len],
        };
        for leg in [&mut frame.open, &mut frame.high, &mut frame.low, &mut frame.close] {
            let leg_len = leg.len();
            for (i, cell) in leg.iter_mut().enumerate() {
                if (i * 7 + leg_len) % 5 == 0 {
                    *cell = f64::NAN;
                }
            }
        }
        for source in PriceSource::all() {
            let column = source.column_of(&frame);
            assert_eq!(column.len(), len, "{source:?} length preserved");
            // Any NaN input leg poisons only derived rows touching it.
            for (i, cell) in column.iter().enumerate() {
                let bar_nan = [frame.open[i], frame.high[i], frame.low[i], frame.close[i]]
                    .iter()
                    .any(|v| v.is_nan());
                let touches_nan = match source {
                    PriceSource::Open => frame.open[i].is_nan(),
                    PriceSource::High => frame.high[i].is_nan(),
                    PriceSource::Low => frame.low[i].is_nan(),
                    PriceSource::Close => frame.close[i].is_nan(),
                    PriceSource::Hl2 => frame.high[i].is_nan() || frame.low[i].is_nan(),
                    _ => bar_nan,
                };
                if touches_nan {
                    assert!(cell.is_nan(), "{source:?} row {i} hole preserved");
                }
            }
        }
    }
}

#[test]
fn names_parse_round_trip_and_reject_unknown() {
    for source in PriceSource::all() {
        let parsed = PriceSource::parse(source.name()).expect("canonical name parses");
        assert_eq!(parsed, source);
        assert_eq!(parsed.pine_leg(), source.name());
    }
    assert!(PriceSource::parse("no-such-source").is_none());
    assert!(PriceSource::parse("").is_none());
    // Close is the default source.
    assert_eq!(PriceSource::default(), PriceSource::Close);
}

#[test]
fn price_source_input_declares_and_refuses_unknown() {
    let mut inputs = InputSet::new();
    inputs
        .declare("src", InputDef::PriceSource { default: "close".to_owned() })
        .expect("known source declares");
    assert!(inputs
        .declare("bad", InputDef::PriceSource { default: "nope".to_owned() })
        .is_err());
    assert!(inputs.set("src", PineValue::Str("hlc3".to_owned())).is_ok());
    assert!(inputs.set("src", PineValue::Str("nope".to_owned())).is_err());
    assert!(inputs.set("src", PineValue::Float(1.0)).is_err());
}

#[cfg(feature = "pine")]
#[test]
fn pine_legs_agree_with_registry_on_spy_trim() {
    use std::path::PathBuf;
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/SPY_15M.csv");
    let content = std::fs::read_to_string(&path).expect("trim present");
    let mut bars = Vec::new();
    for line in content.lines().skip(1) {
        let mut cells = line.split(',');
        let secs: i64 = cells.next().expect("time").parse().expect("epoch");
        let num = |cell: Option<&str>| cell.expect("leg").parse::<f64>().expect("number");
        let (o, h, l, c) = (num(cells.next()), num(cells.next()), num(cells.next()), num(cells.next()));
        let ts = chrono::DateTime::from_timestamp(secs, 0).expect("valid epoch");
        bars.push(
            protopyne_types::Bar::new()
                .ts(ts)
                .o(rust_decimal::Decimal::from_f64_retain(o).expect("finite"))
                .h(rust_decimal::Decimal::from_f64_retain(h).expect("finite"))
                .l(rust_decimal::Decimal::from_f64_retain(l).expect("finite"))
                .c(rust_decimal::Decimal::from_f64_retain(c).expect("finite"))
                .volume(1)
                .maybe_adj(None)
                .build(),
        );
    }
    let frame = OwnedOhlcv::from_bars(&bars);
    let legs = crate::pine::eval::legs_of_for_test(&bars);
    for source in PriceSource::all() {
        let leg = legs.named_for_test(source.pine_leg()).expect("leg exists");
        let column = source.column_of(&frame);
        assert_eq!(leg.len(), column.len());
        for (a, b) in leg.iter().zip(column.iter()) {
            if a.is_nan() || b.is_nan() {
                assert!(a.is_nan() && b.is_nan(), "{source:?} hole agreement");
            } else {
                assert!((a - b).abs() <= 1e-12, "{source:?} pine agreement");
            }
        }
    }
}
