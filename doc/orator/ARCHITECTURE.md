# Orator — Architecture

A code assistant's quick guide to the Orator codebase: what each
part does, which files and named items hold the behavior, and which
design laws every change must obey. Companion to `VISION.md` (the
"why") and `ORATOR.md` (the normative spec); for measured numbers
see `CALIBRATION.md`, for the desktop rollout plan see
`DESKTOP_WIRING.md`, for the last full state see `HANDOFF_Q.md`.

## 1. Processes and crate map

Two processes. One daemon (`oratord`, crate `orator-daemon`) owns
the credentialed Unix socket, an optional loopback-only WebSocket
listener, the session registry, and the supervised inference fleet.
Short-lived CLI processes (`orator`, crate `orator-cli`) run local
commands or forward control calls over either transport.

| Crate | Role |
| --- | --- |
| `orator-core` | Shared types, config schema, wire codec, traits, paths, GPU probe |
| `orator-audio` | Capture/playout backends, resampling, jitter, archive, codec seam |
| `orator-vad` | Energy/zero-crossing voice activity detection |
| `orator-stt` | Whisper backend + streaming commit wrapper |
| `orator-llm` | Endpoint session + supervised `llama-server` child owner |
| `orator-tts` | Kokoro backend, sentence splitter, text normalizer |
| `orator-turn` | Turn state machine, endpointing/hold driver, barge-in gate |
| `orator-tools` | Tool trait/registry, agentic loop, sandbox, approvals, policy |
| `orator-store` | SQLite event log, sessions, transcripts, retention |
| `orator-models` | Model catalog, verification, residency (dependency leaf) |
| `orator-diar` | Segmentation, embeddings, clustering, meeting pipeline |
| `orator-notes` | Live notes, map-reduce summarizer, full-text search |
| `orator-desktop` | Injection ladder, refine chain, clipboard, hotkeys, tray, D-Bus, KRunner |
| `orator-s2s` | Native speech-to-speech adapter over the supervised endpoint |
| `orator-daemon` | `oratord`: sessions, voice serving, admission, listeners |
| `orator-cli` | `orator`: all user commands, TUI, transcribe, dictate |
| `orator-testkit` | Deterministic fakes: clocks, scripted scenarios, slow probes |
| `orator-bench` | Scenario replay, latency/RSS/drop measurement, stat files |

Shared foundations (never reimplemented here; compose them):
`fluent-concurrency` (`Scope`, `SupervisedBatch`, `WorkerPool`,
`ResultPool`, `PriorityResultPool`, `Limiter`, `flow` credits,
`CreditedFeedWorker`, `PriorityQueue`/`BoundedPriorityQueue`,
`StreamAbort`, `first_accept_in_order`), `fluent-wvr`
(`Component`, `WorkUnit`, `FieldAccess`, `Describable`,
`first_accept_in_order_sync`, boundary/capability helpers),
`common-core` (`block_on`/`block_on_shared`, `lock`,
`shared_http_client`/`test_http_client`, `write_atomic`,
`find_hierarchical`, `LatencyHistogram`, calibration helpers),
`fluent-llm` (`LlmRequestQueue`, `LlmClient`, `tokens` incl.
`TokenBudget`, `ContextPacker`, artifact plane), `fluent-db`
(`SqliteStore`, `SqlitePool`, typed queries, `migrate`),
`dag` (`DependencyGraph`, `CheckpointedStepGraph`),
`spacy-rs` (`Sentencizer`).

Dependency edges enforced by `it_source_guard` + `cargo tree`:
`orator-models` is a leaf (config/core only); `orator-s2s` never
depends on `orator-stt`; all project roots carry
`#![forbid(unsafe_code)]`.

## 2. Design laws (read before changing anything)

1. **Confidence vs task-value.** A component's self-report (probe
   2xx, VAD energy, `/models` listing, queue depth, "accepted")
   may gate *whether to try*. Only measured outcomes (words
   transcribed, audio voiced, turns completed, hashes matching)
   gate persistence, fallback, reaping, or billing time. Never let
   one stand in for the other.
2. **No false success.** Every command does the work or exits
   non-zero with a named reason (`ExitCode::{Success, Error,
   Usage, DaemonUnavailable, Interrupted}`). `ok:true` for a no-op
   is forbidden.
3. **Calibration before heuristics.** No trigger persists a
   decision, fills a cache, or reaps anything before its harness is
   green with a non-firing control group. Finals are pinned in
   goldens; perf budgets stay advisory.
4. **Bounded memory.** Streams, never whole-response buffers;
   windows evict; scrollback caps; long sessions hold ceilings. New
   bounds add counters/markers, never silent drops of durable data.
   See §8.
5. **Capability gating.** Serving paths run inside
   `Scope`/`SupervisedBatch` with capabilities installed;
   Listen-in/Meeting sets contain no effectful token, so ambient
   speech fails closed at the first gated call.
6. **Record first.** The archive tap is lossless and credit-gated
   upstream of inference. Realtime is bounded-lossy with drop
   counters; durable is lossless.
7. **Single homes (DRY).** One supervisor, one transport client,
   one socket codec, one method table, one command table, one
   ladder walker, one truncation helper, one calibration harness,
   one bench runner. The table in §4 names each home.
8. **Sync↔async bridging.** Never build a per-call Tokio runtime;
   compose `block_on_shared` (concurrent) or `block_on`
   (serializing). Never hold a lock across `.await` (use
   `common_core::sync::lock`). Never share a pooled HTTP client
   across test runtimes (use `test_http_client`).
9. **Wire stability.** `MessageType` discriminants, `METHOD_NAMES`
   strings, frame bytes (4-byte LE length = body+1, 1-byte type,
   body), JSON envelopes stay byte-identical. Additive deltas only:
   error `kind` fields, `Saturated`/boot-contention replies,
   `daemon.ws_*` leaves.
10. **Hermetic tests.** No GPU, model, network peer, or wall-clock
    audio in unit/integration tests. Real inference is `#[ignore]`
    + `live-ai` only. Unit tests live in
    `src/<crate>/tests/<module>.rs` with a `#[path]` forwarder,
    never inline `mod tests`.

## 3. Data flow (one assistant turn)

```
mic/sys capture → [archive tap, lossless] → resample 16k → AEC →
VAD → STT → turn manager → LLM queue → sentence split → TTS →
playout → speakers. Barge-in trips the epoch StreamAbort; playout
fades 20 ms; history commits only DAC-confirmed words + marker.
```

The LLM serving detail: the turn queue is an `LlmRequestQueue`
fronting the supervised endpoint in `orator-llm`; per-turn prefill
is chunked, speculative prefixes re-request after abort (the
server keeps its KV cache — no client-side KV surgery), and slot
snapshots bound the cost of a wrong guess.

## 4. Crate guide (files → named items)

### `orator-core` — shared vocabulary

- `src/types.rs`: `Frame` (fallible `new`, `NonZeroU32` rate),
  `MediaTime`, `Channel::{Mic, Sys, Reply}`, `AudioChunk`,
  `DuplexOut`, `Epoch`, `Event`, `Word`/`WordMark`, `Token`,
  `Sampler`.
- `src/protocol.rs`: `METHOD_NAMES` (11 methods), `MessageType`
  (+ `to_u8` discriminants), `validate_method`.
- `src/socket.rs`: `SocketFrame::{read_from, write_to}`,
  `check_peer_uid`, `socket_path`, `FrameError`, `SOCKET_MODE`.
- `src/config.rs`: `OratorConfig` + `DaemonConfig` (`ws_listen`,
  `ws_token`, `ws_ping_secs`, `ws_inactivity_secs`,
  `ws_cap_secs`, `admission_max`), `LlmEndpointConfig`,
  `SttConfig`, `TtsConfig`, `TurnConfig`, `AudioConfig`,
  `AudioBundle`/`default_bundle`, `models_dir`, `config_path`,
  `resolve_llm_weights`, `llama_weights_dirs`.
- `src/traits.rs`: `DuplexEngine`, `LlmSession`, `SttStream`,
  `TtsStream` (all `Component`s).
- `src/gpu.rs`: `require_gpu`, `device_present`.
- `src/paths.rs`: `home_dir`, `find_on_path`,
  `validate_server_binary`, `within_root`.

### `orator-llm` — session + supervised children (single home)

- `src/session.rs`: `HttpSession::{open, with_queue, push_text,
  step, plan_task, pack_user_text}`; stored-prefix roll with loud
  truncation marker; packing under `ctx_tokens` via
  `TokenBudget`.
- `src/supervisor.rs`: `Supervisor::{boot, attach,
  attach_with_probe, stop, stop_child, is_stopped, base_url}`,
  `SupervisorConfig` (`port`, `weights`, `ctx`, `health_timeout`,
  `extra_args` allowlist, `health_probe`), free functions
  `boot_with_binary`, `free_port`, `build_args`,
  `probe_voice_ready`, `recovery_action`; `HealthProbe::{GetHealth,
  OptionsCompletions}` (audio differs only by probe+argv).
  Spawns use `env_clear` + pinned `PATH`/`LANG` + process group +
  `kill_on_drop`; `stop_child` reaps boundedly (no zombie).
- `src/probe.rs`: `probe_models`, `PROBE_TIMEOUT`;
  `src/models.rs` (CLI side): `verify_report`.

### `orator-s2s` — native adapter (thin; no second supervisor)

- `src/session.rs`: `AudioSession::{new, transcribe_blocking,
  speak_blocking, chat_turn, step, abort_handle, set_epoch,
  live_text_handle, heard_audio, is_voicing}`,
  `should_advance_chain` (**the** fallback classifier: advance on
  `NoAudio`/`Backend`/`Overflow`, terminal on `Aborted`),
  `audio_input_unsupported`; `S2sConfig`, `S2sError`.
- `src/ensure.rs`: `ensure_audio_server`,
  `AudioEndpoint::{Owned,...}`, per-endpoint served-model hint
  cache with fail-invalidate; `src/config.rs`: `S2sConfig`
  (`turn_window_ms`).
- `src/worker.rs`: pre-spawn binary/bundle validation,
  `ScratchGuard` (drop-guard reaps scratch audio).
- Shares `orator_llm::{Supervisor, shared_http_client,
  LatencyHistogram}`; owns only audio argv/probe/stream shaping.

### `orator-daemon` — serving center

- `src/voice.rs`: `VoiceOwner` (registry `SessionId → Session`
  + `TurnMachine` + `StreamAbort` + activity; `EventLog` per
  turn), `VoiceError` (additive `kind`: `NoAudio`/`Overflow`/
  `Aborted`/`Backend`/`Saturated`/`Ejected`/`BootContention`),
  `start_session`/`stop_session`/`say_via`/`ask_via`/
  `toggle_dictation` (kind-scoped)/`shutdown`/`clear`/
  `reap_if_idle` (leases only, never outcome), `is_alive`,
  `RequestOutcome::{Ok, Backend}`.
- `src/admission.rs`: `AdmissionGate` (bound + depth guard, Drop
  releases), `EjectionTracker` (consecutive-`Backend` counts;
  responsive incl. voiceless turns re-enter), named `Saturated`
  refusal + counter.
- `src/socket.rs`: `serve_call` (transport-neutral dispatch),
  `route` + `reject_unknown_params`, `bridge_concurrent` (in-place
  serving bridge over the shared runtime), bounded probe→unlink→bind
  retry with symlink refusal, `serve_serial` (test helper).
- `src/ws.rs`: `serve_ws` / `serve_ws_on` / `serve_ws_on_tuned`,
  `WsTuning::from_config`, `WS_PING_EVERY` (5 s),
  `WS_INACTIVITY_TIMEOUT` (10 s), `WS_CONNECTION_CAP` (360 s),
  handshake token gate, JoinSet drain.
- `src/lib.rs`: `DISPATCH_METHODS`, `dispatch`,
  `contract_matches` (single-source verification).

### `orator-cli` — every user command

- `src/lib.rs`: `COMMANDS` (19 verbs), `Cli::{surface, methods}`,
  `OutboundRequest`, `ExitCode::{Success, Error, Usage,
  DaemonUnavailable, Interrupted}`.
- `src/main.rs`: `ask` (probe-then-boot, `boot_supervised` with
  fail-fast port/weights, `AskFail::Usage/Error`, one-question
  ownership), `say`/`say_chain` (normalizes once, daemon last
  resort speaks only the unsynthesized suffix),
  `say_audio_server`/`run_audio_turn` (releaser slot),
  `daemon_passthrough`, `chat` (order-preserving overlap,
  `pending_audio ≤ 1`), `dictate_once` (via
  `dictate::finish_turn`), `transcribe`/`dictate_whisper`/
  `stt_backend`, `config get/set`, `models`, `doctor`,
  `status`/`stats`, `listen`, `ptt`, `meeting`, `approve`,
  `daemon`, `tui`, shell-init/completions.
- `src/ask.rs`: `parse_ask_port`, `check_ask_weights`,
  `speak_failure_note`.
- `src/transcribe.rs`: `cue_from_centiseconds`,
  `audio_server_cue` (intentional no-fallback asymmetry),
  `render`, `TranscriptFormat::{Srt, Vtt, Txt, Json}`.
- `src/dictate.rs`: `validate_seconds`, `checked_deadline`,
  `reserve_for`, `capture_choice`, `finish_turn` (refine →
  record → inject), `DEFAULT/MAX_DICTATE_SECS`,
  `MAX_DICTATE_SAMPLES`.
- `src/tui.rs`: `TuiApp`, `drain_capture` (16 kHz remainder
  domain), `SttWorker` (thread-affine whisper reuse —
  context is not `Send`), `WhisperCache`/`KokoroCache`,
  `TranscribeJob`, `MAX_PENDING_UTTERANCE`,
  `SCROLLBACK`, `MicGate`, `voice_reply`.
- `src/socket.rs`: `daemon_call_at`/`call_transport`,
  `ws_timeout_text` vs `refused_text` (distinct),
  `map_connect_error`, `permission_denied_text`.
- `src/stream.rs`: `stream_chunks` (`sync_channel(2)`,
  cancel-on-play-error); `src/say_chain.rs`: `should_advance`
  (composes the s2s classifier); `src/shell.rs`,
  `src/doctor.rs`, `src/models.rs` (CLI side).

### `orator-desktop` — desktop integration

- `src/refine.rs`: `refine_transcript` (LLM → raw → save over
  `first_accept_in_order_sync`), `DEFAULT_REFINE_PROMPT`
  (shares the tool loop's transcript-as-data rule),
  `RefineSource::{Llm, Raw}`, `wrap_transcript`.
- `src/ladder.rs`: `inject`, `Rung::ORDER`,
  `LadderMemory::{last_working, last_inject}` (per-handle;
  `last_inject` drives 10 s continuation spacing),
  `InjectOutcome::{Injected, OutputOnly}`,
  `InjectError::{Failed, Fatal, ...}`.
- `src/clipboard.rs`: `paste_with_restore` (180 ms),
  `capture_selection` (200 ms), 10 ms change polls,
  restore-or-clear, own-write fingerprint ordering;
  `Clipboard`/`ChordSender` traits, `ExternalClipboard`,
  `InMemoryClipboard`, `NoChord`.
- `src/continuation.rs`: `continuation_prefix`,
  `CONTINUATION_WINDOW` (10 s); `src/hotkey.rs`:
  `parse_chord`, `fallback_bindings` (quoted CLI verbs);
  `src/krunner.rs`: `parse_query`; `src/tray.rs`,
  `src/notify.rs`, `src/dbus.rs` (parity-tested models; ambient
  callers can never approve). Wiring status:
  `doc/orator/DESKTOP_WIRING.md`.

### `orator-turn` / `orator-vad` — conversation policy

- `turn/src/machine.rs`: `TurnMachine`, `TurnState`
  (`Idle`/`UserSpeaking`/`Endpointing`/`Thinking`/`ToolWait`/
  `Speaking`/`Yielding`), `TurnEvent`; legality drops counted,
  never errors.
- `turn/src/driver.rs`: `TurnDriver::{with_config, detect}`,
  `HopGate::{pass, cut}`, `Hop::{PostStt, PreLlm, PreSpeak}`
  (three-hop generation cancel), `BargeInOutcome`.
- `vad/src/energy.rs`: `EnergyVad::{with_floor, push}`,
  `VadOutput`; floor/hysteresis/attack/hangover constants
  (calibrated bands in `CALIBRATION.md`).

### `orator-stt` / `orator-tts` — engines

- `stt/src/whisper.rs`: `WhisperBackend::{new, with_gpu,
  decode_blocking}` (lazy context, language/model-keyed
  rebuild); `stt/src/streaming.rs`: `StreamingStt`
  (utterance-absolute incremental commits, tail-only window),
  `BlockDecoder::decode` (+ `offset_ms`).
- `tts/src/kokoro.rs`: `KokoroBackend::{new, with_gpu,
  synth_blocking, synth_async}`, `trim_silence`,
  `uniform_marks`, `KOKORO_SAMPLE_RATE`, load-once session;
  `tts/src/splitter.rs`: `split_all` (MIN=16, FIRST_MAX=100,
  CAP=220, comma fast-path, buffer-end hold, abbreviation
  guard); `tts/src/normalize.rs`, `tts/src/pipeline.rs`
  (`synth_chunks_blocking`); `default_paths`.

### `orator-audio` — sound in/out/on disk

- `src/backend.rs`: `CpalBackend`, `open_default_output`
  (shared device opener); `src/playback.rs`:
  `play_blocking`, `StreamPlayer::{open, push, dropped,
  pushed, played, finish}` (`MAX_QUEUE_SAMPLES` = 480 000,
  drop-oldest + counter, fallible finish).
- `src/jitter.rs`: `JitterBuffer::{for_20ms_frames, push,
  pop}` (80 ms cap, drop-oldest-to-target, floor growth);
  `src/resample.rs`: `resample_linear`; `src/merge.rs`:
  `MergeQueue` (MediaTime-ordered mic/sys/reply);
  `src/wav.rs`: `read_wav_mono`, `WavPcm` (single home);
  `src/codec.rs`: `Codec::{encode_step, decode_step, reset}`,
  `PcmCodec` (supported path; neural adoption deferred with a
  recorded revisit rule); `src/archive.rs`, `src/source.rs`,
  `src/envelope.rs`, `src/doctor.rs`, `src/error.rs`.

### `orator-tools` — agency with guardrails

- `src/tool.rs`: `Tool` trait, `ToolCall`/`ToolRegistry`,
  `truncate_tool_output` + `TOOL_OUTPUT_CHARS` (single home),
  `ToolTier`.
- `src/loop_.rs`: `run_turn`, `bound_prompt` (5 rounds + final
  tool-less pass under `TokenBudget`, `MAX_PROMPT_TOKENS`
  = 8192, `MAX_TOOL_ROUNDS`), `last_prompt_for_tests`.
- `src/sandbox.rs`: `Guarded::{run}`, `GuardedConfig`,
  `shell_escape` (cap-carrying `sh -c`), `kill_program`
  (process-group kill + `kill_on_drop` backstop),
  `open_within`, `within`/`real_dir` (TTL+mtime memo),
  `ToolAudit` rotation; `src/policy.rs`:
  `Policy`/`PolicyCtx`/`CallOrigin`; `src/approve.rs`:
  `ApprovalHub::{wait}` (drains on timeout/cancel/drop),
  `ApprovalId`.
- `src/parse.rs`: `gbnf_for_tools`, `parse_model_output`,
  `parse_xml` (+ `ParseError`); GBNF primary + XML fallback
  with an identical-calls golden. `src/untrusted.rs`:
  `wrap_untrusted`, `wrap_tool_result`,
  `TRANSCRIPT_AS_DATA`. `src/builtins.rs`:
  `default_registry`, `FETCH_CHARS`.

### `orator-store` — one event log, thin views

- `src/events.rs`: `EventLog::{over, in_memory, append,
  replay, replay_kind, count_kind, latest, subscribe,
  publish_ephemeral}`, `StoredEvent`, `REPLAY_PAGE` (5 000),
  `broadcast::channel(10)` fan-out with Lagged-skip, single
  producer.
- `src/schema.rs`: `migrations`, `CURRENT_VERSION`;
  `src/store.rs`: `open_store` (backup-before-migrate),
  `InstanceLock::acquire` (bounded reclaim, symlink refusal),
  `spawn_writer`, `writer_feed_config`, `WriteReq`.

### `orator-diar` / `orator-notes` — meetings into knowledge

- `diar`: `segment`, `align_words`, `speaker_name`,
  `MeetingPipeline` (finalize → offline_stt → diarize →
  align → name → summarize → export over
  `CheckpointedStepGraph` with persisted checkpoints and
  `rewind_to`).
- `notes`: `roll_summary`, `append_fact`, map-reduce
  summarizer with mandatory segment citations (dangling
  citations reject), Markdown facts + rolling summary,
  FTS search over the store.

### `orator-models` / `orator-bench` / `orator-testkit`

- `models/src/ops.rs`: `list_models`, `find_entry`,
  `verify_entry`, `pull_plan`; `models/src/model.rs`:
  `ModelEntry`, `vad_entries`; `models/src/residency.rs`
  (eviction + voice-lease holds); CLI `verify_report` fails
  unpinned/missing loudly.
- `bench/src/`: `scenarios`, `gates` (anti-tautology: a 10×
  slowed fake must fail), `replay::run_scenario`,
  `stats`/`events` (`InputPcm`/`Step`/`StepPostSampling`/
  `SendPcm` to `--stat-file`).
- `testkit/src/fakes.rs`: `FakeClock`, `ScriptStep`,
  `ScriptScenario::{clean, faults_at}`, `SlowFake::scale`.

## 5. Cross-cutting contracts

- **Wire** (`orator-core::protocol`, `orator-core::socket`,
  `orator-daemon::socket`, `orator-cli::socket`): 11 methods;
  `COMMANDS` (19 CLI verbs) ↔ `METHOD_NAMES` ↔
  `DISPATCH_METHODS` pinned equal by `it_parity.rs`;
  `OutboundRequest::new` refuses unknown/ignored params.
- **Errors**: `ExitCode` (CLI), `S2sError` (adapter),
  `VoiceError` (daemon, additive `kind` bodies),
  `DispatchError::NotImplemented`, `FrameError`,
  `AudioError`, `LlmError::is_retryable`.
- **Config** (`OratorConfig`, hierarchical `orator.json`,
  `config get/set/path`): GPU off / WS off / bounds calibrated
  by default; old files load calibrated defaults; garbage
  values are `Usage` errors.
- **Shutdown**: global `shutdown_signal`, per-session
  `StreamAbort` + `SignalWatch`, `VoiceOwner::shutdown`
  (bounded in-flight wait), `serve` JoinSet drains,
  `Supervisor::stop` + drop-guard scratch files.
- **Goldens that guard the whole**: `it_end_to_end.rs`
  (cascade, live barge-in conversation, 18-vector
  no-false-success, config-honored, composed bounds),
  `it_parity.rs` (surface/contract equality),
  `it_source_guard.rs` (no plan words, section-ID scan,
  `forbid(unsafe_code)` roots, manifest edges),
  `it_doc_guard.rs` (spec names resolve).

## 6. Gates (how to verify a change)

Per subtask: `make orator-smoke`. Public types changed: add
`make orator-lint`. Milestone done: add `make orator-test`.
Never run repo-wide `make test`/`make soak` to check a
component; gate logs land in
`/tmp/opencode/monorepo-gates/<target>.log` — grep/tail the log
on failure, never re-run a gate just to see output. Heavy
timing/concurrency work alone may use `make orator-soak`;
live-model checks alone may use `make orator-test-live`
(`#[ignore]` + `live-ai`, skips cleanly without env).
