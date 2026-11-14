# fluent-llm — Vision

## Why this crate exists

Every product crate in this workspace needs the same two things from large
language models: **decisions** (small, cheap, calibrated judgement — route,
filter, rank, gate) and **generation** (open-ended text, embeddings, plans).
Without a shared home, each caller grows its own HTTP client, its own prompt
conventions, its own retry and redaction logic, and its own model catalog —
and the workspace ends up with N incompatible definitions of "what the model
said".

`fluent-llm` is the single shared home for LLM usage. It owns the vocabulary
(callers speak in `ChatMessage` / `LlmConfig`, not raw JSON), the transport
(bounded queues, retries, timeouts, streaming), the safety rails (local-only
defaults, redaction hooks, audit metadata, grant-gated remote calls), and the
calibration story (offline corpora, thresholds, canaries, shadow logs) so
that a decision made in one crate means the same thing in every other crate.

## What it is

- **One client for generative work.** Chat completions over any
  OpenAI-compatible endpoint, with sync and async entry points, streaming
  (SSE) support, think-block stripping, and response-shape repair. Callers
  bring messages and a model reference; the crate handles body construction,
  transport, and degraded-output handling.
- **One embedding story.** A uniform provider interface over local
  (llama.cpp/GGUF, ONNX) and remote (OpenAI-compatible, Qwen) backends, with
  a pinned model catalog, reference resolution (`explicit > existing > env >
  default`), compatibility checks, and response caching.
- **One decision stack.** Structured judgment calls (`choice` / `score` /
  yes-no) with rule → cache → model → fallback ladders, logprob decoding,
  per-question thresholds, batch fan-out, circuit breaking, and offline
  evaluation — so cheap deterministic rules answer first and the model is
  consulted only when it adds value.
- **One safety and observability contract.** PII detection and anonymization,
  localhost-by-default transport with explicit remote opt-in, audit-only
  metadata (never state bytes), and a PII-free telemetry vocabulary shared
  with the router.
- **One artifact and residency story.** Pinned, checksummed model artifact
  resolution with cache locking, plus backend-agnostic residency and session
  abstractions (weights lifecycle, KV-cache, eviction) that the serving crates
  compose rather than reimplement.

## Goals

1. **Correctness before cleverness.** Every LLM output is untrusted input:
   decode strictly, fall back to an explicit degraded prior, and never let a
   parse failure become a silent wrong answer.
2. **Cheapest sufficient inference.** Prefer rules, caches, and small local
   models; escalate to remote models only through explicit grants and budgets.
   Latency and token budgets are first-class, not afterthoughts.
3. **Hermetic by default.** Every network path has a scripted double; every
   decision path runs offline against golden corpora. No test in this crate's
   hermetic suite dials a real endpoint.
4. **Shared, not copied.** When two crates need the same thing (retry,
   hashing, eviction math, calibration sweeps, boundary repair), it lives in
   `common-core`, `fluent-wvr`, or `fluent-concurrency` — `fluent-llm`
   composes those primitives instead of forking them.
5. **Boundary discipline.** Domain crates (router, coral, guidance) depend on
   `fluent-llm`; it never depends on them. The crate compiles and tests with
   no router, no coral, no guidance in the graph.

## Non-goals

- It is not an inference server. It does not run `llama-server`, own ports,
  or schedule GPU placement — it produces the client, catalog, and residency
  abstractions that servers compose.
- It is not a prompt-engineering playground. Prompts here are versioned,
  audited, threshold-gated control-plane artifacts, not free-form strings.
- It is not a second copy of the shared libraries. Generic retry, hashing,
  eviction, calibration, and orchestration primitives belong beneath it, not
  in it.

## Who it serves

Router dispatch and escalation, coral's tier units and cache reactor,
guidance runtimes, and the orator family all build on this crate. Its success
metric is simple: a new model, backend, or decision question becomes additive
— one catalog row, one backend impl, one question spec — with no caller-side
`match` growth and no duplicated transport code.
