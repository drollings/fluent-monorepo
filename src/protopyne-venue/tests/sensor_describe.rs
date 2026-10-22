use crate::{
    BackfillHandle, BackfillUnit, JournalBook, Position, ReconcileHandle, ReconcileSource,
    ReconcileUnit, StateHandle, StreamBreaker, StreamConfig, StreamHealthUnit, TradingState,
    TradingStateUnit, BACKFILL_ASSET, RECONCILE_ASSET, STREAM_HEALTH_ASSET, TRADING_ENABLED_ASSET,
};
use fluent_wvr::{Describable, FieldAccess, WorkUnit};
use protopyne_types::{Bar, GapPolicy, SeriesContext, Timeframe};
use std::collections::HashMap;
use std::sync::Arc;

struct EmptyBackfill;
impl crate::BackfillSource for EmptyBackfill {
    fn fetch(&self, _start_ms: i64, _end_ms: i64) -> Result<Vec<Bar>, crate::VenueError> {
        Ok(Vec::new())
    }
}

struct EmptyReconcile;
impl ReconcileSource for EmptyReconcile {
    fn journal(&self) -> JournalBook {
        JournalBook {
            net: HashMap::new(),
            legs: HashMap::new(),
        }
    }
    fn venue_positions(&self) -> Vec<Position> {
        Vec::new()
    }
    fn venue_legs(&self) -> HashMap<String, i64> {
        HashMap::new()
    }
}

fn backfill_unit() -> BackfillUnit {
    let ctx = SeriesContext::new("SPY", Timeframe::M1, GapPolicy::Dense);
    BackfillUnit::new(ctx, vec![Arc::new(EmptyBackfill)], BackfillHandle::new(), 1)
}

#[test]
fn sensor_describes_use_canonical_shape() {
    let backfill = backfill_unit();
    assert_eq!(backfill.name(), "backfill.sensor");
    assert!(backfill.depends().is_empty());
    assert_eq!(
        backfill.provides().iter().map(|a| &**a).collect::<Vec<_>>(),
        vec![BACKFILL_ASSET]
    );
    assert_eq!(
        backfill.describe(),
        serde_json::json!({
            "name": "backfill.sensor",
            "depends": [],
            "provides": [BACKFILL_ASSET],
        })
    );
    assert!(backfill.describe().get("type").is_none());
    assert!(backfill.describe().get("description").is_none());
    assert!(backfill.field_names().is_empty());

    let reconcile = ReconcileUnit::new(Arc::new(EmptyReconcile), ReconcileHandle::new());
    assert_eq!(reconcile.name(), "reconcile.sensor");
    assert_eq!(
        reconcile.describe(),
        serde_json::json!({
            "name": "reconcile.sensor",
            "depends": [],
            "provides": [RECONCILE_ASSET],
        })
    );
    assert!(reconcile.field_names().is_empty());

    let breaker = Arc::new(StreamBreaker::new(StreamConfig::new().build()));
    let stream = StreamHealthUnit::new(breaker);
    assert_eq!(stream.name(), "stream.health.sensor");
    assert_eq!(
        stream.describe(),
        serde_json::json!({
            "name": "stream.health.sensor",
            "depends": [],
            "provides": [STREAM_HEALTH_ASSET],
        })
    );
    assert!(stream.field_names().is_empty());

    let trading = TradingStateUnit::new(StateHandle::new(TradingState::Enabled));
    assert_eq!(trading.name(), "trading.state.sensor");
    assert_eq!(
        trading.describe(),
        serde_json::json!({
            "name": "trading.state.sensor",
            "depends": [],
            "provides": [TRADING_ENABLED_ASSET],
        })
    );
    assert!(trading.field_names().is_empty());
}
