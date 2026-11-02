# Orator handoff (red-team hardening, Q0–Q15)

Post-hardening product record: what the red-team roadmap changed,
what it guarantees, and how to verify it. Prior program record
(`HANDOFF.md`) is untouched; step-by-step execution notes live in
`ROADMAP_20261101_REDTEAM.md` (§Q0–Q15 notes); measured thresholds in
`doc/orator/CALIBRATION.md` (appendix R + "Q13 new-trigger harnesses");
desktop wiring plan in `doc/orator/DESKTOP_WIRING.md`.

## Architecture (final)

One daemon (`oratord`) owns the credentialed Unix socket, a
loopback-only WebSocket listener, and the supervised inference fleet
(one `llama-server` child per weights file, owned by
`orator_llm::Supervisor`). One CLI (`orator`) runs locally or
forwards control calls over either transport (`--transport
socket|ws`, default socket). The product path is the cascade — VAD →
Whisper STT → supervised local endpoint → Kokoro/local TTS — plus
the native speech-to-speech adapter (`orator-s2s`) over the same
supervised endpoint, a terminal duplex client (`orator tui`),
notes/diarization helpers, and model/config/doctor tooling.

- **Fallback is honest.** `should_advance_chain` (single home:
  `orator-s2s::session`; CLI `say_chain` composes it) advances on
  `NoAudio`/`Backend`/`Overflow`, never on `Aborted`/usage. A
  voiceless `say` turn advances down the chain; a failed
  transcription reports honestly (asymmetry pinned in
  `orator-cli::transcribe::audio_server_cue`). Endpoint errors carry
  additive `kind` (`NoAudio`/`Overflow`/`Aborted`/`Backend`/
  `Saturated`/`Ejected`/`BootContention`).
- **Admission + ejection compose.** The shared gate bounds
  concurrent turns (`daemon.admission_max = 2` default) with fast
  `Saturated` refusals; consecutive transport failures (`Backend` +
  `Overflow` per the calibrated rule) eject the endpoint until a
  cheap probe re-admits it. Responsive turns — including voiceless
  ones — break the streak. No per-session servers, no global lock.
- **One dispatch core, two listeners.** Socket and WS share
  `serve_call` — same methods, same envelopes. WS is loopback-only
  with a bearer token (refused without), 5 s ping / 10 s silence
  bound / 360 s lifetime cap (all `daemon.ws_*` config with stated
  defaults; silence applies between calls, never mid-call).
- **Boot is fail-fast.** `ask` strict-parses the port (garbage is
  `Usage`, never auto-reserve) and refuses missing weights before
  any child boots (no 60 s health burn). `boot_supervised` exit
  class rides `AskFail::Usage/Error` to the CLI. GPU without a
  device refuses before boot, never silently CPU.
- **Dictation keeps words.** Refine chain (`orator-desktop::refine`,
  LLM → raw → history over `first_accept_in_order_sync`,
  transcript-as-data prompt shared with the tool loop) records clean
  text per turn; continuation spacing joins same-window turns via
  `LadderMemory.last_inject`; clipboard discipline (10 ms polls,
  180/200 ms deadlines, restore-or-clear, own-write ordering);
  chunker numbers (`MIN=16, FIRST_MAX=100, CAP=220`, comma
  fast-path, buffer-end hold, abbreviation guard); three-hop
  generation cancel (post-transcribe / pre-refine / pre-commit).
- **Memory is bounded everywhere.** Playback queue (480 k, drop-oldest
  + counter, fallible `finish`), stream channel (2, cancel on play
  error), prompt (`TokenBudget` 8192 + loud truncation marker),
  histories (100 / 32 k-char roll), TUI queue (120 s + drop
  counter), dictate ceiling (120 s / `MAX_DICTATE_SAMPLES`), event
  replay (5 k/page), scratch files (drop-guard). History/usage are
  thin views (`replay_kind`, `count_kind`) over the event log — no
  second store.
- **TUI owns its engines.** `SttWorker` thread owns the whisper
  backend (context is not `Send` — reuse by affinity, keyed cache);
  Kokoro session persists across replies; turn abort flows into the
  transcribe thread (barged results never deliver); 16 kHz remainder
  keeps its own domain across resamples.
- **Jitter/broadcast/watchdog staged as config.** 80 ms jitter cap
  (drop-oldest-to-target), `broadcast::channel(10)` with Lagged-skip,
  single producer. Inference stays delegated (no transformer/RVQ/
  codebook; PCM codec seam is the adopt-or-defer point).
- **Store is one event log.** Append-only `seq` + paged replay +
  negative-seq live-only ephemerals; single writer task; retention
  by policy.

## Gates (final, 2026-11-02)

- `make orator-lint`: clean.
- `make orator-test`: 657 passed / 0 failed / 2 ignored (43 binaries).
- `make orator-smoke`: 637 passed / 0 failed / 2 ignored (20 binaries).
- `rg -i "roadmap|milestone|phase"` over `src/orator-*`: empty;
  `it_source_guard` (forbidden-word + `Q0`–`Q15` section-ID scan,
  crate-root `forbid(unsafe_code)` scan, manifest edge assertions)
  green.
- Logs: `/tmp/opencode/monorepo-gates/orator-{lint,test,smoke*}.log`.
- Workflow close: one repo-wide `make smoke` — 8623 passed / 0
  failed / 66 ignored (60 binaries).
- Flakes seen and retried (pre-existing, unrelated): `free_port`
  AddrInUse under parallel; `orator-s2s` worker ETXTBSY under
  parallel. Both green alone/retry.
- Pacing rule kept throughout: smoke-only per milestone, full
  lint+test on every fifth (Q4, Q9, Q14 closes), Q15 closes the
  workflow. Never ran repo-wide `make test`/`make soak`.

## Calibration register deltas (this program)

Prior rows in `CALIBRATION.md` stay normative. New rows under "Q13
new-trigger harnesses": fallback-advance grid (false/missed-advance
zero; usage errors never reach the gate by type); ejection
streak (precision 1.0, FPR 0.0); admission bound (shared gate,
idle admits all); duplex bands composed; verify gate (false-ok
zero); dictate/transcribe/approval/playback/stream/history/scratch
bounds with must-not-fire controls; chat order + abort; ask
fail-fast; TUI rates; refine/continuation/chunker/cancel; jitter
(80 ms) / broadcast (10, Lagged-skip) / watchdog (5 s / 10 s /
360 s) / multiplex. Performance budgets stay advisory.

## Invariants (must survive any future change)

1. Wire bytes unchanged except additive `kind`, `Saturated` /
   boot-contention replies, and WS tuning leaves. Method strings,
   `MessageType` discriminants, frame shape, JSON envelopes stable;
   `COMMANDS`/`METHOD_NAMES` single-source with parity tests.
2. Socket 0600 in 0700 dir, daemon-uid owned; peer gate reads its
   own credential, fail-closed `u32::MAX`. WS loopback-only +
   bearer token, refused without. Hardening failures fail closed.
3. No `unsafe` in project code (every `lib.rs`/`main.rs` forbids
   it; modules inherit crate-level forbid).
4. Config load-or-default; new leaves default to today's behavior
   (GPU off, WS off, bounds calibrated). Old files load calibrated
   defaults, never zero/open.
5. No false success: every command does the work or exits non-zero
   with a named reason (`daemon_down_is_never_success` covers 18
   vectors across both transports). No leaked children: failed
   boots reap (`failed_boot_reaps_the_spawned_child` asserts
   `/proc/<pid>` gone), shutdown/idle/ejection paths stop what they
   do not serve; `kill_on_drop` stays a backstop.
6. No plan references in source (guard scans `src/`, literals live
   in `tests/` only).
7. Calibration before heuristics: no trigger ships before its
   family is green; finals pinned in goldens.
8. Bounded audio-path memory with counters/markers, never silent
   loss of durable data (`over_bound_inputs_stay_bounded_everywhere`
   composes prefix roll + paged replay + dictate clamp in one pass).
9. Shared primitives only: one supervisor, one transport client,
   one playback path, one ladder walker, one truncation helper, one
   bench runner. `orator-models` stays a leaf; no `orator-stt` in
   `orator-s2s` (verified by `cargo tree`).
10. No git write operations as part of any plan.
11. Local `llama.cpp` first: cascade reaches inference through the
   supervised endpoint; no second inference transport, no neural
   codec without weights + runtime + measured deltas.
12. Murmur/Moshi lineage honored: clipboard/restore discipline,
   continuation spacing, chunker numbers, three-hop cancel, jitter/
   broadcast/watchdog numbers, bench schema + anti-tautology gate.
