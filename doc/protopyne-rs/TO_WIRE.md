# protopyne-rs — Unwired Surfaces Inventory (`TO_WIRE.md`)

_Read-only report for the unwired-module disposition. Every surface below
builds, is tested, and is re-exported, but has **no production caller** today:
the tests (and the soak harness) are the only drivers. Anchors are
`path :: Symbol`. "Seam" names the production caller that is missing; "wiring
cost" is the honest effort, not a promise._

## Summary

| Surface | Home | Wired today | Missing seam | Wiring cost |
|---|---|---|---|---|
| Depth book + dark detectors | `venue/src/depth.rs` | nothing (tests only) | market-event consumer | M–L (needs a depth feed) |
| Manual tickets | `venue/src/ticket.rs` | wired (C73): routes + gateway journal, terminal panel renders/acts | — | done |
| Operator close/cancel | `venue/src/command.rs :: close_all_intents`/`cancel_all_ids`, `session.rs :: submit_close_intents`/`cancel_orders` | wired (C72): route + gateway journal, the live loop executes | — | done |
| Out-of-band alerts | `venue/src/alert.rs`, `session.rs :: poll_alerts` | wired (C71): Scope-owned poller into a `FileSink` on the live loop | — | done |
| Supervised account stream | `venue/src/stream.rs`, `session.rs :: spawn_supervised_stream`/`attach_stream_breaker` | breaker + health unit wired into `run_bar`; `boot_live` attaches nothing | connect closure in `boot_live` | M |

Legend: S ≈ a few hours, M ≈ a day, L ≈ multi-day + a data source.

---

## 1. Depth book + dark detectors

- **What exists.** One pure `DepthBook` per symbol (`snapshot_replace`,
  `apply_delta` with sequence-gap refusal, `top()` delegating to `Quote`),
  a `BookManager` keyed by `SymKey` (`apply_event(&MarketEvent)`, `scan` →
  `Vec<DarkFinding>`), and dark detectors `is_crossed`/`is_stale` returning
  reasons. `DEFAULT_MAX_LEVELS = 64` bounds each book.
- **Already composed.** `BookManager::apply_event` consumes the shared
  `MarketEvent` fan-out (`venue/src/market.rs`), so a stream pump handing it
  `BookDelta`/`Quote` events needs no new event type.
- **Wiring.** A `Scope`-owned consumer per subscribed symbol that folds the
  market-event fan-out into one `BookManager`, then journals/telemetry-writes
  `scan` findings. By its own module contract the detectors stay
  **record-only** (never gate a fill) until the frozen detector suite is
  operator-accepted; `top()` is the only read that could feed quote synthesis.
- **Blocker.** There is no depth feed in the prototype: `TastytradeTransport`
  serves top-of-book quotes and chain snapshots, not ladders. Wiring needs a
  venue/book-delta source first, so this is the most expensive surface.

## 2. Manual tickets (wired)

- **What exists.** `ManualTicket` (an `OrderIntent` + issue stamp, ticket id =
  client id), `TicketTracker` (`issue`/`confirm`, `unconfirmed`, `drifts`,
  `confirmed_net`), journaled `issue_journaled`/`confirm_journaled`, a
  `replay`, and an import seam `apply_import(&dyn TicketRead)` guarding a
  read-only import behind a trait. `TicketDrift`/`DriftKind` classify
  quantity/price/import disagreement.
- **Already composed.** `StateReplica` skips `manual_ticket.*` rows, so a
  ticket never reads as a phantom order in the monitor view.
- **Wired today.** `POST /tickets/issue`, `/tickets/{id}/confirm`,
  `/tickets/import` plus `GET /tickets/waiting|/drifts` and the matching
  gateway verbs journal through the tracker's journal-first path; the
  terminal ticket panel issues, confirms at ticket price, and reads drift.
  The import reuses the `TicketRead` seam (posted lines served, a dark
  reader still reporting without touching state).
- **Blocker.** None was; a live Robinhood read remains a future adapter
  behind the same seam.

## 3. Operator close / cancel (wired)

- **What exists.** `close_all_intents` (one atomic intent per leg-group,
  deterministic `close-{index}-{underlying}` ids, refuses a close into naked
  short) and `cancel_all_ids` (active-order listing); the `CommandUnit`
  `CloseAll`/`CancelAll` arms emit them and journal the audit row. The session
  exposes `submit_close_intents` (closes route even while halted; takes a
  `RiskApproved`) and `cancel_orders`.
- **Wired today.** `POST /operator/close-all` and `/operator/cancel-all` plus
  the `close_all`/`cancel_all` gateway verbs journal the identical command
  row; the live loop's per-bar fold (`server/src/operator.rs ::
  execute_operator_commands`) executes it outside the entry gate. Closes
  settle their own budget claim through `record_operator_fill` (lifetime
  tallies stay monotonic); cancels hold no budget.
- **Wiring.** One operator entry point — an HTTP route and/or a journaled
  command — that builds the `CommandUnit` (or calls the session helpers with
  an exit proposal's `RiskApproved`) and journals before acting. This is the
  cheapest surface: the order path already exists through `safe_submit`.
- **Blocker.** A risk token for the closing proposal must be minted through
  the normal `RiskEngine` path (no token-free order path), so the route needs
  an exit proposal, not a bare "close everything" bypass.

## 4. Out-of-band alerts (wired)

- **What exists.** `Alert` (`AlertSource` × `AlertClass`), `AlertSink` with
  `MemorySink`/`FileSink`/`FanoutSink`, edge-triggered `AlertCursor`, and
  `poll_alerts` draining the breaker trip, reconcile mismatch, backfill hole,
  and kill-switch transition banks exactly once.
- **Wired today.** `TradingSession::spawn_alert_poller` runs `poll_alerts`
  under the feed scope on its own cadence (`ALERT_POLL_INTERVAL`) into a
  `FileSink`; `boot_live` spawns it when `LiveBoot.alerts_path` is set
  (`--live-alerts PATH`). VISION §10 wants alerting independent of the UI;
  a file tail satisfies that with no new transport.
- **Blocker.** None was: delivery is reporting-only and never gates a trade.

## 5. Supervised account stream

- **What exists.** `SupervisedStream` (jittered-backoff reconnect under
  containment, `StreamAbort` clean stop), `StreamBreaker`
  (consecutive-error/restart trip → disarms entries only), `StreamHealthUnit`,
  and `TastytradeTransport::run_stream`/`pump_account_events` producing
  `AccountEvent`s.
- **Wired today.** `TradingSession::run_bar` prepends `StreamHealthUnit` when
  a breaker is attached, and `spawn_supervised_stream` wires the driver; but
  `boot_live` attaches neither, so the live session runs with no account
  stream and no breaker.
- **Wiring.** In `boot_live`: build `SupervisedStream` from `StreamConfig`,
  supply the `connect` closure that dials the venue stream and returns the
  `mpsc::Receiver<String>` of raw messages (`SupervisedStream::run` pumps them
  through `TastytradeTransport::pump_account_events`), attach it, and fold
  events through `TastytradeVenue::apply_account_event`. This closes the live
  truth loop (fills/balances) that reconciliation currently assumes.
- **Blocker.** The transport's stream endpoint/credentials wiring (token
  handoff) is the remaining piece; the supervision machinery is complete.

---

## Hygiene notes (context for the disposition)

- `venue/src/depth.rs` module docs cite an internal "M5 calibration suite"
  label; project rules forbid milestone identifiers in source — scratch it
  when this surface is touched.
- `ARCHITECTURE.md`'s capability index carries the disposition in its `Status`
  column: the depth book is `library-only` (C37), the operator close/cancel,
  ticket, and alert-delivery surfaces are `wired` (C72/C73/C71), and the
  streaming supervision machinery is `wired` with its live-boot gap named
  (C38).
- No deletion is proposed for any surface: all five are exercised by real
  test suites and carry primary-source invariants (sequence-gap refusal,
  naked-short refusal, edge-only alerting, terminal-vs-transient stream
  classification).

## Decision recorded

- Mark **depth** `library-only`: no depth feed exists, so there is nothing to
  wire it to. Revisit only when a book-delta source lands.
- Mark **close/cancel**, **tickets**, and **alerts** `planned` (C71): wiring
  adds an operator route/panel and a session-owned poller, not new logic.
- The **supervised account stream** stays `wired` machinery (C38) with a
  `planned` boot attachment: the session consumes the health unit when a
  breaker is attached, but the live boot still attaches no producer.
