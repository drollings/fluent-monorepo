use super::*;

#[test]
fn wraps_and_unwraps_a_millisecond_count() {
    let stamp = EpochMs::from(1_700_000_000_123_i64);
    assert_eq!(stamp.get(), 1_700_000_000_123);
    assert_eq!(i64::from(stamp), 1_700_000_000_123);
}

#[test]
fn orders_numerically() {
    let mut stamps = vec![EpochMs::from(3), EpochMs::from(-1), EpochMs::from(2)];
    stamps.sort();
    assert_eq!(
        stamps,
        vec![EpochMs::from(-1), EpochMs::from(2), EpochMs::from(3)]
    );
}

#[test]
fn checked_sub_reports_distance() {
    let later = EpochMs::from(1_000);
    let earlier = EpochMs::from(250);
    assert_eq!(later.checked_sub(earlier), Some(750));
    assert_eq!(earlier.checked_sub(later), Some(-750));
}

#[test]
fn checked_sub_is_none_out_of_range() {
    assert_eq!(EpochMs::from(i64::MIN).checked_sub(EpochMs::from(1)), None);
}

#[test]
fn seconds_scale_to_milliseconds() {
    assert_eq!(
        EpochMs::from_seconds_f64(1_700_000_000.0),
        Some(EpochMs::from(1_700_000_000_000))
    );
}

#[test]
fn absent_clocks_read_as_none() {
    assert_eq!(EpochMs::from_seconds_f64(f64::NAN), None);
    assert_eq!(EpochMs::from_millis_f64(f64::NAN), None);
    assert_eq!(EpochMs::from_seconds_f64(f64::INFINITY), None);
    assert_eq!(EpochMs::from_millis_f64(1e300), None);
}

#[test]
fn millisecond_floats_truncate_to_whole_milliseconds() {
    assert_eq!(
        EpochMs::from_millis_f64(1_500.75),
        Some(EpochMs::from(1_500))
    );
}
