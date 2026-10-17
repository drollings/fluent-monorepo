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
fn display_matches_name() {
    assert_eq!(format!("{}", ChartType::Candles), ChartType::Candles.name());
    assert_eq!(format!("{}", ChartType::Heikin), "Heikin Ashi");
}
