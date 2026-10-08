use super::*;

#[test]
fn at_zero_is_current_and_positive_offsets_walk_back() {
    let values = [10, 20, 30, 40];
    let series = SliceSeries::new(&values, 3);
    assert_eq!(series.at(0), Some(&40));
    assert_eq!(series.at(1), Some(&30));
    assert_eq!(series.at(3), Some(&10));
    assert_eq!(series.cursor(), 3);
}

#[test]
fn out_of_range_is_na_never_panic() {
    let empty: &[i32] = &[];
    assert_eq!(SliceSeries::new(empty, 0).at(0), None);
    let values = [1, 2];
    assert_eq!(SliceSeries::new(&values, 99).at(0), None);
    assert_eq!(SliceSeries::new(&values, 0).at(1), None);
}
