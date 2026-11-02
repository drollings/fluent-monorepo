# spacy-rs — Vision

*The aspirational brief: what spacy-rs is for and what finished
looks like. The landed implementation lives in `ARCHITECTURE.md`.*

> **Section status legend.** Each section carries a `Status:` line:
> **Implemented** (exists today), **Partial** (working core plus
> described extensions), **Design-only** (aspirational).

## Mission

> Status: Implemented.

spacy-rs is a native, idiomatic Rust reimplementation of the core
of spaCy: the **deterministic spine** of the monorepo's language
understanding. It is a pipeline — tokenize → annotate → validate →
attach → frame → resolve → sentence boundaries — producing a `Doc`
whose tokens carry surface forms, lemmas, parts of speech,
dependency heads, and content-addressed interlingua ids that are
pure functions of the text and a shared taxonomy. Those ids let
deterministic routing, durable ledgers, and knowledge graphs agree
on what a request is *about* without consulting a model.

The animating belief: **an LLM should be consulted about ambiguity,
not about things already decided.** If three phrasings collapse to
the same predicate + object ids, the router already knows the
intent — the model handles the residue.

## Design principles

- **Deterministic before probabilistic.** Tokenizer, lemmatizer,
  sentencizer, validator, and interlingua resolver are pure and
  model-free; every id is a pure function of content.
- **Honest about its limits.** Heuristic parsing with
  first-class confidence: when unsure, the system says "needs
  disambiguation" rather than guessing silently. Consumers learn
  which rung produced a parse and how much it was doubted.
- **The model is the fallback, not the default.** A two-stage
  shape: an always-accept deterministic base parse, then optional
  model enrichment gated on low confidence. The model amends
  flagged residue through the same validation gate, re-stamped
  with model provenance — never silently overwriting the base, and
  never required: no model wired means the base stands, a fully
  supported mode.
- **Two token surfaces.** The deterministic tokenizer owns the
  exhaustive, offset-exact lexical baseline (alignment,
  reproducibility, microsecond hot path, fail-open with no model).
  The model owns a sparse, high-value index above it — entities,
  concepts, summary tokens referencing the baseline by span, never
  contradicting it. Boundary disagreements become permanent rules
  via golden-corpus rule genesis, not runtime models.
- **Content addressing is the point.** One id discipline across
  annotated text, knowledge stores, and routing tables, so systems
  reconcile without translation.

## Status

> Core pipeline, deterministic parser, interlingua bridge,
> taxonomy, async review, frame extraction, provenance tiers:
> Implemented. Entity resolution and sparse model index: Partial
> (candidate planes live; parse-time stamping is future work).

## What it accomplishes

- **Deterministic routing inputs.** Per-sentence predicate/subject/
  object signals and interlingua frames let classifiers dispatch
  on ids with zero tokens; low-confidence parses are flagged for a
  more capable model.
- **A durable, queryable understanding.** Lemmas, ids, confidence,
  and review status land in shared ledger and graph tables — the
  same taxonomy feeding both, reconciled at boot — joinable across
  days and systems.
- **A deterministic frame layer.** Per-predicate typed role
  structures with typed ambiguity lists: ambiguity-free frames mint
  permanent keys; ambiguous ones mint provisional keys resolved
  through the model and promoted on repetition, so the same pattern
  never re-pays a model call.
- **An honesty layer.** Guessing is reported (oracle ties, role
  coverage, source), so expensive models are spent only where the
  deterministic layer was genuinely unsure.

## When to use which layer

| Job | Right tool | Why |
|---|---|---|
| Token boundaries and surface attributes | Deterministic tokenizer — always | decided facts; hot path; reproducible |
| POS / dependencies / lemmas on common phrasing | Deterministic parser, confidence-scored | free, fail-open; flags doubt |
| Disambiguation, frame resolution, domain semantics | General-purpose local model, grammar-constrained | ambiguity is the residue worth a call |
| Entities, concepts, summary index | Model sparse tokens aligned to the baseline | the model holds the index, not the detail |
| Fixing tokenizer mistakes | Rule genesis into versioned data | stochastic findings become permanent rules |
| Repeating frame ambiguities | Sense promotion on repetition | resolve once, replay free |
| Amending annotations | Async audited review | re-validated through the same gate |

## Where it is going

A single content-addressed understanding shared by the whole
stack — text, knowledge, and workflows in one id language, the LLM
consulted only about residue. spacy-rs is the text half; the
taxonomy is the knowledge half; router, ledger, and graph are the
consumers.
