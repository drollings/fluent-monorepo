# Orator handoff

Product stack after the parity program: what runs, what it
guarantees, and how to verify it. For the step-by-step record see
`ROADMAP_20261101_MOSHI_PARITY.md` (execution notes appendix) and
`ROADMAP_20261101_MOSHI_PARITY_CHECKLIST.md`; for the measured
thresholds see `doc/orator/CALIBRATION.md`; for the capability map
see `doc/orator/PARITY_GAPS.md`.

## Architecture

One daemon (`oratord`) owns the credentialed Unix socket, a
loopback-only WebSocket listener, and the long-lived inference
processes. One CLI (`orator`) runs locally or forwards control calls
over either transport (`--transport socket|ws`, default socket). The
product path is the cascade — VAD → Whisper STT → supervised local
`llama-server` endpoint → Kokoro/local TTS — plus the native
speech-to-speech adapter (`orator-s2s`) over the same supervised
endpoint, a terminal duplex client (`orator tui`), notes/diarization
helpers, and model/config/doctor tooling.

- **Sessions are real.** The daemon registry (`SessionId` → owned
  sessions + `TurnMachine` + `StreamAbort` + activity) serves
  `StartSession`/`StopSession`/`Interrupt`/`ListSessions`/`GetState`
  plus `Say`/`Ask` through the cascade; every turn lands in the
  `EventLog`. Live sessions hold leases on the voice server: the
  idle reap now varies and only fires at zero leases.
- **Duplex is frame-gated.** Per-frame VAD gating (hot opens,
  cold+hangover closes, window as max force-commit, `end_of_turn` on
  commit), abort-at-top, epoch drops with a counter, voicing flag,
  echo suppression by channel, and the `TurnMachine` legality
  policy. Barge-in uses session abort/voicing; `HopGate` stays the
  pipeline primitive in `TurnDriver`.
- **Streaming where it pays.** Chunked sentence TTS with
  play-as-ready (first audio after the first chunk), incremental STT
  with commit eviction (window holds the tail only), exact playout
  drain, pipelined chat, per-endpoint hint cache with
  fail-invalidate.
- **One dispatch core, two listeners.** Socket and WS share
  `serve_call` plus frame parse/encode — same methods, same
  envelopes. WS is loopback-only with a bearer token (refused
  without), server pings, inactivity reap, JoinSet drain, resume by
  session id.
- **Admission is calibrated.** The shared gate bounds concurrent
  turns (default 2, `daemon.admission_max`) with fast named refusals
  past it; consecutive transport failures eject the endpoint until a
  cheap probe re-admits it. Bounds shipped only inside their
  calibrated bands; idle capacity never refuses.
- **Inference is GPU-gated and CPU-default.** `gpu: bool` leaves on
  STT/TTS/LLM (default off); GPU without a device fails actionably,
  never silently CPU. Hermetic suites stay CPU unconditionally.
- **Codec decision: deferred.** The `Codec` seam ships with the PCM
  adapter (round-trip within half an LSB, microsecond steps);
  no neural backend without weights plus an in-workspace runtime.
- **One supervisor, one transport client, one playback path.**
  `orator_llm::Supervisor` owns every child; the process-wide pooled
  HTTP client serves all probes (the audio session keeps only its
  120 s streaming client); `play_blocking` and `StreamPlayer` share
  one opener; one WAV reader, one bundle type, one paths module, one
  envelope dispatch core, one admission policy, one calibration
  harness.

## Invariants (must survive any future change)

1. Wire format byte-identical (4-byte LE length + 1-byte type +
   body; method strings unchanged; WS speaks the same envelopes).
2. Socket file 0600 in 0700 dir owned by the daemon uid;
   fail-closed on any deviation. WS binds loopback only and
   requires the token.
3. No `unsafe` in project code (every `lib.rs`/`main.rs` carries
   `#![forbid(unsafe_code)]`, pinned by `it_source_guard`).
4. Config load-or-default; missing file yields defaults, malformed
   file is an actionable error on write. New leaves default to
   today's behavior (GPU off, WS off, bound calibrated).
5. No false success; no leaked children (shutdown, SIGINT/SIGTERM,
   idle reap, release paths, ejection, and admission refusal all
   stop or never start what they do not serve). Saturated admission
   refuses fast with the bound named; ejection fails named without
   booting.
6. No plan-step references in source (pinned by `it_source_guard`,
   now covering `P0`–`P16` ids too).
7. Calibration before heuristics: the 405 policy, the string match,
   the verify gate, the duplex constants, the admission bound and
   ejection streak, the codec decision, and the perf budgets change
   only with control-group numbers in `CALIBRATION.md`. Budgets are
   advisory, never red gates.
8. Bounded memory on the audio path: windows evict, scrollback caps,
   long sessions hold their ceiling (pinned by the bench golden).

## Gates

Per change: `make orator-smoke` (fast unit gate). Public types or
signatures changed: add `make orator-lint`. Milestone done: add
`make orator-test`. Never run repo-wide `make test`/`make soak`
to check a component; gate logs land in
`/tmp/opencode/monorepo-gates/<target>.log` — grep/tail the log on
failure, never re-run a gate just to see output.

Final component gates: smoke 532 passed / 0 failed / 2 ignored,
lint clean, test 551 passed / 0 failed / 2 ignored (43 binaries).

## Dependency edges (verified by `cargo tree` + `it_source_guard`)

- `orator-models`: common-core, orator-core, serde, serde_json,
  thiserror. No `orator-s2s`, no `whisper-rs`. (`reqwest`/`tokio`
  reach it only transitively through shared crates —
  fluent-concurrency and common-core — never through a runtime-crate
  edge; those shared internals are out of scope.)
- `orator-s2s`: base64, bon, common-core, fluent-concurrency,
  fluent-llm, fluent-wvr, internment, orator-audio, orator-core,
  orator-llm, reqwest, serde, serde_json, thiserror, tokio. No
  `orator-stt`.
- `orator-daemon` keeps `orator-s2s` for the audio argv/probe
  wrapper (documented at the dependency); the shared bundle type
  lives in `orator-core`. Its WS side is `tokio-tungstenite`
  0.29 only — the same line as workspace axum — with no second WS
  or HTTP stack.
- `orator-cli` composes the workspace TUI stack (`ratatui` 0.30,
  `crossterm` only through its re-export), the shared WS crate, and
  `orator-vad` for the TUI mic gate. No second splitter (shared
  `split_all`), no second envelope codec (shared builders), no
  second calibration harness.
- `orator-audio` carries no tensor/WS/TUI edges: the codec seam is
  std-only; neural adoption would arrive through the workspace
  runtime with weights, recorded first.

## Calibration register

`doc/orator/CALIBRATION.md`: readiness probe (strict 2xx, 405
unhealthy), `serves_model` listing (confidence only, `NoAudio`
drives fallback), `audio_input_unsupported` string match, `reap_if_idle`
outcome gate (now with real leases), `say_fallback` `NoAudio`
advance, `models verify` pinned gate, duplex thresholds (VAD spans,
endpointing gaps, barge-in stimuli — all constants kept),
streaming TTS first-audio (102 ms vs ~220 ms whole-turn), admission
bound 2 + ejection streak 2 (both scored, both shipped with
mechanism proofs), codec deferral (seam measured, transcript delta
explicitly unmeasured), and advisory perf budgets (turn,
first-audio, contention, RSS ceiling).
