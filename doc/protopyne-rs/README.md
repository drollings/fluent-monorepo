# protopyne-rs

A Rust trading framework: versioned market history in, supervised decisions
out, with an HTTP surface for review, a native desktop terminal, and a
Python-free container for deployment. Automated live trading is gated behind
explicit journaled arming (`--live`); everything else trades against a
simulated venue over pinned history — what you test is what you would trade.

Rust-only, end to end: there is no browser, JavaScript, or Node.js in the
build, test, or run loop. Charts and the native desktop terminal are views
over this core (`protopyne-chart-model`/`protopyne-chart-scale` are headless;
`protopyne-egui` is the terminal; `fluent-egui` is the paint-edge scaffolding
beneath it). The GUI toolkit (`egui`) is an approved dependency for those two
paint-edge crates only; the `egui-charts` application is studied as reference
and re-expressed in this codebase's idioms, never taken as a dependency.

## What it can do

- **Indicators with Expr-parity.** `protopyne-kernels` evaluates SMA, EMA,
  WMA, HMA, RMA, VWMA, ALMA, RSI, MACD, Stochastic, ATR, Bollinger/Keltner
  bands, and True Range bar-by-bar or in batch, with warmup metadata and a
  compatibility matrix against the staged reference expressions (direct
  TradingView cross-check pending; see the divergence log).
- **Proposals with a journaled lifecycle.** Strategies emit `Proposal`
  intents (direction, structure, limit, max loss, notional, DTE, conviction)
  that move through an explicit state machine; every transition is a journal
  entry the caller persists before acting on it.
- **Mandatory risk gate.** `RiskEngine` mints a single-use `RiskApproved`
  token per proposal. Submission paths demand the token, so strategy code
  cannot bypass risk by construction.
- **Arming grants with worst-case exposure.** Grants authorize scope,
  budgets, signal/trade/portfolio thresholds, and breakers over a time
  window. A live grant can be tightened or revoked, never loosened — and
  arming shows the worst case before confirmation.
- **Paper venue with calibrated fills.** Spread-aware fill model, liquidity
  filter with refusal reasons, slippage, and per-contract fees, journaled in
  a durable CSV `AppendLog` with intent-before-venue ordering: kill the
  process mid-submit and replay plus query-by-ID leaves no unknown position
  and no duplicate order.
- **Backtests that run the real path.** The runner executes
  signal → risk → grant → venue per bar over pinned history and records a
  manifest (data/code/config versions) plus an honest report (assumptions,
  data limits, drawdown, tail loss, capital efficiency). Same inputs give
  byte-identical reruns; backtest and paper reach identical decisions.
- **HTTP surface + WebSocket feed.** Health, proposal preview/inbox, grant
  arming, portfolio, chart-data rows and indicator overlays as JSON, and a
  live bar feed — served by the `protopyne-server` binary, containerized
  without Python. The JSON chart contract feeds the native terminal; no
  browser chart library is involved.
- **Script-to-kernel spike.** A scoped accept-or-refuse transpiler lowers
  `ta.sma`/`ta.ema` Pine v6 scripts to HIR and kernel-calling Rust, and
  refuses everything else (switch, ternaries, `request.*`, loops) with
  reasons instead of miscompiling.
- **Durable price store.** `protopyne-store` is a file-backed tape: a
  versioned CSV `AppendLog` (batch write + fsync, torn-tail repair), a market
  registry, coverage/planning, seals, windowed reads, and Parquet ingest —
  the substrate symbol-scoped chart reads plan through.
- **Pine v6 engine.** `protopyne-kernels` ships a linked-source Pine runtime
  (lexer, modules, `request.security`/lower-timeframe injection from
  datasets only, tick emulation) with a compatibility matrix and a parity
  freeze; unsupported constructs refuse with a reason.

## Crates and dependencies

| Crate | Role |
|---|---|
| `protopyne-types` | Money/time foundations: `Decimal` money and quantity types, timeframes, closed bars, warmup math, option-chain vocabulary |
| `protopyne-kernels` | Indicator kernels, registry, batch/streaming runtimes, the Pine engine, Polars research frames (feature-gated) |
| `protopyne-strategy` | Proposals, strategy elements, risk engine, arming grants, the grant ledger |
| `protopyne-venue` | Venue trait, paper and live venues, journal, safe submission, reconciliation, sessions, the command plane |
| `protopyne-store` | Durable price tape: `AppendLog`, registry, coverage/planning, seals, windowed reads, Parquet ingest |
| `protopyne-view` | Neutral read-model DTOs and the terminal gateway trait |
| `protopyne-chart-model` / `protopyne-chart-scale` | Headless chart data and the scale engine |
| `protopyne-server` | History ingest, backtest runner, HTTP service, live loop, transpile spike, server binary |
| `fluent-egui` / `protopyne-egui` | Paint-edge scaffolding and the native trading terminal (`egui`-only crates) |

Shared monorepo libraries do the heavy lifting: `fluent-wvr`
(components, configuration surfaces, capabilities), `fluent-concurrency`
(`Limiter`, `Scope`, `SupervisedBatch`, `ResultPool`, fallback combinators),
`fluent-dag` (indicator ordering, research replay graphs), and `common-core`
(retries, config loading, error types, calibration, metrics, registries).
External dependencies include `axum`
(HTTP + WebSocket), `csv` (history ingest and the durable journal log),
`chrono` and `rust_decimal` (time and money), `bon`/`serde`/`thiserror`/
`tokio` (plumbing), and `polars` (offline research frames only — never the
hot loop). Barter-rs is a studied reference (order-state modeling), not a
dependency. `egui`/`eframe` are dependencies of the two paint-edge crates
only (`fluent-egui`, `protopyne-egui`); `egui-charts` is reference only.

## Build

Prerequisites: a Rust toolchain and `cargo` (the workspace pins its
settings in the root `Cargo.toml`). No Node.js, no npm, no browser
toolchain — `cargo` alone builds and tests everything.

```sh
# Fast build of the server binary (no run)
make protopyne

# Or with cargo directly
cargo build -p protopyne-server

# Full verification gate (workspace build, tests, clippy)
cargo build --workspace && cargo test --workspace \
  && cargo clippy --workspace --all-targets -- -D warnings
```

## Run

The server needs one thing: bar-history CSV. A small vendored sample ships
with the crate for rehearsal.

```sh
# Start on :9060 over the vendored SPY sample, waiting for /health
make protopyne-start

# Same with overrides
make protopyne-start PROTOPYNE_PORT=9061 \
  PROTOPYNE_HISTORY=/path/to/bars.csv

# Stop it
make protopyne-stop

# Per-component gates (fast path): format / lint / smoke / test / soak
make protopyne-smoke

# Six trading-crate suites (--all-features) + a binary --help dry-run
make protopyne-trading-suite

# Chart crates ride the workspace gate, not protopyne-trading-suite
cargo test -p protopyne-chart-model -p protopyne-chart-scale

# Native desktop terminal (needs a display) and its headless contract
make protopyne-demo          # interactive terminal
make protopyne-demo-check    # headless terminal + chart-shell contract
```

Direct CLI (see `protopyne-server --help`):

```sh
cargo run -p protopyne-server -- \
  --history src/protopyne-server/tests/data/SPY_15M.csv

protopyne-server --bind 127.0.0.1:9060 \
  --history src/protopyne-server/tests/data/SPY_15M.csv
```

## Getting started with the API

With the server up on `:9060`:

```sh
# Liveness
curl -s http://127.0.0.1:9060/health
# {"status":"ok","version":"0.2.0"}

# Run the decision stack over history (bundled rehearsal signal)
curl -s -X POST http://127.0.0.1:9060/proposals/preview \
  -H 'Content-Type: application/json' -d '{"seed":7}' | head -c 300

# Read the rehearsal inbox the preview published (its own named route)
curl -s http://127.0.0.1:9060/proposals/rehearsal | head -c 300

# The authoritative inbox (journaled proposals) and the journal tail
curl -s http://127.0.0.1:9060/proposals | head -c 300
curl -s http://127.0.0.1:9060/journal | head -c 300

# Portfolio: the venue book (source=booked); the rehearsal projection is named
curl -s http://127.0.0.1:9060/portfolio
curl -s http://127.0.0.1:9060/portfolio/rehearsal

# Approve or reject a journaled proposal awaiting a human
curl -s -X POST http://127.0.0.1:9060/proposals/<id>/approve
curl -s -X POST http://127.0.0.1:9060/proposals/<id>/reject \
  -H 'Content-Type: application/json' -d '{"reason":"too much size"}'

# Arm a grant (POST a Grant JSON; loosening an armed grant is 409)
curl -s http://127.0.0.1:9060/grants/current

# Chart data (candle rows) and an SMA overlay, as JSON for the native terminal
curl -s http://127.0.0.1:9060/chart/bars | head -c 200
curl -s 'http://127.0.0.1:9060/chart/overlays?kind=sma&period=14' | head -c 200
```

The WebSocket feed at `ws://127.0.0.1:9060/feed` streams `{"type":"bar",
...}` frames on a rehearsal loop — suitable for driving a native chart
view, never for order flow.

## Dockerized API backend

The container image carries only the release binary, its history file, and
CA certificates on debian-slim — no Python anywhere (the build fails if an
interpreter appears). It serves the same API above on port `9060`, ready
for the native terminal to consume over localhost.

```sh
# Build the image and run the full smoke:
# no-Python proof, /health, one paper preview, chart contract
make protopyne-container-smoke

# Manual equivalent
bash src/protopyne-server/smoke-container.sh
# SMOKE PASS
```

The smoke publishes the container on host port `19060` by default
(`PROTOPYNE_PORT=19060`), so `http://127.0.0.1:19060/health` answers from
the image. See `src/protopyne-server/Containerfile` for the two-stage build
and `src/protopyne-server/smoke-container.sh` for the exact probes.

## Further reading

- `VISION.md` — the full design vision, economics, and platform direction
  (aspirational goals and capabilities, no implementation details).
- `ARCHITECTURE.md` — the implementation architecture map: crates,
  capabilities (with a `status` column), orchestration, reuse findings,
  drill-down queue. This is the authoritative map; there is no separate
  roadmap document.
- `DIVERGENCE_LOG.md` — staged reference-expression divergences still open.
- `RUNBOOKS.md` — operator runbooks: breaker trips, reconcile mismatches,
  kill-mid-submit recovery, backfill holes, ticket drift, live-loop
  start/halt, and the cert-sandbox soak procedure.
