//! A bounded [`HistoryProvider`] over the existing Parquet history root.
//!
//! This adapter adds no parser: it loads one ticker through
//! [`HistoryRoot::load_ticker`] (the server's one Parquet/CSV ladder) and
//! serves span-bounded pages from the merged series. Ingest faults map into
//! [`HistoryError`] with the ladder's own retry taxonomy.

use std::path::PathBuf;

use protopyne_store::{
    BoxFuture, FetchPage, FetchRequest, HistoryError, HistoryProvider, TapeKey, TimeRange,
};
use protopyne_types::Bar;

use crate::history_root::HistoryRoot;

/// One ticker's Parquet history exposed as a repair provider.
#[derive(Debug, Clone)]
pub struct ParquetHistoryProvider {
    symbol: String,
    key: TapeKey,
    bars: Vec<Bar>,
    horizon: Option<TimeRange>,
    page_size: usize,
}

impl ParquetHistoryProvider {
    /// Load `symbol` from `root` and bind it to `key`.
    pub fn new(root: &HistoryRoot, symbol: &str, key: TapeKey) -> Result<Self, HistoryError> {
        let load = root.load_ticker(symbol).map_err(|error| {
            HistoryError::Unavailable(format!("{symbol} Parquet history refused: {error}"))
        })?;
        let horizon = match (load.bars.first(), load.bars.last()) {
            (Some(first), Some(last)) => Some(
                TimeRange::bounded(
                    first.ts.timestamp_millis(),
                    last.ts
                        .timestamp_millis()
                        .saturating_add(key.timeframe.duration_ms()),
                )
                .map_err(|error| HistoryError::Unavailable(error.to_string()))?,
            ),
            _ => None,
        };
        Ok(Self {
            symbol: symbol.to_owned(),
            key,
            bars: load.bars,
            horizon,
            page_size: protopyne_store::DEFAULT_PAGE_BARS,
        })
    }

    /// Load from a directory path.
    pub fn open(path: &std::path::Path, symbol: &str, key: TapeKey) -> Result<Self, HistoryError> {
        let root = HistoryRoot::open(path).map_err(|error| {
            HistoryError::Unavailable(format!(
                "Parquet history refused: root {} unreadable: {error}",
                path.display()
            ))
        })?;
        Self::new(&root, symbol, key)
    }

    /// Override the page size (clamped at one).
    #[must_use]
    pub fn with_page_size(mut self, page_size: usize) -> Self {
        self.page_size = page_size.max(1);
        self
    }

    /// The source symbol.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// The path this provider was not constructed from; retained for symmetry
    /// with the file provider and future diagnostics.
    #[must_use]
    pub fn path_hint(&self) -> Option<PathBuf> {
        None
    }

    fn fetch_sync(&self, req: FetchRequest<'_>) -> Result<FetchPage, HistoryError> {
        if req.key != self.key {
            return Err(HistoryError::Unavailable(format!(
                "provider serves {:?}, not {:?}",
                self.key, req.key
            )));
        }
        let span = req.span;
        let (Some(start), Some(end)) = (span.start_ms, span.end_ms) else {
            return Err(HistoryError::Unavailable(
                "unbounded Parquet page refused".to_owned(),
            ));
        };
        let mut matched: Vec<&Bar> = self
            .bars
            .iter()
            .filter(|bar| {
                let ts = bar.ts.timestamp_millis();
                ts >= start && ts < end
            })
            .collect();
        if matched.len() <= self.page_size {
            return Ok(FetchPage {
                bars: matched.into_iter().cloned().collect(),
                next: None,
            });
        }
        let page: Vec<Bar> = matched.drain(..self.page_size).cloned().collect();
        let next_start = page
            .last()
            .map_or(start, |bar| bar.ts.timestamp_millis())
            .saturating_add(self.key.timeframe.duration_ms());
        let next = (next_start < end).then_some(TimeRange {
            start_ms: Some(next_start),
            end_ms: Some(end),
        });
        Ok(FetchPage { bars: page, next })
    }
}

impl HistoryProvider for ParquetHistoryProvider {
    fn horizon(&self) -> Option<TimeRange> {
        self.horizon
    }

    fn fetch(&self, req: FetchRequest<'_>) -> BoxFuture<'_, Result<FetchPage, HistoryError>> {
        let page = self.fetch_sync(req);
        Box::pin(std::future::ready(page))
    }
}

#[cfg(test)]
#[path = "../tests/parquet_history.rs"]
mod tests;
