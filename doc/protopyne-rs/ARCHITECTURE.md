# protopyne-rs — Architecture Map & Capability Index

## 0. Handoff for new agents

This document is the entry point. It maps all seven `protopyne-*` crates by
capability, with exact `path :: Symbol` references so you can jump straight
to the relevant file without re-exploring. Read it first, then drill into
the cited symbols. Recounted 2026-10-12 (rule: every `*.rs` under
`src/protopyne-*/src/` and `src/protopyne-*/tests/`, `common/` helpers
included): since the 2026-10-11 recount the kernels crate has grown the
native track (`bars.rs` ring/tick/resample, `mtf.rs` alignment + store,
`native_libs.rs` override wiring, `market_context.rs` pure view, plus
`buffers.rs` (the `BufferFloat` hot-method mirror) and the `native-libBuffers`
compat row after the 2026-10-12 recount — §3
kernels row); the registry still covers 50 kernels while the compat matrix
now covers 53 rows (50 kernel + 3 native: `native-libBars`,
`native-market-context`, `native-libBuffers`, §4 C6); the parity matrix grows
182→207 rows under roadmap M7 (counted by verdict); the source tree is 107
files and the test tree 137 files (kernels `tests/common/fixture.rs`
included per the rule). Anything
below that still cites the older shape is marked explicitly.
Companion documents in `doc/protopyne-rs/`:

- `README.md` — what the project does, how to build/run/test it, CLI and
  container usage. Start here for operations.
- `VISION.md` — the design vision: economics, safety invariants, platform
  direction (aspirational goals and capabilities, no implementation
  details).
- `INJECTION_CONTRACT.md` — the headless MTF/tick injection contract for
  the Pine harness: per-`request.*`-arm live-vs-injected behavior,
  tick-leg and confirm accounting, fixture map with recorded baselines,
  and harness bounds.

Direction of record: Rust-only, no browser/JavaScript/Node.js anywhere.
`egui`/`eframe` are approved future dependencies confined to paint-edge
crates; `egui-charts` (sibling checkout) is reference only and never
becomes a dependency. Nothing below depends on it.

Conventions you must follow when working here (from `AGENTS.md` at repo
root): unit tests live in `src/<crate>/tests/<module>.rs` with a `#[path]`
forwarder in the source module, never inline `mod tests`; shared crates
under `src/` must not import from `guidance`/`router`/`coral`/`wasm_ipc`;
never write planning identifiers into code or comments; hermetic tests stay
on loopback. Skill docs that govern implementation live in
`doc/skills/{common-core,fluent-wvr,fluent-concurrency,dag,db}/SKILL.md`.

## 1. Overview

- **Languages:** Rust only (edition 2021/2024 per workspace). No Python in
  the runtime path; the container build fails if an interpreter appears.
  No browser, JavaScript, or Node.js in build, test, or run: chart data
  leaves the server as JSON rows for a native terminal, never for a web
  chart library.
- **Build:** Cargo workspace (`Cargo.toml` at repo root, resolver v3).
  Seven member crates below; shared libraries (`common-core`,
  `fluent-wvr`, `fluent-concurrency`, `fluent-dag`, `fluent-db`) and third-party
  crates come from the workspace `[dependencies]` table. `egui`/`eframe`
  are not dependencies yet; when the native terminal lands they enter
  paint-edge crates only (see direction note in §0).
- **Layout:** `src/protopyne-{types,kernels,strategy,venue,server,chart-model,chart-scale}/` —
  each with `src/*.rs`, `tests/*.rs`, `Cargo.toml`
   (`autotests = false`, explicit test targets via `#[path]` forwarders).
     107 files under `src/protopyne-*/src/` (types 11, kernels 49 — 38
      top-level plus 11 under `pine/` — strategy 7, venue 18, server 11,
      chart-model 7, chart-scale 4) plus 137 test files (~244 total),
      recounted 2026-10-12 (kernels tests 73: 72 top-level plus
      `tests/common/fixture.rs`; per-crate: types 9, strategy 8, venue 26,
      server 14, chart-model 5, chart-scale 2).
    Test histories are small trimmed fixtures: kernels
    `tests/data/SPY_{1M,5M,15M}.csv` hold 436/346/116 bars (verbatim
    copies of the same trims under `data/protopyne-rs/historical/`, which
    additionally carries the full `SPY_ticks.csv` tape and a `SPY_10S.csv`
    slice); server `tests/data/SPY_{15M,5M}.csv` hold 11/32
    trailing-window bars pinned by the `csv.rs` ingest goldens. Kernels
    additionally ships `sma14.json`, `ema14.json`, `parity_sma10.csv`,
    `tests/data/golden/` (incl. `native_freeze_spy_{1m,5m,15m}.json`:
    bit-identical native-vs-Pine agreement goldens over 436/346/116 bars),
    the five tick-harness windows (`tests/data/ticks/`, all verbatim tape
    slices) with `tick_harness_baselines.json`, and server ships the
    `pine_integration_lite_{1m,5m,15m}.json` proof goldens plus the
    `pine_task_rsimd_{1m,5m,15m}.json` indicator goldens. The Pine closure
    fixtures read the real indicator
   sources from `data/protopyne-rs/` (`historical/` tick/minute CSVs,
   `indicators/`, `libraries/`).
  `make protopyne-test` gates the five trading crates
  (`types,kernels,strategy,venue,server`); the two chart crates are covered
  by the workspace build/test, not that target. Server criterion benches
  (`src/protopyne-server/benches/backtest_path.rs`) ride the bench harness,
  not `protopyne-test`.
- **Entry points:** library roots (`src/protopyne-*/src/lib.rs`, all
  `#![forbid(unsafe_code)]`); the two chart crates are library-only.
  One binary (`src/protopyne-server/src/main.rs :: main`, the HTTP
  server, default bind `127.0.0.1:9060`); one container (`src/protopyne-server/Containerfile`) with its
  smoke script (`src/protopyne-server/smoke-container.sh`); Make targets
  (`Makefile :: protopyne`, `protopyne-start`, `protopyne-stop`,
  `protopyne-test`, `protopyne-smoke`).
- **Runtime model:** synchronous decision core (pure functions over values)
  plus a Tokio perimeter: one axum server task, `Scope`-owned feed/session
  tasks, `ResultPool` sweep workers, broadcast-channel fan-out. No global
  singletons in the trading path; per-run contexts (`RunCtx`, `GrantUsage`)
  and per-session scoped indexes (`SymbolIndex`) keep parallel runs isolated.
  Sessions bind a `VenueRegistry` routing table, not a single paper venue:
  single-venue callers pass `VenueRegistry::single` and behave identically.
- **Architecture summary:** layered pipeline `types → kernels → strategy
  → venue → server`, with `chart-model` / `chart-scale` as `egui`-free
  paint-edge leaves off `types` (bars) and `server` (chart rows): display
  `f64` appears only at the `to_display_float` conversion boundary and
  never flows back into decisions. Each layer owns its vocabulary and validates its own
  inputs fail-closed through the shared `Validate` trait
  (`types/validate.rs`, C34). The single decision path is
  `Signal → RiskEngine → evaluate_grant → Venue::submit`, executed
  identically by backtest replay (now multi-symbol, C45), paper sessions,
  and HTTP preview, with byte-identical manifests (`to_canonical_json`
  rerun equality) proving it. Operator control (halt/resume/close-all/
  cancel-all) enters as journaled rows through the command plane (C40),
  never as out-of-band mutation.

## 2. Dependency Map

Counts are files under `src/protopyne-*/src/` with a textual reference
(`use` or fully-qualified path), recounted 2026-10-12. "Workspace analogue" names the
in-repo counterpart where one exists.

| Dependency | Role | Usage breadth (files, approx.) | Workspace analogue / note |
|---|---|---|---|
| `protopyne-types` | Money/time/bar/chain/symbol vocabulary | 39 (highest fan-in) | n/a (foundation itself) |
| `serde` / `serde_json` | Persistence, manifests, HTTP bodies | 31 / 15 (service, csv, reports, market events, journal rows) | n/a |
| `chrono` | Timestamps, windows, DTE | 28 | n/a |
| `rust_decimal` | All money math | 28 | n/a |
| `bon` | Builder DSL for configs/intents | 22 | n/a |
| `common-core` | `retry_async`, `sync::lock`, `config::load_json_or_default`, `IoError`, `impl_from_io_error!`, `registry::ConcurrentRegistry` | 18 | n/a (shared lib) |
| `fluent-wvr` | Components, `FieldAccess`/`Describable`, capabilities, `ComponentAdapter`, `check_for_execute` bridge | 15 | n/a (shared lib) |
| `tokio` | Runtime, listener, sleep, signal, broadcast | 7 | n/a |
| `fluent-concurrency` | `Limiter`, `Scope`, `SupervisedBatch`, `ResultPool`, `flow` credit, fallback combinator, `StreamAbort` | 11 | n/a (shared lib) |
| `thiserror` | Typed errors per crate | 8 (one `error.rs` per crate + re-export) | n/a |
| `csv` | History ingest | 3 (all in server) | n/a |
| `protopyne-strategy` | Proposals, risk, grants, generator | 5 (venue + server) | n/a |
| `protopyne-venue` | Venue trait, paper, journal, registry, commands | 4 (server + self) | n/a |
| `protopyne-kernels` | Indicators for overlays/spike eval; Pine language engine (driven by kernels/server test targets, C46–C49) | 3 (server) | n/a |
| `fluent-dag` | `DependencyGraph`, `CheckpointedStepGraph` | 5 (kernels ×4, server) | n/a (shared lib) |
| `axum` | REST + WebSocket server | 2 (service, main via lib) | n/a |
| `rusqlite` | Journal durability (WAL) | 1 (`venue/journal.rs`) | n/a |
| `fluent-db` | Journal transactions (`query::transaction`) | 1 (`venue/journal.rs`) | n/a (shared lib, path `src/db`) |
| `protopyne-chart-model` | `Decimal`→display rows for service | 1 (`server/service.rs`) | n/a (leaf crate; server is its only consumer) |
| `polars` | Research frames + calibration/parity harnesses | 3 (`kernels/{research,parity,calibration}.rs` code use; doc mentions only in `pine/eval.rs`, `compat.rs`) | n/a |
| `criterion` | Backtest-path benches | 0 in `src` (benches only: `server/benches/backtest_path.rs`) | n/a |
| `syn` | Emitted-Rust parse checks | 0 in `src` (dev-deps, spike tests only) | n/a |
| `reqwest`, `tokio-tungstenite`, `tempfile` | HTTP/WS test clients, temp dirs | 0 in `src` (dev-deps only) | n/a |

## 3. Module Map

`Package | Responsibility | Key exports`

| Directory/Package | Responsibility | Key exports (Symbol list) |
|---|---|---|
| `src/protopyne-types/src/` | Money, time, bars, option chain, symbols, validation | `Money`, `Qty`, `Bar`, `validate_new_bar`, `Adjustment`, `Timeframe`, `BarWindow`, `BarGap`, `AdjustedBounds`, `adjust_fetch_bounds`, `trim_indicator_output`, `warmup_headroom`, `PriceSource`, `SeriesContext`, `GapPolicy`, `LtfView`, `ContractId`, `PortfolioId`, `OptionContract`, `OptionKind`, `Greeks`, `ChainQuote`, `SpreadLeg`, `SpreadLegs`, `LegSide`, `Portfolio`, `MAX_SPREAD_LEGS` (all in `chain.rs`), `SymKey`, `SymbolIndex`, `DEFAULT_MAX_SYMBOLS` (in `symbol.rs`), `Validate`, `check_for_execute`, `valid_output` (in `validate.rs`), `TypesError` |
| `src/protopyne-kernels/src/` | Indicator kernels, registry, runtimes, research frames | `Indicator` (trait), `SmaIndicator`, `EmaIndicator`, `RsiIndicator`, `MacdIndicator`, `StochIndicator`, `AtrIndicator`, `BbIndicator`, `KcIndicator`, `TrIndicator`, `NatrIndicator`, `Eit/Kalman/VossIndicator` (in `filters.rs`), averages (`Alma/Dema/Hma/Rma/Tema/Vwma/WmaIndicator`, all in `averages.rs` except `Sma`/`Ema`), overlap/price/momentum/stat/volume (`Bbands/Midpoint/Midprice/TrimaIndicator` in `ta_overlap.rs`, `AvgPrice/MedPrice/TypPrice/WclPriceIndicator` in `ta_price.rs`, `Apo/Mom/Ppo/Roc/Rocp/Rocr/Rocr100/Rsv/Stochf/Trix/Willr/AroonIndicator` in `ta_momentum.rs`, `Stddev/Var/CorrelIndicator` in `ta_stat.rs`, `Ad/Adosc/Obv/Mfi/VwapIndicator` in `ta_volume.rs`), candle predicates (`doji`, `gap_up`, … in `candles.rs`, free functions), WQ combinators (`ts_*` in `wq_series.rs`, `hl_*` in `wq_half_life.rs`, scalar maps in `wq_comb.rs`, all free functions), `expr_fixture`/`assert_expr_parity` in `parity.rs`, `ParityDashboard`/`run_control`/`canonical_params` in `calibration.rs`, `RunCtx`, `PineRuntime`, `PineRuntimeConfig` (chart symbol/period), `SliceSeries`, `Var`/`VarIp`/`VarStore`, `Ohlcv`, `OwnedOhlcv`, `BarTick`, `FullTick`, `NamedSeries`, `ParamSet`, `IndicatorResult`, `IndicatorRegistry`, `ComponentRegistry`, `IndicatorComponent`, `dynamic_indicator`, `indicator_double`, `InputDef` (now with `Session`/`Source`), `InputSet`, `PineValue` (12 variants incl. `Map`/`Drawing`/`Matrix` in `value.rs`), `map_key_of`, `DrawingHandle`, `DrawingKind`, `MatrixValue`, `Drawing` (drawing.rs annotations), `execution_order`, `builtin_registry` (50 kernels), `compat_for`, `COMPATIBILITY` (53 rows), `assert_matrix_covers`, `KernelCompat`, `frame_of_bars`, `frame_of_values`, `instrumented_kernel`, `KernelUnit`, `KernelError`, `Control`, `ControlViolation`, `ParityRow`, `null_alignment_diffs` (in `calibration.rs`), math fns (`period_len`, `window_sum/mean`, `ema_factor/step`, `wilder_step`, `div_or_none`, `warmup_none`, `std_pop/std_sample`, `corr_pearson`, `linreg_slope_intercept`, `ewm_span_step`, `median_of`, `mode_of`, `percentile_linear`, `standardize_scores`, `covariance_pop/sample`, `midpoint_of`, `rolling_min/max`), util fns (`crossover`, `crossunder`, `highest`, `lowest`, `mom`, `change`, `cross_warmup`, expr arithmetic `add/mul/sub/div_expr`, `max/min/sum_window`, `argmax/argmin_window`), Pine language engine (`pine/`): `parse_program` (`syntax.rs`), `display_program` (`display.rs`), `PineSource`/`load_pine_source`/`SUPPORTED_PINE_VERSION` (`source.rs`), `resolve_imports`/`ModuleGraph` (`modules.rs`), `PineEngine` (`run_linked`, `run_linked_ticks`, `inject_security`, `inject_lower_tf`, `LtfDataset`), `TaBridge`/`batch_security` (`builtins_ta.rs`), core built-ins incl. `matrix.*` (`builtins_core.rs`), `EGUI_PARITY` (207 rows: 160 `Match`, 28 `Correct`, 19 `Refuse`)/`parity_for`/`ParityVerdict` (`egui_parity.rs`), `REAL_LIBRARIES`/`load_closure_sources` (`closure.rs`), native track: `BarRing`/`BarsConfig`/`BarsHistory`/`is_confirmed_bar`/`rebuild_suffix`/`project_bars`/`tick_resample` + drift/spike/gap constants + `DriftReport` (`bars.rs`), grid alignment + `SeriesKey`/`SeriesCache`/`MtfStore`/`build_graph`/`order_for`/`ensure` (`mtf.rs`), `NativeLibraryId`/`NATIVE_LIBRARIES`/`find_native`/`version_is_ignored`/`NativeAudit`/`NativeProvider` (`provides` + `call`, default handles nothing)/`LibBarsProvider` (serves zero-arg `Bars.fetch`)/`LibBuffersProvider` (serves `BufferFloat` hot methods via `buffers.rs :: serve_buffer_method`)/`MethodCtx`/`TickLegs`/`FieldTable`/`NativeOptions`/`apply_overrides`/`pine_sha_of` (`native_libs.rs`), `MarketContext`/`MarketContextKey`/`MarketRegime`/`update`/`fetch_all`/`health_mas`/`detect_regime` (`market_context.rs`), `COMPATIBILITY` (53 rows: 50 kernel + 3 native)/`NATIVE_MATRIX_ROWS`/`assert_native_matrix_covers` (`compat.rs`) |
| `src/protopyne-strategy/src/` | Proposals, signals, risk, grants, generator | `Proposal`, `ProposalState`, `ProposalEvent`, `Direction`, `ProposalSide`, `StructureKind`, `JournaledTransition`, `transition`, `Strategy`, `Signal` (trait), `FixedSignal`, `SignalOutput`, `ExplicitStructure`, `StructureSelector`, `FixedContracts`, `Sizing`, `HoldLoss`, `LossAction`, `LossMgmt`, `LimitMonitor`, `MonitorAlert`, `Monitor`, `ProposeInput`, `NullMonitor`, `RiskEngine`, `RiskLimits`, `RiskApproved`, `RiskRefused`, `RiskReason`, `RiskState`, `Grant`, `GrantScope`, `GrantPatch`, `GrantPermission`, `GrantBudgets`, `SignalThresholds`, `TradeThresholds`, `PortfolioThresholds`, `BreakerLimits`, `evaluate_grant`, `GrantDecision`, `GrantUsage`, `GrantMarket`, `BreakerState`, `Generator`, `GeneratorBuilder`, `GenerateInput`, `REPORT_SLOPE_LOOKBACK`, `REPORT_SCORE_WINDOW`, `Exposure`, `RebalanceMode`, `RebalanceOutcome`, `Rung`, `ladder_rung`, `portfolio_for`, `rebalance`, `rsrs_score`, `utility` (all in `generator.rs`), `StrategyError` |
| `src/protopyne-venue/src/` | Venue abstraction, routing, paper fills, journal, commands, depth, supervision | `Venue` (trait, object-safe boxed futures), `QuoteSource` + `LegQuoteSource` (traits, in `venue.rs`), `DryRunOutcome`, `BalanceEntry`, `AccountSnapshot`, `AccountEvent`, `LegExecution`, `VenueRegistry` (in `registry.rs`; `single`/`add_route`, ordered fallback, query-first cross-venue idempotency), `PaperVenue`, `PaperConfig`, `Fault`, private per-leg `decide_spread` (atomic refuse-or-fill; public surface is `spread_positions`/`set_leg_quotes`), per-OCC `spread_positions`, `set_leg_quotes`, `Quote`, `LiquidityFilter`, `Slippage`, `FeeSchedule`, `DEFAULT_SLIPPAGE_BPS`, `OrderIntent` (multi-leg attachment), `OrderRecord`, `OrderStatus`, `ActiveOrderState` (projection), `OrderSide`, `TimeInForce`, `Fill`, `CorporateEvent`, `Position` (single signed-`i64` struct), `PositionSide`, `PositionFill`, `SpreadPosition`, `PositionExited`, `update_from_fill`, `calculate_pnl_realised/unrealised`, `calculate_price_entry_average` (in `position.rs`), `client_id_for`, `qty_of`, `Journal`, `JournalEvent`, `ReplayedState`, `VenueStorage`, `LEG_FILL_KIND`, `safe_submit`, `SUBMIT_TIMEOUT` (5s), `SUBMIT_RETRIES` (3), `reconcile`, `reconcile_legs`, `journal_net`, `journal_leg_net`, `ReconcileGate`, `ReconcileMismatch`, `ReconcileReport`, `may_trade`, `check_trade`, `HealthLimits`, `HealthState`, `HealthVerdict`, `MarketEvent`, `MarketEventKind` (in `market.rs`), `DepthBook`, `DepthDelta`, `PriceLevel`, `BookManager`, `DarkFinding`, `DEFAULT_MAX_LEVELS` (in `depth.rs`), `SupervisedStream`, `StreamConfig`, `StreamBreaker`, `StreamHealthUnit`, `StreamOutcome`, `STREAM_HEALTH_ASSET`, `terminal_reason` (in `stream.rs`), `TastytradeTransport`, `TastytradeConfig`, `TransportError`, `RETRY_MAX_ATTEMPTS`/`RETRY_BACKOFF_BASE_MS`/`RETRY_JITTER_PCT`, `transform_quote`, `transform_chain_quote`, `transform_account_event`, `validate_subscription` (in `tastytrade.rs`), `CommandKind`, `CommandUnit`, `TradingState`, `TradingStateUnit`, `StateHandle`, `StateReplica`, `PortfolioRegistry`, `PortfolioView`, `ReplicaOrder`, `LegGroup`, `close_all_intents`, `cancel_all_ids`, `replay_trading_state`, `OPERATOR_CLIENT_ID`, `TRADING_ENABLED_ASSET`, `COMMAND_*_KIND` (in `command.rs`), `TradingSession` (registry-bound), `SessionConfig`, `SubmitJob`, `check_unit`, `VenueError` |
| `src/protopyne-server/src/` | Ingest, backtest, metrics, portfolio, HTTP, spike, binary | `load_bars_csv`, `parse_bars_csv`, `BacktestVenue`, `BacktestConfig`, `HistorySource` (`Csv`/`Dynamic`), `load_history`, `load_bars_single`, `load_bars_with_fallback`, `quote_for_bar`, `run_backtest`, `run_multi_backtest`, `MultiSymbolInput`, `MultiBacktestRun`, `MultiRunManifest`, `run_paper_decisions`, `decide_and_submit`, `checked_order_for`, `side_for_direction`, `BacktestRun`, `BarDecision`, `BarHealth`, `CheckedOrder`, `RunnerConfig`, `SweepOutcome`, `equity_curve_for`, `walk_forward_split`, `snapshot_positions`, `expire_position`, `report_rsrs`, `triple_barrier`, `RunManifest`, `BacktestReport`, `build_report`, `rfc3339`, `max_drawdown`, `worst_step_loss`, `capital_efficiency`, `count_unquotable_bars`, `cum_return`, `simple_to_log_return`, `log_to_simple_return`, `render_tearsheet`, `mean`, `population_std`, `sharpe`, `sortino`, `calmar`, `win_rate`, `profit_factor`, `step_returns`, `drawdown_fraction`, `TRADING_DAYS_PER_YEAR` (in `metrics.rs`), `Aggregation`, `aggregate`, `BookedFill`, `BucketKey`, `BucketSummary`, `PortfolioTotals`, `PortfolioReport`, `PAPER_PORTFOLIO` (in `portfolio.rs`), `run_sweep`, `build_replay`, `complete_replay`, `ReplayStep`, `serve`, `ServerConfig`, `ServerHandle`, `AppState`, `CandleRow`, `ChartBars`, `OverlaySeries`, `ProposalView`, `PortfolioView`, `PreviewParams`, `PreviewResponse`, `ArmResponse`, `WorstCase`, `CommandStatus`, `DenyCount`, `ErrorBody`, `compile_spike`, `evaluate_lowered`, `calibrate`, `CalibrationTally`, `Hair`, `Length`, `Binding`, `Lowered`, `Refusal`, `SpikeOutcome` (in `transpile.rs`), `ServerError` |
| `src/protopyne-chart-model/src/` | Headless chart data: display conversion, chart taxonomy, sessions, ranges, bar transforms (`egui`-free) | `ChartRow`, `ChartRows`, `row_of`, `rows_of`, `to_display_float`, `ChartType`, `ChartTypeCategory`, `DateRange`, `SessionProvider` (+ `Daily/Weekly/Monthly/CompositeSessionProvider`), `SessionBreak`, `SessionBreakType`, `find_session_breaks`, `heikin_ashi`, `renko_bricks`, `ChartModelError` |
| `src/protopyne-chart-scale/src/` | Headless scale engine: price modes, ranges, bar/price mapping, ticks (`egui`-free, depends on nothing protopyne) | `ChartMapping`, `PriceScale`, `PriceScaleMode`, `PriceRange`, `Tick`, `nice_ticks`, `ScaleError` |
| `src/protopyne-server/` (ops) | Container + smoke | `Containerfile`, `smoke-container.sh` (no Rust symbols) |
| `src/protopyne-*/tests/` | Per-module suites + CSV/JSON goldens (kernels + server) | Mirror source module names; `csv.rs` pins the trimmed ingest goldens (11-row 15M, 32-row 5M); kernels ships `sma14.json`, `ema14.json`, `parity_sma10.csv`, `tests/data/golden/`, plus Pine suites (`pine_triage`, `pine_task_closure`, `pine_ta_coverage`, `pine_dvp_security` (driver-level probes only, no interpreted DVP run), `pine_task_parity`, `pine_parity_freeze`, `pine_lib{util,bars,buffers,mas,apex}`, `pine_draw_captures`, `pine_closure_parse`, `pine_egui_parity`, `pine_runtime_shims`, `pine_syntax`, `pine_modules`, `pine_eval`, `pine_builtins_{core,ta}`, `pine_calibration`, `pine_v6`, `pine_fastmas`, `pine_lexer`, `pine_source`), native suites (`native_libs`, `native_override`, `native_bridge` (execution dispatch: opt-in Pine-default consult, wholesale field equality, pull-refusal parity), `native_buffers`, `bars`, `bars_calibration`, `bars_history`, `mtf`, `mtf_store`, `market_context_native`, `market_context_calibration`, `native_parity_freeze` with the SPY agreement goldens); server ships the `pine_integration_lite` headless proof with its three timeframe goldens, the `pine_task_indicators` suite (RSIMD timeframe goldens + PPAS refusal pins) plus the `mtf_fanout` shared-store fan-out proof and the `runner` multi-symbol determinism pin |

## 4. Capability Index

Fan-in: H (used across crates/files broadly), M (several callers), L (one or two). Depends-on uses capability IDs.

| ID | Capability | Kind | Files | Key symbols (path :: Symbol) | Depends on | Fan-in | Notes |
|---|---|---|---|---|---|---|---|
| C1 | Decimal money/qty vocabulary | infra | `src/protopyne-types/src/money.rs` | `money.rs :: Money`, `money.rs :: Qty` | — | H | `Copy`, `Decimal`-backed; `Money` and `Qty` never mix by construction |
| C2 | Bar windows, gaps, warmup math | infra | `src/protopyne-types/src/window.rs`, `src/protopyne-types/src/warmup.rs`, `src/protopyne-types/src/timeframe.rs` | `window.rs :: BarWindow`, `warmup.rs :: adjust_fetch_bounds`, `warmup.rs :: trim_indicator_output`, `timeframe.rs :: Timeframe` | C1 | M | Warmup/trim ported from the C ancestor's fetch-bounds logic |
| C3 | Indicator kernels + registry | feature | `src/protopyne-kernels/src/{sma,ema,averages,oscillators,volatility,filters}.rs`, `src/protopyne-kernels/src/indicator.rs`, `src/protopyne-kernels/src/timed.rs` | `indicator.rs :: Indicator`, `indicator.rs :: builtin_registry` (50 kernels), `indicator.rs :: execution_order`, `sma.rs :: SmaIndicator` (et al.; `Dema`/`Tema` also live in `averages.rs`), `filters.rs :: Eit/Kalman/VossIndicator`, `timed.rs :: KernelUnit`, `timed.rs :: instrumented_kernel` | C1, C2 | H | `batch` + `update`/`update_full` + `warmup` per kernel; ordering via `fluent-dag`; `timed.rs` is the supervised-batch path, the hot loop stays free functions; the native `MtfStore`/`MarketContext` read these same kernels through `batch` over borrowed views (one shared compute path, never per-timeframe recompute) |
| C4 | Run context + Pine runtimes | infra | `src/protopyne-kernels/src/context.rs`, `src/protopyne-kernels/src/runtime.rs`, `src/protopyne-kernels/src/series.rs`, `src/protopyne-kernels/src/vars.rs`, `src/protopyne-kernels/src/inputs.rs` | `context.rs :: RunCtx`, `runtime.rs :: PineRuntime`, `runtime.rs :: PineRuntimeConfig` (chart symbol/period), `series.rs :: SliceSeries`, `vars.rs :: VarStore`, `inputs.rs :: InputDef` (now with `Session`/`Source`), `inputs.rs :: InputSet` | C3 | M | One `RunCtx` per run replaces process-global singletons |
| C5 | Polars research frames | infra | `src/protopyne-kernels/src/research.rs`, `parity.rs`, `calibration.rs` (`compat.rs` documents the bridge) | `research.rs :: frame_of_bars`, `research.rs :: frame_of_values`, `research.rs :: rsrs` | C1 | L | Offline analysis only; never the hot loop |
| C6 | Pine compatibility matrix | cross-cutting | `src/protopyne-kernels/src/compat.rs` | `compat.rs :: COMPATIBILITY` (53 rows: 50 kernel + 3 native), `compat.rs :: compat_for`, `compat.rs :: assert_matrix_covers`, `compat.rs :: NATIVE_MATRIX_ROWS`, `compat.rs :: assert_native_matrix_covers` | C3 | L | Declared kernel-vs-Pine coverage, all `eps = 1e-9`; native rows are `native-libBars` (ring/tick/resample surface) + `native-market-context` (the `MarketContext` view over `libApex`) + `native-libBuffers` (the `BufferFloat` hot methods), all `ParityVerified` on freeze-green; spike follows it |
| C7 | Proposal lifecycle | feature | `src/protopyne-strategy/src/proposal.rs` | `proposal.rs :: Proposal`, `proposal.rs :: transition`, `proposal.rs :: ProposalState`, `proposal.rs :: ProposalEvent` | C1 | H | Pure `transition` fn; `apply` returns a caller-persisted journal entry |
| C8 | Signal/structure composition | feature | `src/protopyne-strategy/src/elements.rs` | `elements.rs :: Strategy`, `elements.rs :: Signal`, `elements.rs :: FixedSignal`, `elements.rs :: ExplicitStructure` | C7 | M | `Arc`-composed signal/structure/sizing/loss-monitor roles |
| C9 | Mandatory risk gate | feature | `src/protopyne-strategy/src/risk.rs` | `risk.rs :: RiskEngine`, `risk.rs :: RiskLimits`, `risk.rs :: RiskApproved` | C7 | H | Token minted only by `check`; `safe_submit` demands it (C15) |
| C10 | Arming grants | feature | `src/protopyne-strategy/src/grant.rs` | `grant.rs :: Grant`, `grant.rs :: evaluate_grant`, `grant.rs :: GrantDecision`, `grant.rs :: GrantUsage` | C7 | H | Pure evaluator shared by backtest/paper/preview; tighten-only |
| C11 | Order vocabulary + deterministic IDs | infra | `src/protopyne-venue/src/order.rs` | `order.rs :: OrderIntent` (multi-leg attachment), `order.rs :: OrderRecord`, `order.rs :: client_id_for`, `order.rs :: ActiveOrderState`, `order.rs :: TimeInForce`, `order.rs :: qty_of`, `order.rs :: Position` | C1 | H | Active-vs-terminal states follow the barter-rs pattern; `ActiveOrderState` is the single active/terminal predicate |
| C12 | Quotes, liquidity, slippage, fees | feature | `src/protopyne-venue/src/quote.rs` | `quote.rs :: Quote`, `quote.rs :: LiquidityFilter`, `quote.rs :: Slippage`, `quote.rs :: FeeSchedule` | C1 | H | Refusals carry reasons; 10 bps default frozen by calibration |
| C13 | Paper venue + fault injection + spread fills | feature | `src/protopyne-venue/src/paper.rs` | `paper.rs :: PaperVenue`, `paper.rs :: Fault`, `paper.rs :: spread_positions`, `paper.rs :: set_leg_quotes` | C11, C12, C41 | H | Market-orders-only mock; atomic refuse-or-fill spread packages (private `decide_spread`) with per-leg reasons and net debit/credit gating; idempotent resubmit; kill-mid-submit tests |
| C14 | Journal + capability gate + leg fills | infra | `src/protopyne-venue/src/journal.rs` | `journal.rs :: Journal`, `journal.rs :: VenueStorage`, `journal.rs :: Journal :: replay`, `journal.rs :: JournalEvent`, `journal.rs :: ReplayedState`, `journal.rs :: LEG_FILL_KIND` | C11 | M | SQLite WAL; intent-before-venue; transactions via `fluent-db`; permit bound to file + task capabilities; additive `leg_fill` rows readable by old readers |
| C15 | Safe submission protocol | feature | `src/protopyne-venue/src/submit.rs` | `submit.rs :: safe_submit`, `submit.rs :: SUBMIT_TIMEOUT`, `submit.rs :: SUBMIT_RETRIES` | C9, C13, C14 | M | Journal → dry-run → submit → query-by-ID; retries never blind-resubmit; closes/cancels from C40 flow through this same path |
| C16 | Reconciliation gate (net + OCC legs) | feature | `src/protopyne-venue/src/reconcile.rs` | `reconcile.rs :: reconcile`, `reconcile.rs :: reconcile_legs`, `reconcile.rs :: journal_leg_net`, `reconcile.rs :: ReconcileGate` | C11, C41 | M | Pure journal-vs-venue compare (net + per-OCC leg nets sharing one mismatch core); mismatch halts entries, exits stay open |
| C17 | Health gate | cross-cutting | `src/protopyne-venue/src/health.rs` | `health.rs :: may_trade`, `health.rs :: check_trade`, `health.rs :: HealthLimits`, `health.rs :: HealthState`, `health.rs :: HealthVerdict` | — | M | Quote age / clock skew / stream state; refusals, never warnings |
| C18 | Supervised trading session (registry-bound) | feature | `src/protopyne-venue/src/session.rs` | `session.rs :: TradingSession`, `session.rs :: boot`, `session.rs :: spawn_feed` (Bar pipe over `mpsc`), `session.rs :: spawn_event_feed` (`MarketEvent` over `broadcast`), `session.rs :: check_unit`, `session.rs :: run_bar`, `session.rs :: resolve_quote`, `session.rs :: SubmitJob`, `session.rs :: registry` | C14, C15, C16, C17, C39 | M | Owns journal file + venue tasks; binds `Arc<VenueRegistry>`, not `Arc<PaperVenue>` (single-venue callers pass `VenueRegistry::single`, byte-identical); credit-bounded submits (O11); per-bar checks as supervised units; quote ladder via `first_accept_in_order`; halted bars gate entries while exits route (C40) |
| C19 | CSV history ingest | infra | `src/protopyne-server/src/csv.rs` | `csv.rs :: load_bars_csv`, `csv.rs :: parse_bars_csv` | C1 | M | Header-tolerant, row failures carry 1-based lines; trimmed ingest goldens (11-row 15M, 32-row 5M) |
| C20 | Versioned backtest venue | feature | `src/protopyne-server/src/backtest.rs` | `backtest.rs :: BacktestVenue`, `backtest.rs :: quote_for_bar`, `backtest.rs :: load_bars_with_fallback`, `backtest.rs :: HistorySource`, `backtest.rs :: load_history` | C12, C13, C19 | M | Delegates every `Venue` call to paper fills; `HistorySource::{Csv,Dynamic}`; first-accept source fallback |
| C21 | Runner, parity, sweeps, replay, multi-symbol | feature | `src/protopyne-server/src/runner.rs` | `runner.rs :: run_backtest`, `runner.rs :: run_multi_backtest`, `runner.rs :: MultiSymbolInput`, `runner.rs :: MultiBacktestRun`, `runner.rs :: run_paper_decisions`, `runner.rs :: decide_and_submit`, `runner.rs :: checked_order_for`, `runner.rs :: side_for_direction`, `runner.rs :: run_sweep`, `runner.rs :: build_replay`, `runner.rs :: complete_replay`, `runner.rs :: equity_curve_for` (`Vec<Money>`), `runner.rs :: walk_forward_split`, `runner.rs :: snapshot_positions`, `runner.rs :: expire_position`, `runner.rs :: report_rsrs`, `runner.rs :: triple_barrier` | C8, C9, C10, C20, C35 | M | One shared decide path (multi-symbol shares one tuned venue set with per-symbol reproducibility pins); `ResultPool`-only sweeps; `CheckpointedStepGraph` replay (research-only); `report_rsrs` runs once per run, not per bar |
| C22 | Manifest + honest report + tearsheet | cross-cutting | `src/protopyne-server/src/report.rs`, `src/protopyne-server/src/metrics.rs`, `src/protopyne-server/src/portfolio.rs` | `report.rs :: RunManifest`, `report.rs :: BacktestReport`, `report.rs :: max_drawdown`, `report.rs :: worst_step_loss`, `report.rs :: capital_efficiency`, `report.rs :: count_unquotable_bars`, `report.rs :: build_report`, `report.rs :: render_tearsheet`, `metrics.rs :: sharpe/sortino/calmar/win_rate/profit_factor` (C43), `portfolio.rs :: aggregate` (C44) | C21, C43, C44 | M | Deterministic JSON; assumptions/limits beside metrics; greeks marked unavailable; zero denominators report `None`, never invented values; text tearsheet renders from the same structs as the JSON report |
| C23 | HTTP service + WS + chart-data contract | feature | `src/protopyne-server/src/service.rs` | `service.rs :: serve`, `service.rs :: ServerConfig`, `service.rs :: ServerHandle`, `service.rs :: AppState`, `service.rs :: CandleRow`, `service.rs :: OverlaySeries`, `service.rs :: ProposalView`, `service.rs :: PortfolioView`, `service.rs :: PreviewParams`, `service.rs :: ArmResponse`, `service.rs :: WorstCase`, `service.rs :: CommandStatus`, `service.rs :: is_tightening` (private helper) | C21, C3, C26, C40 | M | 9 routes (`/health`, `/proposals`, `/proposals/preview`, `/grants/arm`, `/grants/current`, `/portfolio`, `/chart/bars`, `/chart/overlays`, `/feed`); `/health` additively carries `trading_state` + `allows_entries/exits` + `last_command` (C40); `/portfolio` reports the `PortfolioView` with trading state; overlay allowlist is `sma`/`ema`/`bb` (mid)/`trima` only (`mult` query param for `bb`, default 2.0); `decimal_to_chart` delegates to `chart-model :: to_display_float`; `Limiter` concurrency cap; `Scope`-owned feed pump; tighten-only arming; boot gates under `SupervisedBatch`. Chart rows/overlays are JSON data contracts for the native terminal, not a browser-chart binding |
| C24 | Transpile spike + calibration | feature | `src/protopyne-server/src/transpile.rs` | `transpile.rs :: compile_spike`, `transpile.rs :: Hair`, `transpile.rs :: evaluate_lowered`, `transpile.rs :: calibrate`, `transpile.rs :: CalibrationTally` (`is_green` = 1.0 accept + 1.0 reject) | C3, C6 | L | Accept-or-refuse HIR for `sma`/`ema`/`trima`/`midpoint` only (8 `DENY_PATTERNS`, version-6 gate; multi-leg `bbands`, nullary price kernels, and leg-hungry `midprice` stay refused); calibration sets are 9-must-transpile / 12-must-reject (`tests/transpile.rs`) |
| C25 | Binary + container ops | infra | `src/protopyne-server/src/main.rs`, `src/protopyne-server/Containerfile`, `src/protopyne-server/smoke-container.sh` | `main.rs :: main`, `main.rs :: parse_args` (private) | C23 | L | `--config/--bind/--history/--help/--version`; two-stage image; `make protopyne-smoke` |
| C26 | Headless chart model (display boundary) | infra | `src/protopyne-chart-model/src/{convert,chart_type,session,range,transform}.rs` | `convert.rs :: to_display_float`, `convert.rs :: row_of`, `convert.rs :: rows_of`, `convert.rs :: ChartRow`, `chart_type.rs :: ChartType` (20 variants), `session.rs :: find_session_breaks`, `range.rs :: DateRange`, `transform.rs :: heikin_ashi`, `transform.rs :: renko_bricks` | C1 | L | `egui`-free by construction; `Decimal`→`f64` happens only here; only consumer is `server/service.rs` (C23) |
| C27 | Headless scale engine | infra | `src/protopyne-chart-scale/src/{mapping,ticks}.rs` | `mapping.rs :: ChartMapping` (= `PriceScale`), `mapping.rs :: PriceScaleMode`, `mapping.rs :: PriceRange`, `ticks.rs :: nice_ticks`, `ticks.rs :: Tick` | — | L | Pure math, `egui`-free; no protopyne dependencies; no production consumer yet inside this workspace |
| C28 | Overlap/price/momentum/stat/volume kernels | feature | `src/protopyne-kernels/src/{ta_overlap,ta_price,ta_momentum,ta_stat,ta_volume}.rs` | `ta_overlap.rs :: TrimaIndicator`, `ta_price.rs :: TypPriceIndicator`, `ta_momentum.rs :: AroonIndicator`, `ta_stat.rs :: CorrelIndicator`, `ta_volume.rs :: ObvIndicator` (et al.; 26 kernels total) | C3 | M | Stateful windowed functions only (Naive-A split); registered in `builtin_registry` with matrix rows; multi-pass kernels compose child states (`trima` via `SmaIndicator`, `adosc` via `EmaIndicator`) |
| C29 | Candle predicates | feature | `src/protopyne-kernels/src/candles.rs` | `candles.rs :: doji`, `candles.rs :: gap_up`, `candles.rs :: TA_EPSILON` | C1 | L | Free functions over `BarTick`, never `Indicator`s (no warmup, no registry, no matrix rows); thresholds frozen at 0 false-positives / full recall on control groups |
| C30 | WQ alpha combinators | feature | `src/protopyne-kernels/src/{wq_series,wq_half_life,wq_comb}.rs` | `wq_series.rs :: ts_delay`, `wq_half_life.rs :: hl_weights`, `wq_comb.rs :: comb_clamp` | C3 | L | Free functions over `&[f64]`/scalars, never registered; `min_periods`-as-warmup convention per fn; M5-operator overlaps delegate to `util.rs` (no second `add`) |
| C31 | Parity harness + calibration gate | cross-cutting | `src/protopyne-kernels/src/{parity,calibration}.rs` | `parity.rs :: expr_fixture`, `parity.rs :: assert_expr_parity`, `calibration.rs :: ParityDashboard`, `calibration.rs :: run_control`, `calibration.rs :: canonical_params` | C3, C6 | M | Research-only `polars` (never the hot loop); dashboard green is 1.0 accept at frozen eps + 0.0 control false-fire; nothing caches parity output until green |
| C32 | Option-chain vocabulary | infra | `src/protopyne-types/src/chain.rs` | `chain.rs :: ContractId`, `chain.rs :: PortfolioId`, `chain.rs :: OptionContract`, `chain.rs :: OptionKind`, `chain.rs :: Greeks`, `chain.rs :: ChainQuote`, `chain.rs :: SpreadLeg`, `chain.rs :: SpreadLegs`, `chain.rs :: LegSide`, `chain.rs :: Portfolio`, `chain.rs :: MAX_SPREAD_LEGS` | C1 | H | Newtype IDs never mix (`ContractId` vs `PortfolioId` is a compile error); `SpreadLegs::try_new` refuses naked-short leg sets with reasons, never clamps; crossed quotes constructible (fill policy must refuse them, C13) but `midpoint` returns `None`; OCC symbol formatting; `Portfolio` derives `FieldAccess`/`Describable` for control-plane membership |
| C33 | Scoped symbol registry | infra | `src/protopyne-types/src/symbol.rs` | `symbol.rs :: SymbolIndex`, `symbol.rs :: SymKey`, `symbol.rs :: DEFAULT_MAX_SYMBOLS` (4_096) | — | H | Per-session/run interner, never process-global: paper book, portfolio registry, and preview attribution each hold their own; `SymKey` is opaque `Copy` (equality/hash only); reads never insert; serialization keeps strings, keys never leave the index |
| C34 | Fail-closed `Validate` trait + execute bridges | cross-cutting | `src/protopyne-types/src/validate.rs` | `validate.rs :: Validate`, `validate.rs :: check_for_execute`, `validate.rs :: valid_output` | — | H | Unifies the nine ad-hoc `validate()` shapes (per-type error types preserved); every supervision `execute` gate routes through `check_for_execute`. Closes extraction candidate E1 |
| C35 | RSRS generator + rebalancer | feature | `src/protopyne-strategy/src/generator.rs` | `generator.rs :: Generator`, `generator.rs :: GeneratorBuilder`, `generator.rs :: GenerateInput`, `generator.rs :: Exposure`, `generator.rs :: RebalanceMode`, `generator.rs :: Rung`, `generator.rs :: ladder_rung`, `generator.rs :: portfolio_for`, `generator.rs :: rebalance`, `generator.rs :: rsrs_score`, `generator.rs :: utility` | C7, C32 | M | RSRS-gated vertical proposals with scored ranking; `report_rsrs` (C21) runs the same scorer once per run |
| C36 | Normalized market events + pump/fan-out | infra | `src/protopyne-venue/src/market.rs` | `market.rs :: MarketEvent` (`Quote`/`Trade`/`BookDelta`/`Candle`), `market.rs :: MarketEventKind`, `market.rs :: matches`, `market.rs :: symbol_key` | C12, C33 | M | One event enum over existing payloads (no new price/fill/candle types); one `Scope`-owned pump per session, one `broadcast` fan-out, per-receiver symbol/kind filtering; lives in venue (not types) so no dependency inverts |
| C37 | Depth book + manager + dark detectors | feature | `src/protopyne-venue/src/depth.rs` | `depth.rs :: DepthBook`, `depth.rs :: DepthDelta`, `depth.rs :: PriceLevel`, `depth.rs :: BookManager`, `depth.rs :: DarkFinding`, `depth.rs :: is_crossed`, `depth.rs :: is_stale`, `depth.rs :: DEFAULT_MAX_LEVELS` (64) | C33, C36 | M | Pure B-tree-level engine: sequence-gap refusal, snapshot-replace/delta-apply, per-symbol manager; crossed/stale detectors record-only (dark) until calibrated, pinned by a frozen-const suite at 1.0 accept/recall/precision |
| C38 | Streaming supervision + breaker | cross-cutting | `src/protopyne-venue/src/stream.rs` | `stream.rs :: SupervisedStream`, `stream.rs :: StreamConfig`, `stream.rs :: StreamBreaker`, `stream.rs :: StreamHealthUnit`, `stream.rs :: StreamOutcome`, `stream.rs :: terminal_reason`, `stream.rs :: STREAM_HEALTH_ASSET` | C17 | M | Terminal-vs-retryable classification, jittered-backoff reconnect inside supervised batches with clean abort, sticky consecutive-error/restart-rate breaker that disarms entries only (pricing/fills untouched); frozen by a soak calibration suite |
| C39 | Venue routing registry | infra | `src/protopyne-venue/src/registry.rs` | `registry.rs :: VenueRegistry`, `registry.rs :: single`, `registry.rs :: add_route` | C11 | M | `Arc<dyn Venue>` handles, no `enum Venue` + per-site `match`; exact-match routes over a scoped index, ordered `first_accept_in_order` failover on retryable errors, terminal refusals stop the chain, unrouted symbols refuse fail-closed, query-first cross-venue idempotency, unioned venue truth |
| C40 | Journal-first command plane | feature | `src/protopyne-venue/src/command.rs` | `command.rs :: CommandKind`, `command.rs :: CommandUnit`, `command.rs :: TradingState`, `command.rs :: TradingStateUnit`, `command.rs :: StateHandle`, `command.rs :: StateReplica`, `command.rs :: PortfolioRegistry`, `command.rs :: PortfolioView`, `command.rs :: close_all_intents`, `command.rs :: cancel_all_ids`, `command.rs :: replay_trading_state`, `command.rs :: OPERATOR_CLIENT_ID` | C14, C15 | M | halt/resume/close-all/cancel-all enter as journaled rows via capability-permitted writes before mutating; replay rebuilds last-wins; halted bars gate entries while exits route; closes/cancels flow through `safe_submit`; health/portfolio routes report trading status |
| C41 | Leg-aware position engine | infra | `src/protopyne-venue/src/position.rs` | `position.rs :: SpreadPosition`, `position.rs :: PositionFill`, `position.rs :: PositionSide`, `position.rs :: PositionExited`, `position.rs :: update_from_fill`, `position.rs :: calculate_pnl_realised/unrealised`, `position.rs :: calculate_price_entry_average` | C1, C32 | M | Pure functions: volume-weighted entry averaging, fee-inclusive realized PnL, exit-fee-adjusted unrealized PnL, open/increase/reduce/close/flip with proportional fee splits; per-OCC spread book feeds C16 |
| C42 | Tastytrade transport | infra | `src/protopyne-venue/src/tastytrade.rs` | `tastytrade.rs :: TastytradeTransport`, `tastytrade.rs :: TastytradeConfig`, `tastytrade.rs :: TransportError`, `tastytrade.rs :: transform_quote`, `tastytrade.rs :: transform_chain_quote`, `tastytrade.rs :: transform_account_event`, `tastytrade.rs :: validate_subscription` | C32 | L | Config-file transport with `retry_async` backoff (`RETRY_MAX_ATTEMPTS` 3, base 100ms, 50% jitter); pure transform fns map venue payloads onto `Quote`/`ChainQuote`/account events; subscription validation fail-closed |
| C43 | Risk-adjusted metrics module | cross-cutting | `src/protopyne-server/src/metrics.rs` | `metrics.rs :: sharpe`, `metrics.rs :: sortino`, `metrics.rs :: calmar`, `metrics.rs :: win_rate`, `metrics.rs :: profit_factor`, `metrics.rs :: mean`, `metrics.rs :: step_returns`, `metrics.rs :: TRADING_DAYS_PER_YEAR` (252) | C1 | M | Pure `Decimal`-slice math (wires into `BacktestReport`, C22); zero denominators yield `None`, never invented values; annualization assumptions documented |
| C44 | Portfolio aggregation + tearsheet | feature | `src/protopyne-server/src/portfolio.rs` | `portfolio.rs :: Aggregation`, `portfolio.rs :: aggregate`, `portfolio.rs :: BookedFill`, `portfolio.rs :: BucketKey`, `portfolio.rs :: PortfolioReport`, `portfolio.rs :: PAPER_PORTFOLIO` | C41, C43 | M | Decimal aggregation by portfolio/underlying/structure/strategy/grant with per-mark unrealized curves; closed-trade and curve-based stats reuse C43; additive report fields; text tearsheet renders from the same structs as the JSON report |
| C45 | Multi-symbol harness + history sources + benches | feature | `src/protopyne-server/src/runner.rs`, `src/protopyne-server/src/backtest.rs`, `src/protopyne-server/benches/backtest_path.rs` | `runner.rs :: run_multi_backtest`, `runner.rs :: MultiSymbolInput`, `runner.rs :: MultiBacktestRun`, `backtest.rs :: HistorySource`, `backtest.rs :: load_history` | C20, C21 | M | One decide path and one tuned venue set across symbols with per-symbol reproducibility pins; CSV-or-dynamic history; criterion benches with recorded baselines (`decide_path` ~86µs, `sweep_fanout` ~416µs, `multi_fanout` ~235µs, `native_vs_pine` ≈140× observed elimination, never gated); documented bounds on every fan-out (broadcast depths, pool queues, credit intake, lag-skip) |
| C46 | Pine language front-end + canonical renderer | infra | `src/protopyne-kernels/src/pine/{lexer,syntax,display}.rs` | `syntax.rs :: parse_program`, `syntax.rs :: PineStmt` (multi-decl/assign, for-in, break, bare switch), `syntax.rs :: PineExpr` (instance calls, field reads, color literals), `display.rs :: display_program` | — | L | Renderer re-emits source that re-parses structurally equal (re-indented, comments dropped); only documented lossy case is `#rrggbbaa` alpha scaling |
| C47 | Pine evaluator + headless injection/tick harness | feature | `src/protopyne-kernels/src/pine/{eval,modules,source}.rs` | `eval.rs :: PineEngine`, `eval.rs :: run_linked`, `eval.rs :: run_linked_ticks`, `eval.rs :: inject_security`, `eval.rs :: inject_lower_tf`, `eval.rs :: LtfDataset`, `eval.rs :: LinkedModule`, `eval.rs :: set_native_options`, `eval.rs :: try_native_call` (private dispatch), `modules.rs :: resolve_imports`, `modules.rs :: ModuleGraph`, `source.rs :: PineSource`, `source.rs :: load_pine_source`, `source.rs :: SUPPORTED_PINE_VERSION` (6) | C1, C3, C4 | L | Multi-module linked execution over bar history; tick emulation with realtime cursor flags; `request.*` served only from injected datasets (confirmed-bar, `gaps_off`+`lookahead_off`), everything else refuses; plot/barcolor/fill captures plus label/table/box/line drawing events on `RunOutcome`; single-threaded, in-repo drivers are kernels/server test targets (C49); `run_method` consults `try_native_call` after arg evaluation (explicit opt-in via `set_native_options`, engine default stays Pine execution; tick runs only, no lower-tf injection registered; any guard closed or provider decline runs the Pine body unchanged) |
| C48 | Pine built-ins + indicator bridge | feature | `src/protopyne-kernels/src/pine/{builtins_core,builtins_ta}.rs` | `builtins_core.rs :: matrix.*`/`array.*`/`map.*` (general keys)/`input.session`/`input.source`, `builtins_core.rs :: color_of_hex`, `builtins_ta.rs :: TaBridge`, `builtins_ta.rs :: batch_security` | C1, C3 | L | Bridge serves kernel-backed single-source `ta.*` over injected security data; window/multi-leg and chart-leg calls refuse; zero denominators read `na`, never abort |
| C49 | Pine parity matrix + freeze + closure fixtures | cross-cutting | `src/protopyne-kernels/src/pine/{egui_parity,closure}.rs`, `doc/protopyne-rs/INJECTION_CONTRACT.md` | `egui_parity.rs :: EGUI_PARITY` (207 rows: 160 `Match`, 28 `Correct`, 19 `Refuse`), `egui_parity.rs :: parity_for`, `egui_parity.rs :: ParityVerdict` (Match/Correct/Refuse), `closure.rs :: REAL_LIBRARIES` (7 pinned), `closure.rs :: load_closure_sources`, `closure.rs :: INDICATOR_PATH` | C47, C48 | L | Every reference-layer construct maps to identical behavior, a pinned divergence, or a fail-closed refusal with an executable probe; trimmed real-tape tick windows plus recorded baselines (`tests/data/ticks/`, `tick_harness_baselines.json`); headless end-to-end proof with timeframe goldens in server tests; native-vs-Pine freeze (`tests/native_parity_freeze.rs`: tick windows, regime tapes, SPY 1M/5M/15M agreement goldens at max diff 0.0) recorded in `COMPATIBILITY`, never as rows here (see the `EGUI_PARITY` const docs); live-vs-injected contract documented in `INJECTION_CONTRACT.md` |

## 5. Concurrency & Orchestration Index

| ID | Pattern | Where (path :: Symbol) | Mechanism | Fitting Rust primitive |
|---|---|---|---|---|
| O1 | Supervised per-bar units | `src/protopyne-venue/src/session.rs :: check_unit`, `session.rs :: run_bar`, `src/protopyne-server/src/service.rs :: serve` (boot gates) | `ComponentAdapter` with execute-override wrapped in `Instrumented`, registered in `SupervisedBatch`; failures cancel dependents; duplicate names surface as `Failed` | `JoinSet` + structured abort (already the underlying shape) |
| O2 | Server-owned looping tasks | `src/protopyne-venue/src/session.rs :: spawn_event_feed`, `src/protopyne-server/src/service.rs :: serve` (feed pump) | `Scope::spawn` future with `sleep` cadence; `Scope::close` on shutdown; session pump publishes `MarketEvent` (C36) to one `broadcast` fan-out (`spawn_feed` is the separate Bar-pipe helper over `mpsc`) | `tokio::task::JoinSet` + `CancellationToken` |
| O3 | Endpoint concurrency cap | `src/protopyne-server/src/service.rs :: preview` (private) via `Limiter::run` | Semaphore permit around the preview future | `tokio::sync::Semaphore` (already the underlying shape) |
| O4 | Parallel sweep workers | `src/protopyne-server/src/runner.rs :: run_sweep` | `ResultPool::new` + `submit` per variant + `shutdown`; outputs re-sorted by index | `tokio::task::JoinSet` or Rayon (CPU-bound alternative) |
| O5 | Broadcast fan-out | `src/protopyne-server/src/service.rs :: feed` | `tokio::sync::broadcast` channel; lagged receivers skip, closed ends stream | `tokio::sync::broadcast` (as-is) or `watch` for latest-only |
| O6 | Shared in-memory views | `src/protopyne-server/src/service.rs :: AppState` | `std::sync::Mutex` + `common_core::sync::lock` (poison-recovering) | `std::sync::Mutex` / `parking_lot::Mutex` (no await across lock) |
| O7 | Retry with backoff + timeout | `src/protopyne-venue/src/submit.rs :: safe_submit` | `common_core::retry::retry_async` + `tokio::time::timeout`, resolve via query-by-ID | `tokio::time::{sleep, timeout}` (already the shape) |
| O8 | Ordered fallback | `src/protopyne-server/src/backtest.rs :: load_bars_with_fallback` (sync ladder), `src/protopyne-venue/src/session.rs :: resolve_quote` (async ladder over `QuoteSource`s), `src/protopyne-venue/src/registry.rs :: VenueRegistry` (routed-venue-first chain, O12) | `first_accept_in_order_sync` / `first_accept_in_order`; `BadHeader` stops, `BadRow`/`Io` skip; registry stops on terminal refusals, fails over on retryable errors | Plain loop over fallible sources (already the shape) |
| O9 | Fault injection + timing control | `src/protopyne-venue/src/paper.rs :: Fault`, `src/protopyne-venue/src/paper.rs :: inject` | Queued faults consumed per submit; deterministic in tests | `tokio::sync::mpsc` or `VecDeque` (already the shape) |
| O10 | Graceful shutdown | `src/protopyne-server/src/service.rs :: ServerHandle`, `src/protopyne-server/src/main.rs :: main` | `Scope::close` then abort serve task; binary awaits `ctrl_c` | `tokio::signal` + `CancellationToken` |
| O11 | Credit-bounded submit flow | `src/protopyne-venue/src/session.rs :: TradingSession` (private `credit_tx`/`credit_rx` fields) | `flow::new(CreditSpec)` pair; `is_blocked` gates intake, `recv` releases per submit; bound is correctness-as-boundedness | `tokio::sync::Semaphore` with acquire/release (already the underlying shape) |
| O12 | Registry-ordered venue failover | `src/protopyne-venue/src/registry.rs :: VenueRegistry` (`Venue::submit`/`cancel` via `impl Venue`, private `query_chain` helper) | `first_accept_in_order` over the routed-venue-first chain; query-first idempotency across the chain; lock held only across sync map work, never across `await` | `tokio::sync::Mutex` would add waker overhead for a microsecond critical section; `std::sync::Mutex` + `common_core::sync::lock` (already the shape) |
| O13 | Supervised reconnect with breaker | `src/protopyne-venue/src/stream.rs :: SupervisedStream`, `stream.rs :: StreamBreaker`, `stream.rs :: StreamHealthUnit` | Jittered-backoff reconnect inside supervised batches with clean abort; sticky `StreamBreaker` trips on consecutive errors / restart rate and disarms entries only; `StreamHealthUnit` exposes the breaker as a supervised unit | `tokio::time::sleep` + `CancellationToken` (no token currently; abort via batch) |
| O14 | Journal-first operator commands | `src/protopyne-venue/src/command.rs :: CommandUnit`, `command.rs :: TradingStateUnit` | `WorkUnit`s whose `execute` validates via `check_for_execute` (C34), journal first, then mutate; replay (`replay_trading_state`, `StateReplica`) rebuilds last-wins | `SupervisedBatch` units (already the shape) |

## 6. Reuse Hotspots and DRY Findings

Most-imported internal modules (by files with a textual reference,
recounted 2026-10-12):

1. `protopyne-types` (39) — `Money`, `Bar`, `Qty`, plus the newer
   `chain`/`symbol`/`validate` vocabulary are the true core; any
   extraction starts here (already a crate, correctly so).
2. `serde` (31), `chrono` (28), `rust_decimal` (28), `bon` (22) — foundation sprawl is
   healthy; no action.
3. `common-core` (18 files) — the `sync::lock` mutex idiom is
   uniform; a good sign. Also funnels `retry_async`, `load_json_or_default`,
   `impl_from_io_error!`, and `registry::ConcurrentRegistry`.
4. `fluent-wvr` (15) / `fluent-concurrency` (11) — every supervision
   `execute` body now bridges through `check_for_execute` (C34); `Scope`,
   `SupervisedBatch`, `ResultPool`, credit, and the ladder combinator cover
   all venue/server concurrency (O1–O14).
5. `Venue` trait + `PaperVenue` + `VenueRegistry` (4–5) — fan-in widened by
   the registry (C39): server + session + registry tests. The second live
   venue (tastytrade transport, C42) composes the same trait; routing only
   selects handles, `safe_submit` stays the only submission path.
6. `frame_of_bars`/`frame_of_values` (research tests only) — thinnest reuse in the
   tree; the production display path now lives in `chart-model`
   (`to_display_float`/`row_of`), so these stay offline-only by design;
   see §7.
7. Pine engine (`pine/`) in-repo drivers are test targets only (kernels
   suites, server headless proof): no production `src/` path outside
   kernels drives `PineEngine`; the live trading path uses C3 kernels
   and the C24 spike, never the language runtime.

Near-duplicate logic (unification signals, not yet violations):

- ~~`runner.rs` skipped-`BarDecision` literals in four places~~ RESOLVED:
  `run_backtest`, `run_paper_decisions`, and `decide_and_submit` all share
  the `skipped_decision` helper now (8 call sites, one constructor).
- ~~Fail-closed `validate()` on nine validators~~ RESOLVED (E1 done):
  the `Validate` trait (`types/validate.rs`, C34) unifies the shape with
  per-type error types preserved, and `check_for_execute`/`valid_output`
  bridge every supervision `execute` body through it.
- `Money::from(0)` vs `Money::zero()` both in use (`service.rs` uses the
  former twice; `grant.rs`, `position.rs`, `portfolio.rs`, `runner.rs` the latter); pick one.
- Grant-builder boilerplate is duplicated between strategy tests and
  server API tests (`tests/api.rs :: test_grant` / `tests/runner.rs :: grant_with`); a shared test-fixture
  helper would remove a drift risk.
- `decimal → f64` fallible conversions are half-centralized:
  `service.rs :: decimal_to_chart` now delegates to
  `chart-model :: to_display_float`, but kernel-internal smoothing
  (`ta_volume.rs`, `volatility.rs`, `ta_stat.rs`, `averages.rs`) and
  research-frame conversion still carry per-site
  `unwrap_or(f64::NAN)` fallbacks; finish the migration to the one helper so
  the skip-vs-NaN policy lives in exactly one place (C26).

## 7. Extraction Candidates

| Candidate | Capability IDs | Why generic/valuable | Trait/type sketch | Priority |
|---|---|---|---|---|
| E1. Fail-closed `Validate` trait | C34 | ~~Nine ad-hoc `validate()`s with five error types~~ DONE 2026-10-10: `types/validate.rs :: Validate` (shape unified, per-type `Err` preserved) + `check_for_execute`/`valid_output` bridges on every supervision `execute` body | `trait Validate { type Err: Display; fn validate(&self) -> Result<(), Self::Err>; }` (exists) | Done |
| E2. Deterministic manifest pattern | C22 | Any reproducible run needs data/code/config pin + canonical JSON + rerun-equality test; multi-symbol runs add per-symbol pins (C45) | `struct RunManifest<V, C> { data_version: String, code_version: &'static str, config: C, span: V }` with `to_canonical_json` | M |
| E3. Backtest analytics (drawdown/tail/efficiency/equity) | C21, C22, C43 | ~~Pure `Decimal`-slice math reused by every future report~~ PARTLY DONE: extracted as `server/metrics.rs` (C43) and reused by report + portfolio (C44); remaining step is moving it to a shared home if a second consumer (second server, terminal) appears | `fn max_drawdown(equity: &[Decimal]) -> Decimal` et al. as a `metrics` module (exists in server); inputs `Copy`, no I/O | M |
| E4. Accept-or-refuse subset recognizer | C24, C46–C49 | ~~The compile-or-refuse scaffold generalizes to any future script coverage~~ REALIZED at language scale: the Pine engine (C47) plus the 207-row checked parity matrix (C49) is the generalized form — every construct outside the matrix refuses with a reason instead of miscomputing. The transpile spike stays as the small-subset fast path (`sma`/`ema`/`trima`/`midpoint`) | `CalibrationTally` as-is; `parity_for` + `ParityVerdict` (C49) | M |
| E5. Execution-policy types | C12 | `LiquidityFilter`/`Slippage`/`FeeSchedule` are venue-independent; the registry (C39) reuses them unchanged across handles, and the second live venue (C42) will too | Existing structs already `Copy + Serialize`; only need a shared home if venue splits | H (on venue split) |
| E6. Chart-data contract rows | C23, C26, C27 | `ChartRow`/`ChartRows`/`row_of`/`rows_of`/`to_display_float` (chart-model) and `ChartMapping`/`nice_ticks` (chart-scale) are the native terminal's data contract — any future chart view or second server reuses them, never a browser-chart binding. `service.rs :: CandleRow`/`OverlaySeries` is the wire mirror; keep the two in step or unify behind the chart-model types | `struct ChartRow { time: i64, open/high/low/close: f64, volume: u64 }` (exists); `Send + Sync` | L |
| E7. Research-frame helpers | C5 | Thin but the only columnar bridge; worth keeping iff chain-analysis features land, else cut `polars` | `fn frame_of_bars(&[Bar]) -> Result<DataFrame, E>` as-is | L |

## 8. Notable Features Worth Reimplementing

- **C9 + C15 (risk token demanded by submission):** typestate makes the
  safety property unrepresentable to violate — the cheapest way to get a
  "cannot bypass" guarantee in any pipeline with a gate.
- **C14 (intent-before-venue + outbox + replay):** the kill-mid-submit
  guarantee from three primitives (durable intent, idempotent IDs,
  query-by-ID resolution) with no distributed machinery.
- **C10 (tighten-only arming with worst-case preview):** monotonic
  authorization (only narrower, never wider) plus showing the exposure
  before confirmation — directly portable to any approval workflow.
- **C21/C22 (shared decide path + byte-identical manifest):** one function
  for backtest and paper plus canonical-JSON rerun equality turns
  "what you test is what you trade" from slogan into test.
- **C24 (refuse-don't-miscompile spike):** accept rate and reject rate
  both gated at 1.0 *before* output may be cached — a calibration
  discipline applicable to any partial-coverage transpiler.
- **C26/C27 (headless chart leaves):** `Decimal`-native bars in,
  plain-data rows out, `egui`-free by construction — the display boundary
  as a pure function, so the paint edge can never smuggle authority or
  rounding back into decisions.
- **C34 (shared `Validate` + execute bridges):** one trait shape for every
  config surface with per-type errors preserved, and one bridge into
  supervision — the pattern for adding a tenth validator without a new
  idiom.
- **C39 (registry routing without an enum):** `Arc<dyn Venue>` handles +
  ordered fallback + query-first idempotency — how to add a second venue
  without touching `safe_submit` or any call site.
- **C40 (journal-first commands):** halt/resume/close-all/cancel-all as
  journaled rows with last-wins replay — operator control that survives a
  kill-mid-command.
- **C32/C41/C16 (chain → spread fills → OCC reconciliation):** defined-risk
  validation at construction, atomic per-leg fills, per-OCC leg-net
  compare — the full options-spread safety loop in three layers.
- **C44 (portfolio aggregation + tearsheet):** one `aggregate` over booked
  fills feeding both JSON report fields and a text tearsheet from the same
  structs — no second summarizer to drift.
- **C46–C49 (Pine engine + checked parity matrix):** a full language
  runtime (parse → link → run over bars/ticks) whose overlap contract
  with the reference layer is machine-checked — 207 rows of
  identical/pinned/refused, each refusal carrying an executable probe —
  so partial coverage can never silently miscompute; trimmed real-tape
  fixtures with recorded baselines pin the tick harness the same way.
  Whole-library native serving rides alongside the engine (not through
  it): `native-libBars` + `native-market-context` + `native-libBuffers`
  live in `COMPATIBILITY`
  (frozen by the SPY agreement goldens), serve by default with Pine
  fallback, and the `.pine` oracles stay byte-identical.

## 9. Gaps & Drill-Down Queue

Closed during the 2026-10-09 re-verification, re-confirmed 2026-10-10
(direct reads; no longer gaps unless noted):

- `session.rs` internals — `boot` (validates, `flow::new({initial:
  credit_limit, more_after: 1})`, `ResultPool` submit via registry),
  `spawn_event_feed` (publishes `MarketEvent`, C36), `run_bar` (installs
  `CapabilitySet(storage)`, wraps
  `Instrumented(ComponentAdapter)`, duplicate names → `Failed`),
  `resolve_quote` (async `first_accept_in_order` ladder), `shutdown`
  (scope close + pool drain). Session is now **registry-bound**
  (`Arc<VenueRegistry>`, C39) — the old "`Arc<PaperVenue>`-bound, not
  generic over `Venue`" note is superseded; single-venue callers pass
  `VenueRegistry::single` with byte-identical behavior.
- `compat.rs` matrix content — 53 rows (50 kernel + 3 native:
  `native-libBars`, `native-market-context`, `native-libBuffers`), all `eps = 1e-9`, all
  `ParityVerified` (numeric authority is `Expr` output; the direct
  export cross-check is superseded by the calibration gate; the native
  rows are frozen by `tests/native_parity_freeze.rs`).
- `paper.rs` fill internals — touch selection (`ask` buy / `bid` sell),
  marketability (`buy: limit >= touch`, `sell: limit <= touch`, else
  `Refused WouldRest`), slippage capped at limit, fee-overflow refusal,
  `LoseAck` commits-then-errors, idempotent resubmit via query-first;
  plus the newer `decide_spread` atomic spread path (C13) with per-leg
  `LegQuoteSource` injection and per-OCC `spread_positions`.
- `service.rs` routes — 9 total (see C23); `/health` additively carries
  `trading_state` + `allows_entries/exits` + `last_command` (C40);
  WS `/feed` skips lagged, ends on closed. Overlay allowlist is
  `sma`/`ema`/`bb`/`trima` only, `period` in `[1, 500]`.
- Spike accept-set boundary — 8 `DENY_PATTERNS`, `//@version=6` +
  `indicator(` required, `ta.sma`/`ta.ema`/`ta.trima`/`ta.midpoint` 2-arg
  only; calibration sets are 9-must-transpile / 12-must-reject at 1.0/1.0
  (`tests/transpile.rs`).
- Risk/grant denial coverage — `RiskEngine::check` collects all 8
  `RiskReason` arms; `evaluate_grant` denies with `String` reasons in fixed
  order (revoked → window → scope → budgets → signal → trade → portfolio →
  breaker).
- `Validate` trait + bridges (C34) — `types/validate.rs`; every
  supervision `execute` body routes through `check_for_execute`.
- Command plane (C40) — journaled halt/resume/close-all/cancel-all,
  `replay_trading_state` last-wins rebuild, `StateReplica` portfolio view.
- Registry routing (C39) — exact-match routes, ordered failover, terminal
  refusals stop the chain, unrouted symbols refuse fail-closed.
- Depth book (C37) — B-tree levels, sequence-gap refusal,
  snapshot-replace/delta-apply, dark crossed/stale detectors.
- Metrics + portfolio (C43/C44) — `server/metrics.rs` pure-`Decimal`
  helpers reused by report and aggregation; `render_tearsheet` reads the
  same structs as the JSON report.
- Pine engine shape (C46–C49, direct reads 2026-10-11) — `PineEngine`
  entry points are `run_linked` / `run_linked_ticks` / `inject_security`
  / `inject_lower_tf` (`eval.rs`); import linking is `resolve_imports`
  → `ModuleGraph` (`modules.rs`); source loading is `load_pine_source`
  with `SUPPORTED_PINE_VERSION = 6` (`source.rs`); the indicator bridge
  is `TaBridge :: batch_security` (kernel-backed single-source calls
  only);   the parity matrix is 207 rows (`EGUI_PARITY`) with
  `Match`/`Correct`/`Refuse` verdicts and executable probes on every
  refusal; the closure pins 7 real libraries plus the integration
  indicator (`REAL_LIBRARIES`, `INDICATOR_PATH`); baselines live in
  `tests/data/tick_harness_baselines.json` (times observed, never
  asserted) with literals in the tests.
- `decide_spread` visibility — private `PaperVenue` method, not a
  public export; the public spread surface is `spread_positions` /
  `set_leg_quotes` (C13 corrected).
- Tastytrade transform names — `transform_quote`,
  `transform_chain_quote`, `transform_account_event` (C42 corrected);
  retry constants confirmed in `tastytrade.rs` (`RETRY_MAX_ATTEMPTS` 3,
  base 100ms, 50% jitter).

Remaining low-confidence or unmeasured areas:

- `service.rs` WS backpressure posture under a slow subscriber (lag-skip
  is implemented; load behavior unmeasured).
- `run_sweep` / multi-symbol worker sizing (`clamp(1, 8)`) — throughput unmeasured.
- Kernel `update()` incremental-engine details beyond the
  `batch`/`update` agreement suites; `RunCtx` cursor semantics beyond the
  public fn list.
- `chart-scale` still has no in-workspace consumer (E6); first wiring will
  test whether `PriceScaleMode` covers the terminal's needs.
- Tastytrade transport (C42) against the live/cert sandbox — transform fns
  are unit-pinned; end-to-end soak behavior unmeasured.
- Anything outside `src/protopyne-*`: shared-library internals
  (`Scope`, `SupervisedBatch`, capability enforcement) were used as
  documented, not re-verified here.

Suggested drill-downs, ordered by value, each with a starter command
(run from repo root):

1. Risk gate completeness (all refusal arms):
   `grep -n "RiskReason::" src/protopyne-strategy/src/risk.rs`
2. Grant denial coverage (all evaluator arms):
   `grep -n "reasons.push" src/protopyne-strategy/src/grant.rs`
3. Fill-policy edge cases (marketability, partials, spreads):
   `grep -n "WouldRest\|partial\|marketable\|decide_spread" src/protopyne-venue/src/paper.rs`
4. Journal schema and replay fidelity (incl. leg fills):
   `grep -n "CREATE TABLE\|INSERT INTO\|LEG_FILL_KIND" src/protopyne-venue/src/journal.rs`
5. Session supervision depth:
   `ast-grep --json -l rust -p 'Scope::$M($$$)' src/protopyne-venue/src/session.rs`
6. Spike accept-set boundary (what exactly parses):
   `grep -n "DENY_PATTERNS\|fn parse_" src/protopyne-server/src/transpile.rs`
7. Endpoint inventory vs. router surface:
   `grep -n "\.route(" src/protopyne-server/src/service.rs`
8. Registry routing rules (order matters):
   `sed -n '1,30p' src/protopyne-venue/src/registry.rs`
9. Command-plane journaling (what is journaled, what replays):
   `grep -n "COMMAND_.*_KIND\|replay_trading_state\|apply_one" src/protopyne-venue/src/command.rs`
10. Depth-book invariants (sequence gaps, dark detectors):
    `grep -n "sequence\|DarkFinding\|is_crossed\|is_stale" src/protopyne-venue/src/depth.rs`
11. Pine engine entry points (link → inject → run):
    `grep -n "pub fn resolve_imports\|pub fn run_linked\|pub fn inject_\|pub fn batch_security" src/protopyne-kernels/src/pine/eval.rs src/protopyne-kernels/src/pine/modules.rs src/protopyne-kernels/src/pine/builtins_ta.rs`
12. Parity-matrix verdict split:
    `grep -o "ParityVerdict::[A-Za-z]*" src/protopyne-kernels/src/pine/egui_parity.rs | sort | uniq -c`
13. Tick-harness baselines (recorded, never asserted):
    `ls src/protopyne-kernels/tests/data/ticks/ && grep -n "observed_secs\|\"ticks\"\|pull_count" src/protopyne-kernels/tests/data/tick_harness_baselines.json`
