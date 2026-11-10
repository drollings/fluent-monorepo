# guidance — Vision

**A deterministic-first code navigation and maintenance engine: exact
answers from a local mirror, a convergent loop that keeps the mirror
and the baseline clean, and model judgment only where judgment
belongs, measured before it is trusted.**

## Mission

guidance has three planes over one mirror of the codebase.

- **Read plane.** Answer navigation and analysis questions ("where is
  X defined?", "what calls Y?", "how does this codebase handle Z?")
  from a structured, incrementally updated mirror, with verbatim
  excerpts and hash-verified citations, before an orchestrator spends
  inference. Primitive navigation is inference-free; semantic help is
  a separate, labelled layer.
- **Maintain plane.** Run a bounded, resumable loop over the
  mechanical work of keeping a codebase healthy: knowledge-base
  freshness, formatting, lint, compile, simple fixes. The loop works
  through exactly the files and components that are stale, certifies
  every step with deterministic tools, and converges on a clean
  state.
- **Control plane.** Own the policy, work queue, plans, calibration,
  and orchestrator contract around the other two planes: what may run,
  what ran, what was measured, and what needs a human.

The mirror is sidecar-first: per-file facts live beside their sources
in a durable, diffable form, and every index is derived and
rebuildable from them. Nothing derived is trusted without its marker.
Control state that cannot be re-derived (queue, calibration, ledger,
baselines, escalations) lives in a separate durable store that is
backed up, never regenerated.

guidance is a substrate for AI orchestration, not a competitor to it.
It owns the work queue, context packs, gates, ledger, and circuit
breaker, and it auto-executes only classes of work that policy marks
mechanical. Everything else is handed up with a compact escalation
packet, or is posted to the queue by the orchestrator and done by the
orchestrator.

Scope is Rust-first inside a language-neutral envelope: Rust gets
full fidelity (feature and configuration awareness, exact reference
tiers), and the schema envelope stays open so other languages keep
working and can grow the same fidelity later.

## Guarantees

These are the properties a finished guidance must hold. Each is
checkable, not aspirational.

- **Primitive navigation is inference-free.** Exact primitives run
  without any model in the call path or the crate closure. Model
  assistance, where it exists, lives in separate routing and recall
  layers and is always labelled.
- **Every result line carries a citation** that was hash-verified
  against live bytes at read time. A wrong excerpt is structurally
  impossible; a stale one is labelled, never hidden.
- **No silent staleness.** Every answer is labelled for freshness
  (fresh, pending, stale, or dependent-stale). A strict mode refuses
  anything but fresh.
- **No silent inexactness.** Answers carry their resolution tier,
  method, confidence, and gaps. Negative results read as "none found
  in covered index", never as proof of absence. Destructive-adjacent
  questions are answered conservatively with an explicit list of what
  could not be seen.
- **Complete implies verified.** A file marked complete has passed
  format, lint, compile, and index checks against the exact inputs
  recorded in its receipt. Violations are detectable by a verifier.
- **Every edit is journaled write-ahead and reversible**, with
  provenance, forward and reverse changes, and gate results.
- **Idempotence and no wasted work.** A full pass over a converged
  tree makes zero edits and reprocesses zero clean files. The run
  report counts reprocessed clean files; the target is zero.
- **Termination.** Every run ends in done, stuck (with escalation
  records), halted (breaker tripped, resumable), or tool error.
  Done is never declared without a clean no-op verification pass.
- **Escalations are self-contained and actionable**: what was tried,
  what rejected each attempt and why, what remains, who owns it, and
  what a human or orchestrator must supply.
- **No model acts uncalibrated.** Unauthorized decision points are
  measured, never obeyed, and fail closed to the deterministic path.
- **Crash-safe and non-destructive.** A killed run resumes without
  redoing finished work. A human edit is never overwritten.
- **Tests and human prose stay byte-identical.** This is verified by
  gates and property checks, not asserted.

## Refusals

Some refusals are hard: compiled in, and no policy can loosen them.
guidance will not edit tests (they run only as a read-only oracle),
edit comments, doc comments, string literals, or markdown, touch build
and toolchain configuration, add suppressions or new unsafe, panic,
or placeholder code to satisfy a gate, use the network, update
dependencies, commit to the user's branch, write outside its own state
directory plus the permitted byte ranges of non-test items, fix
pre-existing test failures, act on an uncalibrated model verdict,
treat indexed text as instruction, run workspace-wide gates inside the
loop, or declare done without a clean witness pass.

A second class is policy-gated: refused by default, enabled only by
explicit policy or per-run flags. This covers public API signature
changes, item deletion, configuration-attribute changes,
auto-application of model patches, generated commentary, and
overriding protected paths. Behaviour-risky lint classes are never
auto-executed even when machine-applicable.

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
- **Plan-then-execute steering** supplies the human
  layer. People set intent and policy up front, intent becomes
  durable artifacts the loop consumes, and re-planning merges with
  completed work instead of restarting it.

One pass: **observe** what is stale, **derive** the ordered queue,
**act** on one unit at the cheapest sufficient tier, **verify** with
deterministic gates, **record** the outcome, repeat.

Deterministic stages run batched — one toolchain invocation covers
every dirty file in its scope, since compile latency has a floor per
invocation. Per-item isolation is kept only where it earns its keep:
model attempts, which verify alone before any merge. Accepted patches
for one scope are merged and verified once; a merged failure is
bisected to the culprit and the rest requeued.

Per-file pipeline order is fixed: fingerprint, then lightweight index
so navigation never waits on the build, then format, then combined
lint and compile with diagnostics attributed to files by span, then the
fix ladder to fixpoint, no-progress, or budget, then final verify
(format check, lint and compile clean, tests against the baseline),
then index finalization, then completion marking.

Format runs before lint because spans shift after formatting, and the
lint invocation is also the compile step. Compile here means
check-level; post-monomorphization and linker errors belong to the
seal step, not the loop. Machine-applicable suggestions are applied by
guidance itself from structured diagnostics, filtered by permitted
ranges and applied in reverse offset order, never by a wholesale
fix tool that would also rewrite tests or prose.

### Store of record

Four kinds of store share one mirror, with different durability:

- **Source** is truth.
- **Sidecars** are per-file derived facts and receipts. They are the
  source of truth for the index and the unit of freshness, diff, and
  repair.
- **The index** (symbols, edges, trigram postings, optional
  embeddings) is derived from sidecars and disposable. It carries a
  generation marker over the sidecar manifest; queries refuse a
  mismatched index and rebuild instead of answering from it.
- **Control state** (work queue with write-ahead intents, calibration
  entries, journal, escalations, plans, reports, environment and
  baseline records) is not derivable. Losing it loses the queue, the
  measurements, and the audit trail, so it is backed up, never
  regenerated.

Generated commentary, when enabled, lives only in sidecars, always
labelled generated. It never enters source. References from tests to
non-test items are recorded as read-only edges for impact and pack
assembly; tests themselves are never listed, processed, or edited.

### The mtime protocol

Whole-repo freshness must cost a stat sweep, not a hash sweep. The
protocol is:

- A file is **complete** exactly when its sidecar's modification time
  is strictly newer than its source's modification time. A missing
  sidecar means incomplete.
- **Completion writes** go to a temporary file, are atomically
  renamed into place, then have their timestamps set to no earlier
  than the source time plus the filesystem granularity. Granularity
  is probed at initialization so strict-newer holds on coarse
  filesystems.
- **Always mark on completion**, even when nothing changed. A touched
  or checked-out file whose content hash still matches is revalidated
  by one hash and one timestamp update, with no lint, compile, or
  index work.
- **Stuck files stay incomplete**: the sidecar is still written with
  status, attempts, and the last good index, but its timestamp is set
  older than the source so the freshness verdict stays truthful while
  the record explains why.
- **Invalidation never touches source.** Dependents of a changed
  interface have their sidecars backdated, never their sources.
- **Renames** move an orphaned sidecar when its recorded content hash
  matches the new source, instead of reprocessing.
- **Skew defence**: a verifier audits hashes against timestamps for
  clock skew and copied timestamps, and every query independently
  re-verifies the cited range, so a lying timestamp can cost work but
  never produce a wrong citation.

### Truth model and inputs

Staleness follows make semantics. Environment is a vector of named
input components (toolchain, format and lint configuration, lockfile,
configuration matrix, policy sections, parser and schema versions,
model and prompt identities per role, embedder identity), not a single
hash. Each artifact records exactly the components its receipt
consumed: formatting consumes source layout plus toolchain and format
configuration; lint and check consume item hashes, dependency
signatures, toolchain, lint configuration, lockfile, and matrix cell;
index entries consume source plus parser and schema; model patch cache
entries consume context hash plus model and prompt identity;
embeddings consume item hash plus embedder identity.

When a component changes, one sweep over compact sidecar headers
backdates just the receipts that consumed it. Changing a prompt never
re-lints; changing lint configuration never re-embeds. The per-item
work key combines item hash, dependency signatures, and consumed
components; a matching key reuses the stored receipt.

Two fingerprints per member, one for its interface and one for its
full content, keep a body-only edit from cascading. Content identity
is a hash over the normalized token stream, so whitespace and plain
comments do not invalidate downstream work, while documentation hashes
stay separate so prose edits do not masquerade as code changes.

### Fixpoint

The loop is done when the workspace is at a fixpoint:

- the mirror and every derived artifact are fresh for their inputs;
- the build is clean at the relevant scope (formatted, compiling,
  lint-clean, tests passing);
- nothing is queued, and nothing is blocked without an owner.

Running the loop again at a fixpoint does zero work — zero edits and
zero reprocessing of clean files. Done is never declared from one
edit's success alone: a final witness pass over the changed set must
come back empty. A loop that cannot reach a fixpoint must say why
and stop; it must not spin.

Done is layered. File done means the timestamp invariant holds with a
full receipt consistent with current inputs. Package done means every
file in the package is file-done with format, lint, and check clean
across the matrix and tests showing no new failures against the
recorded baseline. Changed-set fixpoint, which is what the loop
converges to, additionally requires an empty queue, owned blocks only,
and a witness pass with zero edits and zero diagnostics. Workspace
seal is a separate human or CI action over the whole tree, including a
full build that catches errors below check-level and feature
unification failures per-package gates cannot see.

Workspace-wide verification is a human or CI action, never a loop
step: per-package builds can pass while whole-workspace unification
fails, and that deserves an explicit gate rather than a hidden one.

Termination is structural. Every accepted edit strictly shrinks the
diagnostic set, so the primary measure is monotone non-increasing.
Attempts and identical patches are bounded, a visited-state set of
content hashes detects cycles such as formatter-versus-lint fights,
and budgets plus the breaker bound everything else. Every run ends in
one of a small set of exit states: done, work remaining, stuck with
escalations, tool error, or halted by the breaker and resumable.

### Work items

Every item is in exactly one state: queued, in progress, verified,
failed, or escalated. Escalated items carry a substate that says
whether they need a human, failed on something external, or are
blocked with a named owner. State is persisted, so a crash or
interruption resumes rather than restarts, and finished work is never
redone. The queue is dependency-ordered, idempotent, and derived,
never authored by a model. Known failures carry an owner instead of
blocking the fixpoint.

Before applying a patch the loop writes a write-ahead intent record
with pre- and post-hashes. Restart reconciles by hash: source equal
to post-hash means mark applied, equal to pre-hash means requeue,
anything else means a human edited in between, so the intent is
dropped and the item requeued. Orphaned scratch work is discarded.

Unit kinds span file-level work (fingerprint, format, index),
package-level work (lint, check, tests), item-level fix attempts, and
optional commentary refresh and plan items. Ordering is
dependency-first (leaves before dependents), then by ascending blast
radius. First runs on a dirty tree ratchet: record the baseline, then
work leaf-first in capped waves so the first run produces many small
reviewable entries instead of one large diff.

## Principles

- **Deterministic first.** Exact identifier lookup, then keyword, then
  fusion, then vectors; formatters and machine-applicable fixes
  before any model. Nothing exact is ever re-decided by a model.
- **Make semantics for derived artifacts.** Every derived artifact
  declares the inputs it consumed and is stale if any changed. Inputs
  include the source content, the toolchain and lint configuration,
  the prompt version, the generating model's identity, and the
  maintenance policy. Changing one input re-derives exactly the
  affected receipts and nothing else. Two fingerprints per member,
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
  gate, and no decision is the sole guard of a side effect. Static
  tables decide before any model is asked, and model output is
  constrained to a closed set of verdicts it cannot wander out of.
- **Grounded or silent.** Synthesis receives verbatim source
  excerpts, never bare paths or memory; no excerpts, no synthesis.
  Every cited line carries a citation that was re-hashed against live
  bytes at answer time, so a wrong excerpt is structurally
  impossible; a stale one is labelled, never hidden. Changed source
  always wins over a stored summary.
- **Human prose is sacred.** Human-written commentary survives every
  re-sync untouched. Machines may nominate refreshes, never impose
  them, and never over cosmetic diffs. Formatting that would alter
  prose under project settings is treated as a conflict finding with
  prose-preserving splicing, not as convergence failure.
- **Never silently exact.** Analysis answers state how they were
  resolved and how much to trust them. Every relation carries the
  tier of machinery that produced it. Blind spots (macro expansion,
  build output, dynamic dispatch, disabled configurations, anything
  below check-level compilation) are published and attached to answers
  that touch them. Questions whose wrong answer invites destructive
  action (no callers, safe to delete) are answered conservatively and
  say what could not be seen — deletion is never proven by an index.
  For private items the compiler's own dead-code analysis across the
  matrix is the authority; for anything public, exported, derived, or
  macro-adjacent the answer is at best unknown.

## Tiers of action

Each tier is cheaper than the next, and nothing moves up without
evidence that the lower tier could not do the job.

0. **Deterministic fixers.** Formatters and machine-applicable lint
   fixes. Zero model calls.
1. **Indexed recipes.** Error-code-keyed repairs backed by the index
   (unambiguous imports, missing mutability, trait-method imports).
   Zero model calls.
2. **Triage.** A static table first; then a small decision model, if
   enabled, classifies what the tools could not fix into mechanical,
   local rewrite, human judgment, or external failure.
3. **Draft.** A generative model drafts patches only for items policy
   marks mechanical, each verified alone in a scratch copy, then
   merged per scope and verified again — with the merge bisected to
   the culprit on failure and the rest requeued. Unclassified work
   verifies in scratch and holds for review, never auto-applies.
4. **Escalate.** Judgment and ambiguous items go to the orchestrator
   or a human as an escalation packet: the failure excerpt, the
   relevant context, and what was already tried.

Autonomy is a coarse cap over these tiers: index only, plus
deterministic fixers, plus indexed recipes by default, plus model
drafts only when explicitly enabled. Policy declares which diagnostic
classes are mechanical; the autonomy level caps what may run. Anything
outside the mechanical set, and anything classified as needing a
human, escalates immediately without burning attempts. Model drafts
merge into the working tree only when gates pass and the three-way
merge is clean.

## Gates

Independent, model-free checks certify every change. A failure records
its gate and discards the patch. At minimum the set covers:
precondition (target unchanged since analysis), scope (only permitted
ranges, no excluded spans, protected paths, or configuration files; no
new suppression, unsafe, placeholder, panic, or signature and deletion
changes unless policy-enabled), prose (comments, documentation, and
string contents identical in order, ignoring indentation),
well-formedness (no new parse errors), format stability (formatter is
idempotent on the result), diagnostics (fingerprint set strictly
shrinks with nothing added), build (check and lint clean across the
matrix), tests (no new failures against the baseline, with regressions
reverting the change), size (diff and item-count caps, with larger
changes escalating), and edit shape (the diff matches the declared
shape of its fix class, with zero tolerance for violations in
mechanical classes).

Formatting preserves prose and test regions by construction: format a
scratch copy, parse both versions, and splice back only token-gap and
non-test, non-prose changes. What cannot be spliced becomes an
explicit finding rather than silent drift or a non-converging loop.

## Loop safety

- **Isolation.** Model-originated patches are applied and verified in
  a scratch copy before anything touches the working tree.
- **Reversibility.** Every applied edit is journaled write-ahead with
  its provenance, its forward and reverse change, and its gate
  results. Review groups changes by file and kind; undo restores or
  refuses with a conflict report.
- **Optimistic concurrency.** A patch applies only if its target is
  unchanged since the loop read it; otherwise the item is requeued.
  A human editing a file is never overwritten.
- **Circuit breaker.** Bounded attempts per item, and a stop when the
  same failure repeats, the revert rate climbs, a pass makes no
  progress, or a budget runs out. On a trip model tiers stop while
  deterministic tiers continue, and the report names the reason.
- **Tests are not negotiable.** Automation never edits a test to make
  it pass, and never commits. Every change it applies is listed in a
  reviewable ledger.
- **Untrusted input.** Indexed source text is data, not instruction.
  Model inputs render source as fenced inert blocks, and
  instruction-shaped packets downgrade to held review.
- **One loop.** A process-level lock allows one maintenance loop per
  workspace at a time.

## Measured judgment

Where a bounded semantic judgment is the real task (routing an
ambiguous query, filtering excerpts, deciding whether a diff changes
what a comment describes, triaging a failure), resolution runs rule,
then cache, then model, then static fallback, and fails closed to
the deterministic path. Passing calibration measures that a decision
point is reliable — precision and recall on golden sets — and only
points that clear their bars auto-apply. With no authorization, the
loop degrades into measurement rather than silence: drafts verify in
scratch and hold for review, and every accept or reject becomes a
label. The deterministic result stays authoritative until
measurement says otherwise.

Each decision point carries a closed output vocabulary, a question
version, and a mode: shadow (scored, never used), advisory (may affect
ordering only), or authorized (may steer behaviour, requiring a
calibration entry for that point, model, question version, and dataset
hash). Changing the model, prompt, or question version returns the
point to shadow. Labels come from golden sets and from journal
outcomes; a calibration command computes entries from them.

Model use is reproducible: fixed decoding settings and seed, no tool
access, context packets assembled deterministically from the index and
identified by hash, outputs constrained to anchored search-and-replace
patches scoped to one item plus import lines, results cached by context
hash, model identity, and prompt version with identical patches never
retried. Inputs and outputs are stored in the journal, neighbouring
source is fenced as inert data, and suspicious context forces held
review even when the gates would otherwise allow automatic handling.

## Capabilities

### Read plane

- **Incremental codebase mirror.** Parse sources into structured
  metadata (signatures, spans, line numbers, content hashes) with a
  language-neutral envelope; preserve human commentary; persist
  everything in sidecars plus a queryable local index rebuilt from
  them.
- **Tiered retrieval.** Exact symbol and edge lookup first, then
  lexical search over names, signatures, documentation, and paths,
  then fused ranking of exact, lexical, and graph proximity, then
  optional embeddings over item chunks keyed by content and embedder
  identity. Every hit and edge is tagged with the tier that produced
  it; the exact tier is always available even on files that do not
  compile, labelled degraded rather than blocking navigation.
- **Staged question answering.** Route each query by intent to the
  cheapest search primitive that can satisfy it, answer with cited
  excerpts, and report gaps and confidence honestly. A grammar decides
  first; a small model may translate a question into a closed query
  plan only at an authorized decision point, and it translates but
  never answers. The plan, the path taken, and whether routing was by
  rule, cache, model, or fallback are explainable on request.
- **Analysis.** Definitions, callers, callees, imports, impls, and
  dependents, with the resolution tier and method attached.
- **Answer envelope.** Machine-readable answers carry intent, plan,
  resolution, freshness, cited results, gaps, and human-readable
  rendering, in a schema-versioned form.
- **Context packs.** For a task or question, a token-budgeted bundle
  of exactly the excerpts, symbols, and tests a worker needs —
  assembled deterministically so the same inputs yield the same
  pack — so each loop pass and each orchestrator step starts from a
  small, sufficient context. Packs combine the target source and
  diagnostic, signatures of dependencies, trait contracts, a capped
  sample of callers and siblings, read-only test excerpts, and a
  manifest of citations and hashes. Fix attempts, review, and
  orchestrators all consume the same builder.
- **Impact and scope.** Which dependents a change touches, which
  component owns a file, which tests exercise it, and the narrowest
  gate that covers it.
- **Rust fidelity.** Configuration-matrix verification per file,
  syntactic-versus-resolved edge tiers, and explicit findings for
  configuration-gated items and files outside any package — instead
  of silent clean. Diagnostics from the toolchain are attributed to
  files by span; items record which matrix cells verified them, and
  items no cell exercises are flagged accordingly. Detached files are
  reported and left incomplete until acknowledged in policy.
- **Conservative destructive answers.** Deletion safety returns at
  most unreferenced-with-caveats, never a bare yes, with the unseen
  channels (macros, dynamic dispatch, foreign interfaces, build
  scripts, examples, downstream crates, disabled configurations)
  listed explicitly.

### Maintain plane

- **Stale-only knowledge base.** Source, derived artifacts, and
  indexes are kept current by processing only stale inputs; clean
  files are never reprocessed.
- **Early cutoff and sig-gated invalidation.** Token-identical edits
  skip lint and compile; interface changes pull exactly their
  dependents back for reprocessing; body-only edits invalidate
  nothing downstream. Over-approximate syntactic edges may cause
  extra work but never missed work; macro and glob-import changes
  invalidate conservatively, and errors in files that were not dirty
  backdate those files while learning the missing edge.
- **Certified commentary.** Generated prose is written only after
  deterministic checks pass: referenced symbols exist, line
  references resolve, and size fits budget. Generation is optional
  and off by default.
- **Baseline hygiene.** Formatting, compile, lint, and test results
  are brought clean through the tiers above, at the narrowest scope.
- **Pre-commit integrity.** One command seals the mirror and the
  baseline for the changed set — matrix checks, full build, tests
  against the baseline, and a witness pass — and reports what, if
  anything, remains.

### Control plane

- **Policy as staleness input.** Scope, mechanical classes, approvals,
  and budgets versioned beside the code; changing them re-derives
  exactly the affected work. Policy is split into separately
  fingerprinted sections so a scope change does not re-lint and a
  prompt change does not re-embed.
- **Calibrated autonomy.** Decision points measured before they act,
  with cold-start behavior that holds work for review. Without
  authorizations the queue degrades into measurement: drafts verify
  in scratch and wait, and each verdict becomes a label.
- **Review and undo.** Grouped changes, one-command reversal with
  conflict refusal, and a one-minute run summary. Optional commits go
  to a dedicated automation branch with actor, attempt, and gate
  trailers, never to the user's branch.
- **Measure and ratchet.** Navigation accuracy, latency budgets, model
  calls avoided, offload share, and convergence tracked per release
  with regressions blocking release. A benchmark over golden queries,
  seeded-broken trees, and a frozen orchestrator task set compares
  against a stored baseline; clean files reprocessed must stay zero
  and behaviour-changing passes in mechanical classes must stay zero.

### Handoff contract

- **To the orchestrator.** Machine-readable answers (intent,
  confidence, results, citations, gaps, resolution method), plus
  human-readable markdown. Escalation packets are small, complete,
  and self-contained. The ledger, context packs, and one-minute
  summary are all consumable outputs.
- **From the orchestrator.** The orchestrator can post work to the
  queue, read the ledger, claim or resolve escalations, import plans,
  and request context packs. Transports start as a CLI with machine
  output modes and may share the same operations over a local socket.
  guidance does not implement features on its behalf.

## Human steering

- **Maintenance policy.** A declarative, versioned statement of
  scope, which fix classes are mechanical, which documents are
  human-owned, attempt limits, and what requires approval. Policy is
  an input to staleness, so changing it re-derives exactly the
  affected work.
- **Plans.** Non-mechanical work becomes a reviewable plan of ordered
  items with stable identities and acceptance gates. When a plan is
  revised, completed items are preserved, new and changed items are
  merged in, and a changelog tells the loop what is new. A verified
  plan item reopens if its acceptance gate later regresses. The loop
  tracks and verifies plans but never executes non-mechanical work
  itself.
- **Review.** Every loop run ends with a compact summary a person can
  read in under a minute: what was fixed, what was reverted, what was
  escalated, and why, with a link to the full report of files by fix
  class and tier, model calls and tokens against budget, breaker
  state, escalations, matrix cells and tests run, and baseline delta.

## Priority and resources

Interactive queries preempt background maintenance. Queries are
read-only against memory-mapped state and never wait on maintenance;
a dedicated build directory avoids toolchain lock fights. Maintenance
work is preemptible and resumable, and shares model capacity by
priority: interactive judgments first, shadow measurement next,
background drafting last. Build waves yield while queries are active.
A busy or unreachable model degrades to the deterministic path with
an explicit unavailable state, never to a silent wrong answer. Model
tiers keep queued work without consuming attempts while the model is
unavailable, and the report lists what waited.

## Performance budgets

Targets, hardware-dependent, that releases are held to: exact
navigation in single-digit milliseconds cold on large trees; text
search in tens of milliseconds; whole-repo freshness from timestamps
in tens of milliseconds with identical-but-touched files revalidated
in single-digit milliseconds; single-file syntactic indexing in tens
of milliseconds; dirty-set formatting in tens of milliseconds;
incremental toolchain waves bounded by the compiler floor; context
pack assembly in tens of milliseconds with optional semantic recall
around a hundred milliseconds; model attempts in seconds, free on
cache hit.

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
- **Calibration.** Precision and recall per decision point, and the
  share authorized to act.

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

## Known limits

- Compile truth is package-scoped. File-level verdicts record the
  package state they were verified against; dependency backdating
  plus a self-healing graph keep them honest, and the seal is the
  backstop.
- Syntactic edges can over- or under-report for trait dispatch and
  macros. They are labelled, not hidden. Published blind spots
  travel with answers: macro expansion, build-script output, dynamic
  dispatch at the syntactic tier, disabled configurations, and
  anything past check-level compilation.
- Local model quality bounds drafting. Gates keep bad patches out, so
  a weak model produces more escalations, never more bad edits.
- Calibration needs labels. Golden sets and journal outcomes supply
  them, but a new project runs mostly in deterministic and held-review
  modes until entries accumulate.
- Test-gate cost scales with suite size. It runs once per converged
  wave per affected package, not per file.
