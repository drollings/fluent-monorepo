use super::*;
use protopyne_chart_model::DateRange;

#[test]
fn mtf_shape_and_toolbar_labels_come_from_canonical_lists() {
    let rows = vec![
        MtfRow { timeframe: "M15".to_owned(), bias: "bull".to_owned(), strength: 120.0 },
        MtfRow { timeframe: "H1".to_owned(), bias: "flat".to_owned(), strength: 10.0 },
    ];
    assert_eq!(mtf_table_shape(&rows), (3, 2));
    assert_eq!(mtf_table_shape(&[]), (3, 0));
    let labels = toolbar_labels();
    let expected: Vec<&str> =
        DateRange::default_presets().iter().map(|preset| preset.label()).collect();
    assert_eq!(labels, expected);
    assert!(labels.contains(&"1M"));
}

#[test]
fn timeframe_labels_follow_key_order() {
    assert_eq!(timeframe_labels(), vec!["1M", "5M", "15M", "1H", "4H"]);
}
