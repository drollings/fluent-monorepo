# protopyne-rs — Vision

**Status:** Draft v0.1 · October 2026
**Scope:** Top-level vision. Not a specification; it fixes direction, principles, boundaries, and the questions the specification must answer.

---

## 1. What this is

protopyne-rs is a lightweight, pure-Rust platform for **researching, backtesting, and semi-automatically trading stocks, options, and option spreads**, built around one idea:

> **Your indicator ideas should move from TradingView/Pine to Python to production Rust without being rewritten, and nothing should trade your money unless you have explicitly authorized it.**

It is a personal, low-volume, high-trust trading system, not an exchange-grade engine. It optimizes for **correctness, auditability, and fail-safe behavior** rather than raw speed. The minimum decision timeframe is 1 minute.

## 2. Guiding principles

1. **Reliability over features.** Any failure must leave you with known positions, resting protective orders, and a halted system rather than a surprised one. "Absolute" reliability is not achievable by any software; fail-safe behavior is.
2. **Human authority.** The system proposes; you authorize. Autonomy exists only inside a time-boxed, bounded **arming grant** that you created and confirmed.
3. **The broker is the source of truth.** The platform's own state is a journaled, reconcilable view of it, never a substitute.
4. **One code path.** Backtest, paper, and live share the same strategy, risk, and authorization logic and differ only at the venue boundary.
5. **Compose, don't reinvent.** Prefer proven crates and your monorepo primitives over new infrastructure. Never re-implement graph algorithms, retry logic, or supervision.
6. **Fail closed.** If data, clock, journal, or evaluator is uncertain, the answer is "do not trade; ask a human."
7. **Honest backtests.** Results are only as trustworthy as their data and fill model; the platform must make those limits visible rather than flatter the strategy.
8. **Pine-portable by design, not by accident.** Compatibility is measured, tested, and documented, with a clear line between portable and native code.

## 3. Who it's for and what it does day to day

A single experienced discretionary-plus-systematic trader who:

- builds indicators (volume profiles, trend ribbons, options-activity signals) in Pine v6 and protopyne's Python API;
- wants spreads and options as the primary instrument, with stock trades as secondary;
- trades at low volume, with trades placed on **tastytrade** (automated or semi-automated) and **Robinhood** (human-executed);
- wants to preview, approve, or pre-authorize trades, and to arm automation for a bounded period (one hour to one week).

A typical day: review overnight proposals, tune thresholds with a backtest preview, arm an exit-only or tightly bounded entry grant for the session, monitor portfolios, and review the journal afterwards.

## 4. Non-goals

- Not a high-frequency or sub-minute system.
- Not a fully autonomous, unattended fund.
- Not a multi-user or multi-tenant service (initially).
- Not a general-purpose broker aggregator; venues are added only when they can be made reliable.
- Not an automated Robinhood execution engine (see §9).
- Not a Pine interpreter for every Pine feature; it targets the subset your library uses, defined by a compatibility matrix.

## 5. Architecture at a glance

| Layer | Responsibility |
|---|---|
| **Data** | 1m candles, option chains (bid/ask, OI, greeks/IV), corporate actions, event calendar; TimescaleDB for operational data, Parquet/Polars for research |
| **Analytics** | Indicator kernels with warmup metadata, volume profile, trend ribbons, IV surface and greeks, options-activity scoring, Pine-compatible runtime |
| **Strategy** | Composable strategy elements (§6) that turn closed bars into *proposals* |
| **Risk & authorization** | Mandatory risk engine, arming grants, approval policies, kill switch |
| **Execution** | Single-writer order/position state machine, tastytrade adapter, Robinhood ticket adapter, reconciliation |
| **Simulation** | Backtester and paper venue running the same strategy, risk, and grant logic |
| **Presentation** | axum REST/WebSocket API, web UI with TradingView Lightweight Charts, approval inbox, portfolio dashboards |

Data flows in one direction: **closed bar → indicators → strategy → proposal → risk engine → grant/approval → execution → journal → reconciliation**, with the broker's reported state flowing back as the authority.

## 6. Strategy model: a core plus optional elements

A strategy is a small, declarative composition. The **signal** is the only required element. Everything else is optional and independently swappable.

| Element | Required? | Purpose |
|---|---|---|
| **Signal** | Yes | Indicator and options-activity logic evaluated on closed bars; emits a direction and conviction |
| **Structure selector** | Optional | Chooses instrument, structure (single, vertical, calendar, condor), strikes, and DTE; defaults to a user-supplied explicit structure |
| **Sizing** | Optional | Determines quantity; defaults to a fixed contract count |
| **Loss management** | Optional | Defines how a position is protected and unwound |
| **Monitoring** | Optional | Observes positions, portfolios, and health; alerts or proposes action |

### 6.1 Sizing (optional)
Pluggable policies, each bounded by hard caps: fixed contracts; fixed percentage of net liquidation at max loss; volatility-scaled sizing; buying-power-based; capped fractional-Kelly for research. Sizing is **per portfolio**, reads the broker's buying power and margin dry-run, and can only ever reduce a quantity that the risk engine would permit, never exceed it.

### 6.2 Loss management (optional)
Composable rules attached to a strategy or an individual position: stop or loss threshold (premium-based, spread-value-based, or underlying-level-based); profit targets; time stops (e.g. close at N DTE); assignment and ex-dividend protection for short legs; roll and adjustment rules (submitted as one multi-leg order, never leg by leg); and portfolio-level drawdown responses. Wherever the broker supports it, protective exits rest **at the broker** so they survive an outage of this platform.

### 6.3 Monitoring of multiple portfolios (optional)
A **portfolio** is a named grouping of positions, strategies, and accounts: a tastytrade account, a Robinhood account tracked from manually confirmed tickets, a paper portfolio, or a backtest run. Monitoring is **read-only and independent of strategy logic**:

- aggregate P&L, exposure, net delta/gamma/theta/vega, buying power, and margin per portfolio and across portfolios;
- concentration by underlying, expiry, and event exposure;
- limit status versus configured thresholds, with alerts that escalate out of band;
- reconciliation health and unconfirmed-ticket tracking;
- strategy attribution, showing which strategy and which grant produced each position.

Monitors may *propose* actions (for example "close this leg before ex-dividend"), but only an approval or an armed grant can turn a proposal into an order.

### 6.4 What is never optional
Optional elements shape *what* the strategy does. A **mandatory risk engine** sits outside the strategy and cannot be configured away: position and margin limits, stale-data and clock checks, reconciliation gates, order sanity checks (limit orders only for spreads), and the kill switch. Strategy authors cannot bypass it.

## 7. Human authority: approvals and arming

### 7.1 Proposal lifecycle
`Proposed → RiskChecked → (AutoApproved | PendingHuman) → Approved | Rejected | Expired → Submitted → Working → Filled | Partial | Cancelled`. Every transition is a journaled event with reasons.

### 7.2 Arming grants
You can optionally **arm** the system for a bounded window (1 hour to 7 days) to enter, exit, or roll automatically. A grant is an immutable, journaled object containing:

- **Scope and permissions:** strategies, underlyings, structures, direction; entry, exit, and roll are separate permissions (exit-only is far lower risk than entry).
- **Budgets:** trades, contracts, committed max loss, notional, reserved at submission and consumed on fill.
- **Thresholds in three tiers:** signal (indicator state, options-activity score), trade (structure, price versus mid, spread width, liquidity, delta/DTE, max loss), and portfolio/market (open risk, net greeks, IV/VIX bounds, trade windows, event blackouts).
- **Circuit breakers:** loss limits, consecutive errors, slippage beyond tolerance, reconciliation mismatch, stale data, abnormal gaps, invariant violations. Any trip disarms entries, keeps managing existing positions, and alerts you.

Rules:

1. Arming is a **deliberate act**, confirmed on a screen that shows the computed **worst-case exposure** if every budget is consumed at its limit.
2. A live grant can be **tightened or revoked instantly, never loosened**; loosening needs a new grant and confirmation.
3. The grant evaluator is a **pure function**, `evaluate(proposal, grant, state, quotes) → Allow | Deny{reasons}`, run at decision time and again immediately before submission against a fresh quote.
4. Grants **never bypass** the risk engine.
5. Longer windows get **tighter defaults** and optional daily re-attestation, dropping to notify-only if unconfirmed.
6. Because the evaluator is pure, the backtester runs the **same grant logic** over history. You can preview what a grant would have done and tune thresholds before arming.
7. Initial grants allow **defined-risk structures only**; naked short options are excluded.

## 8. Pine compatibility and the transpile path

The goal is a Rust API that feels like Pine v6 and like protopyne's Python API, so a script ports across all three with minimal change.

Findings from the protopyne overview that shape the plan:

- The existing production path is **vectorized** (whole DataFrame in, DataFrame out), while real Pine is **stateful bar-by-bar** (`var`/`varip`, series history). protopyne-rs defines Pine semantics as a **per-bar stateful executor with a vectorized fast path** for provably stateless indicators.
- protopyne relies on six **process-global singletons**; protopyne-rs replaces them with an explicit, per-run context. This enables isolated, parallel backtests.
- The Python strategy engine is orphaned and `varip` currently behaves like `var`; both are treated as references, not contracts.
- Numeric parity of the pandas_ta-backed operators versus Pine is **unverified**. protopyne-rs adopts **golden-file parity tests** against actual TradingView output.
- The existing transpiler (HIR/LIR, type inference, source maps) is the intended front end for a Pine/Python → Rust path. The Rust backend needs to grow well beyond its current `Series → Vec` mapping.
- `request.security` semantics (`lookahead`, `gaps`) are a classic source of look-ahead bias and must be specified exactly.
- Pine has no concept of options, so the Rust API **extends** Pine with chain access, greeks, multi-leg orders, and portfolio constructs, with a clear boundary between *Pine-portable* and *native* code.
- Decisions run on **closed 1m bars**, so the tick-synthesis subsystem in protopyne is deferred rather than ported.

A **compatibility matrix** (supported, partial, unsupported) is a first-class deliverable, derived from which Pine features your actual indicator library uses.

## 9. Venues

| Venue | Role | Notes |
|---|---|---|
| **tastytrade** | Market data, account streaming, and automated or semi-automated order entry | Public REST API plus DXLink streaming; has a dry-run endpoint and a certification sandbox. No idempotency key: safe retry depends on deterministic external identifiers and status checks. Streaming keeps you in sync; REST is authoritative. Auth has changed during 2026 (legacy session login removed), so track the developer release notes. |
| **Robinhood** | Human-executed only | The official API found is crypto-only; stock and option access is unofficial and fragile. The platform generates an exact trade ticket (legs, limit, quantity, good-until), you place it, and you confirm the fill. The platform tracks unconfirmed tickets and flags drift. Any read-only import from Robinhood is isolated behind an adapter that can fail without affecting the rest. |
| **Paper / simulated** | Rehearsal and backtest | The same interface as live; the cert sandbox is used for soak tests. |

Execution code sits behind a venue trait so additional venues can be added later without touching strategy, risk, or grant logic.

## 10. Reliability architecture

- **Single-writer order and position state machine,** owned by this project rather than delegated to a third-party engine.
- **Event-sourced journal in PostgreSQL** with a transactional outbox: intent is durable before any broker call. State is rebuilt by replay, which also gives auditability and backtest/live parity.
- **Safe submission protocol:** deterministic external identifier → journal intent → dry-run and margin check → submit → on any timeout or 5xx, *query by identifier, never blindly resubmit*.
- **Reconciliation loop** against broker positions and live orders. Any mismatch halts new entries and alerts.
- **Staleness and health gate:** quote age, stream state, token expiry, and clock skew feed a "may trade" decision that fails closed.
- **Reconnect and resync:** after any stream drop, missing 1m candles are backfilled before signals resume, so indicators never evaluate on a gapped series.
- **Rust discipline:** `Decimal` for all money, no `unwrap`/`panic` in trading paths, `deny(unsafe_code)`, dependency auditing in CI, supervised tasks with restart policies.
- **Testing:** fault injection (dropped sockets, 5xx, delayed fills, duplicate submits), deterministic replay, and long sandbox soak runs before any live money.
- **Out-of-band alerting** independent of the platform's own UI.

## 11. Backtesting philosophy

- Realistic option data matters most: bid/ask rather than last or mid, point-in-time, adjusted for corporate actions. The platform makes data quality and its limits explicit in every report.
- Spread-aware fill model with liquidity filters and multi-leg slippage; per-contract fees included.
- Expiration, assignment, exercise, dividends, and splits handled as explicit events.
- Walk-forward and out-of-sample evaluation, parameter sweeps, and metrics beyond Sharpe (drawdown, tail loss, greeks exposure over time, capital efficiency).
- Every run is reproducible from **(data version, code version, configuration)**.
- Backtests exercise the real risk engine, sizing, loss-management, and grant logic, so what you test is what you trade.

## 12. Technology and monorepo composition

| Concern | Choice | Role and caveat |
|---|---|---|
| Event model and simulation | barter-rs (reference, verified against a local checkout: `barter-execution 0.9`, `barter-data 0.13`) | `rust_decimal` throughout; `Active`/`Inactive` order states (the pattern behind our active-vs-terminal lifecycle); `ExecutionClient` trait plus a mock exchange; engine `RiskManager` with advisory `RiskApproved`/`RiskRefused` filters. Not adopted: integrations are crypto-venue-only (no equity-options broker); the mock fills market orders only with percentage fees (no limit flow, slippage, or liquidity filter) and mints IDs from `rand`; no journal, outbox, or reconciliation; the risk gate is advisory, not mandatory. Borrowed: order-state modeling. |
| Research data frames | Polars | Feature engineering and chain analysis; not used for the stateful bar-by-bar loop. |
| API server | axum | REST and WebSocket. |
| Charts | TradingView Lightweight Charts | Candles, ribbons, markers; volume profile needs its custom primitives API. |
| Storage | PostgreSQL + TimescaleDB | Journal and operational time-series, with explicit retention and compression policy. |
| Dependency ordering | `fluent-dag` (`DependencyGraph`, `CheckpointedStepGraph`) | Indicator and multi-timeframe dependency ordering; pipeline orchestration. Rewind is for research and replay, never for live order state. |
| Supervision | `SupervisedBatch` / tokio guardrails | Feeds, executors, reconcilers. |
| Reflection, polymorphism | Monorepo libraries | Roles to be confirmed once their documentation is shared. |

## 13. Phased path

1. **Foundation:** run context, series model, and a small set of indicator kernels with Pine golden tests; journal and state machine skeleton.
2. **Vertical slice:** one underlying, vertical spreads, the tastytrade cert sandbox, proposal → risk → approval → paper execution, with reconciliation and fault injection.
3. **Backtest realism:** historical chains, fill model, grant preview and threshold sweeps.
4. **Arming:** grants, budgets, breakers, exit-only first, then entries.
5. **Optional strategy elements:** sizing, then loss management, then multi-portfolio monitoring.
6. **Pine transpile path:** compatibility matrix, Rust backend, parity suite across Pine, Python, and Rust.
7. **Live hardening:** long soak, small live size, Robinhood ticket workflow, operational runbooks.

## 14. Success criteria

- A Pine indicator from your library produces **numerically matching output** in Pine, Python, and Rust, within a documented tolerance.
- The same strategy produces **identical decisions** in backtest replay and live paper trading on the same data.
- Killing the process, the network, or the database at any point leaves **no unknown position and no unauthorized order**.
- An armed grant can **never** exceed its stated worst-case exposure.
- Every order can be traced to a proposal, a risk decision, and an approval or grant.

## 15. Open decisions for the specification

- Options data source for history and real-time greeks, and its depth and cost.
- Account facts: margin type, account size, and PDT status.
- Strategy universe: underlyings, structures, and DTE range.
- Which options-activity signals, and from what feed.
- Exact Pine feature coverage required by your indicator library.
- Presence requirements, revocation channels, and alert channels for arming.
- Hosting, secrets handling, backup, and monitoring approach.
- Whether any component of barter-rs earns a place beyond inspiration, determined by a short spike. Pre-verified mismatches narrow the spike: a `Venue` over its mock would need limit-order fills, deterministic IDs, and journal integration to preserve backtest parity — the open question is whether that adaptation beats the owned paper venue, not whether the patterns fit.
- The roles of the reflection, polymorphism, and tokio-guardrail libraries, pending their documentation.
