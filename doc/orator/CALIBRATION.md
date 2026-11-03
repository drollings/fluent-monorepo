# Orator calibration register

Measurements behind the readiness/fallback heuristics. Every entry
names the trigger, the axis it measures, the control group that must
not fire, and the decision the numbers support. Fixture suites live in
`orator-s2s/tests/calibration.rs` (probe-vs-audio control groups) and
`orator-s2s/tests/session.rs` (error-string pinning); the shared
scoring harness is `common_core::calibration`.

## Readiness probe (2xx vs 405)

- **Trigger:** `HealthProbe::OptionsCompletions` classifying an audio
  endpoint ready on 2xx only.
- **Axis:** producer confidence (cheap, can be wrong).
- **Control group that must not fire:** dead servers (refused
  connections, 500 on every endpoint) must never read ready.
- **Fixture measurement** (`probe_predicts_audio_on_the_fixture_set`):
  4 cases — refused, 500, text-only (405 + text-only completions),
  audio-ready (200 + audio completions).

  | outcome | count |
  | --- | --- |
  | true positives (probe ready, audio flows) | 1 |
  | false positives (probe ready, no audio) | 0 |
  | true negatives (probe silent, no audio) | 3 |
  | false negatives (probe silent, audio would flow) | 0 |

  Precision 1.0, recall 1.0, FPR 0.0 — passes the shared
  `passes_gate` (precision ≥ 0.90, FPR ≤ 0.05).

- **Decision:** keep the strict policy — 405 is not success. The
  audio-ready reference answers 200; the text-only gateway answers
  405. Accepting 405 as "alive but not audio-capable" would turn
  every text-only endpoint into a false-ready, trusting the request
  path to catch it. The current policy has zero false-not-ready on
  the fixtures, so strictness costs no working server.

## `serves_model` `/models` hit

- **Trigger:** treating a `/models` (or `/v1/models`) listing as
  "the endpoint serves this model".
- **Axis:** producer confidence.
- **Control group that must not fire:** a text-only server that
  lists models but voices nothing must not be treated as
  audio-ready.
- **Measurement:** the text-only fixture lists its model and still
  yields `NoAudio` on a speech turn — the listing never gates audio
  success. Assurance advances on `NoAudio` (task-value), never on
  the listing alone.

## `audio_input_unsupported` string match

- **Trigger:** the exact substring `audio input is not supported`
  inside a `Backend` error body.
- **Axis:** producer confidence (an error-string convention, not a
  structured signal — the reference server provides none).
- **Control group that must not fire:** other 400-class errors
  (unknown model, unexpected field), 500s, transport failures,
  `NoAudio`, `Aborted`, and `Overflow` must all miss the match.
- **Fixture measurement** (`audio_input_unsupported_*`): 1 pinned
  transcript hits, 7 controls miss — precision 1.0, FPR 0.0 on the
  fixture set. The pin is deliberate: if the server ever ships a
  structured signal, the match goes away in favor of it.

## `reap_if_idle` outcome gate

- **Trigger:** reaping the daemon-owned voice server after the idle
  budget with no active leases and a last outcome of `None`/`Ok`.
- **Axis:** task-value (`Ok` = a real turn was served; `Backend` = a
  real request failed) plus the lease count (structurally zero —
  no lease protocol exists yet, so the leg always passes and the
  field reserves the future "in use" signal).
- **Control group that must not fire:** a server whose last request
  failed must not be reaped by the timer; the next use clears it
  outright instead.
- **Decision:** reap on idle + `None`/`Ok` only. A `Backend`
  outcome holds the child past the budget — the next `ensure`
  probes it and clears it on failure, so nothing leaks; the timer
  never kills a server mid-recovery.

## `say_fallback` `NoAudio` advance

- **Trigger:** advancing the voice chain on `NoAudio`.
- **Axis:** task-value (the server produced no audio for a speech
  request).
- **Control group that must not fire:** a server that produced
  audio must never advance the fallback.
- **Measurement:** `NoAudio` fires only when the accumulated audio
  buffer is empty after a complete turn; any voiced turn returns
  its samples. False-fallback rate on the fixtures: zero.

## Duplex thresholds (VAD, endpointing, barge-in)

- **Trigger:** `EnergyVad` defaults (floor 0.02, hysteresis 0.01,
  attack 3, hangover 15, ceiling 0.45), `TurnConfig` holds
  (300/700 ms), `turn_window_ms` cap (2000 ms), barge-in sustain
  (150 ms) gating per-frame duplex turns.
- **Axis:** producer confidence (frame verdicts, sustain timing) scored
  as a predictor of task-value ground truth (spans that must turn,
  gaps that must hold, stimuli that must open a turn).
- **Control groups that must not fire:** silence-only spans (zero
  turns), loud high-frequency tones and loud noise (zero speech
  verdicts — ceiling rejects what the floor passes), echo-only frames
  during voicing (zero candidates), short hot blips and noise bursts
  (zero turns).
- **Fixture measurement** (`orator-s2s/tests/duplex_calibration.rs`,
  scripted frames, no wall-clock audio), all through the shared
  `calibrate_threshold` gate (precision ≥ 0.90, FPR ≤ 0.05):

  | family | cases | TP | FP | TN | FN | precision | FPR | gate |
  | --- | --- | --- | --- | --- | --- | --- | --- | --- |
  | VAD spans (harmonic must fire; silence/tones/noise must not) | 12 | 3 | 0 | 9 | 0 | 1.0 | 0.0 | pass |
  | endpointing gaps (100/200/280 ms hold; 300/800 ms cut) | 5 | 2 | 0 | 3 | 0 | 1.0 | 0.0 | pass |
  | barge-in stimuli (speech opens; echo/burst/blip do not) | 5 | 2 | 0 | 3 | 0 | 1.0 | 0.0 | pass |

  False-cut (mid-word endpoint) 0/3, false-hold (missed end cut)
  0/2, false-barge 0/3, missed-barge 0/2. Onset sits exactly one
  attack run (3 frames / 60 ms) past span start; close sits exactly
  one hangover run (15 frames / 300 ms) past the last hot frame —
  both pinned. The noise/tone controls are load-bearing: white noise
  at RMS 0.30 and 4–7 kHz tones at peak 0.4 both clear the floor and
  are rejected by the ceiling alone; the quiet 440 Hz tone is
  rejected by the floor alone.
- **Decision:** keep every constant. All three families pass with
  zero false fires and zero misses, so no number moves. The finals
  are pinned in `calibrated_constants_stay_pinned` (VAD
  0.02/0.01/3/15/0.45, holds 300/700 ms, barge-in 150 ms, window
  2000 ms); any future change re-runs these groups first.

## Streaming TTS first-audio (chunked synth + play-as-ready)

- **Trigger:** splitting the turn on sentence boundaries and playing
  chunk N while chunk N+1 synthesizes, versus synthesizing the whole
  turn before the first audible sample.
- **Axis:** task-value (milliseconds to first audible chunk) on a
  scripted proportional-cost fixture (2 ms synth per char; no model,
  no device — the seam cost stands in for inference).
- **Control group that must not fire:** chunk order and word coverage —
  the partition never loses, reorders, or duplicates words
  (`chunk_partition_covers_the_text_in_order`), and single-sentence
  input collapses to one identical synth call (transparency golden).
- **Fixture measurement** (`orator-cli/tests/stream.rs`,
  3-chunk turn, ~110 chars):

  | shape | first audible |
  | --- | --- |
  | whole-turn synth then play | ~220 ms (full text cost) |
  | chunked synth + overlap | 102 ms (first-chunk cost) |

- **Decision:** ship the chunked path. Per-chunk prosody is
  per-synthesis-unit by design, so multi-chunk output is *not*
  claimed sample-equal to whole-turn output — the guarantees are
  word coverage, chunk order, per-device determinism, and earlier
  first audio. `ChatTurn` shape and 24 kHz rate are unchanged.

## Performance budgets (scripted fixtures, advisory only)

- **Trigger:** regression visibility — first-audio latency, turn
  latency, concurrent-session throughput, memory bounds on fixtures.
- **Axis:** task-value latency on hermetic stubs; never a red gate.
  A regression opens an investigation, never a failure.
- **Fixture measurement** (`orator-bench/tests/scenarios.rs`,
  stub audio endpoint + synthetic 16 kHz utterances + scripted synth
  seam; histograms rendered through the shared `render_stats`):

  | scenario | fixture condition | observed |
  | --- | --- | --- |
  | single-turn voice (VAD gate + stub turn) | 100 ms stub delay | max 250 ms, band 10 s |
  | first audio (scripted 50 ms first-chunk cost) | 3-sentence fixture | max 50 ms, band 5 s |
  | sustained duplex (10 sequential stub turns) | 0 ms stub delay | 24 ms total (~25k turns/min), band 60 s |
  | contention (3 contenders, bound 2, 300 ms stub) | 2 complete + 1 refusal | completions max 500 ms, refusal max 1 ms, bands 5 s / 2 s |
  | long session (200 scripted turns) | VAD + PCM codec + gate cycles | loop max 25 ms, RSS growth ~14 MB, bands 60 s / 128 MB |

  Single-sample percentiles are bucket noise at n=1 — the max
  column is the anchor. The RSS figure includes first-touch
  allocator arenas, not a leak (the loop itself retains nothing;
  the scrollback cap and window eviction bound live data).
- **Decision:** budgets recorded, never gated. The scenarios assert
  completion, shape, and generous bands only.

### Operator wall-clock soak (manual, never CI)

- **Trigger:** confidence that an 8-hour production-shaped run holds
  counters flat (drops, RSS, ejection streaks) outside logical scale.
- **Procedure:** `make orator-soak` (heavy tier: 400 faulted turns
  plus live-AI skips) on the target machine; then run the daemon
  under load (`orator tui` or scripted `Ask`/`Say` loop) with
  `orator stats --file stat.jsonl` recording; gate the file with
  `orator stats` render and the `gate()` ceilings (GPU profile).
- **Watch:** `drops == 0` on clean replays, RSS flat across hours
  (first-touch arenas excluded), no ejection on responsive servers,
  WS ping steady at 5 s with no silence kills on an active call.
- **Decision:** wall-clock numbers are operator evidence, not gates —
  shared runners flake on timing, so CI asserts logical scale only.

## Neural codec: adopt-or-defer decision (DEFERRED)

- **Trigger:** shipping low-bitrate streaming neural audio (24 kHz
  in/out, step latency under one frame quantum) behind
  `orator_audio::Codec { encode_step, decode_step, reset }`.
- **Axis:** task-value (step latency vs frame quantum; round-trip
  STT quality delta vs PCM) — measured on fixtures, never opinion.
- **Workspace verification first:** the in-tree tensor runtime is
  `ort` (via `fluent-onnx`; Kokoro already speaks it). `candle` is
  absent from the manifests and lock. No Mimi-compatible weights,
  graph, or codebook exists anywhere in the tree or the model
  layout. Adopting would add the first `candle` (or foreign ONNX
  Mimi) dependency *and* a non-hermetic weight fetch — a new stack
  plus a new download, both barred without a decision.
- **Fixture measurement** (`orator-audio/tests/codec.rs`, scripted
  24 kHz harmonic content, no models):

  | shape | measured |
  | --- | --- |
  | seam step cost (encode+decode, 480-sample step) | ~1 µs release (~50 µs debug), ceiling 5 ms — 20 ms quantum has ~20,000× headroom |
  | PCM round-trip max sample error | ≤ 1/32768 (half LSB per conversion) |
  | PCM round-trip energy drift | < 1e-4 |
  | streaming-vs-whole composition | byte-identical |
  | reset across turns | zero leakage (stateless by construction) |

- **Control group that must not fire:** every existing golden keeps
  passing with the seam present (539/0/2) — the adapter changes no
  production path (`read_wav_mono`/`WavPcm` stay the disk format,
  no caller switched transports).
- **Decision: DEFER.** The seam ships (`Codec` trait + `PcmCodec`
  adapter, one implementation, no dead code) with its fidelity
  contract pinned; the neural backend does not. What is missing is
  stated, not silently dropped: Mimi-compatible weights plus an
  in-workspace runtime for them. The transcript-level quality
  delta (codec transcript vs PCM transcript) cannot be measured
  without weights — scripted STT decoders ignore audio by contract,
  so a 0.0 delta there would be a false measurement, and none is
  claimed. Gap C01/C11-transport stays explicitly open; PCM is the
  supported path.
- **Revisit rule:** adoption needs (1) weights resolvable through
  the pinned model layout, (2) no new tensor stack (ort-family
  graph or a workspace-standardized crate), (3) step latency under
  one quantum and transcript delta within tolerance on fixed
  corpora — all recorded here first.

## Admission: concurrency bound (serial-lane fixtures)

- **Trigger:** admitting a turn while `inflight` others serve the same
  shared endpoints, against a single-lane scripted server (mutex +
  fixed scripted cost, the shape of the serial synth path).
- **Axis:** producer confidence (queue-depth-at-admit) scored as a
  predictor of task-value breach (turn latency over budget).
- **Control group that must not fire:** idle capacity (bound 8, 4
  concurrent turns) admits everything — zero refusals.
- **Fixture measurement** (`orator-daemon/tests/admission.rs`): the
  single-turn quantum is measured on the lane (~500 ms); 6 threads
  admit through the real gate under a start barrier (depths 0–5
  observed exactly); breach is modeled as `(depth+1) × quantum` past
  a 1200 ms budget:

  | bound | TP | FP | TN | FN | precision | FPR | gate |
  | --- | --- | --- | --- | --- | --- | --- | --- |
  | 1 | 4 | 1 | 1 | 0 | 0.80 | 0.50 | fail |
  | 2 | 4 | 0 | 2 | 0 | 1.0 | 0.0 | pass |
  | 3 | 3 | 0 | 2 | 1 | 1.0 | 0.0 | pass |
  | 4 | 2 | 0 | 2 | 2 | 1.0 | 0.0 | pass |

  False-refusal (healthy depth refused) 0/2 at the shipped bound;
  missed-breach 0/4. Higher bounds pass the gate but miss real
  breaches, so the decision rule is the smallest passing bound with
  zero missed breaches: **2**.
- **Mechanism proof (real threads, banded timing):** bound 2 with 4
  contenders admits exactly 2 (both serve, lane hits == 2) and
  refuses 2 fast (< 1 s, named "saturated" with the bound).
- **Decision:** ship bound 2 as `daemon.admission_max` (default 2,
  `config get/set` wired, old files load calibrated). Fixture
  condition: single-lane server; servers that parallelize raise the
  leaf. The gate, not the number, is the durable part — the number
  re-scores with the harness.

## Admission: ejection streak (stub-endpoint fixtures)

- **Trigger:** consecutive transport (`Backend`) voice-turn failures
  ejecting the endpoint; any responsive turn (including voiceless
  `NoAudio`) breaks the streak and re-enters.
- **Axis:** task-value (served vs failed turns) gating rotation.
- **Control group that must not fire:** a single transient failure
  followed by success never ejects; a responsive-but-voiceless
  server (three text-only turns) never ejects — `NoAudio` is alive,
  not failed.
- **Fixture measurement** (same harness, httpmock stubs): threshold
  1 false-ejects the transient controls (precision 0.50, gate
  fails); threshold 2 is exact (3/0/3/0, precision 1.0, FPR 0.0,
  gate passes).
- **Mechanism proof (real owner, stub servers):** two consecutive
  500s eject — the next `ensure` fails `Ejected` fast without
  booting; a healthy stub turn clears the ejection and the next
  `ensure` proceeds to boot (`Boot`, proving re-entry); a healthy
  OPTIONS probe with failing turns re-enters toward boot the same
  way. While ejected `ensure` probes only — no boot runs.
- **Decision:** ship `EJECTION_THRESHOLD = 2`, enforced in
  `say_via` outcomes and consulted in `ensure`. New named codes
  (`Saturated`, `Ejected`) reach the CLI with remedies.

## `models verify` pinned gate

- **Trigger:** the `pinned` catalog flag gating the `verified: ok`
  report.
- **Axis:** producer confidence (the flag) vs task-value (the
  SHA-256 over bytes on disk).
- **Control group that must not fire:** unpinned or mismatched
  files must never report `ok`.
- **Fixture measurement** (`orator-models/tests/calibration.rs`,
  hermetic temp HOME):

  | group | entries | `verify_entry` |
  | --- | --- | --- |
  | pinned + matching hash | 1 | `Ok(true)` |
  | pinned + mismatched bytes | 1 | `Ok(false)` |
  | unpinned + present | 1 | `Err(Unpinned)` |
  | unpinned + absent | 1 | `Err(Unpinned)` |

  False-ok (an `Ok(true)` for any group but the first): 0/3.
  False-fail (anything but `Ok(true)` for the first): 0/1. The
  report layer (`orator_cli::models::verify_report`) additionally
  never prints `ok` for unpinned/missing rows — pinned by its own
  fixture suite.
- **Decision:** the gate holds as built. Any future "verified"
  heuristic (size-only, ETag, mtime) ships only with a new control
  group here that must not fire.

## R8 calibration harnesses (2026-11-02)

Each row below ships with its control group (must not fire) and its
must-fire case, all through the shared `common_core::calibration`
gate where scored (precision ≥ 0.90, FPR ≤ 0.05). Uncalibrated paths
stay behind config flags defaulting to the safe value.

### VAD confidence isolation (`vad_alone_never_sets_a_hold`)

- **Trigger:** VAD flaps (`VadStart`/`VadEnd`) with no silence ruling.
- **Axis:** producer confidence (frame verdicts) must never set a hold.
- **Control:** 10 VAD flap cycles plus 250 ms sub-hold silence stay in
  `UserSpeaking` — the floor never moves on VAD alone.
- **Decision:** keep the rule; holds come only from `SilenceElapsed`
  at or past the band. Gate: hermetic unit test, always run.

### Injection rungs exact fallback (`each_failing_rung_falls_back…`,
  `failed_paste_skips_to_output_only`)

- **Trigger:** forcing each ladder rung to fail in isolation.
- **Axis:** task-value (the text reaches the user exactly once).
- **Control/must-fire:** every failing rung (`RemoteDesktop`,
  `ClipboardPaste`, `VirtualKeyboard`, `Uinput`) resolves to the exact
  `OutputOnly` text; a lost chord skips identically to headless.
- **Decision:** skip-on-failure holds; `Fatal` still aborts (existing
  pin). Gate: hermetic unit tests, always run.

### Speculative prefix equality (`speculation_resume_is_exact…`)

- **Trigger:** abort at every token position of the stable text.
- **Axis:** task-value (re-request byte-equals the armed prefix, so
  the server reuses its cache).
- **Control/must-fire:** all 17 prefixes of `"the stable text"`
  round-trip exactly and cut the epoch (`live == Epoch(6)`,
  `check(5, PreLlm) == false`); empty arm resumes to `Some("")`.
- **Decision:** no speculation on mismatch — resume always re-sends
  exactly. Gate: hermetic unit test, always run.

### TTS single-sentence transparency
  (`single_sentence_is_one_identical_synth_call`)

- **Trigger:** one sentence through the chunked synth seam.
- **Axis:** task-value (one chunk, one call, identical bytes).
- **Control/must-fire:** `split_all` yields 1 chunk; the streamed
  samples equal the whole-turn synth byte-for-byte.
- **Decision:** chunking never rewrites a one-chunk turn. Gate:
  hermetic unit test, always run.

### Backchannel recall per language (`recall_splits_evenly…`)

- **Trigger:** must-cut interruptions scored per lexicon language.
- **Axis:** task-value (no language's interruptions protected).
- **Control/must-fire:** `en` and `de` must-cuts each recall at 1.0 on
  the barge-in corpus (aggregate 0 false cuts already pinned).
- **Decision:** keep the lexicon; per-language split guards future
  lexicon edits. Gate: hermetic unit test, always run.

### WER per tier (`live_wer_per_tier`, `wer` goldens)

- **Trigger:** decoding the checked-in corpus per pinned tier.
- **Axis:** task-value (words transcribed as intended).
- **Control:** empty reference pairs (`0.0`/`1.0` contract); the live
  runner skips cleanly without models or corpus — never fails
  hermetic gates. Scripted-decoder WER is explicitly NOT measured
  (scripted decoders ignore audio by contract; a 0.0 there would be a
  false measurement).
- **Corpus:** `src/orator-stt/tests/corpus/` (`name.wav` 16 kHz mono
  plus `name.txt` reference; contract in `README.md`). No budgets gate
  yet — budgets land here once measured on real audio.
- **Decision:** harness green (`wer` goldens always run; live runner
  skip-clean verified via the live-ai target). Safe default holds:
  larger model until tiers score.

## Appendix R: residual findings to fixing sections (2026-11-01)

One row per residual finding named in the red-team closure order.
`Axis` follows the confidence vs task-value rule. `Fix` names the
section that owns the change. Production behavior stays as documented
until the Q13 harness for that family is green.

| Finding | File / function | Axis | Fix |
| --- | --- | --- | --- |
| `say` chain advances only on `NoAudio`; sibling empty-`Backend`/`Overflow` terminate | `src/orator-cli/src/main.rs::say`, `run_audio_turn`, `say_audio_server` | Task-value (empty voiced buffers) | Q1 |
| `audio_input_unsupported` retry arm dead-ends on boot failure (`?`) instead of falling through | `src/orator-s2s/src/session.rs::audio_input_unsupported`, `src/orator-s2s/src/ensure.rs::boot_audio_server` | Task-value (voiceless turn) | Q1 |
| `Owned` + `NoAudio` boots a second supervised server instead of consulting the daemon URL | `src/orator-cli/src/main.rs::say_audio_server`, `daemon_voice_endpoint` | Task-value (one boot per utterance) | Q1 |
| `daemon_voice_endpoint` erases `Ejected`/`Saturated` into boot-your-own | `src/orator-cli/src/main.rs::daemon_voice_endpoint` | Task-value (ejection honored) | Q1, Q2 |
| `say_kokoro` splits before normalize; daemon last resort can double-speak played prefix | `src/orator-tts/src/normalize.rs::normalize_for_speech`, `src/orator-tts/src/pipeline.rs`, `src/orator-cli/src/main.rs::say_kokoro` | Task-value (words spoken once) | Q1 |
| `say_via` collapses voiceless/aborted/over-bound into one transport string | `src/orator-daemon/src/voice.rs::say_via` | Correctness (taxonomy preserved) | Q2 |
| `say_blocking` re-wraps boot contention as turn failure | `src/orator-daemon/src/voice.rs::ensure`, `say_blocking` | Correctness | Q2 |
| Over-bound turns reset the ejection streak instead of counting toward it | `src/orator-daemon/src/voice.rs::record_voice_response`, `record_voice_backend`, `admission.rs::EjectionTracker` | Task-value (server misbehavior) | Q2 |
| Healthy `OPTIONS` probe re-admits a `POST`-failing child forever | `src/orator-daemon/src/voice.rs::is_alive`, `ensure` | Producer confidence vs task-value | Q2 |
| Failing children pinned past idle budget by outcome alone | `src/orator-daemon/src/voice.rs::reap_if_idle` | Resource (bounded work) | Q2 |
| Dead `RequestOutcome::NoAudio` variant never constructed | `src/orator-daemon/src/voice.rs::RequestOutcome` | Correctness (no dead taxonomy) | Q2 |
| Over-cap/slow-peer connections close without a reply | `src/orator-daemon/src/socket.rs::serve`, `serve_call`, `src/orator-daemon/src/ws.rs` | Task-value (bounded work) | Q3 |
| Booting owner holds the handler for a 240 s boot instead of answering contention | `src/orator-daemon/src/voice.rs::ensure`, `VOICE_BOOT_TIMEOUT`, `wait_for_boot` | Task-value (billed time) | Q3 |
| CLI 10 s timeout under 240 s boot + 120 s stream + 300 s worker with no overall budget | `src/orator-cli/src/socket.rs::call_socket_at`, `call_ws_at`, `src/orator-s2s/src/session.rs`, `src/orator-s2s/src/worker.rs::synth_blocking` | Task-value (billed time) | Q3 |
| Daemon serving bridges on serializing fallback; one cold boot blocks all turns | `src/common-core/src/runtime.rs::block_on`, `block_on_shared` | Task-value (concurrency) | Q3 |
| `free_port` hint-then-reuse races; `extra_args` unvalidated; supervisor env/group unpinned | `src/orator-llm/src/supervisor.rs::free_port`, `extra_args`, `spawn`, `stop_child` | Security/correctness | Q3 |
| Permission-denied connects report as absent daemons | `src/orator-cli/src/socket.rs::call_socket_at`, `call_ws_at`, `CallFailure`, `ExitCode` | Correctness (exit code) | Q4 |
| `serve_stream` takes peer uid as caller argument instead of reading its own credential | `src/orator-daemon/src/socket.rs::serve_stream`, `own_uid_of`, `src/orator-core/src/socket.rs::check_peer_uid` | Security (fail-closed gate) | Q4 |
| Hardening `chmod` failures ignored (`let _`) on daemon and store paths | `src/orator-daemon/src/socket.rs::restrict`, `bind`, `src/orator-store/src/store.rs` | Security (fail-closed) | Q4 |
| Single-unlink reclaim races shutdown unlink; symlink paths accepted | `src/orator-store/src/store.rs`, `src/orator-daemon/src/main.rs`, `src/orator-daemon/src/socket.rs::bind` | Security/correctness | Q4 |
| `sh -c` wrapper necessity undocumented; timeout `kill` unpinned; supervisor env unpinned | `src/orator-tools/src/sandbox.rs::Guarded::run`, `shell_escape` | Security invariant | Q5 |
| Check-then-open readers/writers race symlink swaps | `src/orator-tools/src/sandbox.rs::within`, `real_dir`, `src/orator-tools/src/builtins.rs` | Security invariant | Q5 |
| `real_dir` memo never invalidates | `src/orator-tools/src/sandbox.rs::real_dir` | Correctness | Q5 |
| Timed-out approvals leak (`pending_count` never drains) | `src/orator-tools/src/approve.rs::ApprovalHub::wait` | Correctness (no leak) | Q5 |
| Tool outputs unbounded into prompts; 5-round loop unbounded; audit vector unbounded | `src/orator-tools/src/loop_.rs`, `builtins.rs::WebFetch`, `sandbox.rs` capture | Task-value (prompt integrity) | Q5, Q8 |
| Transcribe narrows `i64` centiseconds through lossy cast; `NaN`/`Inf` saturate clocks | `src/orator-cli/src/main.rs::transcribe`, `src/orator-cli/src/transcribe.rs::stamp_srt`, `src/orator-stt/src/whisper.rs::RawSegment` | Correctness (timestamps) | Q6 |
| `dictate --once` panics on huge durations, ignores signals, unbounded reserve | `src/orator-cli/src/main.rs::dictate_once`, `dictate_whisper`, `dictate_audio_server` | Resource (bounded capture) | Q6 |
| `toggle_dictation` stops every session (id-prefix match) | `src/orator-daemon/src/session.rs::SessionRegistry::start`, `src/orator-daemon/src/voice.rs::toggle_dictation` | Correctness (session scope) | Q6 |
| Dead leaves/flags/arms undecided (`turn.barge_in_ms`, `audio.*`, selectors, bare `--seconds`, double `dispatch`, `VadEnd`, drops) | `src/orator-turn/src/machine.rs`, `driver.rs`, `src/orator-core/src/config.rs`, `src/orator-daemon/src/socket.rs::route` | Correctness | Q6, Q7 |
| `Reload`, meeting `show`/`export`/`status`, `listen` flags, `approve --deny` accepted but never served | `src/orator-daemon/src/socket.rs::route`, `src/orator-core/src/protocol.rs`, `src/orator-cli/src/main.rs::listen`, `meeting`, `approve` | Correctness (no false success) | Q7 |
| `Vad` catalog family unreachable; list vs find family sets diverge; unpinned pull reaches network | `src/orator-models/src/model.rs::ModelKind`, `ops.rs::list_models`, `find_entry`, `verify_entry` | Correctness (catalog total) | Q7 |
| Playback queue, stream channel, history/prefix, scratch files unbounded | `src/orator-audio/src/playback.rs::StreamPlayer`, `src/orator-cli/src/stream.rs`, `src/orator-s2s/src/session.rs::history`, `src/orator-llm/src/session.rs`, `src/orator-s2s/src/worker.rs` | Task-value (bounded memory) | Q8 |
| Scratch WAV unowned; `TTS_TIMEOUT` outside chain budget; bench stat file non-atomic | `src/orator-s2s/src/worker.rs::synth_blocking`, `src/orator-bench/src/events.rs`, `src/common-core/src/io.rs::write_atomic` | Correctness (no leak) | Q8 |
| Absorbed vs illegal turn drops conflated; `VadEnd` undecided | `src/orator-turn/src/machine.rs::transition`, `is_legal`, `drops` | Correctness (telemetry) | Q6, Q9 |
| Ladder exhaustion arm unreachable; headless `OutputOnly` reports `Injected`; double-print | `src/orator-desktop/src/ladder.rs::inject`, `attempt_rung`, `src/orator-cli/src/main.rs::dictate_once` | Correctness (sink report) | Q9 |
| `chat` pipelining comment claims overlap but turns run serial; abort key dead; `pending_audio` unbounded | `src/orator-cli/src/main.rs::chat`, `pending_audio`, `src/orator-s2s/src/session.rs::chat_turn` | Task-value (ordered audio) | Q9 |
| `ask` strict port/weights fail slow (`unwrap_or(0)`, 60 s boot on missing weights) | `src/orator-cli/src/main.rs::ask`, `boot_supervised`, `src/orator-llm/src/session.rs` | Correctness (fail fast) | Q10 |
| TUI mixes resample rates, unbounded `pending_utterance`, conflates WS timeout/refusal | `src/orator-cli/src/tui.rs::drain_capture`, `TranscribeJob`, `src/orator-cli/src/socket.rs` | Correctness (resampling/order) | Q10 |
| Dictation continuation untested at CLI seam; clean-text history gap | `src/orator-desktop/src/continuation.rs`, `clipboard.rs`, `src/orator-cli/src/main.rs::dictate_once` | Task-value (words kept) | Q11 |
| Hold-to-talk needs live hotkey; portal/tray/D-Bus modeled but unwired | `src/orator-desktop/src/hotkey.rs`, `tray.rs`, `dbus.rs`, `krunner.rs` | Task-value (reliability) | Q11 |
| Neural-codec decision deferred without revisit owner | `src/orator-audio/src/codec.rs`, `doc/orator/CALIBRATION.md` codec row | Task-value (fidelity) | Q12 |
| Jitter/broadcast/watchdog/admission numbers advisory without red-gate owners | `src/orator-audio/src/jitter.rs`, `merge.rs`, `playback.rs`, `src/orator-daemon/src/ws.rs`, `src/orator-bench/src/*` | Task-value (liveness) | Q12, Q13 |

## Q13 new-trigger harnesses (2026-11-02)

Every trigger below ships only with a green harness family: a control
group that must not fire and a must-fire case. Hermetic only (no
wall-clock audio, no models). Performance budgets stay advisory.

| Trigger / threshold | Shipped value | Must-not-fire control | Must-fire case | Golden |
| --- | --- | --- | --- | --- |
| Fallback-advance classifier | Advance on `NoAudio`/`Backend`/`Overflow`; terminal on `Aborted` | Cancelled turns (`Aborted`); usage errors never reach the gate (distinct type) | Voiceless + transport failures advance | `orator-s2s/tests/session.rs::fallback_advance_grid_with_must_not_fire_controls` |
| Ejection counting/streak | `Backend` + `Overflow` eject; responsive (incl. voiceless) turns break the streak | Single transient + success; voiceless streak | Two consecutive transports eject | `orator-daemon/tests/admission.rs` (`transient_failure_then_success_never_ejects`, `two_consecutive_backend_turns_eject_and_fail_fast`, `text_only_voiceless_turns_never_eject`), `orator-daemon/tests/voice.rs::overflow_counts_toward_ejection_like_backend` |
| Admission bound | Shared gate, idle admits all, saturation refuses fast with `Saturated`, probe re-enters | Idle capacity | Over-cap burst | `orator-daemon/tests/admission.rs::idle_capacity_admits_everything`, `saturation_refuses_fast_with_a_named_reason`, `recovered_endpoint_re_enters_through_the_probe` |
| Duplex bands (composed) | VAD spans, endpointing holds, barge-in stimuli | Silence/tone/noise/echo controls | Speech spans, complete holds, sustained+words barge | `orator-s2s/tests/duplex_calibration.rs`, `orator-turn/tests/{hold,barge_in}_calibration.rs`, `orator-stt/tests/streaming.rs::silence_and_noise_commit_nothing` |
| Model verify gate (composed) | Pinned+matching ok; mismatch fails; unpinned never-ok | Unpinned entries | Pinned mismatch | `orator-models/tests/{calibration,ops}.rs` |
| Dictate cap/ceiling | Default 30 s, ceiling 120 s, `MAX_DICTATE_SAMPLES`, checked deadline, `SignalWatch`, drop counters | `--seconds 5` valid; short captures unaffected | `0`/`u64::MAX`/over-ceiling refuse; overflow truncates with notice | `orator-cli/tests/dictate.rs` |
| Transcribe clamp set + strip parity | `u64` conversion, `end.max(start)`, `NaN`/`Inf` → usage; audio-server strips, whisper verbatim | In-range stamps byte-identical | Huge/negative/reversed/`NaN`/`Inf` | `orator-cli/tests/transcribe.rs` |
| Approval-timeout drain | `wait` drains on timeout/cancel/drop; late answers refuse | Answered rounds resolve | Timeout/cancel/drop → `None` + count 0 | `orator-tools/tests/approve.rs` |
| Playback/stream/history/scratch bounds | Queue 480k drop-oldest + fallible `finish`; `sync_channel(2)` + cancel; history 100 / prefix 32k + markers; scratch drop-guard | Small/short sessions verbatim | Over-cap drops/truncates/marks; scratch absent after drop | `orator-audio/tests/playback.rs`, `orator-cli/tests/stream.rs`, `orator-s2s/tests/{session,worker}.rs`, `orator-llm/tests/session.rs`, `orator-bench/tests/events.rs` |
| Chat order + abort | Order-preserving join, abort cancels in-flight turn, `pending_audio` ≤ 1 | Serial fallback order-identical | Overlap + abort + bound | `orator-cli/tests/stream.rs`, `orator-turn/tests/driver.rs`, `orator-s2s/tests/session.rs::abort_*` |
| Ask fail-fast | Bad port → `Usage`; missing weights → fast `Error`; speak failure distinct | Valid ports/weights boot | Garbage port, missing weights, speak failure | `orator-cli/tests/ask.rs` |
| TUI rates | 16 kHz remainder domain, `MAX_PENDING_UTTERANCE` + counter, distinct WS texts | Single-rate path byte-identical | Mixed-rate exact; slow transcribe bounds; barge aborts | `orator-cli/tests/tui.rs`, `orator-cli/tests/socket.rs` |
| Refine/continuation/chunker/cancel | Chain LLM → raw → save; 10 s join; MIN/FIRST_MAX/CAP + holds; 3-hop cut | Clean turns byte-identical; fresh memory no prefix | LLM decline → raw + saved; joined output; cut retires generation | `orator-desktop/tests/refine.rs`, `ladder.rs`, `continuation.rs`, `orator-tts/tests/splitter.rs`, `orator-turn/tests/driver.rs` |
| Jitter/broadcast/watchdog | 80 ms cap; `channel(10)` Lagged-skip; 5 s ping / 10 s silence / 360 s cap | Idle/local paths unaffected | Cap closes; lagged skips; over-cap drops | `orator-audio/tests/jitter.rs`, `orator-store/tests/events.rs`, `orator-daemon/tests/ws.rs`, `orator-core/tests/config.rs` |
