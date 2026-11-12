//! Walk-forward / out-of-sample evaluation over one shared bar history.
//!
//! [`run_walk_forward`] promotes [`walk_forward_split`](crate::runner::walk_forward_split)
//! from a bar split into a driver: every [`WalkForwardFold`] trains on an
//! in-sample window and is evaluated on the non-overlapping window that
//! follows, and the two [`BacktestRun`]s are compared side by side in
//! [`WalkForwardReport`]. Folds fan out over the shared
//! [`ResultPool`](fluent_concurrency::pool::ResultPool) and return
//! index-sorted, so parallelism never perturbs the pin: reruns are
//! byte-identical with or without the pool. This produces evidence, not a
//! gate — no aggregate or per-fold threshold is applied here.

use std::ops::Range;

use fluent_wvr::validate::Validate;
use protopyne_strategy::{Grant, RiskEngine, RiskLimits, Strategy};
use protopyne_types::{Bar, Money};

use crate::backtest::BacktestConfig;
use crate::error::ServerError;
use crate::report::BacktestReport;
use crate::runner::{run_backtest, BacktestRun, RunnerConfig};

/// One walk-forward fold: an in-sample window immediately followed by a
/// non-overlapping out-of-sample window, as bar-index bounds into one shared
/// history. The test window begins exactly where training ends, so no bar is
/// evaluated in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// One walk-forward fold: an in-sample window immediately followed by a
/// non-overlapping out-of-sample window, as bar-index bounds into one shared
/// history. The test window begins exactly where training ends, so no bar is
/// evaluated in both.
pub struct WalkForwardFold {
    /// First in-sample bar index (inclusive).
    pub train_start: usize,
    /// In-sample end / out-of-sample start (exclusive).
    pub train_end: usize,
    /// Out-of-sample end (exclusive).
    pub test_end: usize,
}

impl WalkForwardFold {
    /// A fold whose train window starts at `offset` and runs `train_len` bars,
    /// then tests the next `test_len`.
    #[must_use]
    pub fn sequential(offset: usize, train_len: usize, test_len: usize) -> Self {
        Self {
            train_start: offset,
            train_end: offset + train_len,
            test_end: offset + train_len + test_len,
        }
    }

    /// In-sample bar indices (half-open).
    #[must_use]
    pub fn train_range(&self) -> Range<usize> {
        self.train_start..self.train_end
    }

    /// Out-of-sample bar indices (half-open).
    #[must_use]
    pub fn test_range(&self) -> Range<usize> {
        self.train_end..self.test_end
    }

    /// Bars in the in-sample window.
    #[must_use]
    pub fn train_len(&self) -> usize {
        self.train_end.saturating_sub(self.train_start)
    }

    /// Bars in the out-of-sample window.
    #[must_use]
    pub fn test_len(&self) -> usize {
        self.test_end.saturating_sub(self.train_end)
    }

    /// Fail-closed check against a history of `bars` bars: both windows exist
    /// and the test window stays inside the history.
    fn validate(&self, bars: usize) -> Result<(), ServerError> {
        if self.train_start >= self.train_end {
            return Err(ServerError::Backtest(format!(
                "fold train window {}..{} is empty",
                self.train_start, self.train_end
            )));
        }
        if self.train_end >= self.test_end {
            return Err(ServerError::Backtest(format!(
                "fold test window {}..{} is empty",
                self.train_end, self.test_end
            )));
        }
        if self.test_end > bars {
            return Err(ServerError::Backtest(format!(
                "fold test window {}..{} runs past {bars} bars",
                self.train_end, self.test_end
            )));
        }
        Ok(())
    }
}

/// Deterministic sequential folds over `bar_count` bars: fold `i` trains
/// `train_len` bars and tests the next `test_len`, advancing `step` bars each
/// time. A rolling plan uses `step == test_len` (trains overlap); an anchored
/// plan uses `step == train_len` (nothing overlaps). `step` must be at least
/// `test_len`, so out-of-sample windows never overlap. Windows that would run
/// past the history are dropped, so the plan ends at the last whole fold;
/// a plan that fits none refuses. Degenerate windows refuse.
pub fn walk_forward_folds(
    bar_count: usize,
    train_len: usize,
    test_len: usize,
    step: usize,
) -> Result<Vec<WalkForwardFold>, ServerError> {
    if train_len == 0 || test_len == 0 {
        return Err(ServerError::Backtest(
            "walk-forward train and test windows must be positive".to_owned(),
        ));
    }
    if step == 0 {
        return Err(ServerError::Backtest(
            "walk-forward step must be positive".to_owned(),
        ));
    }
    if step < test_len {
        return Err(ServerError::Backtest(format!(
            "walk-forward step {step} would overlap out-of-sample windows of {test_len} bars"
        )));
    }
    let mut folds = Vec::new();
    let mut offset = 0;
    while offset + train_len + test_len <= bar_count {
        folds.push(WalkForwardFold::sequential(offset, train_len, test_len));
        offset += step;
    }
    if folds.is_empty() {
        return Err(ServerError::Backtest(format!(
            "no walk-forward window fits {bar_count} bars with train {train_len} + test {test_len}"
        )));
    }
    Ok(folds)
}

/// One window's generalization metrics, projected from its [`BacktestReport`]
/// so the IS/OOS section and the standalone report can never disagree.
/// [`metrics`](crate::metrics) computed the underlying numbers once, inside
/// the report; this view only selects them. `None` is a metric's own
/// zero-denominator refusal, never invented.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
/// One window's generalization metrics, projected from its [`BacktestReport`]
/// so the IS/OOS section and the standalone report can never disagree.
/// [`metrics`](crate::metrics) computed the underlying numbers once, inside
/// the report; this view only selects them. `None` is a metric's own
/// zero-denominator refusal, never invented.
pub struct WalkMetrics {
    /// Bars replayed in the window.
    pub bars: u64,
    /// Proposals emitted.
    pub proposals: u64,
    /// Orders filled.
    pub fills: u64,
    /// Total cumulative return of the marked equity curve.
    pub cum_return: Option<f64>,
    /// Annualized Sharpe ratio of per-step equity returns.
    pub sharpe: Option<f64>,
    /// Annualized Sortino ratio of per-step equity returns.
    pub sortino: Option<f64>,
    /// Annualized Calmar-style ratio.
    pub calmar: Option<f64>,
    /// Winning equity steps over decisive steps.
    pub win_rate: Option<f64>,
    /// Gross equity gains over gross equity losses.
    pub profit_factor: Option<f64>,
    /// Peak-to-trough drawdown of the marked curve.
    pub drawdown: Money,
    /// Worst single-bar equity decline.
    pub tail_loss: Money,
    /// Filled notional over reserved notional.
    pub capital_efficiency: Option<f64>,
}

impl From<&BacktestReport> for WalkMetrics {
    fn from(report: &BacktestReport) -> Self {
        Self {
            bars: report.bars,
            proposals: report.proposals,
            fills: report.fills,
            cum_return: report.cum_return,
            sharpe: report.sharpe,
            sortino: report.sortino,
            calmar: report.calmar,
            win_rate: report.win_rate,
            profit_factor: report.profit_factor,
            drawdown: report.drawdown,
            tail_loss: report.tail_loss,
            capital_efficiency: report.capital_efficiency,
        }
    }
}

/// Side-by-side in-sample / out-of-sample metrics for one fold.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
/// Side-by-side in-sample / out-of-sample metrics for one fold.
pub struct FoldComparison {
    /// Metrics over the fold's train window.
    pub in_sample: WalkMetrics,
    /// Metrics over the fold's test window.
    pub out_of_sample: WalkMetrics,
}

impl FoldComparison {
    fn of(fold: &WalkForwardFoldRun) -> Self {
        Self {
            in_sample: WalkMetrics::from(&fold.in_sample.report),
            out_of_sample: WalkMetrics::from(&fold.out_of_sample.report),
        }
    }
}

/// One fold's completed runs: the in-sample window and the out-of-sample
/// window it generalizes to. Fold boundaries live once, on
/// [`WalkForwardManifest::folds`]; this list is parallel to that one.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
/// One fold's completed runs: the in-sample window and the out-of-sample
/// window it generalizes to. Fold boundaries live once, on
/// [`WalkForwardManifest::folds`]; this list is parallel to that one.
pub struct WalkForwardFoldRun {
    /// Run over the fold's train window.
    pub in_sample: BacktestRun,
    /// Run over the fold's test window.
    pub out_of_sample: BacktestRun,
}

/// The walk-forward report section: side-by-side IS/OOS metrics per fold, in
/// fold order. A projection of the fold runs, so the section and the reports
/// always read the same numbers.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
/// The walk-forward report section: side-by-side IS/OOS metrics per fold, in
/// fold order. A projection of the fold runs, so the section and the reports
/// always read the same numbers.
pub struct WalkForwardReport {
    /// Per-fold comparisons, in fold input order.
    pub comparisons: Vec<FoldComparison>,
}

impl WalkForwardReport {
    /// Project the section from the completed fold runs.
    #[must_use]
    pub fn of(folds: &[WalkForwardFoldRun]) -> Self {
        Self {
            comparisons: folds.iter().map(FoldComparison::of).collect(),
        }
    }
}

/// The walk-forward pin: code version, shared seed, history size, and the fold
/// boundaries in input order. Serialization order is struct order, so reruns
/// are byte-identical.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// The walk-forward pin: code version, shared seed, history size, and the fold
/// boundaries in input order. Serialization order is struct order, so reruns
/// are byte-identical.
pub struct WalkForwardManifest {
    /// Crate version at run time.
    pub code_version: String,
    /// Caller-supplied seed shared across folds (0 = none).
    pub seed: u64,
    /// Bars the folds index into.
    pub bars: usize,
    /// Fold boundaries, in input order.
    pub folds: Vec<WalkForwardFold>,
}

impl WalkForwardManifest {
    /// Deterministic JSON encoding of this pin. Serialization of plain pin
    /// data cannot fail in practice; a failure still reports loudly inside the
    /// payload instead of emitting an empty object.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        common_core::hash::canonical_json(self)
    }
}

/// One completed walk-forward run: the pin, per-fold IS/OOS runs in fold
/// order, and the side-by-side report section.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
/// One completed walk-forward run: the pin, per-fold IS/OOS runs in fold
/// order, and the side-by-side report section.
pub struct WalkForwardRun {
    /// Reproducibility pin (code version, seed, fold boundaries).
    pub manifest: WalkForwardManifest,
    /// Per-fold runs, in fold input order.
    pub folds: Vec<WalkForwardFoldRun>,
    /// Side-by-side IS/OOS metrics per fold.
    pub report: WalkForwardReport,
}

impl WalkForwardRun {
    /// Deterministic JSON over the pin plus every fold's runs, for rerun
    /// comparison. M8: composite shape, not `canonical_json` alone.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        let folds = serde_json::to_string(&self.folds).unwrap_or_else(|e| {
            format!(
                "{{\"serialization_error\":{}}}",
                serde_json::to_string(&e.to_string())
                    .unwrap_or_else(|_| "\"unencodable\"".to_owned())
            )
        });
        format!("{}\n{folds}", self.manifest.to_canonical_json())
    }
}

/// One walk-forward job: a fold's two windows, fully owned so pool workers
/// stay `Send` with no borrowed state.
struct FoldJob {
    /// Index into the fold list (output order is by index).
    index: usize,
    /// Shared decision stack, cloned per fold (element `Arc`s bump, nothing
    /// duplicates).
    strategy: Strategy,
    /// Fresh risk limits per fold (each fold tunes its own engine).
    risk_limits: RiskLimits,
    /// Grant under evaluation, shared read-only across folds.
    grant: Grant,
    /// Quote synthesis plus data-version pin for this fold.
    venue_config: BacktestConfig,
    /// Attribution plus economics for this fold.
    runner_config: RunnerConfig,
    /// In-sample bars.
    train_bars: Vec<Bar>,
    /// Out-of-sample bars.
    test_bars: Vec<Bar>,
    /// Suite seed shared across folds.
    seed: u64,
}

/// Run the shared decide path once per fold: train on the in-sample window,
/// evaluate on the out-of-sample window that follows.
///
/// Every fold runs through the same [`run_backtest`] the single-symbol path
/// uses, so a walk-forward verdict equals a standalone run on the same window
/// bit for bit. Folds fan out over the shared
/// [`ResultPool`](fluent_concurrency::pool::ResultPool) with outcomes
/// re-sorted by fold index, so parallelism never perturbs determinism: reruns
/// are byte-identical with or without the pool. The result is evidence: no
/// aggregate or per-fold threshold is applied here.
#[allow(clippy::too_many_arguments)]
/// Run the shared decide path once per fold: train on the in-sample window,
/// evaluate on the out-of-sample window that follows.
/// 
/// Every fold runs through the same [`run_backtest`] the single-symbol path
/// uses, so a walk-forward verdict equals a standalone run on the same window
/// bit for bit. Folds fan out over the shared
/// [`ResultPool`](fluent_concurrency::pool::ResultPool) with outcomes
/// re-sorted by fold index, so parallelism never perturbs determinism: reruns
/// are byte-identical with or without the pool. The result is evidence: no
/// aggregate or per-fold threshold is applied here.
pub async fn run_walk_forward(
    strategy: &Strategy,
    risk: &RiskEngine,
    grant: &Grant,
    venue_config: &BacktestConfig,
    runner_config: &RunnerConfig,
    bars: &[Bar],
    folds: &[WalkForwardFold],
    seed: u64,
) -> Result<WalkForwardRun, ServerError> {
    if folds.is_empty() {
        return Err(ServerError::Backtest(
            "walk-forward needs at least one fold".to_owned(),
        ));
    }
    if bars.is_empty() {
        return Err(ServerError::Backtest(
            "history must not be empty".to_owned(),
        ));
    }
    runner_config.validate()?;
    venue_config.validate()?;
    // Boundaries validate up front: a bad fold refuses before the pool spawns.
    for fold in folds {
        fold.validate(bars.len())?;
    }
    for pair in folds.windows(2) {
        if pair[0].test_end > pair[1].train_end {
            return Err(ServerError::Backtest(format!(
                "fold out-of-sample windows overlap: {}..{} then {}..{}",
                pair[0].train_end, pair[0].test_end, pair[1].train_end, pair[1].test_end
            )));
        }
    }
    let mut jobs = Vec::with_capacity(folds.len());
    for (index, fold) in folds.iter().enumerate() {
        jobs.push(FoldJob {
            index,
            strategy: strategy.clone(),
            risk_limits: risk.limits().clone(),
            grant: grant.clone(),
            venue_config: venue_config.clone(),
            runner_config: runner_config.clone(),
            train_bars: bars[fold.train_range()].to_vec(),
            test_bars: bars[fold.test_range()].to_vec(),
            seed,
        });
    }
    // Bounded fan-out: workers clamp to 1..=8 and the queue fits exactly one
    // slot per fold, so oversubscription waits its turn — the queue never
    // grows past the fold count.
    let workers = folds.len().clamp(1, 8);
    let pool = fluent_concurrency::pool::ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        workers,
        folds.len().max(1),
        move |job: FoldJob| async move {
            let risk = RiskEngine::new(job.risk_limits);
            let in_sample = run_backtest(
                &job.strategy,
                &risk,
                &job.grant,
                &job.venue_config,
                &job.runner_config,
                &job.train_bars,
                job.seed,
            )
            .await
            .map_err(|e| e.to_string())?;
            let out_of_sample = run_backtest(
                &job.strategy,
                &risk,
                &job.grant,
                &job.venue_config,
                &job.runner_config,
                &job.test_bars,
                job.seed,
            )
            .await
            .map_err(|e| e.to_string())?;
            Ok::<(usize, WalkForwardFoldRun), String>((
                job.index,
                WalkForwardFoldRun {
                    in_sample,
                    out_of_sample,
                },
            ))
        },
    );
    let mut indexed = Vec::with_capacity(jobs.len());
    for outcome in pool.submit_all(jobs, None).await {
        indexed
            .push(outcome.map_err(|e| {
                ServerError::Backtest(format!("walk-forward submit failed: {e:?}"))
            })?);
    }
    pool.shutdown().await;
    indexed.sort_by_key(|(index, _)| *index);
    let mut runs = Vec::with_capacity(indexed.len());
    for (_, run) in indexed {
        runs.push(run);
    }
    let report = WalkForwardReport::of(&runs);
    Ok(WalkForwardRun {
        manifest: WalkForwardManifest {
            code_version: env!("CARGO_PKG_VERSION").to_owned(),
            seed,
            bars: bars.len(),
            folds: folds.to_vec(),
        },
        folds: runs,
        report,
    })
}

#[cfg(test)]
#[path = "../tests/walk_forward.rs"]
mod tests;