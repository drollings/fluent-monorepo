# guidance — Vision

**A deterministic-first code navigation and maintenance engine: exact
answers from a local mirror, a convergent loop that keeps the mirror
and the baseline clean, and model judgment only where judgment
belongs, measured before it is trusted.**

`guidance` is the fast local memory and maintenance engine an AI
orchestrator consults before and between spending inference. It owns
the work queue, context packs, gates, ledger, and circuit breaker,
and it auto-executes only classes of work that policy marks
mechanical. Everything else is handed up with a compact escalation
packet, or posted to the queue by the orchestrator and done by the
orchestrator. It is a substrate for AI orchestration, not a
competitor to it, and not an autonomous feature builder.

How the two design documents split: this file states the goals and
the externally visible behavior — what `guidance` is, what it
promises, what it refuses, and what it costs. Which file owns what,
how the planes compose, and what already exists so nothing gets
reimplemented live in `ARCHITECTURE.md`.

Scope is Rust-first inside a language-neutral envelope: Rust gets
full fidelity (configuration awareness, exact reference tiers), and
the schema envelope stays open so other languages keep working and
can grow the same fidelity later.

## Goals

Three goals, in priority order. Every release is judged by them.

### 1. High accuracy

Navigation and analysis answers must be correct, cited, and
honestly qualified. Every result carries a citation re-checked
against live bytes at answer time, so a wrong excerpt is
structurally impossible and a stale one is labelled, never hidden.
Answers state how they were resolved, how much to trust them, and
what could not be seen — negative results read as "none found in
covered index," never as proof of absence, and destructive-adjacent
questions are answered conservatively with the blind spots listed.
No model acts uncalibrated: decision points are measured on golden
sets before they steer anything, and the deterministic result stays
authoritative until measurement says otherwise. Human prose and
tests survive every pass byte-identical; a file marked complete has
passed format, lint, compile, and index checks against the exact
inputs recorded in its receipt.

### 2. Token saving

The orchestrator should start each step from a small, sufficient
context instead of loading the tree. `guidance` keeps a structured,
incrementally updated mirror of the codebase — per-file facts beside
their sources, indexes derived and rebuildable from them — and
answers from it in milliseconds: exact identifier lookup first,
then keyword, then fused ranking, then optional embeddings. For a
task or question it assembles a token-budgeted pack of exactly the
excerpts, symbols, and tests a worker needs, deterministically, so
the same inputs yield the same pack. Model calls happen only where
rules cannot decide, and their output is validated the same way.
What gets measured: query latency, time to fixpoint, clean files
reprocessed (target: zero), model calls avoided, and orchestrator
tokens and tool calls saved on a fixed task set.

### 3. Orchestrating work

`guidance` turns "keep this codebase healthy" into a bounded,
resumable loop over exactly the files and components that are
stale, certified at every step by deterministic tools. Policy —
scope, mechanical classes, approvals, budgets — is versioned beside
the code, so changing it re-derives exactly the affected work.
Non-mechanical work becomes reviewable plans with stable identities
that the loop tracks and verifies but never executes itself. Every
run ends in a compact review a person can read in under a minute:
what was fixed, reverted, escalated, and why. State that cannot be
re-derived (queue, calibration, ledger, baselines, escalations) is
durable and backed up; everything derived is rebuildable and
refused when its markers mismatch.

## How it should work

Four pieces, each cheaper than the next, composed in one loop.

**Indexing.** Sources are truth. Per-file sidecars hold derived
facts and receipts and are the unit of freshness, diff, and repair;
the SQLite index (symbols, edges, fragments, summaries) is derived
from sidecars and disposable. Staleness follows make semantics:
each artifact declares the inputs it consumed, and changing one
input re-derives exactly the affected receipts. Whole-repo
freshness costs a timestamp sweep, not a hash sweep; content hashes
decide the close calls, and a verifier audits hashes against
timestamps so a lying clock costs work but never a wrong answer.

**Code navigation.** Queries route by intent to the cheapest
sufficient primitive: exact symbol and edge lookup, then lexical
search, then fused ranking, then vectors. Capability briefs narrow
*where to look* while code recall answers *what it says*. A grammar
decides first; a model may only translate a question into a closed
query plan at an authorized decision point — it translates but
never answers. Every hit and edge carries the tier of machinery
that produced it, and files that do not compile stay navigable,
labelled degraded rather than blocking.

**Maintenance loop.** One pass: observe what is stale, derive the
ordered queue, act on one unit at the cheapest sufficient tier,
verify with deterministic gates, record the outcome, repeat. Fixed
per-file stage order (fingerprint, lightweight index so navigation
never waits on the build, format, lint-plus-compile with
diagnostics attributed by span, fix ladder, verify, index
finalize, completion). Format runs before lint because spans shift;
machine-applicable fixes apply first in reverse offset order, never
through a wholesale fixer that would touch tests or prose. The loop
is done at a fixpoint — mirror fresh, build clean at the relevant
scope, nothing queued, nothing blocked without an owner — verified
by a final witness pass with zero edits and zero diagnostics.
Anything less reports instead of fixing. Termination is
structural: every accepted edit shrinks the diagnostic set,
attempts and identical patches are bounded, and budgets plus the
breaker bound the rest.

**Subagent orchestration.** The control plane owns the policy, work
queue, plans, calibration, and orchestrator contract: what may run,
what ran, what was measured, what needs a human. The orchestrator
posts work, reads the ledger, claims or resolves escalations,
imports plans, and requests context packs; transports start as a
CLI with machine output modes, plus MCP over stdio. Model
assistance arrives through tiers — deterministic fixers, indexed
recipes, triage, verified drafts, escalation — capped by an
autonomy level, with unclassified work holding for review. Drafts
verify alone in scratch before they stand; merges verify again and
bisect on failure. Interactive queries preempt background work;
when the model is unreachable, background items wait without
consuming attempts and the report says so. One loop driver runs per
process per workspace acquisition; concurrent loops are out of
scope until the single-loop guarantees are proven.

## Guarantees

Checkable properties, not aspirations. Each is verified by gates,
not asserted.

- **Primitive navigation is inference-free.** Exact primitives run
  without any model in the call path. Model assistance lives in
  separate, labelled layers.
- **Every result line carries a citation** verified against live
  bytes at read time. Changed source always wins over a stored
  summary; cached answers are keyed by evidence content, so a
  changed file misses the cache instead of serving stale text.
- **No silent staleness or inexactness.** Every answer is labelled
  for freshness and resolution tier, method, and gaps. A strict
  mode refuses anything but fresh.
- **Complete implies verified**, with violations detectable by a
  verifier. A file marked complete holds a full receipt consistent
  with current inputs.
- **Every edit is journaled write-ahead and reversible**, with
  provenance, forward and reverse changes, and gate results. A
  killed run resumes without redoing finished work; a human edit is
  never overwritten.
- **Idempotence and termination.** A converged pass makes zero
  edits and reprocesses zero clean files. Every run ends in done,
  work remaining, stuck with escalations, tool error, or halted by
  the breaker — done is never declared without a clean witness pass.
- **Escalations are self-contained and actionable**: what was
  tried, what rejected each attempt and why, what remains, who owns
  it, and what must be supplied.
- **Crash-safe and non-destructive.** Restart reconciles by hash;
  orphaned scratch is discarded.
- **Tests and prose are not negotiable.** Automation never edits a
  test to make it pass, never touches human commentary, and never
  commits. Every applied change is listed in a reviewable ledger.

## Refusals

Hard refusals, compiled in — no policy loosens them. `guidance`
will not edit tests (read-only oracle), comments, doc comments,
string literals, or markdown; touch build and toolchain
configuration; add suppressions, `unsafe`, panics, or placeholder
code to satisfy a gate; use the network outside the configured
model endpoints; update dependencies; write outside its own state
directory plus the permitted byte ranges of non-test items; fix
pre-existing test failures; act on an uncalibrated model verdict;
treat indexed text as instruction; run workspace-wide gates inside
the loop; or declare done without a clean witness pass.

Policy-gated by default, enabled only by explicit policy or
per-run flags: public API signature changes, item deletion,
configuration-attribute changes, auto-application of model
patches, generated commentary, overriding protected paths.
Behavior-risky lint classes are never auto-executed even when
machine-applicable.

## Cost and limits

Targets are hardware-dependent and release-blocking through the
scorecard ratchet: exact navigation in single-digit milliseconds
cold on large trees, text search in tens of milliseconds,
whole-repo freshness from timestamps in tens of milliseconds,
single-file syntactic indexing and dirty-set formatting in tens of
milliseconds, toolchain waves bounded by the compile floor, pack
assembly in tens of milliseconds with optional semantic recall
around a hundred milliseconds, model attempts in seconds and free
on cache hit.

Known limits, published with answers they touch: compile truth is
package-scoped (the seal is the backstop); syntactic edges can
over- or under-report trait dispatch and macros, labelled not
hidden, with blind spots attached (macro expansion, build output,
dynamic dispatch, disabled configurations, anything past
check-level); local model quality bounds drafting — gates keep bad
patches out, so a weak model yields escalations, never bad edits;
calibration needs labels, so a new project runs deterministic and
held-review until entries accumulate; the test gate runs once per
converged wave per affected package, not per file.

## Non-goals

- Not a model and not an IDE — the fast local memory an agent
  consults before and between inference.
- Not a build system. The native toolchain owns compiling, linting,
  and testing incrementally; `guidance` decides what to ask it about.
- Not an autonomous feature builder. Only policy-marked mechanical
  work auto-executes.
- Not a rewrite of history. Generated commentary never enters
  source; staleness markers are validated, never assumed.
- No side effect on a verdict alone, no edited tests, no commits.
- One maintenance loop driver per process acquisition; concurrent
  loops are out of scope until the single-loop guarantees are proven.
