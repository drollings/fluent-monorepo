//! The fold-calibration negative control: truncating the edges must report
//! partial buckets and must differ from the complete-bucket fold.
//!
//! Without this, a regression to truncate-by-default would pass every
//! positive check: the positive suite pins what the extend policy produces,
//! and only this suite pins that the truncate policy produces something
//! else. The difference is the finding, not a bug.

use super::*;

use crate::fold::test_fold::{load_goldens, load_spy, seed_spy_slice};
use crate::fold::FoldPolicy;
use crate::plan::SpanOrigin;
use crate::range::TimeRange;
use crate::store::{CandleStore, ReadRequest};
use crate::tape::test_setup::{with_registry_caps, VENUE};

#[tokio::test]
async fn truncate_edges_reports_partial_and_differs() {
    with_registry_caps(async {
        let goldens = load_goldens();
        let minute = load_spy(&goldens.complete_run.fixture);
        let slice = &minute[..10];
        let (tape, _) = seed_spy_slice("SPY", slice).await;
        let bucket_ms = slice[0].ts.timestamp_millis();
        let window = TimeRange::bounded(bucket_ms + 120_000, bucket_ms + 600_000)
            .expect("ordered fixture bounds");
        let request = |policy| ReadRequest {
            symbol: "SPY".to_owned(),
            venue: VENUE.to_owned(),
            timeframe: Timeframe::M5,
            range: window,
            limit: None,
            policy,
        };
        let whole = tape
            .read(&request(FoldPolicy::ExtendToFullBuckets))
            .expect("the extend arm serves");
        let truncated = tape
            .read(&request(FoldPolicy::TruncateEdges))
            .expect("the truncate arm serves");
        assert_eq!(
            truncated.bars.len(),
            whole.bars.len(),
            "both policies answer the requested window"
        );
        assert!(
            truncated.provenance.iter().any(|span| !span.complete()),
            "a truncated edge reports partial: {:?}",
            truncated.provenance
        );
        assert_ne!(
            truncated.bars[0], whole.bars[0],
            "the truncated first bucket is built from fewer prints"
        );
        assert!(
            truncated
                .provenance
                .iter()
                .all(|span| span.origin() == SpanOrigin::Stored),
            "the difference is in the buckets, not the source"
        );
        assert!(
            !whole.provenance.iter().any(|span| !span.complete()),
            "the extend arm stays the whole-bucket reference: {:?}",
            whole.provenance
        );
    })
    .await;
}
