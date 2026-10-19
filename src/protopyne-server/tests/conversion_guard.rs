//! Single display-conversion guard: the HTTP/chart layer converts `Decimal`
//! to display `f64` only through `protopyne_chart_model::to_display_float`,
//! never a scattered `.to_f64()` or `as f64`. Display floats must leave
//! through one fence so the route and the chart model cannot disagree about
//! what a price prints as — and so a display value never quietly re-enters a
//! decision.

const VIEWS: &str = include_str!("../src/views.rs");
const ROUTES: &str = include_str!("../src/routes/mod.rs");

#[test]
fn display_float_has_one_fence() {
    for (name, source) in [("views.rs", VIEWS), ("routes/mod.rs", ROUTES)] {
        assert!(
            !source.contains(".to_f64("),
            "{name} must not call `.to_f64()`; route display conversion through `to_display_float`"
        );
        assert!(
            !source.contains("as f64"),
            "{name} must not cast `as f64`; route display conversion through `to_display_float`"
        );
    }
    assert!(
        VIEWS.contains("protopyne_chart_model::to_display_float"),
        "views.rs is the one display fence and must delegate to `to_display_float`"
    );
}
