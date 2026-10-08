use crate::research::{frame_of_bars, frame_of_values};
use protopyne_types::Bar;

fn sample_bars() -> Vec<Bar> {
    use chrono::TimeZone as _;
    (0..4)
        .map(|i| {
            Bar::new()
                .ts(chrono::Utc
                    .with_ymd_and_hms(2026, 1, 2, 9, i * 15, 0)
                    .single()
                    .expect("valid timestamp"))
                .o("100.00".parse().expect("decimal"))
                .h("101.00".parse().expect("decimal"))
                .l("99.00".parse().expect("decimal"))
                .c("100.50".parse().expect("decimal"))
                .volume(1_000_u64)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

#[test]
fn bars_frame_carries_ohlcv_columns() {
    let frame = frame_of_bars(&sample_bars()).expect("frame builds");
    assert_eq!(frame.height(), 4);
    assert_eq!(
        frame.get_column_names(),
        vec!["ts", "open", "high", "low", "close", "volume"]
    );
    let close = frame.column("close").expect("close column");
    assert_eq!(close.f64().expect("float").get(0), Some(100.50));
}

#[test]
fn values_frame_preserves_nulls() {
    let frame = frame_of_values("sma", &[None, None, Some(2.0)]).expect("frame builds");
    assert_eq!(frame.height(), 3);
    assert_eq!(frame.column("sma").expect("column").null_count(), 2);
}
