use super::*;

#[test]
fn catalog_covers_twenty_views() {
    assert_eq!(ChartType::all().len(), 20);
}

#[test]
fn default_view_is_candles() {
    assert_eq!(ChartType::default(), ChartType::Candles);
}

#[test]
fn families_group_correctly() {
    let standard = ChartType::in_category(ChartTypeCategory::Standard);
    assert!(standard.contains(&ChartType::Candles));
    assert!(standard.contains(&ChartType::Bars));
    assert!(!standard.contains(&ChartType::Line));
    assert_eq!(ChartTypeCategory::all().len(), 6);
}

#[test]
fn ohlc_membership_splits_on_close_only_views() {
    assert!(ChartType::Candles.uses_ohlc());
    assert!(!ChartType::Line.uses_ohlc());
    assert!(!ChartType::Area.uses_ohlc());
}

#[test]
fn time_independence_marks_price_only_views() {
    assert!(ChartType::Renko.is_time_independent());
    assert!(ChartType::Kagi.is_time_independent());
    assert!(!ChartType::Candles.is_time_independent());
}

#[test]
fn parameter_rule_names_param_views() {
    assert!(ChartType::Renko.requires_parameters());
    assert!(ChartType::Baseline.requires_parameters());
    assert!(!ChartType::Candles.requires_parameters());
}

#[test]
fn transform_rule_names_reshaping_views() {
    assert!(ChartType::Heikin.transforms_data());
    assert!(ChartType::Renko.transforms_data());
    assert!(!ChartType::Candles.transforms_data());
}

#[test]
fn display_matches_name() {
    assert_eq!(format!("{}", ChartType::Candles), ChartType::Candles.name());
    assert_eq!(format!("{}", ChartType::Heikin), "Heikin Ashi");
}
