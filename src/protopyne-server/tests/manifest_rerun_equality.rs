//! Rerun equality over tape data: the same tape and the same config pin
//! byte-identical JSON including the digest, and a different coverage
//! digests differently.
//!
//! Plans resolve against a real in-memory tape, so the provenance the
//! digest covers is planner output rather than a hand-built stand-in: the
//! same bars served the same way digest the same way, fewer bars digest
//! differently, and the same bars served with different edge treatment
//! digest differently too. The digest identifies the exact bytes; the
//! manifest equality is what makes a rerun prove it changed nothing.

use super::*;
use chrono::DateTime;
use protopyne_store::{
    AssetClass, CandleStore as _, FoldPolicy, PriceScale, ReadPlan, ReadRequest, SpanPlan,
    SqliteTape, StoreStorage, TapeKey, TimeRange,
};
use protopyne_types::{Bar, Timeframe};

const VENUE: &str = "test-venue";

/// Five flat minute bars from a fixed session stamp: contiguous, whole,
/// and two-decimal so the seeded market stores every row.
fn five_bars() -> Vec<Bar> {
    let base_ms = 1_767_974_400_000;
    (0..5)
        .map(|step| {
            Bar::new()
                .ts(DateTime::from_timestamp_millis(base_ms + step * 60_000).expect("in range"))
                .o("100".parse().expect("decimal"))
                .h("100".parse().expect("decimal"))
                .l("100".parse().expect("decimal"))
                .c("100".parse().expect("decimal"))
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

/// Seed `bars` on an ephemeral tape and resolve `request` against it,
/// returning the owned bars plus the plan's spans. Everything capability-
/// gated runs inside the installed scope; only owned facts leave it.
async fn plan_spans(bars: &[Bar], request: &ReadRequest) -> (Vec<Bar>, Vec<SpanPlan>) {
    let caps = fluent_wvr::CapabilitySet::new().with(StoreStorage::new(":memory:".into()));
    fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            let tape = SqliteTape::open_in_memory().expect("ephemeral tape opens");
            let symbol_id = tape
                .register(
                    "SPY",
                    VENUE,
                    AssetClass::Stock,
                    PriceScale::of(2).expect("valid scale"),
                    "UTC",
                    Timeframe::M1,
                )
                .expect("registration succeeds");
            let key = TapeKey {
                symbol_id,
                timeframe: Timeframe::M1,
            };
            tape.append(key, bars, protopyne_store::WritePolicy::InsertIfAbsent)
                .expect("seed append succeeds");
            let coverage = tape.coverage(key).expect("coverage reads");
            let plan = ReadPlan::resolve(tape.catalog(), coverage, request).expect("plan resolves");
            (bars.to_vec(), plan.spans().to_vec())
        })
        .await
}

fn direct_request(start_ms: i64, end_ms: i64) -> ReadRequest {
    ReadRequest {
        symbol: "SPY".to_owned(),
        venue: VENUE.to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange::bounded(start_ms, end_ms).expect("ordered bounds"),
        limit: None,
        policy: FoldPolicy::ExtendToFullBuckets,
    }
}

fn pinned(bars: &[Bar], spans: &[SpanPlan]) -> RunManifest {
    RunManifest::new("tape-v1", "{\"warmup\":0}", bars, 0).with_tape_data(bars, spans)
}

#[tokio::test]
async fn same_tape_same_config_reruns_identically() {
    let bars = five_bars();
    let first = bars[0].ts.timestamp_millis();
    let request = direct_request(first, first + 300_000);
    let (bars_a, spans_a) = plan_spans(&bars, &request).await;
    let (bars_b, spans_b) = plan_spans(&bars, &request).await;
    let first_run = pinned(&bars_a, &spans_a);
    let second_run = pinned(&bars_b, &spans_b);
    assert_eq!(
        first_run.to_canonical_json(),
        second_run.to_canonical_json(),
        "the same tape and config must pin byte-identical JSON"
    );
    let digest = first_run.data_digest.expect("tape runs carry a digest");
    assert_eq!(digest.len(), 16, "the digest is sixteen hex chars");
    assert_eq!(first_run.data_bar_count, Some(5));
    assert_eq!(
        first_run
            .data_provenance
            .expect("tape runs carry provenance")
            .len(),
        1,
        "one contiguous run plans one span"
    );
}

#[tokio::test]
async fn different_coverage_digests_differently() {
    let bars = five_bars();
    let first = bars[0].ts.timestamp_millis();
    let (_, full_spans) = plan_spans(&bars, &direct_request(first, first + 300_000)).await;
    let short = &bars[..4];
    let (_, short_spans) = plan_spans(short, &direct_request(first, first + 240_000)).await;
    let full = pinned(&bars, &full_spans);
    let partial = pinned(short, &short_spans);
    assert_ne!(
        full.data_digest, partial.data_digest,
        "one fewer bar is a different dataset and must digest differently"
    );
    assert_ne!(
        full.to_canonical_json(),
        partial.to_canonical_json(),
        "the rerun must show the coverage change in its bytes"
    );
}

#[tokio::test]
async fn same_bars_different_edges_digest_differently() {
    let bars = five_bars();
    let first = bars[0].ts.timestamp_millis();
    let (_, extended) = plan_spans(
        &bars,
        &ReadRequest {
            policy: FoldPolicy::ExtendToFullBuckets,
            ..direct_request(first, first + 300_000)
        },
    )
    .await;
    let folded_request = ReadRequest {
        symbol: "SPY".to_owned(),
        venue: VENUE.to_owned(),
        timeframe: Timeframe::M5,
        range: TimeRange::bounded(first + 60_000, first + 300_000).expect("ordered bounds"),
        limit: None,
        policy: FoldPolicy::TruncateEdges,
    };
    let (_, truncated) = plan_spans(&bars, &folded_request).await;
    assert!(
        truncated.iter().any(|span| !span.complete()),
        "the truncated fold must flag its partial span"
    );
    assert_ne!(
        pinned(&bars, &extended).data_digest,
        pinned(&bars, &truncated).data_digest,
        "provenance is covered: the same bars served differently digest differently"
    );
}
