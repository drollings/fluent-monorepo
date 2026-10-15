//! The async handle: pooled, bounded, and gated on both tokens.
//!
//! [`StoreHandle`] serves the same contract as the synchronous core over
//! pooled connections: register once, append idempotently, read back
//! ascending and valid. Two wirings are pinned here because both are easy
//! to get wrong silently. First, the handle's pool calls require a
//! `DbCapability` in the task-local set — "this task may use SQLite at
//! all" — alongside the store permit; a session that installs only the
//! permit is denied, which proves the pool gate is load-bearing rather
//! than decorative. Second, a permit bound to another file is refused on
//! its face, which proves the binding half of the gate.

use super::*;
use protopyne_types::Timeframe;

use crate::commit::test_tapes::at_ms;
use crate::fold::FoldPolicy;
use crate::key::TapeKey;
use crate::range::TimeRange;

fn db_token() -> fluent_db::capability::DbCapability {
    fluent_db::capability::DbCapability::open_with_config(
        ":memory:",
        fluent_db::pool::PoolConfig::new(1),
    )
    .expect("token-only in-memory pool opens")
}

fn file_config(path: &std::path::Path) -> StoreConfig {
    StoreConfig::new()
        .path(path.to_string_lossy().into_owned())
        .build()
}

/// Open a file-backed handle with both tokens installed, and register one
/// market. The caller keeps the permit for the assertions that follow.
async fn opened(dir: &tempfile::TempDir, symbol: &str) -> (StoreHandle, StoreStorage, TapeKey) {
    let path = dir.path().join("store.db");
    let config = file_config(&path);
    let permit = StoreStorage::new(path);
    let caps = fluent_wvr::CapabilitySet::new()
        .with(permit.clone())
        .with(db_token());
    fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            let handle = StoreHandle::open(&config).await.expect("handle opens");
            let symbol_id = handle
                .register(
                    &permit,
                    symbol,
                    "test-venue",
                    AssetClass::Stock,
                    crate::catalog::PriceScale::of(2).expect("valid scale"),
                    "UTC",
                    protopyne_types::Timeframe::M1,
                )
                .await
                .expect("register");
            (
                handle,
                permit,
                TapeKey {
                    symbol_id,
                    timeframe: Timeframe::M1,
                },
            )
        })
        .await
}

/// Both tokens a session installs: the file-bound store permit and the
/// database token the pool's own calls require.
fn both_caps(permit: &StoreStorage) -> fluent_wvr::CapabilitySet {
    fluent_wvr::CapabilitySet::new()
        .with(permit.clone())
        .with(db_token())
}

#[tokio::test]
async fn handle_round_trip_matches_the_sync_core() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (handle, permit, key) = opened(&dir, "SPY").await;
    fluent_wvr::CURRENT_CAPS
        .scope(both_caps(&permit), async {
            let bars: Vec<Bar> = (0..5_i64)
                .map(|step| at_ms(1_767_225_900_000 + step * 60_000))
                .collect();
            let outcome = handle
                .append(&permit, key, &bars, WritePolicy::InsertIfAbsent)
                .await
                .expect("append");
            assert_eq!(outcome.inserted, 5);
            let replay = handle
                .append(&permit, key, &bars, WritePolicy::InsertIfAbsent)
                .await
                .expect("re-delivery");
            assert_eq!(replay.inserted, 0);
            assert_eq!(replay.conflicts, 5);
            let coverage = handle
                .coverage(&permit, key)
                .await
                .expect("coverage")
                .expect("covered");
            assert_eq!(coverage.bars, 5);
            assert_eq!(coverage.last.ts_ms, 1_767_225_900_000 + 4 * 60_000);
            let series = handle
                .read(
                    &permit,
                    &ReadRequest {
                        symbol: "SPY".to_owned(),
                        venue: "test-venue".to_owned(),
                        timeframe: Timeframe::M1,
                        range: TimeRange::unbounded(),
                        limit: Some(5),
                        policy: FoldPolicy::ExtendToFullBuckets,
                    },
                )
                .await
                .expect("read");
            assert_eq!(series.bars.len(), 5);
            assert_eq!(series.key, key);
        })
        .await;
}

#[tokio::test]
async fn missing_db_token_is_denied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (handle, permit, key) = opened(&dir, "SPY").await;
    // Only the store permit, no DbCapability: the pool's own gate fires,
    // surfaced as a denial rather than a database error.
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    let err = fluent_wvr::CURRENT_CAPS
        .scope(caps, async { handle.coverage(&permit, key).await })
        .await
        .expect_err("no database token");
    assert!(
        matches!(err, StoreError::Denied(_)),
        "pool access without the database token is denied: {err:?}"
    );
}

#[tokio::test]
async fn foreign_permit_is_refused_on_its_face() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (handle, _permit, key) = opened(&dir, "SPY").await;
    let foreign = StoreStorage::new("/elsewhere/store.db".into());
    let caps = fluent_wvr::CapabilitySet::new()
        .with(foreign.clone())
        .with(db_token());
    let err = fluent_wvr::CURRENT_CAPS
        .scope(caps, async { handle.coverage(&foreign, key).await })
        .await
        .expect_err("foreign permit");
    assert!(
        matches!(err, StoreError::Denied(_)),
        "a permit bound elsewhere cannot operate this file: {err:?}"
    );
}

#[tokio::test]
async fn invalid_config_refuses_at_open() {
    let config = StoreConfig::new().pool_size(0).build();
    let caps = fluent_wvr::CapabilitySet::new().with(db_token());
    let err = fluent_wvr::CURRENT_CAPS
        .scope(caps, async { StoreHandle::open(&config).await })
        .await
        .expect_err("zero-sized pool refuses");
    assert!(
        matches!(err, StoreError::Config(_)),
        "an incoherent configuration refuses before any database touch: {err:?}"
    );
}
