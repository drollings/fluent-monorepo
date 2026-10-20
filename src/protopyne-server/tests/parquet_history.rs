//! Parquet history provider: horizon, span paging, and fault mapping.

use std::path::PathBuf;

use protopyne_store::{FetchRequest, HistoryError, HistoryProvider, SymbolId, TapeKey, TimeRange};
use protopyne_types::Timeframe;

use crate::history_root::HistoryRoot;
use crate::parquet_history::ParquetHistoryProvider;

fn root_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical")
}

fn key() -> TapeKey {
    TapeKey {
        symbol_id: SymbolId(1),
        timeframe: Timeframe::M1,
    }
}

#[test]
fn horizon_is_real_and_pages_by_span() {
    let root = HistoryRoot::open(&root_path()).expect("root");
    let provider = ParquetHistoryProvider::new(&root, "SPY", key())
        .expect("provider")
        .with_page_size(10);
    let horizon = provider.horizon().expect("ticker has a horizon");
    assert!(horizon.start_ms.unwrap() < horizon.end_ms.unwrap());
    let span = TimeRange::bounded(
        horizon.start_ms.unwrap(),
        horizon.start_ms.unwrap() + 60 * 60 * 1_000,
    )
    .expect("span");
    let first = provider
        .fetch(FetchRequest {
            key: key(),
            span: &span,
        })
        .now_or_never()
        .expect("immediate")
        .expect("page");
    assert_eq!(first.bars.len(), 10, "page bound honored");
    let next = first.next.expect("more within the hour");
    assert!(next.start_ms.unwrap() > horizon.start_ms.unwrap());
    let second = provider
        .fetch(FetchRequest {
            key: key(),
            span: &next,
        })
        .now_or_never()
        .expect("immediate")
        .expect("page");
    assert_eq!(second.bars.len(), 10);
    assert!(second.bars[0].ts > first.bars[9].ts);
}

#[test]
fn missing_root_maps_to_unavailable() {
    let err = ParquetHistoryProvider::open(
        std::path::Path::new("/nonexistent/protopyne-history-root"),
        "SPY",
        key(),
    )
    .expect_err("missing root refuses");
    assert!(matches!(err, HistoryError::Unavailable(_)));
    assert!(err.to_string().contains("Parquet history refused"));
}

/// Minimal immediate-future driver: the provider's fetch is synchronous.
trait NowOrNever: Sized {
    type Output;
    fn now_or_never(self) -> Option<Self::Output>;
}

impl<T> NowOrNever for std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + '_>> {
    type Output = T;
    fn now_or_never(mut self) -> Option<T> {
        use std::task::{Context, Poll, Waker};
        let mut context = Context::from_waker(Waker::noop());
        match self.as_mut().poll(&mut context) {
            Poll::Ready(value) => Some(value),
            Poll::Pending => None,
        }
    }
}
