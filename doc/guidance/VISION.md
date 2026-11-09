# guidance — Vision

**A deterministic-first code navigation and maintenance engine: exact
answers from a local mirror, a convergent loop that keeps the mirror
and the baseline clean, and model judgment only where judgment
belongs.**

## Mission

guidance has two planes over one mirror of the codebase.

- **Read plane.** Answer navigation and analysis questions ("where is
  X defined?", "what calls Y?", "how does this codebase handle Z?")
  from a structured, incrementally updated mirror, with verbatim
  excerpts and line citations, before an orchestrator spends
  inference.
- **Maintain plane.** Run a bounded, resumable loop over the
  mechanical work of keeping a codebase healthy: knowledge-base
  freshness, formatting, lint, compile, simple fixes. The loop works
  through exactly the files and components that are stale, certifies
  every step with deterministic tools, and converges on a clean
  state.

guidance is a substrate for AI orchestration, not a competitor to it.
It owns the work queue, context packs, gates, ledger, and circuit
breaker, and it auto-executes only classes of work that policy marks
mechanical. Everything else is handed up with a compact escalation
packet, or is posted to the queue by the orchestrator and done by the
orchestrator.

## The maintenance loop

The loop synthesizes three disciplines:

- **Incremental maintenance** supplies the work queue. The stale set
  is derived deterministically from inputs; no model and no
  hand-written task list decides what needs work.
- **Ralph-style iteration** supplies the execution discipline. Each
  pass handles one bounded unit of work from fresh context; state
  lives in files and the mirror database, never in a model's memory;
  deterministic gates certify progress; a circuit breaker stops runs
  that are not converging.
- **Plan-then-execute (BMALPH-style) steering** supplies the human
  layer. People set intent and policy up front, intent becomes
  durable artifacts the loop consumes, and re-planning merges with
  completed work instead of restarting it.

One pass: **observe** what is stale, **derive** the ordered queue,
**act** on one unit at the cheapest sufficient tier, **verify** with
deterministic gates, **record** the outcome, repeat.

### Fixpoint

The loop is done when the workspace is at a fixpoint:

- the mirror and every derived artifact are fresh for their inputs;
- the build is clean at the relevant scope (formatted, compiling,
  lint-clean, tests passing);
- nothing is queued, and nothing is blocked without an owner.

Running the loop again at a fixpoint does zero work. A loop that
cannot reach a fixpoint must say why and stop; it must not spin.

### Work items

Every item is in exactly one state: queued, in progress, verified,
failed, or escalated. State is persisted, so a crash or interruption
resumes rather than restarts, and finished work is never redone. The
queue is dependency-ordered, idempotent, and derived, never authored
by a model.

## Principles

- **Deterministic first.** Exact identifier lookup, then keyword, then
  fusion, then vectors; formatters and machine-applicable fixes
  before any model. Nothing exact is ever re-decided by a model.
- **Make semantics for derived artifacts.** Every derived artifact
  (sidecar metadata, commentary, index entries) declares its inputs
  and is stale if any input changed. Inputs include the source
  content, the prompt version, the generating model's identity, and
  the maintenance policy. Changing policy or model makes exactly the
  affected items stale and nothing else. Two fingerprints per member,
  one for its interface and one for its full content, keep a
  body-only edit from cascading through every dependent.
- **Narrowest scope that can answer.** Work runs at three scopes:
  per-file (mirror, formatting, commentary), per-component (compile,
  lint, test), and workspace-wide. The workspace-wide gate is a human
  pre-commit action, never a loop step. guidance chooses which units
  to ask about and delegates incrementality to the native toolchain;
  it does not reimplement build caching.
- **Fix before judge.** Gate order follows convergence: deterministic
  fixers that rewrite files (formatting, machine-applicable lint
  fixes) run first; then compile, lint, and test certify the result;
  then the mirror and its derived views are brought current.
- **Proposals versus gates.** Models triage and draft; deterministic
  tools certify. Nothing a model says is accepted without a passing
  gate, and no decision is the sole guard of a side effect.
- **Grounded or silent.** Synthesis receives verbatim source
  excerpts, never bare paths or memory; no excerpts, no synthesis.
  Changed source always wins over a stored summary.
- **Human prose is sacred.** Human-written commentary survives every
  re-sync untouched. Machines may nominate refreshes, never impose
  them, and never over cosmetic diffs.
- **Never silently exact.** Analysis answers state how they were
  resolved and how much to trust them. Questions whose wrong answer
  invites destructive action (no callers, safe to delete) are
  answered conservatively and say what could not be seen.

## Tiers of action

Each tier is cheaper than the next, and nothing moves up without
evidence that the lower tier could not do the job.

0. **Deterministic fixers.** Formatters and machine-applicable lint
   fixes. Zero model calls.
1. **Triage.** A small decision model classifies what the tools could
   not fix: mechanical, judgment, or ambiguous.
2. **Draft.** A generative model drafts patches only for items policy
   marks mechanical, in an isolated working copy, verified by the
   narrowest gate for that scope and reverted on failure.
3. **Escalate.** Judgment and ambiguous items go to the orchestrator
   or a human as an escalation packet: the failure excerpt, the
   relevant context, and what was already tried.

## Loop safety

- **Isolation.** Patches are applied and verified in a scratch copy
  before anything touches the working tree.
- **Optimistic concurrency.** A patch applies only if its target is
  unchanged since the loop read it; otherwise the item is requeued.
  A human editing a file is never overwritten.
- **Circuit breaker.** Bounded attempts per item, and a stop when the
  same failure repeats, the revert rate climbs, or a pass makes no
  progress.
- **Tests are not negotiable.** Automation never edits a test to make
  it pass, and never commits. Every change it applies is listed in a
  reviewable ledger.
- **Untrusted input.** Indexed source text is data, not instruction.

## Calibrated judgment

Where a bounded semantic judgment is the real task (routing an
ambiguous query, filtering excerpts, deciding whether a diff changes
what a comment describes, triaging a failure), a small decision model
judges inside strict guardrails. Every verdict runs rule, then cache,
then model, then static fallback, and fails closed to the
deterministic path. An uncalibrated model measures; it never acts:
each decision point stays shadow-only until a calibration entry
authorizes that point, model, and question version, and the
deterministic result stays authoritative until then.

## Capabilities

### Read plane

- **Incremental codebase mirror.** Parse sources into structured
  metadata (signatures, line numbers, content hashes) with identical
  schema across supported languages; preserve human commentary;
  persist everything in a queryable local database.
- **Staged question answering.** Route each query by intent to the
  cheapest search primitive that can satisfy it, answer with cited
  excerpts, and report gaps and confidence honestly.
- **Analysis.** Definitions, callers, callees, imports, and
  dependents, with the resolution method and confidence attached.
- **Context packs.** For a task or question, a token-budgeted bundle
  of exactly the excerpts, symbols, and tests a worker needs, so each
  loop pass and each orchestrator step starts from a small, sufficient
  context.
- **Impact and scope.** Which dependents a change touches, which
  component owns a file, and the narrowest gate that covers it.

### Maintain plane

- **Stale-only knowledge base.** Source, derived artifacts, and
  indexes are kept current by processing only stale inputs; clean
  files are never reprocessed.
- **Certified commentary.** Generated prose is written only after
  deterministic checks pass: referenced symbols exist, line
  references resolve, and size fits budget.
- **Baseline hygiene.** Formatting, compile, lint, and test results
  are brought clean through the tiers above, at the narrowest scope.
- **Pre-commit integrity.** One command brings the mirror and the
  baseline to a fixpoint for the changed set and reports what, if
  anything, remains.

### Handoff contract

- **To the orchestrator.** Machine-readable answers (intent,
  confidence, results, citations, gaps, resolution method), plus
  human-readable markdown. Escalation packets are small, complete,
  and self-contained.
- **From the orchestrator.** The orchestrator can post work to the
  queue, read the ledger, and consume context packs. guidance does
  not implement features on its behalf.

## Human steering

- **Maintenance policy.** A declarative, versioned statement of
  scope, which fix classes are mechanical, which documents are
  human-owned, attempt limits, and what requires approval. Policy is
  an input to staleness, so changing it re-derives exactly the
  affected work.
- **Plans.** Non-mechanical work becomes a reviewable plan of ordered
  items. When a plan is revised, completed items are preserved, new
  and changed items are merged in, and a changelog tells the loop
  what is new.
- **Review.** Every loop run ends with a compact summary a person can
  read in under a minute: what was fixed, what was reverted, what was
  escalated, and why.

## Priority and resources

Interactive queries preempt background maintenance. Maintenance work
is preemptible and resumable, and shares model capacity by priority:
interactive judgments first, shadow measurement next, background
drafting last. A busy or unreachable model degrades to the
deterministic path with an explicit unavailable state, never to a
silent wrong answer.

## Success measures

guidance is judged by the work it takes off an orchestrator, each
measured, ratcheted, and tracked per release:

- **Correctness.** Navigation and analysis accuracy; zero
  ungrounded answers; zero wrongly confident "no callers"; human
  prose preserved byte for byte.
- **Speed.** Query latency (p50 and p95), time for a one-file
  incremental pass, and time to reach a fixpoint.
- **Efficiency.** Clean files reprocessed (zero), model calls avoided,
  and orchestrator tokens and tool calls saved on a fixed task set.
- **Offload.** Share of findings resolved without escalation, patch
  revert rate, and rate of patches that pass the gate but change
  behavior (zero tolerated in mechanical classes).
- **Convergence.** Passes to fixpoint, and loops stopped by the
  breaker.

## Non-goals

- Not a model and not an IDE; it is the fast local memory and
  maintenance engine an agent consults before and between spending
  inference.
- Not a build system. The native toolchain owns compiling, linting,
  and testing incrementally; guidance decides what to ask it about.
- Not an autonomous feature builder. It auto-executes only
  policy-marked mechanical work.
- Not a rewrite of history. Generated commentary lives in source or
  in the database, and staleness markers are validated, never
  assumed.
- Cached answers never override freshness: changed source always wins
  over a stored summary.
- No side effect on a verdict alone, no edited tests to make a gate
  pass, and no commits.
- One maintenance loop per workspace at a time. Concurrent loops are
  out of scope until the single-loop guarantees are proven.
