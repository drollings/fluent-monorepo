# CreditFlow — control-plane wiring decision (ledger tier feed)

Decision record for the Tokio Streamline roadmap milestone M7
("CreditFlow: control-plane review and wiring").

## Candidates reviewed

1. **Ledger tier feed (WIRED).** `ContentNodeStore::tier_events` is an
   `mpsc::UnboundedSender<NodeId>` (`src/router/src/node_store.rs:67,139-146`);
   the producer is the agent loop (`LedgerAgentCoordinator::run_agent`
   enqueues nodes for LOD4/LOD5, `ledger/orchestrator.rs:323`) and the
   consumer is the tier worker (`ledger/tiering.rs:88-110,169`). It is
   background derivation — off the request hot path — and unbounded, so a
   burst of agent turns can grow memory without bound. This is exactly the
   chain-backpressure shape `CreditFlow` models (producer acquires credit,
   consumer `recv()`s and bumps after processing). **Selected.**
2. **`coral/src/cache/reactor.rs`** — the reactor's queue is already bounded
   by the worker-pool cap/queue-capacity (`ResultPool`); no producer-consumer
   imbalance found. Not wired.
3. **Sidecar residency loop** (`instances/pool.rs`, `InstanceManager`) —
   polling-based, not producer-consumer; low fit. Not wired.

## Excluded (compute/request hot paths)

- Router streaming dispatch (`server/dispatch.rs`, `dispatch/backend.rs`).
- The classifier / escalation `ResultPool` fan-out
  (`dispatch/escalation/modes.rs`).
- Any per-request path. `CreditFlow`'s `AtomicIsize` CAS + blocking send is
  fine in the control plane and wrong on the request path.

## Wiring

- The tier feed channel becomes a **bounded** `tokio::sync::mpsc`
  (`queue_capacity`, default 1024) — the memory bound.
- `LedgerTierWorker` owns a `flow::new(CreditSpec { initial, more_after })`
  pair. The async producer path (`run_agent` step 5) calls
  `enqueue_with_credit(id).await`, which acquires a credit token (blocking
  when exhausted) before forwarding the `NodeId`.
- The consumer (`run` loop / `process_batch`) calls
  `credit_receiver.recv()` after each processed node, releasing credit.
- The store's **synchronous** write paths (`insert_node` / `record_result`)
  keep enqueuing via `try_send` on the bounded channel (no credit — a sync
  context cannot await a bump). On `Full` they skip: agent nodes are still
  covered by `run_agent`'s credit-gated enqueue, and boot backfill
  (`node_ids_needing_tier`) catches stragglers. The bounded channel bounds
  memory even on the sync path; the credit token adds producer blocking on
  the burst producer the roadmap targets.
- Config knobs: `ledger.tier_credit_limit` (default 256) and
  `ledger.tier_credit_more_after` (default 8), threaded into `TierConfig`.

## Tests

- The crate's `flow.rs` tests (`tests/m4.rs`) already cover the required
  credit semantics: producer blocks at credit exhaustion, consumer bump
  releases it, `is_blocked()` reflects state.
- A tiering-level test verifies `enqueue_with_credit` blocks when credit is
  exhausted and releases after the worker processes a node.
- The orchestrator/tiering suites (`cargo test -p fluent-router --lib ledger`)
  stay green.
