# protopyne-rs — Architecture Map & Capability Index

## 0. Handoff for new agents

This document is the entry point. It maps all seven `protopyne-*` crates by
capability, with exact `path :: Symbol` references so you can jump straight
to the relevant file without re-exploring. Read it first, then drill into
the cited symbols. The two headless chart crates (`protopyne-chart-model`,
`protopyne-chart-scale`, §3 rows C26–C27) postdate the M1–M9 prototype and
are `egui`-free leaves; everything else follows the prototype plan.
Companion documents in `doc/protopyne-rs/`:

- `README.md` — what the project does, how to build/run/test it, CLI and
  container usage. Start here for operations.
- `VISION.md` — the design vision: economics, safety invariants, platform
  direction (aspirational goals and capabilities, no implementation
  details).
- `ROADMAP_20261008_PROTOPYNE_RS_PROTOTYPE.md` + `..._CHECKLIST.md` (repo
  root) — the milestone plan the current prototype implements and its
  completion state. The prototype implements milestones M1–M9; all
  checklist boxes are checked.
- `PROMPT_PRIMITIVE_REVIEW.md` + `..._CHECKLIST.md`,
  `PROMPT_FEATURE_PARITY.md` + `..._CHECKLIST.md`,
  `PROMPT_BUILD_PRIMITIVE.md` + `..._CHECKLIST.md`,
  `PROMPT_BUILD_PARITY.md` + `..._CHECKLIST.md` (repo root) — the
  forward program: reverse-engineer reference charting into monorepo
  idioms (analysis → design → primitives → parity), ending in a native
  `egui` terminal. Read these for anything not yet built.

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
   77 files under `src/*/src/` (types 8, kernels 32, strategy 6, venue 11,
   server 9, chart-model 7, chart-scale 4) plus 67 test files (~144 total).
  `tests/data/SPY_15M.csv` ships in kernels and server (385 lines = header +
  384 data rows). `make protopyne-test` gates the five trading crates
  (`types,kernels,strategy,venue,server`); the two chart crates are covered
  by the workspace build/test, not that target.
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
  keep parallel runs isolated.
- **Architecture summary:** layered pipeline `types → kernels → strategy
  → venue → server`, with `chart-model` / `chart-scale` as `egui`-free
  paint-edge leaves off `types` (bars) and `server` (chart rows): display
  `f64` appears only at the `to_display_float` conversion boundary and
  never flows back into decisions. Each layer owns its vocabulary and validates its own
  inputs fail-closed. The single decision path is
  `Signal → RiskEngine → evaluate_grant → Venue::submit`, executed
  identically by backtest replay, paper sessions, and HTTP preview, with
  byte-identical manifests (`to_canonical_json` rerun equality) proving it.

## 2. Dependency Map

Counts are files under `src/protopyne-*/src/` with a textual reference
(`use` or fully-qualified path), recounted 2026-10-09. "Workspace analogue" names the
in-repo counterpart where one exists.

| Dependency | Role | Usage breadth (files, approx.) | Workspace analogue / note |
|---|---|---|---|
| `protopyne-types` | Money/time/bar vocabulary | 22 (highest fan-in) | n/a (foundation itself) |
| `serde` / `serde_json` | Persistence, manifests, HTTP bodies | 25 / JSON in service, csv, reports | n/a |
| `chrono` | Timestamps, windows, DTE | 21 | n/a |
| `rust_decimal` | All money math | 19 | n/a |
| `bon` | Builder DSL for configs/intents | 15 | n/a |
| `common-core` | `retry_async`, `sync::lock`, `config::load_json_or_default`, `IoError`, `impl_from_io_error!`, `registry::ConcurrentRegistry` | 10 | n/a (shared lib) |
| `fluent-wvr` | Components, `FieldAccess`/`Describable`, capabilities, `ComponentAdapter`, `DynamicComponent` | 7 | n/a (shared lib) |
| `tokio` | Runtime, listener, sleep, signal, broadcast | 6 | n/a |
| `fluent-concurrency` | `Limiter`, `Scope`, `SupervisedBatch`, `ResultPool`, `flow` credit, fallback combinator | 6 | n/a (shared lib) |
| `thiserror` | Typed errors per crate | 7 (one `error.rs` per crate) | n/a |
| `csv` | History ingest | 4 (all in server) | n/a |
| `protopyne-strategy` | Proposals, risk, grants | 3 (venue + server) | n/a |
| `protopyne-venue` | Venue trait, paper, journal | 3 (server + self) | n/a |
| `protopyne-kernels` | Indicators for overlays/spike eval | 2 (server) | n/a |
| `fluent-dag` | `DependencyGraph`, `CheckpointedStepGraph` | 3 (kernels ×2, server) | n/a (shared lib) |
| `axum` | REST + WebSocket server | 2 (service, main via lib) | n/a |
| `rusqlite` | Journal durability (WAL) | 1 (`venue/journal.rs`) | n/a |
| `fluent-db` | Journal transactions (`query::transaction`) | 1 (`venue/journal.rs`) | n/a (shared lib, path `src/db`) |
| `protopyne-chart-model` | `Decimal`→display rows for service | 1 (`server/service.rs`) | n/a (leaf crate; server is its only consumer) |
| `polars` | Offline research frames only | 1 (`kernels/research.rs`) | n/a |
| `syn` | Emitted-Rust parse checks | 0 in `src` (dev-deps, spike tests only) | n/a |
| `reqwest`, `tokio-tungstenite`, `tempfile` | HTTP/WS test clients, temp dirs | 0 in `src` (dev-deps only) | n/a |

## 3. Module Map

`Package | Responsibility | Key exports`

| Directory/Package | Responsibility | Key exports (Symbol list) |
|---|---|---|
| `src/protopyne-types/src/` | Money, time, bars, windows, warmup math, price legs | `Money`, `Qty`, `Bar`, `validate_new_bar`, `Adjustment`, `Timeframe`, `BarWindow`, `BarGap`, `AdjustedBounds`, `adjust_fetch_bounds`, `trim_indicator_output`, `warmup_headroom`, `PriceSource`, `SeriesContext`, `GapPolicy`, `LtfView`, `TypesError` |
| `src/protopyne-kernels/src/` | Indicator kernels, registry, runtimes, research frames | `Indicator` (trait), `SmaIndicator`, `EmaIndicator`, `RsiIndicator`, `MacdIndicator`, `StochIndicator`, `AtrIndicator`, `BbIndicator`, `KcIndicator`, `TrIndicator`, `NatrIndicator`, averages (`Alma/Dema/Hma/Rma/Tema/Vwma/WmaIndicator`, all in `averages.rs` except `Sma`/`Ema`), overlap/price/momentum/stat/volume (`Bbands/Midpoint/Midprice/TrimaIndicator` in `ta_overlap.rs`, `AvgPrice/MedPrice/TypPrice/WclPriceIndicator` in `ta_price.rs`, `Apo/Mom/Ppo/Roc/Rocp/Rocr/Rocr100/Rsv/Stochf/Trix/Willr/AroonIndicator` in `ta_momentum.rs`, `Stddev/Var/CorrelIndicator` in `ta_stat.rs`, `Ad/Adosc/ObvIndicator` in `ta_volume.rs`), candle predicates (`doji`, `gap_up`, … in `candles.rs`, free functions), WQ combinators (`ts_*` in `wq_series.rs`, `hl_*` in `wq_half_life.rs`, scalar maps in `wq_comb.rs`, all free functions), `expr_fixture`/`assert_expr_parity` in `parity.rs`, `ParityDashboard`/`run_control`/`canonical_params` in `calibration.rs`, `RunCtx`, `PineRuntime`, `PineRuntimeConfig`, `SliceSeries`, `Var`/`VarIp`/`VarStore`, `Ohlcv`, `OwnedOhlcv`, `BarTick`, `FullTick`, `NamedSeries`, `ParamSet`, `IndicatorResult`, `IndicatorRegistry`, `ComponentRegistry`, `IndicatorComponent`, `InputDef`, `InputSet`, `PineValue`, `Drawing`, `execution_order`, `builtin_registry` (43 kernels), `compat_for`, `COMPATIBILITY`, `frame_of_bars`, `frame_of_values`, `instrumented_kernel`, `KernelUnit`, `KernelError`, math fns (`period_len`, `window_sum/mean`, `ema_factor/step`, `wilder_step`, `div_or_none`, `warmup_none`, `std_pop/std_sample`, `corr_pearson`, `linreg_slope_intercept`, `ewm_span_step`), util fns (`crossover`, `crossunder`, `highest`, `lowest`, `mom`, `change`, `cross_warmup`) |
| `src/protopyne-strategy/src/` | Proposals, signals, risk, grants | `Proposal`, `ProposalState`, `ProposalEvent`, `Direction`, `ProposalSide`, `StructureKind`, `JournaledTransition`, `transition`, `Strategy`, `Signal` (trait), `FixedSignal`, `ExplicitStructure`, `FixedContracts`, `HoldLoss`, `LimitMonitor`, `NullMonitor`, `RiskEngine`, `RiskLimits`, `RiskApproved`, `RiskRefused`, `RiskReason`, `RiskState`, `Grant`, `GrantScope`, `GrantPatch`, `GrantPermission`, `GrantBudgets`, `SignalThresholds`, `TradeThresholds`, `PortfolioThresholds`, `BreakerLimits`, `evaluate_grant`, `GrantDecision`, `GrantUsage`, `GrantMarket`, `BreakerState`, `StrategyError` |
| `src/protopyne-venue/src/` | Venue abstraction, paper fills, journal, submission, supervision | `Venue` (trait), `QuoteSource` (trait, lives in `venue.rs`), `DryRunOutcome`, `PaperVenue`, `PaperConfig`, `Fault`, `Quote`, `LiquidityFilter`, `Slippage`, `FeeSchedule`, `DEFAULT_SLIPPAGE_BPS`, `OrderIntent`, `OrderRecord`, `OrderStatus`, `OrderSide`, `Fill`, `CorporateEvent`, `Position` (single signed-`i64` struct), `client_id_for`, `Journal`, `JournalEvent`, `ReplayedState`, `VenueStorage`, `safe_submit`, `SUBMIT_TIMEOUT` (5s), `SUBMIT_RETRIES` (3), `reconcile`, `journal_net`, `ReconcileGate`, `ReconcileMismatch`, `ReconcileReport`, `may_trade`, `check_trade`, `HealthLimits`, `HealthState`, `HealthVerdict`, `TradingSession`, `SessionConfig`, `SubmitJob`, `check_unit`, `VenueError` |
| `src/protopyne-server/src/` | Ingest, backtest, HTTP, spike, binary | `load_bars_csv`, `parse_bars_csv`, `BacktestVenue`, `BacktestConfig`, `load_bars_single`, `load_bars_with_fallback`, `quote_for_bar`, `run_backtest`, `run_paper_decisions`, `decide_and_submit`, `checked_order_for`, `BacktestRun`, `BarDecision`, `BarHealth`, `CheckedOrder`, `RunnerConfig`, `SweepOutcome`, `equity_curve_for`, `walk_forward_split`, `snapshot_positions`, `expire_position`, `RunManifest`, `BacktestReport`, `build_report`, `max_drawdown`, `worst_step_loss`, `capital_efficiency`, `count_unquotable_bars`, `run_sweep`, `build_replay`, `complete_replay`, `ReplayStep`, `serve`, `ServerConfig`, `ServerHandle`, `AppState`, `CandleRow`, `OverlaySeries`, `compile_spike`, `evaluate_lowered`, `calibrate`, `CalibrationTally`, `ServerError` |
| `src/protopyne-chart-model/src/` | Headless chart data: display conversion, chart taxonomy, sessions, ranges, bar transforms (`egui`-free) | `ChartRow`, `ChartRows`, `row_of`, `rows_of`, `to_display_float`, `ChartType`, `ChartTypeCategory`, `DateRange`, `SessionProvider` (+ `Daily/Weekly/Monthly/CompositeSessionProvider`), `SessionBreak`, `find_session_breaks`, `heikin_ashi`, `renko_bricks`, `ChartModelError` |
| `src/protopyne-chart-scale/src/` | Headless scale engine: price modes, ranges, bar/price mapping, ticks (`egui`-free, depends on nothing protopyne) | `ChartMapping`, `PriceScale`, `PriceScaleMode`, `PriceRange`, `Tick`, `nice_ticks`, `ScaleError` |
| `src/protopyne-server/` (ops) | Container + smoke | `Containerfile`, `smoke-container.sh` (no Rust symbols) |
| `src/protopyne-*/tests/` | Per-module suites + `SPY_15M.csv` goldens (kernels + server) | Mirror source module names; `csv.rs` pins the 384-row ingest golden |

## 4. Capability Index

Fan-in: H (used across crates/files broadly), M (several callers), L (one or two). Depends-on uses capability IDs.

| ID | Capability | Kind | Files | Key symbols (path :: Symbol) | Depends on | Fan-in | Notes |
|---|---|---|---|---|---|---|---|
| C1 | Decimal money/qty vocabulary | infra | `src/protopyne-types/src/money.rs` | `money.rs :: Money`, `money.rs :: Qty` | — | H | `Copy`, `Decimal`-backed; `Money` and `Qty` never mix by construction |
| C2 | Bar windows, gaps, warmup math | infra | `src/protopyne-types/src/window.rs`, `src/protopyne-types/src/warmup.rs`, `src/protopyne-types/src/timeframe.rs` | `window.rs :: BarWindow`, `warmup.rs :: adjust_fetch_bounds`, `warmup.rs :: trim_indicator_output`, `timeframe.rs :: Timeframe` | C1 | M | Warmup/trim ported from the C ancestor's fetch-bounds logic |
| C3 | Indicator kernels + registry | feature | `src/protopyne-kernels/src/{sma,ema,averages,oscillators,volatility}.rs`, `src/protopyne-kernels/src/indicator.rs` | `indicator.rs :: Indicator`, `indicator.rs :: builtin_registry` (43 kernels), `indicator.rs :: execution_order`, `sma.rs :: SmaIndicator` (et al.; `Dema`/`Tema` also live in `averages.rs`) | C1, C2 | H | `batch` + `update`/`update_full` + `warmup` per kernel; ordering via `fluent-dag` |
| C4 | Run context + Pine runtimes | infra | `src/protopyne-kernels/src/context.rs`, `src/protopyne-kernels/src/runtime.rs`, `src/protopyne-kernels/src/series.rs`, `src/protopyne-kernels/src/vars.rs` | `context.rs :: RunCtx`, `runtime.rs :: PineRuntime`, `series.rs :: SliceSeries`, `vars.rs :: VarStore` | C3 | M | One `RunCtx` per run replaces process-global singletons |
| C5 | Polars research frames | infra | `src/protopyne-kernels/src/research.rs` | `research.rs :: frame_of_bars`, `research.rs :: frame_of_values` | C1 | L | Offline analysis only; never the hot loop |
| C6 | Pine compatibility matrix | cross-cutting | `src/protopyne-kernels/src/compat.rs` | `compat.rs :: COMPATIBILITY`, `compat.rs :: compat_for` | C3 | L | Declared kernel-vs-Pine coverage; spike follows it |
| C7 | Proposal lifecycle | feature | `src/protopyne-strategy/src/proposal.rs` | `proposal.rs :: Proposal`, `proposal.rs :: transition`, `proposal.rs :: ProposalState`, `proposal.rs :: ProposalEvent` | C1 | H | Pure `transition` fn; `apply` returns a caller-persisted journal entry |
| C8 | Signal/structure composition | feature | `src/protopyne-strategy/src/elements.rs` | `elements.rs :: Strategy`, `elements.rs :: Signal`, `elements.rs :: FixedSignal`, `elements.rs :: ExplicitStructure` | C7 | M | `Arc`-composed signal/structure/sizing/loss-monitor roles |
| C9 | Mandatory risk gate | feature | `src/protopyne-strategy/src/risk.rs` | `risk.rs :: RiskEngine`, `risk.rs :: RiskLimits`, `risk.rs :: RiskApproved` | C7 | H | Token minted only by `check`; `safe_submit` demands it (C15) |
| C10 | Arming grants | feature | `src/protopyne-strategy/src/grant.rs` | `grant.rs :: Grant`, `grant.rs :: evaluate_grant`, `grant.rs :: GrantDecision`, `grant.rs :: GrantUsage` | C7 | H | Pure evaluator shared by backtest/paper/preview; tighten-only |
| C11 | Order vocabulary + deterministic IDs | infra | `src/protopyne-venue/src/order.rs` | `order.rs :: OrderIntent`, `order.rs :: OrderRecord`, `order.rs :: client_id_for`, `order.rs :: Position` | C1 | H | Active-vs-terminal states follow the barter-rs pattern |
| C12 | Quotes, liquidity, slippage, fees | feature | `src/protopyne-venue/src/quote.rs` | `quote.rs :: Quote`, `quote.rs :: LiquidityFilter`, `quote.rs :: Slippage`, `quote.rs :: FeeSchedule` | C1 | H | Refusals carry reasons; 10 bps default frozen by calibration |
| C13 | Paper venue + fault injection | feature | `src/protopyne-venue/src/paper.rs` | `paper.rs :: PaperVenue`, `paper.rs :: Fault` | C11, C12 | H | Market-orders-only mock; idempotent resubmit; kill-mid-submit tests |
| C14 | Journal + capability gate | infra | `src/protopyne-venue/src/journal.rs` | `journal.rs :: Journal`, `journal.rs :: VenueStorage`, `journal.rs :: replay`, `journal.rs :: JournalEvent`, `journal.rs :: ReplayedState` | C11 | M | SQLite WAL; intent-before-venue; transactions via `fluent-db`; permit bound to file + task capabilities |
| C15 | Safe submission protocol | feature | `src/protopyne-venue/src/submit.rs` | `submit.rs :: safe_submit`, `submit.rs :: SUBMIT_TIMEOUT` | C9, C13, C14 | M | Journal → dry-run → submit → query-by-ID; retries never blind-resubmit |
| C16 | Reconciliation gate | feature | `src/protopyne-venue/src/reconcile.rs` | `reconcile.rs :: reconcile`, `reconcile.rs :: ReconcileGate` | C11 | M | Pure journal-vs-venue compare; mismatch halts entries, exits stay open |
| C17 | Health gate | cross-cutting | `src/protopyne-venue/src/health.rs` | `health.rs :: may_trade`, `health.rs :: HealthVerdict` | — | M | Quote age / clock skew / stream state; refusals, never warnings |
| C18 | Supervised trading session | feature | `src/protopyne-venue/src/session.rs` | `session.rs :: TradingSession`, `session.rs :: boot`, `session.rs :: spawn_feed`, `session.rs :: check_unit`, `session.rs :: run_bar`, `session.rs :: resolve_quote`, `session.rs :: SubmitJob` | C14, C15, C16, C17 | M | Owns journal file + venue tasks; credit-bounded submits (O11); per-bar checks as supervised units; quote ladder via `first_accept_in_order` |
| C19 | CSV history ingest | infra | `src/protopyne-server/src/csv.rs` | `csv.rs :: load_bars_csv`, `csv.rs :: parse_bars_csv` | C1 | M | Header-tolerant, row failures carry 1-based lines; 384-row golden |
| C20 | Versioned backtest venue | feature | `src/protopyne-server/src/backtest.rs` | `backtest.rs :: BacktestVenue`, `backtest.rs :: quote_for_bar`, `backtest.rs :: load_bars_with_fallback` | C12, C13, C19 | M | Delegates every `Venue` call to paper fills; first-accept source fallback |
| C21 | Runner, parity, sweeps, replay | feature | `src/protopyne-server/src/runner.rs` | `runner.rs :: run_backtest`, `runner.rs :: run_paper_decisions`, `runner.rs :: decide_and_submit`, `runner.rs :: checked_order_for`, `runner.rs :: run_sweep`, `runner.rs :: build_replay`, `runner.rs :: complete_replay`, `runner.rs :: equity_curve_for` (`Vec<Money>`), `runner.rs :: walk_forward_split`, `runner.rs :: snapshot_positions`, `runner.rs :: expire_position` | C8, C9, C10, C20 | M | One shared decide path; `ResultPool`-only sweeps; `CheckpointedStepGraph` replay (research-only) |
| C22 | Manifest + honest report | cross-cutting | `src/protopyne-server/src/report.rs` | `report.rs :: RunManifest`, `report.rs :: BacktestReport`, `report.rs :: max_drawdown`, `report.rs :: worst_step_loss`, `report.rs :: capital_efficiency`, `report.rs :: count_unquotable_bars`, `report.rs :: build_report` | C21 | M | Deterministic JSON; assumptions/limits beside metrics; greeks marked unavailable |
| C23 | HTTP service + WS + chart-data contract | feature | `src/protopyne-server/src/service.rs` | `service.rs :: serve`, `service.rs :: ServerConfig`, `service.rs :: ServerHandle`, `service.rs :: AppState`, `service.rs :: CandleRow`, `service.rs :: OverlaySeries`, `service.rs :: is_tightening` | C21, C3, C26 | M | 9 routes (`/health`, `/proposals`, `/proposals/preview`, `/grants/arm`, `/grants/current`, `/portfolio`, `/chart/bars`, `/chart/overlays`, `/feed`); overlay allowlist is `sma`/`ema`/`bb` (mid)/`trima` only (`mult` query param for `bb`, default 2.0); `decimal_to_chart` delegates to `chart-model :: to_display_float`; `Limiter` concurrency cap; `Scope`-owned feed pump; tighten-only arming; boot gates under `SupervisedBatch`. Chart rows/overlays are JSON data contracts for the native terminal, not a browser-chart binding |
| C24 | Transpile spike + calibration | feature | `src/protopyne-server/src/transpile.rs` | `transpile.rs :: compile_spike`, `transpile.rs :: Hair`, `transpile.rs :: evaluate_lowered`, `transpile.rs :: calibrate`, `transpile.rs :: CalibrationTally` (`is_green` = 1.0 accept + 1.0 reject) | C3, C6 | L | Accept-or-refuse HIR for `sma`/`ema`/`trima`/`midpoint` only (8 `DENY_PATTERNS`, version-6 gate; multi-leg `bbands`, nullary price kernels, and leg-hungry `midprice` stay refused); calibration sets are 9-must-transpile / 12-must-reject (`tests/transpile.rs`) |
| C25 | Binary + container ops | infra | `src/protopyne-server/src/main.rs`, `src/protopyne-server/Containerfile`, `src/protopyne-server/smoke-container.sh` | `main.rs :: main`, `main.rs :: parse_args` | C23 | L | `--config/--bind/--history/--help/--version`; two-stage image; `make protopyne-smoke` |
| C26 | Headless chart model (display boundary) | infra | `src/protopyne-chart-model/src/{convert,chart_type,session,range,transform}.rs` | `convert.rs :: to_display_float`, `convert.rs :: row_of`, `convert.rs :: rows_of`, `convert.rs :: ChartRow`, `chart_type.rs :: ChartType` (20 variants), `session.rs :: find_session_breaks`, `range.rs :: DateRange`, `transform.rs :: heikin_ashi`, `transform.rs :: renko_bricks` | C1 | L | `egui`-free by construction; `Decimal`→`f64` happens only here; only consumer is `server/service.rs` (C23) |
| C27 | Headless scale engine | infra | `src/protopyne-chart-scale/src/{mapping,ticks}.rs` | `mapping.rs :: ChartMapping` (= `PriceScale`), `mapping.rs :: PriceScaleMode`, `mapping.rs :: PriceRange`, `ticks.rs :: nice_ticks`, `ticks.rs :: Tick` | — | L | Pure math, `egui`-free; no protopyne dependencies; no production consumer yet inside this workspace |
| C28 | Overlap/price/momentum/stat/volume kernels | feature | `src/protopyne-kernels/src/{ta_overlap,ta_price,ta_momentum,ta_stat,ta_volume}.rs` | `ta_overlap.rs :: TrimaIndicator`, `ta_price.rs :: TypPriceIndicator`, `ta_momentum.rs :: AroonIndicator`, `ta_stat.rs :: CorrelIndicator`, `ta_volume.rs :: ObvIndicator` (et al.; 26 kernels total) | C3 | M | Stateful windowed functions only (Naive-A split); registered in `builtin_registry` with matrix rows; multi-pass kernels compose child states (`trima` via `SmaIndicator`, `adosc` via `EmaIndicator`) |
| C29 | Candle predicates | feature | `src/protopyne-kernels/src/candles.rs` | `candles.rs :: doji`, `candles.rs :: gap_up`, `candles.rs :: TA_EPSILON` | C1 | L | Free functions over `BarTick`, never `Indicator`s (no warmup, no registry, no matrix rows); thresholds frozen at 0 false-positives / full recall on control groups |
| C30 | WQ alpha combinators | feature | `src/protopyne-kernels/src/{wq_series,wq_half_life,wq_comb}.rs` | `wq_series.rs :: ts_delay`, `wq_half_life.rs :: hl_weights`, `wq_comb.rs :: comb_clamp` | C3 | L | Free functions over `&[f64]`/scalars, never registered; `min_periods`-as-warmup convention per fn; M5-operator overlaps delegate to `util.rs` (no second `add`) |
| C31 | Parity harness + calibration gate | cross-cutting | `src/protopyne-kernels/src/{parity,calibration}.rs` | `parity.rs :: expr_fixture`, `parity.rs :: assert_expr_parity`, `calibration.rs :: ParityDashboard`, `calibration.rs :: run_control`, `calibration.rs :: canonical_params` | C3, C6 | M | Research-only `polars` (never the hot loop); dashboard green is 1.0 accept at frozen eps + 0.0 control false-fire; nothing caches parity output until green |

## 5. Concurrency & Orchestration Index

| ID | Pattern | Where (path :: Symbol) | Mechanism | Fitting Rust primitive |
|---|---|---|---|---|
| O1 | Supervised per-bar units | `src/protopyne-venue/src/session.rs :: check_unit`, `session.rs :: run_bar`, `src/protopyne-server/src/service.rs :: serve` (boot gates) | `ComponentAdapter` with execute-override wrapped in `Instrumented`, registered in `SupervisedBatch`; failures cancel dependents; duplicate names surface as `Failed` | `JoinSet` + structured abort (already the underlying shape) |
| O2 | Server-owned looping tasks | `src/protopyne-venue/src/session.rs :: spawn_feed`, `src/protopyne-server/src/service.rs :: serve` (feed pump) | `Scope::spawn` future with `sleep` cadence; `Scope::close` on shutdown | `tokio::task::JoinSet` + `CancellationToken` |
| O3 | Endpoint concurrency cap | `src/protopyne-server/src/service.rs :: preview` via `Limiter::run` | Semaphore permit around the preview future | `tokio::sync::Semaphore` (already the underlying shape) |
| O4 | Parallel sweep workers | `src/protopyne-server/src/runner.rs :: run_sweep` | `ResultPool::new` + `submit` per variant + `shutdown`; outputs re-sorted by index | `tokio::task::JoinSet` or Rayon (CPU-bound alternative) |
| O5 | Broadcast fan-out | `src/protopyne-server/src/service.rs :: feed` | `tokio::sync::broadcast` channel; lagged receivers skip, closed ends stream | `tokio::sync::broadcast` (as-is) or `watch` for latest-only |
| O6 | Shared in-memory views | `src/protopyne-server/src/service.rs :: AppState` | `std::sync::Mutex` + `common_core::sync::lock` (poison-recovering) | `std::sync::Mutex` / `parking_lot::Mutex` (no await across lock) |
| O7 | Retry with backoff + timeout | `src/protopyne-venue/src/submit.rs :: safe_submit` | `common_core::retry::retry_async` + `tokio::time::timeout`, resolve via query-by-ID | `tokio::time::{sleep, timeout}` (already the shape) |
| O8 | Ordered fallback | `src/protopyne-server/src/backtest.rs :: load_bars_with_fallback` (sync ladder), `src/protopyne-venue/src/session.rs :: resolve_quote` (async ladder over `QuoteSource`s) | `first_accept_in_order_sync` / `first_accept_in_order`; `BadHeader` stops, `BadRow`/`Io` skip | Plain loop over fallible sources (already the shape) |
| O9 | Fault injection + timing control | `src/protopyne-venue/src/paper.rs :: Fault`, `src/protopyne-venue/src/paper.rs :: inject` | Queued faults consumed per submit; deterministic in tests | `tokio::sync::mpsc` or `VecDeque` (already the shape) |
| O10 | Graceful shutdown | `src/protopyne-server/src/service.rs :: ServerHandle`, `src/protopyne-server/src/main.rs :: main` | `Scope::close` then abort serve task; binary awaits `ctrl_c` | `tokio::signal` + `CancellationToken` |
| O11 | Credit-bounded submit flow | `src/protopyne-venue/src/session.rs :: TradingSession` (`credit_tx`, `credit_rx`) | `flow::new(CreditSpec)` pair; `is_blocked` gates intake, `recv` releases per submit; bound is correctness-as-boundedness | `tokio::sync::Semaphore` with acquire/release (already the underlying shape) |

## 6. Reuse Hotspots and DRY Findings

Most-imported internal modules (by files with a textual reference,
recounted 2026-10-09):

1. `protopyne-types` (22) — `Money`, `Bar`, `Qty` are the true core; any
   extraction starts here (already a crate, correctly so).
2. `serde` (25), `chrono` (21), `rust_decimal` (19), `bon` (15) — foundation sprawl is
   healthy; no action.
3. `common-core` (10 files) — the `sync::lock` mutex idiom is
   uniform; a good sign. Also funnels `retry_async`, `load_json_or_default`,
   `impl_from_io_error!`, and `registry::ConcurrentRegistry`.
4. `Venue` trait + `PaperVenue` (3) — narrow fan-in is by design (server is
   the only external caller); reuse grows only when a second venue appears.
5. `frame_of_bars`/`frame_of_values` (research tests only) — thinnest reuse in the
   tree; the production display path now lives in `chart-model`
   (`to_display_float`/`row_of`), so these stay offline-only by design;
   see §7.

Near-duplicate logic (unification signals, not yet violations):

- ~~`runner.rs` skipped-`BarDecision` literals in four places~~ RESOLVED:
  `run_backtest`, `run_paper_decisions`, and `decide_and_submit` all share
  the `skipped_decision` helper now (7 call sites, one constructor).
- Fail-closed `validate()` exists on nine validators
  (`RiskLimits`, `ServerConfig`, `BacktestConfig`, `RunnerConfig`,
  `SessionConfig`, `PineRuntimeConfig`, `InputDef`, plus
  `service.rs :: validate_grant` and `types :: Bar::validate` /
  `validate_new_bar`). Same shape, five error types
  (`String`, `ServerError`, `KernelError`, `TypesError`, inline). See extraction
  candidate E1.
- `Money::from(0)` vs `Money::zero()` both in use (`service.rs` uses the
  former twice; `grant.rs`, `order.rs`, `paper.rs`, `runner.rs` the latter); pick one.
- Grant-builder boilerplate is duplicated between strategy tests and
  server API tests (`test_grant`/`grant_with`); a shared test-fixture
  helper would remove a drift risk.
- `decimal → f64` fallible conversions are half-centralized:
  `service.rs :: decimal_to_chart` now delegates to
  `chart-model :: to_display_float`, but overlay closes, the spike
  evaluator, and research-frame conversion still carry per-site
  `unwrap_or(NaN)` fallbacks; finish the migration to the one helper so
  the skip-vs-NaN policy lives in exactly one place (C26).

## 7. Extraction Candidates

| Candidate | Capability IDs | Why generic/valuable | Trait/type sketch | Priority |
|---|---|---|---|---|
| E1. Fail-closed `Validate` trait | C2, C9, C18, C23 | Nine ad-hoc `validate()`s with five error types; every future config will add a tenth | `trait Validate { type Err; fn validate(&self) -> Result<(), Self::Err>; }` with a blanket `WorkUnit` bridge | H |
| E2. Deterministic manifest pattern | C22 | Any reproducible run needs data/code/config pin + canonical JSON + rerun-equality test | `struct RunManifest<V, C> { data_version: String, code_version: &'static str, config: C, span: V }` with `to_canonical_json` | M |
| E3. Backtest analytics (drawdown/tail/efficiency/equity) | C21, C22 | Pure `Decimal`-slice math reused by every future report or sweep comparison | `fn max_drawdown(equity: &[Decimal]) -> Decimal` et al. as a `metrics` module; inputs `Copy`, no I/O | M |
| E4. Accept-or-refuse subset recognizer | C24 | The compile-or-refuse scaffold (deny patterns → HIR → emit → calibrate) generalizes to any future script coverage | `trait Subset { type Hir; fn recognize(src: &str) -> Result<Self::Hir, Refusal>; }` + `CalibrationTally` as-is | M |
| E5. Execution-policy types | C12 | `LiquidityFilter`/`Slippage`/`FeeSchedule` are venue-independent; the second venue (or barter spike) reuses them unchanged | Existing structs already `Copy + Serialize`; only need a shared home if venue splits | H (on second venue) |
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

## 9. Gaps & Drill-Down Queue

Closed during the 2026-10-09 re-verification (direct reads; no longer gaps):

- `session.rs` internals — `boot` (validates, `flow::new({initial:
  credit_limit, more_after: 1})`, `ResultPool` submit via `venue.submit`),
  `spawn_feed`, `run_bar` (installs `CapabilitySet(storage)`, wraps
  `Instrumented(ComponentAdapter)`, duplicate names → `Failed`),
  `resolve_quote` (async `first_accept_in_order` ladder), `shutdown`
  (scope close + pool drain). Session is `Arc<PaperVenue>`-bound, not
  generic over `Venue`.
- `compat.rs` matrix content — 43 rows, all `eps = 1e-9`, all
  `ParityVerified` (numeric authority is `Expr` output; the direct
  export cross-check is superseded by the calibration gate).
- `paper.rs` fill internals — touch selection (`ask` buy / `bid` sell),
  marketability (`buy: limit >= touch`, `sell: limit <= touch`, else
  `Refused WouldRest`), slippage capped at limit, fee-overflow refusal,
  `LoseAck` commits-then-errors, idempotent resubmit via query-first.
- `service.rs` routes — 9 total (see C23); WS `/feed` skips lagged, ends on
  closed. Overlay allowlist is `sma`/`ema`/`bb`/`trima` only, `period` in `[1, 500]`.
- Spike accept-set boundary — 8 `DENY_PATTERNS`, `//@version=6` +
  `indicator(` required, `ta.sma`/`ta.ema`/`ta.trima`/`ta.midpoint` 2-arg
  only; calibration sets are 9-must-transpile / 12-must-reject at 1.0/1.0
  (`tests/transpile.rs`).
- Risk/grant denial coverage — `RiskEngine::check` collects all 8
  `RiskReason` arms; `evaluate_grant` denies with `String` reasons in fixed
  order (revoked → window → scope → budgets → signal → trade → portfolio →
  breaker).

Remaining low-confidence or unmeasured areas:

- `service.rs` WS backpressure posture under a slow subscriber (lag-skip
  is implemented; load behavior unmeasured).
- `run_sweep` worker sizing (`clamp(1, 8)`) — throughput unmeasured.
- Kernel `update()` incremental-engine details beyond the
  `batch`/`update` agreement suites; `RunCtx` cursor semantics beyond the
  public fn list.
- `chart-scale` has no in-workspace consumer yet (E6); first wiring will
  test whether `PriceScaleMode` covers the terminal's needs.
- Anything outside `src/protopyne-*`: shared-library internals
  (`Scope`, `SupervisedBatch`, capability enforcement) were used as
  documented, not re-verified here.

Suggested drill-downs, ordered by value, each with a starter command
(run from repo root):

1. Risk gate completeness (all refusal arms):
   `grep -n "RiskReason::" src/protopyne-strategy/src/risk.rs`
2. Grant denial coverage (all evaluator arms):
   `grep -n "reasons.push" src/protopyne-strategy/src/grant.rs`
3. Fill-policy edge cases (marketability, partials):
   `grep -n "WouldRest\|partial\|marketable" src/protopyne-venue/src/paper.rs`
4. Journal schema and replay fidelity:
   `grep -n "CREATE TABLE\|INSERT INTO" src/protopyne-venue/src/journal.rs`
5. Session supervision depth:
   `ast-grep --json -l rust -p 'Scope::$M($$$)' src/protopyne-venue/src/session.rs`
6. Spike accept-set boundary (what exactly parses):
   `grep -n "DENY_PATTERNS\|fn parse_" src/protopyne-server/src/transpile.rs`
7. Endpoint inventory vs. router surface:
   `grep -n "\.route(" src/protopyne-server/src/service.rs`
