# protopyne-rs — Vision

## 1. What this is

A personal, robust, semi-automated options-trading platform built with
ruthless reliability over features — something closer to a flight controller
than a quant fund. It runs one operator's strategies against a small,
well-understood universe, with every automated action flowing through
explicit human authorization and every state change recorded for audit.

The whole platform is implemented in a single systems language with no
browser or JavaScript toolchain anywhere in the loop: charts, approval
screens, and portfolio views are native views over the same core that
decides and journals, not a web frontend over an API.

## 2. Guiding principles

- **Reliability over features.** Every capability earns its place by making the system safer or more auditable. Complexity that cannot be supervised is not added.
- **Two distinct roles.** The human sets intent, bounds, and approvals; the machine executes within them and reports back. The machine never widens its own authority.
- **Own the critical path.** Order state, risk checks, and the journal are first-party logic, not delegated to a framework or a venue SDK. Third-party code may render, analyze, or transport — never authorize or execute.
- **One code path, two modes.** Live and paper run the same supervised
  strategy, risk, and authorization logic over the same state machine —
  divergence between rehearsal and reality on *that* path is a defect. The
  research backtest is a deliberately simpler mode (see
  `ADR_SIMPLE_BACKTEST.md`): it shares signal evaluation, sizing math, exit
  management, and fee application with the supervised path, but evaluates
  no grant, risk gate, or journal. A research result is never a transfer
  certificate for live behavior.
- **Fail closed.** Stale data, clock skew, reconciliation mismatch, expired credentials, or an offline broker halt new entries and alert — they never degrade into silent best-effort trading.
- **Honest backtests.** Every report states its data limits and assumptions next to its metrics. A backtest that cannot name what it ignores is not trusted.
- **Compose, don't reinvent.** Shared platform capabilities (task supervision, retry, dependency ordering, durable storage, calibration) are reused from common infrastructure; trading-domain logic lives in the trading crates. Nothing is implemented twice.
- **Measured Pine portability.** Pine compatibility is verified against actual reference output with a declared coverage matrix — never assumed from formula resemblance.

## 3. Who this is for

One operator who:

- builds indicators (volume profiles, trend ribbons, options-activity signals) in Pine v6 and a matching native API;
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
| **Data** | 1-minute candles, option chains (bid/ask, open interest, greeks/IV), corporate actions, event calendar; a durable operational store for journal and time-series plus columnar frames for research |
| **Analytics** | Indicator kernels with warmup metadata, volume profile, trend ribbons, IV surface and greeks, options-activity scoring, Pine-compatible execution |
| **Strategy** | Composable strategy elements (§6) that turn closed bars into *proposals* |
| **Risk & authorization** | Mandatory risk engine, arming grants, approval policies, kill switch |
| **Execution** | Single-writer order/position state machine, tastytrade adapter, Robinhood ticket adapter, reconciliation |
| **Simulation** | Backtester and paper venue running the same strategy, risk, and grant logic |
| **Presentation** | Native desktop terminal in the same language as the core: charts, approval inbox, portfolio dashboards — no browser or JavaScript toolchain required |

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
6. Because the evaluator is pure, the supervised rehearsal path runs the **same grant logic** over history. You can preview what a grant would have done and tune thresholds before arming.
   (The equity research backtest is exempt: it evaluates no grant by design —
   see `ADR_SIMPLE_BACKTEST.md`. Its reports state the absence.)
7. Initial grants allow **defined-risk structures only**; naked short options are excluded.

## 8. Pine compatibility

The goal is a native API that feels like Pine v6, so a script ports across Pine and this platform with minimal change. The requirements that shape the work:

- Pine is **stateful bar-by-bar** (`var`/`varip`, series history), while bulk analysis is naturally vectorized. The platform defines Pine semantics as a **per-bar stateful executor with a vectorized fast path** for provably stateless indicators.
- Execution state is an **explicit per-run context**, never process-global state. This is what makes isolated, parallel backtests possible.
- Numeric parity of operators versus Pine is **verified, not assumed**: golden-file tests against actual reference output, within documented tolerances.
- Multi-timeframe access semantics (lookahead behavior, gap handling) are specified exactly — they are a classic source of look-ahead bias and must not be left implicit.
- Pine has no concept of options, so the native API **extends** Pine with chain access, greeks, multi-leg orders, and portfolio constructs, with a clear boundary between *Pine-portable* and *native* code.
- Decisions run on **closed bars** at the chart timeframe and above; sub-bar tick synthesis is deferred, not ported.

A **compatibility matrix** (supported, partial, unsupported) is a first-class deliverable, derived from which Pine features your actual indicator library uses. Anything outside the matrix refuses with a reason instead of miscomputing.

## 9. Venues

| Venue | Role | Notes |
|---|---|---|
| **tastytrade** | Market data, account streaming, and automated or semi-automated order entry | Public REST API plus streaming; has a dry-run endpoint and a certification sandbox. No idempotency key: safe retry depends on deterministic external identifiers and status checks. Streaming keeps you in sync; the request API is authoritative. |
| **Robinhood** | Human-executed only | Stock and option access is unofficial and fragile. The platform generates an exact trade ticket (legs, limit, quantity, good-until), you place it, and you confirm the fill. The platform tracks unconfirmed tickets and flags drift. Any read-only import from Robinhood is isolated behind an adapter that can fail without affecting the rest. |
| **Paper / simulated** | Rehearsal and backtest | The same interface as live; the cert sandbox is used for soak tests. |

Execution sits behind a venue interface so additional venues can be added later without touching strategy, risk, or grant logic.

## 10. Reliability architecture

- **Single-writer order and position state,** owned by this project rather than delegated to a third-party engine.
- **Durable event-sourced journal:** intent is recorded before any broker call. State is rebuilt by replay, which also gives auditability and backtest/live parity.
- **Safe submission:** deterministic external identifiers → journal intent → dry-run and margin check → submit → on any timeout or ambiguous failure, *query by identifier, never blindly resubmit*.
  (Live and paper only. The research backtest writes no journal and submits
  no orders; its fills price directly off the bar — see
  `ADR_SIMPLE_BACKTEST.md`.)
- **Reconciliation loop** against broker positions and live orders. Any mismatch halts new entries and alerts.
- **Staleness and health gate:** quote age, stream state, credential expiry, and clock skew feed a "may trade" decision that fails closed.
- **Reconnect and resync:** after any stream drop, missing bars are backfilled before signals resume, so indicators never evaluate on a gapped series.
- **Exact money arithmetic, no silent failures, memory safety:** all money math is exact decimal; trading paths never panic or unwrap on bad input; unsafe code is forbidden; dependencies are audited.
- **Testing:** fault injection (dropped sockets, error responses, delayed fills, duplicate submits), deterministic replay, and long sandbox soak runs before any live money.
- **Out-of-band alerting** independent of the platform's own UI.

## 11. Backtesting philosophy

- Realistic option data matters most: bid/ask rather than last or mid, point-in-time, adjusted for corporate actions. The platform makes data quality and its limits explicit in every report.
- Spread-aware fill model with liquidity filters and multi-leg slippage; per-contract fees included.
- Expiration, assignment, exercise, dividends, and splits handled as explicit events.
- Walk-forward and out-of-sample evaluation, parameter sweeps, and metrics beyond Sharpe (drawdown, tail loss, greeks exposure over time, capital efficiency).
- Every run is reproducible from **(data version, code version, configuration)**.
- The supervised (live/paper) rehearsal path exercises the real risk engine, sizing, loss-management, and grant logic, so what you test is what you trade.
  The equity research backtest instead shares signal evaluation, sizing math,
  exit management, and fee application with that path while omitting risk,
  grant, and journal layers; its reports state the omission next to the
  metrics, and its numbers are never transfer certificates (see
  `ADR_SIMPLE_BACKTEST.md`).

## 12. Platform direction

- **One language, one toolchain.** The core, the Pine runtime, the charting, and the desktop terminal share a single systems-language stack. There is no browser, JavaScript, or Node.js anywhere in the build, test, or run loop.
- **Native terminal.** Charts, the approval inbox, grant arming screens, replay, and portfolio dashboards are native views over the platform core — rendering journaled state, never holding order authority of their own.
- **Borrow the toolkit, own the application.** The immediate-mode GUI toolkit is a dependency; the charting application built on it is studied as reference and re-expressed in this codebase's idioms, never taken as a dependency. Display code may be reused for pixels; nothing that reaches an order is inherited from display code.
- **Borrow patterns, own the critical path.** External execution frameworks are studied for their state modeling and deliberately not adopted where their semantics (venue coverage, fill models, identifier discipline, journaling, risk posture) fall short of §§9–11. Whatever executes or authorizes is first-party.

## 13. Phased path

1. **Foundation:** run context, series model, and a small set of indicator kernels with golden tests against reference output; journal and state machine skeleton.
2. **Vertical slice:** one underlying, vertical spreads, the tastytrade cert sandbox, proposal → risk → approval → paper execution, with reconciliation and fault injection.
3. **Backtest realism:** historical chains, fill model, grant preview and threshold sweeps.
4. **Arming:** grants, budgets, breakers, exit-only first, then entries.
5. **Optional strategy elements:** sizing, then loss management, then multi-portfolio monitoring.
6. **Pine v6 runtime:** compatibility matrix, native execution with series-correct semantics, parity suite across Pine and native — libraries and imports included, arbitrary user scripts supported.
7. **Native terminal:** charts, approval inbox, grant screens, replay, and portfolio views over the journaled core.
8. **Live hardening:** long soak, small live size, Robinhood ticket workflow, operational runbooks.

## 14. Success criteria

- A Pine indicator from your library produces **numerically matching output** in Pine and in this platform, within a documented tolerance.
- The supervised rehearsal path produces **identical decisions** in backtest replay and live paper trading on the same data.
  (The simple research backtest is exempt by design — see
  `ADR_SIMPLE_BACKTEST.md` — and its reports say so.)
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
