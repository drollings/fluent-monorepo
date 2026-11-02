# Orator Red-Team Findings

*Evidence capture for the remediation work recorded in
`ROADMAP_20261101_ORATOR_REDTEAM.md`. One row per finding: where it is, which
axis it measures (producer confidence vs task-value/correctness), and which
remediation step fixes it. Axis vocabulary follows the roadmap's "Confidence vs
task-value" section.*

## Class 1 — Dishonest contracts (false success)

| # | Finding | File / function | Axis | Fixed by |
|---|---------|-----------------|------|----------|
| F1 | `dispatch` answers `Ok(())` for every contract method (`StartSession`, `StopSession`, `Interrupt`, `Say`, `Ask`, `ToggleDictation`, `ListSessions`, `GetState`) although none of them does any work | `src/orator-daemon/src/lib.rs::dispatch` | Producer confidence (daemon reports success) | R1 |
| F2 | `route` turns that `Ok` into `ok: true` with an empty body, so every stub call looks like a completed call on the wire | `src/orator-daemon/src/socket.rs::route` | Producer confidence (`ok: true` for a no-op) | R1 |
| F3 | `orator say --backend daemon` ends in the daemon stub and exits 0; no audio is ever spoken | `src/orator-cli/src/main.rs::say` → `daemon_passthrough("Say")` | Task-value (audio actually produced) | R1 |
| F4 | `ask --speak` discards the `Say` result (`let _ = daemon_call("Say", …)`), so a refused or impossible speak step still exits 0 | `src/orator-cli/src/main.rs::ask` | Task-value | R1 |
| F5 | `daemon stop` / `daemon reload` forward `GetState` with a `params.action` key — the daemon never stops or reloads; the CLI reports success | `src/orator-cli/src/main.rs::daemon_lifecycle` | Task-value (stop actually stops) | R1 |
| F6 | `listen`, `ptt start/stop` and `meeting start/stop/…` forward to stub methods (`ListSessions`, `StartSession`, `StopSession`) and print success | `src/orator-cli/src/main.rs::{listen, ptt, meeting}` | Task-value (session actually started) | R1 |
| F7 | `DISPATCH_METHODS` is a second hand-maintained copy of `METHOD_NAMES`; two tables can drift | `src/orator-daemon/src/lib.rs::DISPATCH_METHODS` vs `src/orator-core/src/protocol.rs::METHOD_NAMES` | DRY (single source of truth) | R1 |
| F8 | `config get` / `config set` are stubs: they never read or write the layered JSON file | `src/orator-cli/src/main.rs::config` | Task-value (config actually read/written) | R4 |
| F9 | `models verify` skips missing and unpinned entries silently: exit 0, no output, so an unverified model looks verified | `src/orator-cli/src/main.rs::models` (`ModelsAction::Verify`) | Confidence (`pinned` flag) vs task-value (hash matches) | R9 |

## Class 2 — Resource and security defects

| # | Finding | File / function | Axis | Fixed by |
|---|---------|-----------------|------|----------|
| F10 | `socket_path` falls back to `/tmp` (world-writable, predictable) when `XDG_RUNTIME_DIR` and `HOME` are both unset — a hijackable socket path | `src/orator-core/src/socket.rs::socket_path` | Security invariant | R2 |
| F11 | `bind` ignores `set_permissions` failures on the parent directory and never validates that the parent is owned by the daemon uid with mode `0700` | `src/orator-daemon/src/socket.rs::{bind, restrict}` | Security invariant | R2 |
| F12 | `serve` re-reads the mutable socket path's owner once per connection instead of caching the uid at bind time | `src/orator-daemon/src/socket.rs::serve` (`own_uid_of(path)` in the accept loop) | Security invariant (fail-closed gate input) | R2 |
| F13 | No concurrency cap and no read/write timeouts on accepted connections; a peer can spawn unbounded blocking tasks and stall them forever | `src/orator-daemon/src/socket.rs::serve` | Resource (bounded work) | R2 |
| F14 | The 5-byte frame header is parsed by hand twice (CLI reply path and daemon request path) on top of `SocketFrame::decode` | `src/orator-cli/src/socket.rs::read_reply`, `src/orator-daemon/src/socket.rs::read_request` | DRY | R2 |
| F15 | `main` calls `server.abort()` before `voice.shutdown()` and never drains in-flight handlers; the accept loop has no graceful path | `src/orator-daemon/src/main.rs::main` | Task-value (no leaked children/handlers) | R3 |
| F16 | `VoiceOwner` has no shutting-down flag: a boot in flight during `shutdown()` publishes its server after `shutdown()` returned, leaking the child | `src/orator-daemon/src/voice.rs::{ensure, boot, shutdown}` | Leak | R3 |
| F17 | `VoiceServer::release` calls `start_kill()` but never waits: the child is not reaped (zombie) on the release path | `src/orator-s2s/src/voice.rs::VoiceServer::release` | Leak | R3 (moves to `Supervisor::stop_child` in R5) |
| F18 | The CLI installs no SIGINT/SIGTERM handler; Ctrl-C during `say`/`ask`/`chat`/`run_audio_turn` leaves spawned supervisors and voice servers running | `src/orator-cli/src/main.rs::{run_audio_turn, say_fallback, ask, chat}` | Leak | R3 |
| F19 | `Frame::new` panics (`expect("rate checked nonzero")`) when a device reports sample rate 0 | `src/orator-core/src/types.rs::Frame::new` | Panic safety (audio path) | R11 |
| F20 | `audio_server_home` falls back to `/tmp`; `resolve_audio_server` accepts any `is_file()` binary with no owner/world-writable validation and duplicates the `$PATH` loop | `src/orator-s2s/src/voice.rs::{audio_server_home, resolve_audio_server}` | Security invariant (trusted binary) | R10 |
| F21 | `models_dir`/`config_dir` return a **relative** path when `HOME` is unset (cwd-dependent behavior) | `src/orator-core/src/config.rs::{models_dir, config_dir}` | Security/path invariant | R10 |
| F22 | Readiness/fallback heuristics are uncalibrated: `OPTIONS` 405 handling, the `audio_input_unsupported` string match, and the `reap_if_idle` timer are all confidence signals driving persistent decisions | `src/orator-s2s/src/voice.rs::wait_ready`, `src/orator-daemon/src/voice.rs::{is_alive, reap_if_idle}`, `src/orator-cli/src/main.rs::audio_input_unsupported` | Confidence vs task-value | R13 |

## Class 3 — Bloat and duplication

| # | Finding | File / function | Axis | Fixed by |
|---|---------|-----------------|------|----------|
| F23 | `VoiceServer` is a second process supervisor (spawn/kill/wait/ready) next to `orator_llm::Supervisor` | `src/orator-s2s/src/voice.rs::VoiceServer` | DRY (one supervisor) | R5 |
| F24 | Six per-call `reqwest::Client::builder()` constructions instead of the shared pooled client | `src/orator-daemon/src/voice.rs::is_alive`, `src/orator-s2s/src/voice.rs::wait_ready`, `src/orator-s2s/src/ensure.rs::serves_model`, `src/orator-s2s/src/session.rs` (session client), `src/orator-llm/src/supervisor.rs::probe_health`, `src/orator-tools/src/builtins.rs` | DRY (one transport) | R6 |
| F25 | `/models` then `/v1/models` parsing is implemented twice (`serves_model` vs `served_models_hint`) | `src/orator-s2s/src/ensure.rs::serves_model`, `src/orator-s2s/src/session.rs::served_models_hint` | DRY | R6 |
| F26 | `stream_turn` buffers the whole response (`response.bytes()`) before parsing — unbounded memory, no mid-stream cancellation | `src/orator-s2s/src/session.rs::stream_turn` | Bounded memory | R7 |
| F27 | The CLI reimplements cpal output-stream playback that `orator-audio` already owns | `src/orator-cli/src/main.rs::play_f32_blocking` | DRY | R8 |
| F28 | `audio_server_home`'s `$HOME` fallback, `models_dir`'s fallback and `config_dir`'s fallback are three separate home resolvers (one of them `/tmp`, one of them relative) | `src/orator-s2s/src/voice.rs`, `src/orator-core/src/config.rs` | DRY | R10 |
| F29 | `orator-models` (a catalog crate) depends on the runtime crate `orator-s2s`, dragging in `whisper-rs`, `reqwest`, `tokio` transitively | `src/orator-models/Cargo.toml` | Layering / DRY | R12 |
| F30 | `read_wav_mono`/`WavPcm` live in `orator-stt` but are consumed by `orator-s2s` (and the CLI), forcing the `orator-stt`/`whisper-rs` edge into `orator-s2s` | `src/orator-stt/src/wav.rs`, callers in `src/orator-s2s/src/worker.rs`, `src/orator-cli/src/main.rs` | DRY | R12 |
| F31 | The configuration file is loaded only by tests; every command constructs `TtsConfig::default()` / `SttConfig::default()` / `LlmEndpointConfig::default()` at the call site | `src/orator-cli/src/main.rs::{say, stt_backend, transcribe, dictate_once, ask, audio_base}` | Configuration honored | R4 |
| F32 | Source comments reference roadmaps/milestones/phases, which workspace rules forbid | `src/orator-cli/src/lib.rs:25,106`, `src/orator-cli/src/main.rs:32`, `src/orator-tools/src/builtins.rs:3` | Rule violation | R15 |
| F33 | `src/orator-s2s/src/lib.rs` is the only crate root missing `#![forbid(unsafe_code)]` (19 of 20 roots carry it) | `src/orator-s2s/src/lib.rs` | Safety invariant | R15 |

## Baseline gate record (R0.3)

| Gate | Result | Log |
|------|--------|-----|
| `make orator-smoke` | SUCCEEDED — compiled; 338 passed, 0 failed, 2 ignored (20 binaries) | `/tmp/opencode/monorepo-gates/orator-smoke.build.log`, `/tmp/opencode/monorepo-gates/orator-smoke.unit.log` |
| `make orator-lint` | SUCCEEDED — clippy clean | `/tmp/opencode/monorepo-gates/orator-lint.log` |
| `make orator-test` | SUCCEEDED — 345 passed, 0 failed, 2 ignored (41 binaries) | `/tmp/opencode/monorepo-gates/orator-test.log` |

No source remediation was started to obtain this baseline; all three gates were
green on the tree as found.
