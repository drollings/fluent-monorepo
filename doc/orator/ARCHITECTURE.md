# Orator — Architecture

A code assistant's quick guide to the Orator codebase: what each
part does, which files and named items hold the behavior, and which
design laws every change must obey. Companion to `VISION.md` (the
"why"); for measured numbers see `CALIBRATION.md`, for the desktop
rollout plan see `DESKTOP_WIRING.md`, for the last full state see
`HANDOFF_Q.md`.

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
| `orator-vad` | Energy detector plus tonal commit gate |
| `orator-stt` | Whisper backend, Moonshine streaming backend, commit wrapper |
| `orator-llm` | Endpoint session plus supervised `llama-server` child owner |
| `orator-tts` | Kokoro backend, sentence splitter, text normalizer |
| `orator-turn` | Turn state machine, endpointing/hold driver, barge-in gate |
| `orator-tools` | Tool trait/registry, agentic loop, sandbox, approvals, policy |
| `orator-store` | SQLite event log, sessions, transcripts, retention |
| `orator-models` | Model catalog, verification, residency (dependency leaf) |
| `orator-diar` | Segmentation, embeddings, clustering, meeting pipeline |
| `orator-notes` | Live notes, map-reduce summarizer, full-text search |
| `orator-desktop` | Probes, injection ladder, refine chain, clipboard, hotkeys, tray, D-Bus, KRunner |
| `orator-s2s` | Native speech-to-speech adapter over the supervised endpoint |
| `orator-lfm` | LFM3-Audio reference config: arch, chat template, generation params, layout |
| `orator-daemon` | `oratord`: sessions, voice serving, admission, listeners |
| `orator-cli` | `orator`: all user commands, TUI, transcribe, dictate, fallback |
| `orator-testkit` | Deterministic fakes: clocks, scripted scenarios, slow probes |
| `orator-bench` | Scenario replay, latency/RSS/drop stat files |

Shared foundations (never reimplemented here; compose them):

- `fluent-concurrency` (`Scope`, `SupervisedBatch`, `WorkerPool`,
  `ResultPool`, `PriorityResultPool`, `Limiter`, `flow` credits,
  `CreditedFeedWorker`, `PriorityQueue`/`BoundedPriorityQueue`,
  `StreamAbort`; re-exports the `fluent-wvr` ladders).
- `fluent-wvr` (`Component`, `WorkUnit`, `FieldAccess`,
  `Describable`, `delegate_component_fields!`,
  `first_accept_in_order` / `first_accept_in_order_sync`,
  `first_some_in_order_sync`, boundary/capability helpers).
- `common-core` (`block_on`/`block_on_shared`, `lock`,
  `shared_http_client`/`test_http_client`, `write_atomic`,
  `find_hierarchical`, `LatencyHistogram`, `calibrate_threshold` /
  `passes_gate`, `pcm`, `envelope`, `hash::verify_sha256_file`,
  `path::normalize_std_path` / `MAX_SYMLINK_HOPS`,
  `io::bind_unix_listener_with_stale_reclaim`,
  `retry::{run_with_budget, capped_exp_backoff_ms, PollWithBackoff}`,
  `string::LoweredHaystack`).
- `fluent-llm` (`LlmRequestQueue`, `LlmClient`, `tokens` incl.
  `TokenBudget`, `ContextPacker`, `truncate_middle`,
  `openai::model_ids`, artifact plane, SSE).
- `fluent-db` (`SqliteStore`, `SqlitePool`, typed queries, `migrate`),
  `dag` (`DependencyGraph`, `CheckpointedStepGraph`),
  `spacy-rs` (`Sentencizer`).

Dependency edges enforced by `it_source_guard` plus `cargo tree`:
`orator-models` is a leaf (config/core only); `orator-s2s` never
depends on `orator-stt`; `common-core` stays zero-domain (no
`fluent-*`, `guidance`, `router`, `coral`, `wasm_ipc` imports);
`fluent-concurrency` consumes `fluent-wvr` control-plane types
without redefining them; all Orator roots carry
`#![forbid(unsafe_code)]`.

## 2. Design laws (read before changing anything)

1. **Confidence vs task-value.** A component's self-report (probe
   2xx, VAD energy, `/models` listing, queue depth, "accepted")
   may gate *whether to try*. Only measured outcomes (words
   transcribed, audio voiced, turns completed, hashes matching)
   gate persistence, fallback, reaping, or billed time. Never let
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
   See §8 of the old map: playback, jitter, archive spill, TUI,
   transcribe, and history/prefix ceilings.
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
   across test runtimes (use `test_http_client` or a per-test
   client).
9. **Wire stability.** `MessageType` discriminants, `METHOD_NAMES`
   strings, frame bytes (4-byte LE length = body+1, 1-byte type,
   body), JSON envelopes stay byte-identical. Additive deltas only:
   error `kind` fields, `Saturated`/boot-contention replies,
   `daemon.ws_*` leaves.
10. **Hermetic tests.** No GPU, model, network peer, or wall-clock
    audio in unit/integration tests. Real inference is `#[ignore]`
    plus `live-ai` only. Unit tests live in
    `src/<crate>/tests/<module>.rs` with a `#[path]` forwarder,
    never inline `mod tests`.

## 3. Data flow and model selection

```
mic/sys capture → [archive tap, lossless] → resample 16k → AEC →
VAD gate → STT → turn manager → LLM queue → sentence split → TTS →
playout → speakers. Barge-in trips the epoch StreamAbort; playout
fades 20 ms; history commits only DAC-confirmed words + marker.
```

The LLM serving detail: the turn queue is an `LlmRequestQueue`
fronting the supervised endpoint in `orator-llm`; per-turn prefill
is chunked, speculative prefixes re-request after abort (the
server keeps its KV cache — no client-side KV surgery), and slot
snapshots bound the cost of a wrong guess.

Serving selection is role-table authority, shared with the router's
shape but trimmed to Orator backends:

- `orator.json` holds a `models` table (`tag → {weights, backends,
  params, roles overrides}`), a `roles` table (`asr`, `tts`,
  `chat`, each `{models: [ordered tags], params}`), and a
  top-level `default` model (LFM3-Audio-2B).
- Composition per `(role, tag)` is `overlay_params` shallow
  per-key: model base, then the role run block, then the
  model-side per-role override. Unknown roles/tags compose to an
  empty object and fail closed at the backend, never at params.
- Resolution: role head first, then `default` when the list is
  empty, never a substitution for a listed-but-unresolvable head
  (that fails loudly). Unknown roles resolve to `None` even with
  `default` set. Whisper walks heads, then `default`, then the
  `stt.model` leaf (deduped, so default configs serve the same
  tried list as before). Ask uses the `roles.chat` head with the
  `llm.model` leaf as fallback. Language/voice read composed
  params first with the leaf as fallback.
- Native preference (`should_prefer_s2s`) is pure tag logic: the
  same tag heads both voice roles through the `audio-server`
  backend. The native session model (`agreed_s2s_model`) is that
  agreed tag, else the `s2s.model` leaf. Servability is decided at
  assurance time, never guessed at selection.
- `doctor roles:` renders the winner plus composed params and
  names any ignored leaf (`stt.model`/`llm.model`/`s2s.model`/
  `tts.voice` differs from effective); `speech:` adds
  `session model:`. The operator fix is to move the leaf value
  into the winning layer, never to clear the role list.

Fallback order is load-bearing (`orator-cli/src/fallback.rs`):
daemon first, local specialized inference second, loud error last.
Only *unavailable* refusals fall through (8 markers via
`LoweredHaystack::contains_any`); busy/transient/usage stay
terminal. Local ASR/TTS runs under `LOCAL_OP_BUDGET` (10 s) via
`common_core::retry::run_with_budget`, detached on timeout.

## 4. Crate guide (files → named items)

### `orator-core` — shared vocabulary and selection

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
  `ws_cap_secs`, `admission_max`), `AudioConfig`, `SttConfig`,
  `LlmEndpointConfig`, `TtsConfig`, `S2sConfig`, `TurnConfig`,
  `AudioBundle`/`default_bundle`/`audio_bundle_dir`/
  `bundle_from_s2s`, `models_dir`/`meetings_dir`/`config_dir`/
  `config_path`, `DEFAULT_GGUF_DIR`, `DEFAULT_ASR_MODEL`,
  `DEFAULT_TTS_MODEL`, `DEFAULT_MODEL`, `DEFAULT_LLM_MODEL`,
  `resolve_llm_weights`, `llama_weights_dirs`.
  Tables: `ModelsConfig` (transparent over `tag → ModelSpec`),
  `ModelSpec{weights, backends, params, roles}`, `RoleSpec{models,
  params}`, `RolesConfig{asr, tts, chat}`,
  `default_models`/`default_chat_role`. Composition:
  `overlay_params`, `compose_role_params`,
  `resolve_role_model`, `resolve_role_model_or_default`,
  `agreed_s2s_model`, `should_prefer_s2s`,
  `resolve_model_weights`. Root derives `FieldAccess +
  Describable` (`#[field(nested)]`; tables stay whole-JSON like
  the router's).
- `src/paths.rs`: `home_dir`, `find_on_path`/`find_all_on_path`,
  `validate_server_binary`, `ExplicitBinary`,
  `filter_validated_binaries`, `resolve_validated_binary`,
  `within_root`, `has_symlink_parent`, `resolve_hop_capped`
  (via `common_core::path::normalize_std_path`,
  `MAX_SYMLINK_HOPS = 40`).
- `src/traits.rs`: `DuplexEngine`, `LlmSession`, `SttStream`,
  `TtsStream` (all `Component`s).
- `src/gpu.rs`: `require_gpu`, `device_present`.

### `orator-llm` — session plus supervised children (single home)

- `src/session.rs`: `HttpSession::{open, with_queue, push_text,
  step, plan_task, pack_user_text}` via
  `delegate_component_fields!(HttpSession, config)`; stored-prefix
  roll with loud truncation marker; packing under `ctx_tokens` via
  `TokenBudget`.
- `src/supervisor.rs`: `Supervisor::{boot, attach,
  attach_with_probe, stop, stop_child, is_stopped, base_url}`
  (`boot` walks owned candidates via `first_accept_in_order`,
  warn-and-continue, last error wins; empty stays `NoBinary`);
  `SupervisorConfig` (`port`, `weights`, `ctx`, `health_timeout`,
  `extra_args` allowlist, `health_probe`), `boot_with_binary`,
  `free_port`, `build_args`, `probe_voice_ready`,
  `recovery_action`; `HealthProbe::{GetHealth,
  OptionsCompletions}`. Spawns use `env_clear` + pinned
  `PATH`/`LANG` + process group + `kill_on_drop`; `stop_child`
  reaps boundedly.
- `src/probe.rs`: `probe_models` walks
  `["/models", "/v1/models"]` via `first_accept_in_order`
  (stop only on `ProbeError::Unreachable`; first-path transport
  failure sanitized to `Unreachable(base, "connection failed")`
  so no `/v1` request fires); `ProbeInfo.via_v1_fallback`;
  ID extraction composes `fluent_llm::openai::model_ids`.

### `orator-s2s` — native adapter (thin; no second supervisor)

- `src/session.rs`: `AudioSession::{new, transcribe_blocking,
  speak_blocking, chat_turn, step, abort_handle, set_epoch,
  live_text_handle, heard_audio, is_voicing}`,
  `should_advance_chain` (**the** fallback classifier: advance on
  `NoAudio`/`Backend`/`Overflow`, terminal on `Aborted`),
  `audio_input_unsupported`; `S2sConfig`, `S2sError`.
- `src/ensure.rs`: `ensure_audio_server`,
  `boot_audio_server` (candidate walk via `first_accept_in_order`,
  per-attempt `audio_supervisor_config`, boxed supervisor on
  success, exact `every voice server candidate failed (last: …)`
  exhaustion), `AudioEndpoint::{Owned, …}`, per-endpoint
  served-model hint cache with fail-invalidate,
  `fetch_listed_model_ids` via the shared `model_ids` parser.
- `src/protocol.rs`: `encode_wav_pcm16` delegates to
  `common_core::pcm::encode_wav_mono_pcm16`; `audio_msg`,
  `chat_body`, `system_msg`, `text_msg`, `parse_sse_line`,
  `StreamEvent`.
- `src/voice.rs` / `src/worker.rs`: candidate resolution,
  `voice_server_args`, `audio_supervisor_config`,
  `managed_binary`, `resolve_tts_binary`, `synth_blocking`,
  `ScratchGuard`. Shares
  `orator_llm::{Supervisor, shared_http_client,
  LatencyHistogram}`; owns only audio argv/probe/stream shaping.
- `src/config.rs`: `S2sConfig` (`turn_window_ms`).

### `orator-daemon` — serving center

- `src/voice.rs`: `VoiceOwner` (registry `SessionId → Session`
  + `TurnMachine` + `StreamAbort` + activity; `EventLog` per
  turn), `VoiceError` (additive `kind`: `NoAudio`/`Overflow`/
  `Aborted`/`Backend`/`Saturated`/`Ejected`/`BootContention`),
  `boot` via `first_accept_in_order` (pinned vs resolved
  candidates before the ladder; per-attempt
  `audio_supervisor_config`; exhaustion maps to the exact
  `VoiceError::Boot` text), `start_session`/`stop_session`/
  `say_via`/`ask_via`/`toggle_dictation` (kind-scoped)/
  `shutdown`/`clear`/`reap_if_idle` (leases only, never outcome),
  `is_alive`, `RequestOutcome::{Ok, Backend}`.
- `src/admission.rs`: `AdmissionGate` (bound + depth guard, Drop
  releases), `EjectionTracker` (consecutive-`Backend` counts;
  responsive incl. voiceless turns re-enter), named `Saturated`
  refusal + counter.
- `src/socket.rs`: `serve_call` (transport-neutral dispatch),
  `route` + `reject_unknown_params`, `bridge_concurrent` (in-place
  serving bridge over the shared runtime), `bind` via
  `common_core::io::bind_unix_listener_with_stale_reclaim`
  (probe → unlink → bind, 3 reclaims, symlink refusal,
  `restrict` `0600`, nonblocking + Tokio conversion),
  `serve_serial` (test helper).
- `src/ws.rs`: `serve_ws` / `serve_ws_on` / `serve_ws_on_tuned`,
  `WsTuning::from_config`, `WS_PING_EVERY` (5 s),
  `WS_INACTIVITY_TIMEOUT` (10 s), `WS_CONNECTION_CAP` (360 s),
  handshake token gate, JoinSet drain, per-IP wait via
  `common_core::retry::capped_exp_backoff_ms`.
- `src/lib.rs`: `DISPATCH_METHODS`, `dispatch`,
  `contract_matches` (single-source verification).

### `orator-cli` — every user command

- `src/lib.rs`: `COMMANDS` (18 verbs), `Cli::{surface, methods}`,
  `OutboundRequest`, `ExitCode::{Success, Error, Usage,
  DaemonUnavailable, Interrupted}`.
- `src/fallback.rs`: `LOCAL_OP_BUDGET` (10 s),
  `budgeted` (thin wrapper over `run_with_budget` keeping the
  `orator: local …` refusal verbatim), `voice_unavailable` (8
  markers over `LoweredHaystack`), `fallback_if_unavailable`,
  `whisper_model_path`, `resolve_whisper_model` (heads →
  `default` → leaf, deduped), `resolve_asr_language` /
  `resolve_tts_voice` (composed params + leaf fallback),
  `ask_dial`/`say_dial`/`transcribe_dial`, `local_transcribe`/
  `local_say`/`local_ask`.
- `src/ask.rs`: `resolve_ask_model` (`roles.chat` head, else
  `llm.model` leaf — never top-level `default`),
  `effective_llm_config` (resolved head as model, composed
  `ctx`/`n_batch` only when tables specify them under
  leaf-minimum validation, table weights when declared),
  `verify_pinned_ask_weights`, `parse_ask_port`,
  `check_ask_weights`, `speak_failure_note`.
- `src/chat.rs`: `asr_serving`/`tts_serving`/`llm_serving`
  (labels follow the same head/leaf resolution),
  `resolve_turn_path` (native iff preference AND assurance),
  slash parsers.
- `src/doctor.rs`: `full_report`/`render_full`, `roles_lines`
  (effective model + backends + weights + composed params per
  role, `chat` line included, `role head`/`role default` plus
  ignored-leaf notes), `speech_lines` (`session model:`),
  `lfm_readiness_lines`, injection/shortcut/desktop sections.
- `src/config.rs`: `config_get`/`set`/`keys` over the root
  `FieldAccess` handle (one family at a time; `models`/`roles`/
  `default` round-trip as whole JSON; `config_keys` order stays
  hand-built); serving selectors are typed field reads.
- `src/main.rs`: `ask` (probe-then-boot, `boot_supervised` with
  fail-fast port/weights, `AskFail::Usage/Error`, one-question
  ownership), `say`/`say_chain` (normalize once, daemon last
  resort speaks only the unsynthesized suffix),
  `say_audio_server`/`run_audio_turn`, `daemon_passthrough`,
  `chat` (order-preserving overlap, `pending_audio ≤ 1`),
  `dictate_once`, `transcribe`/`dictate_whisper`/`stt_backend`,
  `config`, `models`, `doctor`, `status`/`stats`, `listen`,
  `meeting`, `approve`, `daemon`, `tui`,
  shell-init/completions.
- `src/ask_cmd.rs`: I/O only (`effective_llm_config` does the
  pure composition); `src/transcribe.rs`:
  `cue_from_centiseconds`, `audio_server_cue`, `render`,
  `TranscriptFormat::{Srt, Vtt, Txt, Json}`.
- `src/dictate.rs`: `validate_seconds`, `checked_deadline`,
  `reserve_for`, `capture_choice`, `finish_turn`
  (refine → record → inject), `DEFAULT`/`MAX_DICTATE_SECS`,
  `MAX_DICTATE_SAMPLES`.
- `src/tui.rs`: `TuiApp`, `drain_capture` (16 kHz remainder
  domain), `SttWorker` (thread-affine whisper reuse),
  `WhisperCache`/`KokoroCache`, `TranscribeJob`,
  `MAX_PENDING_UTTERANCE`, `SCROLLBACK`, `MicGate` (shared RMS),
  `voice_reply`.
- `src/socket.rs`: `daemon_call_at`/`call_transport`,
  `ws_timeout_text` vs `refused_text` (distinct),
  `map_connect_error`, `permission_denied_text`.
- `src/stream.rs`: `stream_chunks` (`sync_channel(2)`,
  cancel-on-play-error); `src/say_chain.rs`: `should_advance`
  (composes the s2s classifier); `src/shell.rs`,
  `src/models.rs`, `src/stats.rs`, `src/progress.rs`,
  `src/signal.rs`.

### `orator-desktop` — desktop integration

- `src/bind.rs`: `probe_hotkeys`/`probe_tray`/`probe_notify`/
  `probe_dbus`/`probe_krunner` each via
  `first_some_in_order_sync` (order and route values pinned;
  fallbacks `HotkeyRoute::Fallback`, `TrayRoute::Headless`,
  `NotifyRoute::CliFallback`, `DbusRoute::Absent`,
  `KrunnerRoute::ParserOnly`).
- `src/refine.rs`: `refine_transcript` (`[Llm, Raw]` via the same
  helper, raw defensive fallback, `RefineSource`,
  `save_history` on every path), `DEFAULT_REFINE_PROMPT`,
  `wrap_transcript`.
- `src/ladder.rs`: `inject`, `Rung::ORDER`,
  `LadderMemory::{last_working, last_inject}` (per-handle;
  `last_inject` drives 10 s continuation spacing),
  `InjectOutcome::{Injected, OutputOnly}`,
  `InjectError::{Failed, Fatal, …}`.
- `src/clipboard.rs`: `paste_with_restore` (180 ms),
  `capture_selection` (200 ms), 10 ms change polls,
  `bounded_wait` (2000 ms child deadline, kill/reap/restore),
  restore-or-clear, own-write fingerprint ordering;
  `Clipboard`/`ChordSender` traits, `ExternalClipboard`,
  `InMemoryClipboard`, `NoChord`.
- `src/continuation.rs`: `continuation_prefix`,
  `CONTINUATION_WINDOW` (10 s); `src/hotkey.rs`:
  `parse_chord`, `fallback_bindings`; `src/krunner.rs`:
  `parse_query`; `src/tray.rs`, `src/notify.rs`, `src/dbus.rs`.
  Wiring status: `doc/orator/DESKTOP_WIRING.md`.

### `orator-turn` / `orator-vad` — conversation policy

- `turn/src/machine.rs`: `TurnMachine` via
  `delegate_component_fields!(TurnMachine, config)`,
  `TurnState` (`Idle`/`UserSpeaking`/`Endpointing`/`Thinking`/
  `ToolWait`/`Speaking`/`Yielding`), `TurnEvent`; legality drops
  counted, never errors.
- `turn/src/driver.rs`: `TurnDriver::{with_config, detect}`,
  `HopGate::{pass, cut}`, `Hop::{PostStt, PreLlm, PreSpeak}`
  (three-hop generation cancel), `BargeInOutcome`.
- `vad/src/energy.rs`: `EnergyVad::{with_floor, push}` (RMS via
  `common_core::pcm::root_mean_square_f32`); floor/hysteresis/
  attack/hangover constants (calibrated bands in
  `CALIBRATION.md`).
- `vad/src/gate.rs`: `speech_present_16k` (energy fraction AND
  NOT stationary tone — the single home for file and daemon
  gating), `stationary_tone_present`,
  `energy_gate` (shared RMS, 20 ms frames, re-arm cadence).

### `orator-stt` / `orator-tts` — engines

- `stt/src/whisper.rs`: `WhisperBackend::{new, with_gpu,
  decode_blocking}` (lazy context, language/model-keyed
  rebuild); `stt/src/streaming.rs`: `StreamingStt`
  (utterance-absolute incremental commits, tail-only window),
  `BlockDecoder::decode` (+ `offset_ms`);
  `stt/src/moonshine.rs`: `MoonshineBackend` (streaming ASR
  backing the default ASR tag); `stt/src/wer.rs`.
- `tts/src/kokoro.rs`: `KokoroBackend::{new, with_gpu,
  synth_blocking, synth_async}`, `trim_silence`,
  `uniform_marks`, `KOKORO_SAMPLE_RATE`, load-once session;
  `tts/src/splitter.rs`: `split_all` (MIN=16, FIRST_MAX=100,
  CAP=220, comma fast-path, buffer-end hold, abbreviation
  guard); `tts/src/normalize.rs`
  (`normalize_for_speech` — the oracle single home, composed by
  `say_chain`); `tts/src/pipeline.rs`
  (`synth_chunks_blocking`); `default_paths`.

### `orator-audio` — sound in/out/on disk

- `src/backend.rs`: `CpalBackend`, `open_default_output`
  (shared device opener), `collapse_to_mono` via
  `common_core::pcm::extend_mono_from_interleaved` (reused
  `mono` vector, partial-frame drop counter, real-time
  no-lock/no-await).
- `src/wav.rs`: `read_wav_mono`, `decode_wav_bytes` (both
  decoders via the shared interleaved helper — no intermediate
  `Vec`), `encode_wav_mono` via
  `common_core::pcm::encode_wav_mono_pcm16`
  (`Symmetric32767` + `TowardZero`), `WavPcm` (parser home).
- `src/pcm.rs` family composes `common_core::pcm`
  (`Scale16::{Symmetric32767, Symmetric32768}`,
  `Rounding::{Nearest, TowardZero}`, `encode_i16_le`,
  base64 pair with verbatim `bad base64: ` prefix).
- `src/envelope.rs`: re-exports and delegates to
  `common_core::envelope` (`DUCK_DEPTH_DB`/`DUCK_RAMP_MS`/
  `CUT_FADE_MS`, `db_to_linear`, `duck_gain_at`,
  `apply_fade_out`); `turn/src/driver.rs` consts are aliases of
  the same home.
- `src/playback.rs`: `play_blocking`,
  `StreamPlayer::{open, push, dropped, pushed, played, finish}`
  (`MAX_QUEUE_SAMPLES` = 480 000, drop-oldest + counter,
  fallible finish).
- `src/jitter.rs`: `JitterBuffer::{for_20ms_frames, push, pop}`
  (80 ms cap, drop-oldest-to-target, floor growth);
  `src/resample.rs`: `resample_linear`; `src/merge.rs`:
  `MergeQueue` (MediaTime-ordered mic/sys/reply);
  `src/codec.rs`: `Codec::{encode_step, decode_step, reset}`,
  `PcmCodec` (supported path; neural adoption deferred with a
  recorded revisit rule); `src/archive.rs`
  (`CreditedFeedWorker`, `feed_config` 64/32/4, batch 4,
  25 ms), `src/source.rs` (already composes `retry_async`),
  `src/doctor.rs`, `src/error.rs`.

### `orator-lfm` — LFM3-Audio reference knowledge (no tensors)

Config/processing/layout home for
`Lfm3AudioForConditionalGeneration`: architecture constants,
special tokens, chat-template assembly, generation defaults, and
weight-layout verification. Owns no tensor math (no
candle/safetensors/RoPE/KV-cache; `ort` cannot express hybrid
shortconv transformers; the local `llama-server` has no `lfm3`
backend), so inference stays with a future LFM-capable server
composed over these definitions.

- `src/arch.rs`: `AUDIO_START_TOKEN_ID`, `AUDIO_TOKEN_ID`,
  `AUDIO_OUT_TOKEN_ID`, `MIXED_START_TOKEN_ID`,
  `AUDIO_EOS_CODE`, `BOS`/`EOS`/`PAD`/`TEXT_END` ids,
  `AUDIO_VOCAB_SIZE`, `CODEBOOKS`, `INTERLEAVED_N_AUDIO`/
  `INTERLEAVED_N_TEXT`, `N_MELS`, `SAMPLE_RATE_IN`/`OUT`,
  `MASKGIT_STEPS`.
- `src/template.rs`: `assemble_prompt` (`replacen` at the
  `<audio_out>` marker — in-place expansion as the reference
  does; clip-without-marker and marker-without-clip refuse as
  `OrphanAudio`), `AudioClip`/`AudioOutClip`/`TextTurn`.
- `src/params.rs`: `GenerationDefaults`, `TextGen`, `AudioGen`
  (anchored to `generation_config.json` + `config.json`;
  floats within one `f32` ulp).
- `src/manifest.rs`: `LfmLayout`/`LfmWeights`,
  `verify_weights`; fixtures
  `tests/fixtures/reference_expansion.json` (160→1 frame/1
  token, 16000→99/13, 48000→299/38; 24000-sample codec
  13 frames → 14 placeholders incl. EOS 2016) plus
  `probe_reference.py` re-recording the reference on a CPU
  venv. Hermetic goldens pin all six vectors byte-for-byte;
  three `live-ai` tests re-run the probe (staleness guard),
  re-verify assembly, and anchor generation params.

### `orator-tools` — agency with guardrails

- `src/tool.rs`: `Tool` trait, `ToolCall`/`ToolRegistry`,
  `truncate_tool_output` + `TOOL_OUTPUT_CHARS` (single home),
  `ToolTier`.
- `src/loop_.rs`: `run_turn`, `bound_prompt` via
  `fluent_llm::tokens::truncate_middle` (5 rounds + final
  tool-less pass under `TokenBudget`, `MAX_PROMPT_TOKENS`
  = 8192, `MAX_TOOL_ROUNDS`), `last_prompt_for_tests`.
- `src/sandbox.rs`: `Guarded::{run}`, `GuardedConfig`,
  `shell_escape` (cap-carrying `sh -c`), `kill_program`
  (process-group kill + `kill_on_drop` backstop),
  `open_within`, `within`/`real_dir` (via shared
  `normalize_std_path`, hop cap `MAX_SYMLINK_HOPS` from
  `common_core::path`; TTL+mtime memo), `ToolAudit`
  rotation; `src/policy.rs`:
  `Policy`/`PolicyCtx`/`CallOrigin`; `src/approve.rs`:
  `ApprovalHub::{wait}` (drains on timeout/cancel/drop),
  `ApprovalId`.
- `src/parse.rs`: `gbnf_for_tools`, `parse_model_output`,
  `parse_xml` (+ `ParseError`); GBNF primary + XML fallback
  with an identical-calls golden. `src/untrusted.rs`:
  `wrap_untrusted`, `wrap_tool_result`,
  `TRANSCRIPT_AS_DATA`. `src/builtins.rs`:
  `default_registry`, `FETCH_CHARS`. Wording scans compose
  `common_core::string` without merging the two classifier
  tables (their disagreements are pinned).

### `orator-store` — one event log, thin views

- `src/events.rs`: `EventLog::{over, in_memory, append,
  replay, replay_kind, count_kind, latest, subscribe,
  publish_ephemeral}`, `StoredEvent`, `REPLAY_PAGE` (5 000),
  `broadcast::channel(10)` fan-out with Lagged-skip, single
  producer; `writer_feed_config` (256/64/8, serial, 100 ms —
  documented against the archive's 64/32/4: lossless archive
  vs serial writer, both compose `CreditedFeedWorker`).
- `src/schema.rs`: `migrations`, `CURRENT_VERSION`;
  `src/store.rs`: `open_store` (backup-before-migrate),
  `InstanceLock::acquire` (via the shared reclaim helper:
  runtime-dir `0700` enforcement, symlink checks, no added
  socket chmod), `spawn_writer`, `WriteReq`.

### `orator-diar` / `orator-notes` — meetings into knowledge

- `diar`: `segment`, `align_words`, `speaker_name`,
  `MeetingPipeline` (finalize → offline_stt → diarize →
  align → name → summarize → export over
  `CheckpointedStepGraph` with persisted checkpoints and
  `rewind_to`).
- `diar/src/embed.rs`: `batch_embed` composes
  `ResultPool::new` + `try_join_all` (indexed jobs, input-order
  results, `parallel.max(1)`, short-circuit on first error;
  `Pool` keeps the pool-refusal text, `Canceled` keeps the
  empty-worker text).
- `notes`: `roll_summary` (keeps shared `TokenBudget`;
  prompt bounding via the shared `truncate_middle` splitter —
  markers stay at the call site), `append_fact`, map-reduce
  summarizer (`chunk_document` + `ResultPool(2)`) with
  mandatory segment citations (dangling citations reject),
  Markdown facts + rolling summary, deterministic export, FTS
  search over the store.

### `orator-models` / `orator-bench` / `orator-testkit`

- `models/src/ops.rs`: `list_models`, `find_entry`,
  `verify_entry` (size-then-SHA-256 via
  `common_core::hash::verify_sha256_file`; `Match` → `true`,
  `SizeMismatch`/`HashMismatch` → `false`, I/O → `Io`;
  audio bundles handled separately), `pull_plan`;
  `models/src/model.rs`: `ModelEntry`, `vad_entries`;
  `models/src/residency.rs` (eviction + voice-lease holds).
  CLI `verify_report` fails unpinned/missing loudly.
- `bench/src/`: `scenarios`, `gates` (anti-tautology: a 10×
  slowed fake must fail), `replay::run_scenario`,
  `stats`/`events` (`InputPcm`/`Step`/`StepPostSampling`/
  `SendPcm` to `--stat-file`).
- `testkit/src/fakes.rs`: `FakeClock`, `ScriptStep`,
  `ScriptScenario::{clean, faults_at}`, `SlowFake::scale`.

## 5. Cross-cutting contracts

- **Wire** (`orator-core::protocol`, `orator-core::socket`,
  `orator-daemon::socket`, `orator-cli::socket`): 11 methods;
  `COMMANDS` (18 CLI verbs) ↔ `METHOD_NAMES` ↔
  `DISPATCH_METHODS` pinned equal by `it_parity.rs`;
  `OutboundRequest::new` refuses unknown/ignored params.
- **Errors**: `ExitCode` (CLI), `S2sError` (adapter),
  `VoiceError` (daemon, additive `kind` bodies),
  `DispatchError::NotImplemented`, `FrameError`,
  `AudioError`, `LlmError::is_retryable`.
- **Config** (`OratorConfig`, hierarchical `orator.json`,
  `config get/set/path`): GPU off / WS off / bounds calibrated
  by default; old files load calibrated defaults (missing
  `roles.chat` loads today's chat behavior); garbage values are
  `Usage` errors. Precedence is role run-block/model-override
  composition first, root leaf fallback, code default last —
  see §3. `config get models|roles|default` round-trips the
  bare-map shape; `config set` accepts exactly that shape.
- **Shutdown**: global `shutdown_signal`, per-session
  `StreamAbort` + `SignalWatch`, `VoiceOwner::shutdown`
  (bounded in-flight wait), `serve` JoinSet drains,
  `Supervisor::stop` + drop-guard scratch files.
- **Goldens that guard the whole**: `it_end_to_end.rs`
  (cascade, live barge-in conversation, no-false-success
  vectors, config-honored, composed bounds),
  `it_parity.rs` (surface/contract equality),
  `it_source_guard.rs` (no plan words, section-ID scan,
  `forbid(unsafe_code)` roots, manifest edges),
  `it_doc_guard.rs` (spec names resolve). Calibration goldens
  go through `common_core::calibration` (`passes_gate`:
  precision ≥ 0.90, FPR ≤ 0.05); changed golden bytes are a
  regression — never update a golden to make a refactor pass.

## 6. Gates (how to verify a change)

Narrowest gate that answers the question, once:

- Per change: `make orator-smoke` (all-target type-check plus
  unit tests). Public types/signatures changed: add
  `make orator-lint`. Touching a shared crate: add that
  component's gate (`make common-core-test`,
  `make fluent-wvr-test`, `make fluent-concurrency-test`,
  `make test-llm`).
- Finished work: `make orator-test` (full hermetic suite),
  then one repo-wide `make smoke` at workflow end.
- Never run repo-wide `make test` / `make soak` to check a
  component; the user runs the full suite before a commit.
  Heavy timing/concurrency work alone may use
  `make orator-soak` (plus `make fluent-concurrency-soak`
  for pool/queue changes); live-model checks alone use
  `make orator-test-live` (`#[ignore]` + `live-ai`, skips
  cleanly without env).
- Gate logs land in
  `/tmp/opencode/monorepo-gates/<target>.log` — grep/tail the
  log on failure, never re-run a gate just to see output.
  For gate terminology see `doc/router/TESTING.md`.
