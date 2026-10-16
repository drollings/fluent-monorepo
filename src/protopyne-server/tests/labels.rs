//! The toolbar-label bridge agrees everywhere: `timeframe_of_label` is the
//! one bridge's serving-boundary name, so it must resolve exactly what
//! `Timeframe::from_chart_label` resolves — toolbar labels, resolution
//! slang, and unknown spellings alike.

use super::*;
use protopyne_types::Timeframe;

#[test]
fn the_label_bridge_matches_the_one_bridge_on_every_spelling() {
    for label in [
        "1M", "5M", "15M", "1H", "4H", "1D", "D", "60", "1W", "5S", "10",
    ] {
        assert_eq!(
            timeframe_of_label(label),
            Timeframe::from_chart_label(label),
            "{label} resolves the same on both spellings of the bridge"
        );
    }
    assert_eq!(timeframe_of_label("1M"), Some(Timeframe::M1));
    assert_eq!(timeframe_of_label("5M"), Some(Timeframe::M5));
    assert_eq!(timeframe_of_label("15M"), Some(Timeframe::M15));
    assert_eq!(timeframe_of_label("1H"), Some(Timeframe::H1));
    assert_eq!(timeframe_of_label("4H"), Some(Timeframe::H4));
    assert_eq!(timeframe_of_label("1D"), Some(Timeframe::D1));
    for unknown in ["10M", "3M", "1m", "", "  ", "XYZ"] {
        assert_eq!(
            timeframe_of_label(unknown),
            None,
            "{unknown:?} is no timeframe on either spelling"
        );
    }
}
