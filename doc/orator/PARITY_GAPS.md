# Orator parity gaps (Moshi C01–C18, X01–X12)

Evidence baseline for the parity program. Each row names the Moshi
capability, orator's current status, which axis the gap lives on, and
the program step that closes it. Axis follows the program rule:
producer confidence (self-report, cheap, can be wrong) versus
task-value (words voiced, turns completed, bytes hashed).

Baseline gates (2026-11-01): `make orator-smoke` 423 passed, 0 failed,
2 ignored (20 binaries); `make orator-lint` clean; `make orator-test`
439 passed, 0 failed, 2 ignored (43 binaries). Logs:
`/tmp/opencode/monorepo-gates/orator-smoke.build.log`,
`orator-smoke.unit.log`, `orator-lint.log`, `orator-test.log`.

## Capabilities

| ID | Capability | Orator today | Axis | Closes in |
|---|---|---|---|---|
| C01 | Streaming neural codec encode/decode (24 kHz ↔ 12.5 Hz RVQ) | None — PCM WAV plumbing only (`read_wav_mono`, `WavPcm`) | Task-value (audio fidelity on the wire) | Codec adopt-or-defer decision + seam |
| C02 | Streaming conv/SeaNet backbone | Delegated to `llama-server`; not owned | Task-value (owned inference) | Adopt-or-defer; never reimplement |
| C03 | Residual VQ codebooks | Delegated; not owned | Task-value | Same as C02 |
| C04 | Streaming Transformer + KV cache + RoPE | Delegated; server owns KV | Task-value | Same as C02 |
| C05 | Full-duplex dialogue LM (listen + talk, ~200 ms) | Turn-windowed interleaved turns (`AudioSession::chat_turn`) | Task-value (turn completion) | Frame engine, dialogue policy, duplex calibration |
| C06 | Speaker/text conditioning | Voice prompt strings only | Producer confidence | Echo reference; no new conditioner stack |
| C07 | Logit sampling top-k/p + CFG | Inside `llama-server` | Delegated | None (stays delegated) |
| C08 | Checkpoint loading + hub + conversion | `resolve_*` + pins + `verify_report` (done) | Task-value (hash match) | Reuses existing; codec decision reuses |
| C09 | Full-duplex inference servers | Daemon stubs + supervised boots (`Supervisor`, `VoiceOwner`) | Task-value (served turns) | Session factory, WS listener, multiplex |
| C10 | Binary WS protocol codec | Unix-socket JSON envelopes v0–7 (`SocketFrame`, `METHOD_NAMES`) | Task-value (envelope bytes) | WS listener, same envelopes |
| C11 | Audio transport/resample/jitter/playout buffer | cpal + rubato + local jitter + `BoundedPriorityQueue` (done) | Task-value (audible audio) | Heartbeat for networked use |
| C12 | Conversation UI | None (CLI only) | Task-value | TUI client |
| C13 | Queue/matchmaking + validation | Out of scope (no accounts, no cloud) | — | Non-goal |
| C14 | Streaming TTS/STT/ASR | Whole-turn synth; windowed re-decode (`StreamingStt`, `KokoroBackend`) | Task-value (first audio, committed words) | Streaming TTS, incremental STT |
| C15 | Observability: tracing/metrics/stats | tracing + `LatencyHistogram` + bench render | Advisory | Perf budgets |
| C16 | Rust↔Python bridge + batching | Non-goal (Rust-native only) | — | Non-goal |
| C17 | Local/TUI/offline clients | CLI (rich), no TUI | Task-value | TUI |
| C18 | Quantization/LoRA/export | Delegated to llama.cpp gguf | — | Non-goal (documented) |

## Concurrency patterns

| ID | Pattern | Orator today | Axis | Closes in |
|---|---|---|---|---|
| X01 | Streaming state + reset | Rings, abort, JoinSet (partial) | Task-value | Frame engine |
| X02 | Bounded/unbounded duplex channels | Partial (rings, abort) | Task-value | Frame engine, session tasks |
| X03 | `spawn_blocking` for inference | Yes (blocking steps on pool) | — | Reuse |
| X04 | `select!` recv/process/send + timeout | Abort race in `stream_turn` | Task-value | Frame engine extends |
| X05 | Shared model behind `Arc` | `Supervisor` + Kokoro `Limiter` (done) | Task-value | TTS audit reuses |
| X06 | Serializing mutex (TTS) | Kokoro `Limiter` (done) | Task-value | Audit keeps or scopes |
| X07 | Retry queue | `retry_async` in llm/audio paths | — | Reuse, no new queue |
| X08 | Ordered marker heap (batched ASR) | Not needed (no batching) | — | Non-goal |
| X09 | Single global lock server | One shared voice server | Task-value | Multiplex (no model batching) |
| X10 | Browser worklet buffering/backpressure | Local merge only | Task-value | WS heartbeat for networked use |
| X11 | Heartbeat/inactivity watchdog | None | Producer confidence | WS listener |
| X12 | Batch scheduler | None (single shared server) | Task-value | Multiplex (no model batching) |

## Hollow center (drove the order — closed)

The daemon `Say`/`Ask`/session methods were honest `NotImplemented`s
when this baseline was taken. The session factory landed first
(P1: registry, dispatch, socket execution) because every later step
(latency, duplex, transport, admission) needed sessions to measure
against; the rows above record the converged state.
