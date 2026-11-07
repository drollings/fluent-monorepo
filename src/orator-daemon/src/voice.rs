//! Daemon-owned voice server: one long-lived supervised child
//! shared by every CLI call.
//!
//! Per-command boots pay a full weight load per utterance; the
//! daemon instead boots once (lazily, on the first voice request),
//! hands out the endpoint URL, reaps the child after an idle
//! budget, and reboots on demand. Concurrent first callers
//! serialize on one boot — never two servers. The owner never
//! holds its lock across an await: boot and liveness probes run
//! outside the critical section with a re-check under it.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use orator_core::LlmSession as _;

use crate::admission::{AdmissionGate, EjectionTracker, EJECTION_THRESHOLD};

/// Idle budget before the child is reaped (matches the gateway's
/// sleep-idle shape: minutes, not seconds).
pub const VOICE_IDLE_TIMEOUT: Duration = Duration::from_mins(15);

/// Single-shot transcribe input bound: 1.92M f32 samples (120 s at
/// 16 kHz, the TUI utterance bound) as i16 base64 (~5.1 MiB) fits the
/// 8 MiB frame cap. Longer utterances refuse with `Overflow` instead
/// of desyncing the frame.
pub const MAX_TRANSCRIBE_SAMPLES: usize = 1_920_000;

/// Failures serving the voice endpoint. Every variant names the
/// remedy for the CLI to carry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VoiceError {
    /// Voice serving is disabled in this context (tests, dry runs).
    #[error("voice server disabled here")]
    Disabled,
    /// The owner is shutting down; no new boots start.
    #[error("voice server shutting down")]
    ShuttingDown,
    /// No server binary resolves.
    #[error("no voice server binary: {0}")]
    NoBinary(String),
    /// Auto-boot is off and no server answers: start one instead of
    /// spawning a child.
    #[error("voice auto-boot is off: {0}")]
    AutoBootDisabled(String),
    /// The boot failed.
    #[error("voice server boot failed: {0}")]
    Boot(String),
    /// A concurrent boot is in progress. Retry shortly; the first
    /// caller owns the boot, the rest wait on this reply instead of
    /// holding a handler for the whole boot.
    #[error("voice server boot in progress — retry shortly (concurrent boot)")]
    BootContention,
    /// A turn failed against its endpoint.
    #[error("voice turn failed: {0}")]
    Backend(String),
    /// A turn arrived past the concurrency bound. Retry when a turn
    /// completes; raise `daemon.admission_max` where the server
    /// parallelizes.
    #[error("admission saturated: {0}")]
    Saturated(String),
    /// The endpoint ejected after consecutive transport failures. The
    /// next ensure re-probes instead of booting; no boot runs while
    /// ejected.
    #[error("voice endpoint ejected: {0}")]
    Ejected(String),
    /// The server took the turn but voiced nothing. Responsive, so it
    /// breaks the ejection streak without ejecting; the caller falls
    /// through to the next voice in its chain.
    #[error("server sent no audio: {0}")]
    NoAudio(String),
    /// The streamed audio passed its bound. Server misbehavior, so it
    /// counts toward ejection like a transport failure.
    #[error("voice turn exceeded its bound: {0}")]
    Overflow(String),
    /// The turn was cancelled. Terminal; never advances the fallback
    /// chain and never counts toward ejection.
    #[error("voice turn aborted: {0}")]
    Aborted(String),
    /// A peer was too slow to complete its read or write inside the
    /// handler budget. Transport-level, never a capacity signal: the
    /// caller may retry. Counted separately from saturation.
    #[error("slow peer: {0}")]
    SlowPeer(String),
    /// No such session.
    #[error("unknown session: {0}")]
    UnknownSession(String),
}

impl VoiceError {
    /// Stable wire kind preserved across the daemon boundary. The
    /// socket envelope carries this alongside the human line so the
    /// CLI can route without string matching; existing display
    /// strings stay unchanged.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Disabled => "Disabled",
            Self::ShuttingDown => "ShuttingDown",
            Self::NoBinary(_) => "NoBinary",
            Self::AutoBootDisabled(_) => "AutoBootDisabled",
            Self::Boot(_) => "Boot",
            Self::BootContention => "BootContention",
            Self::Backend(_) => "Backend",
            Self::Saturated(_) => "Saturated",
            Self::Ejected(_) => "Ejected",
            Self::NoAudio(_) => "NoAudio",
            Self::Overflow(_) => "Overflow",
            Self::Aborted(_) => "Aborted",
            Self::SlowPeer(_) => "SlowPeer",
            Self::UnknownSession(_) => "UnknownSession",
        }
    }
}

struct VoiceState {
    server: Option<orator_llm::Supervisor>,
    last_used: Instant,
    booting: bool,
    /// Task-value outcome of the last voice request: a real turn
    /// served (`Ok`), a transport failure (`Backend`), or nothing yet.
    /// `NoAudio` never lands here — the daemon serves the endpoint,
    /// the caller classifies voiceless turns. The idle reap only runs
    /// on `None`/`Ok`: a failing server waits for the next use (which
    /// clears it outright) instead of churning on the timer.
    last_outcome: Option<RequestOutcome>,
    /// Active endpoint leases. No lease protocol exists yet, so this
    /// stays zero and the reap always passes this leg; it reserves
    /// the task-value "in use" signal against the timer.
    leases: u32,
    /// Endpoint ejection tracker: consecutive transport failures eject,
    /// any responsive turn re-enters. The threshold is calibrated, not
    /// tuned — see the admission module.
    ejection: EjectionTracker,
    /// Last URL that served a turn: the ejection probe target.
    last_url: Option<String>,
}

impl VoiceState {
    /// Fresh serving state.
    fn fresh() -> Self {
        Self {
            server: None,
            last_used: Instant::now(),
            booting: false,
            last_outcome: None,
            leases: 0,
            ejection: EjectionTracker::new(EJECTION_THRESHOLD),
            last_url: None,
        }
    }
}

/// Task-value outcome of one voice request, for the reap gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestOutcome {
    /// A turn was served through the endpoint.
    Ok,
    /// The endpoint failed the request.
    Backend,
}

/// Kept turns per session for conversational context. Past this the
/// oldest turns drop; the context builder additionally caps chars so
/// long sessions hold a ceiling and the newest question always fits.
const MAX_SESSION_TURNS: usize = 20;

/// Context chars past which oldest turns drop (newest question is
/// never cut to fit history).
const MAX_HISTORY_CHARS: usize = 6_000;

/// One daemon-owned voice server. `binary` pins the server binary
/// (`None` resolves at boot); `disabled` never boots (tests).
pub struct VoiceOwner {
    binary: Option<std::path::PathBuf>,
    idle_timeout: Duration,
    disabled: bool,
    shutting_down: std::sync::atomic::AtomicBool,
    /// The daemon's loaded configuration, kept for the session factory
    /// that serves the remaining contract methods: it reads the same
    /// leaves the CLI honors. The voice path itself still uses the
    /// compiled-in bundle, binary resolution and idle budget — the
    /// schema carries no voice-server leaves, so there is nothing
    /// configured to consult yet.
    config: Box<orator_core::OratorConfig>,
    /// Bounded concurrency gate: turns past the configured bound
    /// refuse fast instead of queueing. Composes the admission module
    /// (the harness scores its bound); the owner never reimplements it.
    /// Visible inside the crate so the harness can pre-saturate it.
    pub(crate) gate: AdmissionGate,
    state: Mutex<VoiceState>,
    sessions: Mutex<crate::session::SessionRegistry>,
    log: std::sync::Arc<orator_store::EventLog>,
    /// Pending human approvals. The socket route answers through this
    /// hub; the D-Bus edge gates on `Caller::may_approve` before ever
    /// dialing `Approve`, so ambient speech can never approve.
    approvals: orator_tools::ApprovalHub,
    /// Conversational context per session: past (question, reply)
    /// turns, oldest first. `ask_via` with a session id voices the
    /// joined history plus the new question, so voice and text chat
    /// build context turn by turn; `stop_session` drops the entry.
    /// History is text only (transcripts + replies), bounded by
    /// [`MAX_SESSION_TURNS`] and [`MAX_HISTORY_CHARS`].
    llm_history: Mutex<std::collections::HashMap<String, Vec<(String, String)>>>,
}

impl VoiceOwner {
    /// Owner that boots on demand with the standard resolution, using
    /// the daemon's loaded configuration for the session factory.
    #[must_use]
    pub fn new(config: &orator_core::OratorConfig) -> Self {
        Self {
            binary: None,
            idle_timeout: VOICE_IDLE_TIMEOUT,
            disabled: false,
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            config: Box::new(config.clone()),
            gate: AdmissionGate::new(usize::from(config.daemon.admission_max)),
            state: Mutex::new(VoiceState::fresh()),
            sessions: Mutex::new(crate::session::SessionRegistry::new()),
            log: memory_log(),
            approvals: orator_tools::ApprovalHub::new(),
            llm_history: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Owner that never boots. Test and dry-run shape: endpoint
    /// calls fail closed with the remedy named.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            binary: None,
            idle_timeout: VOICE_IDLE_TIMEOUT,
            disabled: true,
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            config: Box::new(orator_core::OratorConfig::default()),
            gate: AdmissionGate::new(default_max_inflight()),
            state: Mutex::new(VoiceState::fresh()),
            sessions: Mutex::new(crate::session::SessionRegistry::new()),
            log: memory_log(),
            approvals: orator_tools::ApprovalHub::new(),
            llm_history: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Owner with an explicit binary and idle budget. Test seam:
    /// `/nonexistent` binaries fail fast without loading weights.
    #[must_use]
    pub fn with_binary(binary: std::path::PathBuf, idle_timeout: Duration) -> Self {
        Self {
            binary: Some(binary),
            idle_timeout,
            disabled: false,
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            config: Box::new(orator_core::OratorConfig::default()),
            gate: AdmissionGate::new(default_max_inflight()),
            state: Mutex::new(VoiceState::fresh()),
            sessions: Mutex::new(crate::session::SessionRegistry::new()),
            log: memory_log(),
            approvals: orator_tools::ApprovalHub::new(),
            llm_history: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Open an approval round for `summary`. Test seam for the
    /// approval-spoof golden; production rounds open through the tool
    /// loop.
    #[cfg(test)]
    pub fn request_approval_for_tests(
        &self,
        summary: String,
    ) -> (String, tokio::sync::oneshot::Receiver<bool>) {
        self.approvals.request(summary)
    }

    /// Answer an approval round. The socket route (trusted caller) uses
    /// this; D-Bus callers gate on `Caller::may_approve` before dialing.
    pub fn answer_approval(&self, id: &str, approve: bool) -> bool {
        self.approvals.answer(id, approve)
    }

    /// The loaded configuration this owner was built with, for the
    /// session factory that shares the daemon's leaves.
    #[must_use]
    pub fn config(&self) -> &orator_core::OratorConfig {
        &self.config
    }

    /// A responsive voice turn: breaks the failure streak, re-enters,
    /// and remembers the URL for the ejection probe.
    fn record_voice_success(&self, url: &str) {
        let mut state = common_core::sync::lock(&self.state);
        state.ejection.record_success();
        state.last_url = Some(url.to_string());
    }

    /// A responsive-but-unsuccessful turn (voiceless, cut, overfull):
    /// the server answered, so the streak breaks without ejecting.
    fn record_voice_response(&self) {
        common_core::sync::lock(&self.state)
            .ejection
            .record_success();
    }

    /// A transport-failed voice turn: counts toward ejection.
    fn record_voice_backend(&self) {
        common_core::sync::lock(&self.state)
            .ejection
            .record_backend();
    }

    /// Endpoint URL, booting on demand. Boot failures and a disabled
    /// owner report actionably; the CLI falls back to its own boot
    /// or the next voice in its chain.
    ///
    /// An independently-started server at the configured `s2s`
    /// endpoint is used as-is (shared, never booted, never reaped):
    /// only when nothing serves there does the owner consider a
    /// supervised child — and only when the file opts in
    /// (`s2s.auto_boot`) or an explicit binary pins one. Off means
    /// the voice server is independently started.
    pub async fn ensure(&self) -> Result<String, VoiceError> {
        if self.disabled {
            return Err(VoiceError::Disabled);
        }
        if self.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(VoiceError::ShuttingDown);
        }
        // Shared first: the configured endpoint serves, use it. No
        // child, no boot, no timer. Explicit-binary owners skip this
        // (a pinned binary means boot exactly that).
        if self.binary.is_none() && !self.has_server() {
            let base = self.config.s2s.endpoint.clone();
            let model = self.config.s2s.model.clone();
            if crate::socket::bridge_concurrent(orator_s2s::serves_model(&base, &model)) {
                self.record_voice_success(&base);
                return Ok(base);
            }
        }
        // Ejected fast path: one cheap probe, never a boot. A healthy
        // probe drops the suspect child and clears the streak so the
        // normal path boots fresh; the same POST-failing child is never
        // re-admitted on probe health alone. Only a served turn keeps
        // the fresh child. Probe failure stops and drops the child and
        // stays ejected.
        if let Some(url) = {
            let state = common_core::sync::lock(&self.state);
            if state.ejection.is_ejected() {
                state
                    .server
                    .as_ref()
                    .map(|server| server.base_url().to_string())
                    .or_else(|| state.last_url.clone())
            } else {
                None
            }
        } {
            if is_alive(&url).await {
                let mut state = common_core::sync::lock(&self.state);
                if let Some(server) = state.server.take() {
                    server.stop();
                }
                state.ejection.record_success();
                state.last_used = Instant::now();
                state.last_outcome = Some(RequestOutcome::Ok);
                state.last_url = Some(url.clone());
            } else {
                let mut state = common_core::sync::lock(&self.state);
                if let Some(server) = state.server.take() {
                    server.stop();
                }
                let streak = state.ejection.consecutive();
                return Err(VoiceError::Ejected(format!(
                    "voice endpoint ejected after {streak} consecutive failures (last probe failed) — next ensure re-probes"
                )));
            }
        }
        loop {
            // Decide under the lock; act outside it. No await ever
            // runs while the guard is alive.
            enum Next {
                Use(String),
                Wait,
                Boot,
            }
            let next = {
                let mut state = common_core::sync::lock(&self.state);
                Self::reap_if_idle(&mut state, self.idle_timeout, Instant::now());
                if let Some(server) = state.server.as_ref() {
                    Next::Use(server.base_url().to_string())
                } else if state.booting {
                    Next::Wait
                } else {
                    state.booting = true;
                    Next::Boot
                }
            };
            match next {
                Next::Use(url) => {
                    if is_alive(&url).await {
                        let mut state = common_core::sync::lock(&self.state);
                        state.last_used = Instant::now();
                        state.last_outcome = Some(RequestOutcome::Ok);
                        state.last_url = Some(url.clone());
                        return Ok(url);
                    }
                    {
                        let mut state = common_core::sync::lock(&self.state);
                        state.last_outcome = Some(RequestOutcome::Backend);
                    }
                    self.clear();
                }
                Next::Wait => {
                    // A sibling call owns the boot; answer immediately
                    // with a retryable contention instead of holding
                    // this handler for the whole boot. The CLI renders
                    // progress, never a read failure.
                    return Err(VoiceError::BootContention);
                }
                Next::Boot => {
                    let booted = self.boot().await;
                    let mut state = common_core::sync::lock(&self.state);
                    state.booting = false;
                    match booted {
                        Ok(server) => {
                            if self.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
                                server.stop();
                                return Err(VoiceError::ShuttingDown);
                            }
                            let url = server.base_url().to_string();
                            state.server = Some(server);
                            state.last_used = Instant::now();
                            state.last_url = Some(url.clone());
                            return Ok(url);
                        }
                        Err(error) => {
                            state.last_outcome = Some(RequestOutcome::Backend);
                            return Err(error);
                        }
                    }
                }
            }
        }
    }

    /// Stop and drop the child, if any. Sets the shutdown flag first,
    /// drains every session (firing their aborts and releasing their
    /// leases), then clears. No waiting: an in-flight boot that lands
    /// after the clear checks the shutdown flag and releases instead of
    /// publishing (see `ensure`), and dropping the supervisor reaps the
    /// child — so nothing leaks and no worker ever sleeps here.
    pub fn shutdown(&self) {
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        for id in self.list_sessions() {
            let _ = self.stop_session(&id);
        }
        common_core::sync::lock(&self.llm_history).clear();
        self.clear();
    }

    async fn boot(&self) -> Result<orator_llm::Supervisor, VoiceError> {
        // Explicit binaries pin exactly one candidate below; the
        // resolved path boots only when the file opts in. Off means
        // the voice server is independently started: absence fails
        // loudly with the remedy instead of spawning a child.
        let auto_boot = self.binary.is_some() || self.config.s2s.auto_boot;
        if !auto_boot {
            return Err(VoiceError::AutoBootDisabled(format!(
                "no voice server answers (start a llama-server for '{}', or set s2s.auto_boot to boot one automatically)",
                self.config.s2s.model,
            )));
        }
        let bundle = orator_core::bundle_from_s2s(
            &self.config.s2s,
            &self.config.models,
            &self.config.gguf_dir,
        );
        if !bundle.present() {
            return Err(VoiceError::Boot(format!(
                "audio bundle incomplete: {}",
                bundle
                    .missing()
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        // Every candidate binary is tried in order until one serves:
        // an explicit `with_binary` pins exactly one, otherwise a
        // pre-LFM3 install fails fast on the bundle and the next
        // serves instead of vetoing voice.
        if self.config.s2s.gpu {
            orator_core::gpu::require_gpu("llama-server").map_err(VoiceError::Boot)?;
        }
        let candidates: Vec<std::path::PathBuf> = match self.binary.as_ref() {
            Some(pinned) => vec![pinned.clone()],
            None => orator_s2s::resolve_audio_server_candidates(),
        };
        if candidates.is_empty() {
            return Err(VoiceError::NoBinary(format!(
                "no voice server (set {} or install {}; search hits that fail the ownership check are refused)",
                orator_s2s::AUDIO_SERVER_ENV,
                orator_s2s::AUDIO_SERVER_BIN
            )));
        }
        match fluent_wvr::ladder::first_accept_in_order(
            candidates,
            |binary: std::path::PathBuf| {
                let mut config = orator_s2s::audio_supervisor_config(&bundle, 0);
                if self.config.s2s.gpu {
                    config.extra_args.extend([
                        "--n-gpu-layers".to_string(),
                        orator_llm::GPU_OFFLOAD_LAYERS.to_string(),
                    ]);
                }
                async move {
                    match orator_llm::boot_with_binary(config, &binary).await {
                        Ok(server) => Ok(Some(server)),
                        Err(error) => Err(error.to_string()),
                    }
                }
            },
            |_: &String| false,
        )
        .await
        {
            Ok(Some(server)) => Ok(server),
            Ok(None) => Err(VoiceError::Boot(
                "every voice server candidate failed (last: )".to_string(),
            )),
            Err(last_error) => Err(VoiceError::Boot(format!(
                "every voice server candidate failed (last: {last_error})"
            ))),
        }
    }

    fn clear(&self) {
        let mut state = common_core::sync::lock(&self.state);
        if let Some(server) = state.server.take() {
            server.stop();
        }
    }

    fn reap_if_idle(state: &mut VoiceState, idle_timeout: Duration, now: Instant) {
        let idle = state.server.is_some() && now.duration_since(state.last_used) > idle_timeout;
        // Leases gate the timer; the last outcome never pins a failing
        // child past the budget. A failing server reaps here and the
        // next use boots fresh; a responsive server reaps the same way.
        let releasable = state.leases == 0;
        if idle && releasable {
            if let Some(server) = state.server.take() {
                server.stop();
            }
        }
    }

    /// Start one session of `kind`; returns its stable id. The
    /// `meeting` kind additionally builds its post-processing
    /// pipeline the way the diarization tests do, so a broken
    /// pipeline fails the start instead of a later turn.
    pub fn start_session(&self, kind: &str) -> Result<String, String> {
        let kind = if kind.trim().is_empty() {
            "assistant".to_string()
        } else {
            kind.to_string()
        };
        let id = {
            let mut sessions = common_core::sync::lock(&self.sessions);
            sessions.start(&kind, &self.config)
        };
        // A live session pins its server: checkout on start, released
        // on stop or shutdown drop.
        common_core::sync::lock(&self.state).leases += 1;
        if kind == "meeting" {
            build_meeting_pipeline(&id)?;
        }
        let _ = self.log.append(
            &id,
            0,
            "note",
            &format!("session {id} started ({kind})"),
            wall_ms(),
        );
        Ok(id)
    }

    /// Stop `id`: fire its abort and drop the entry.
    pub fn stop_session(&self, id: &str) -> Result<(), VoiceError> {
        let id = id.trim();
        let target: Option<String> = if id.is_empty() {
            let sessions = common_core::sync::lock(&self.sessions);
            let listed = sessions.list();
            if listed.len() == 1 {
                Some(listed[0].clone())
            } else {
                return Err(VoiceError::UnknownSession(format!(
                    "no session id: {} live (pass one id)",
                    listed.len()
                )));
            }
        } else {
            Some(id.to_string())
        };
        let Some(target) = target else {
            return Err(VoiceError::UnknownSession(
                "no session id: no live session to stop (pass one id)".to_string(),
            ));
        };
        {
            let mut sessions = common_core::sync::lock(&self.sessions);
            sessions.stop(&target).map_err(VoiceError::UnknownSession)?;
        }
        self.drop_history(&target);
        // Dropping the entry releases its lease.
        let mut state = common_core::sync::lock(&self.state);
        state.leases = state.leases.saturating_sub(1);
        let _ = self.log.append(
            &target,
            0,
            "note",
            &format!("session {target} stopped"),
            wall_ms(),
        );
        Ok(())
    }

    /// Interrupt `id`: fire its in-flight abort and re-arm.
    pub fn interrupt_session(&self, id: &str) -> Result<(), VoiceError> {
        let id = id.trim();
        if id.is_empty() {
            return Err(VoiceError::UnknownSession("no session id".to_string()));
        }
        let mut sessions = common_core::sync::lock(&self.sessions);
        sessions.interrupt(id).map_err(VoiceError::UnknownSession)?;
        Ok(())
    }

    /// Live session ids in stable order.
    #[must_use]
    pub fn list_sessions(&self) -> Vec<String> {
        common_core::sync::lock(&self.sessions).list()
    }

    /// Session count plus endpoint liveness for `GetState`.
    #[must_use]
    pub fn session_count(&self) -> usize {
        common_core::sync::lock(&self.sessions).len()
    }

    /// Export the meeting record for `id` to the managed meetings
    /// dir: the session-scoped events (lifecycle notes plus turns)
    /// rendered to deterministic markdown and written atomically.
    /// Returns the artifact path. Unknown ids refuse with
    /// `UnknownSession` — every start appends a lifecycle note, so a
    /// session with no events never existed. Empty sessions still
    /// render their record, never an empty file. Re-running
    /// reproduces identical bytes (the renderer is pure), so a crash
    /// mid-render resumes by re-running; the atomic write never
    /// leaves a torn file behind.
    pub fn export_meeting(&self, id: &str) -> Result<std::path::PathBuf, VoiceError> {
        let id = id.trim();
        if id.is_empty() {
            return Err(VoiceError::UnknownSession(
                "no session id (pass one id)".to_string(),
            ));
        }
        // Registry ids are `sN`, but the param arrives over the wire:
        // anything outside the safe filename charset refuses instead
        // of escaping the meetings dir.
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(VoiceError::UnknownSession(id.to_string()));
        }
        let mut events = Vec::new();
        let mut since = 0i64;
        loop {
            let page = self
                .log
                .replay_session(id, since, orator_store::REPLAY_PAGE)
                .map_err(|error| VoiceError::Backend(error.to_string()))?;
            let full = page.len() >= orator_store::REPLAY_PAGE;
            if let Some(last) = page.last() {
                since = last.seq;
            }
            events.extend(page);
            if !full {
                break;
            }
        }
        if events.is_empty() {
            return Err(VoiceError::UnknownSession(id.to_string()));
        }
        let text = orator_notes::render_meeting_export(id, &events);
        let dir = orator_core::meetings_dir().ok_or_else(|| {
            VoiceError::Backend("no meetings dir (HOME and XDG_DATA_HOME are unset)".to_string())
        })?;
        std::fs::create_dir_all(&dir).map_err(|error| {
            VoiceError::Backend(format!("cannot create {}: {error}", dir.display()))
        })?;
        let dest = dir.join(format!("{id}.md"));
        common_core::io::write_atomic(&dest, text.as_bytes()).map_err(|error| {
            VoiceError::Backend(format!("cannot write {}: {error}", dest.display()))
        })?;
        Ok(dest)
    }

    /// Record one scripted turn for `session` through the production
    /// record path (history plus event log). Test seam: hermetic
    /// scripted turns without inference.
    #[cfg(test)]
    pub(crate) fn record_turn_for_tests(&self, session: &str, question: &str, reply: &str) {
        self.record_history(session, question, reply);
        let _ = self
            .log
            .append(session, 0, "transcript", question, wall_ms());
        let _ = self.log.append(session, 0, "reply", reply, wall_ms());
    }

    /// Whether a voice child is currently owned.
    #[must_use]
    pub fn has_server(&self) -> bool {
        common_core::sync::lock(&self.state).server.is_some()
    }

    /// Test seam: mark a boot in flight so contention paths answer
    /// immediately without spawning a child.
    #[cfg(test)]
    pub(crate) fn set_booting_for_tests(&self, booting: bool) {
        common_core::sync::lock(&self.state).booting = booting;
    }

    /// Toggle dictation: starting one session enables, stopping every
    /// dictation session disables. Only `kind == "dictation"` sessions
    /// stop; other kinds survive. Returns `(enabled, session)`.
    pub fn toggle_dictation(&self, enabled: Option<bool>) -> (bool, Option<String>) {
        let dictating: Vec<String> = common_core::sync::lock(&self.sessions).of_kind("dictation");
        let want = enabled.unwrap_or(dictating.is_empty());
        if want {
            match self.start_session("dictation") {
                Ok(id) => (true, Some(id)),
                Err(_) => (false, None),
            }
        } else {
            for id in dictating {
                let _ = self.stop_session(&id);
            }
            (false, None)
        }
    }

    /// Speak `text` through the daemon-owned voice server. Returns the
    /// voiced samples and their rate; the caller plays them locally.
    /// Audio bytes travel in the socket envelope for the CLI to voice.
    pub fn say_blocking(
        &self,
        text: &str,
        session: Option<&str>,
    ) -> Result<(Vec<f32>, u32), VoiceError> {
        if text.trim().is_empty() {
            return Err(VoiceError::Backend("nothing to speak".to_string()));
        }
        if let Some(id) = session {
            let sessions = common_core::sync::lock(&self.sessions);
            if !sessions.contains(id) {
                return Err(VoiceError::UnknownSession(id.to_string()));
            }
        }
        // Serving path, concurrent callers: the in-place bridge on
        // runtime workers and blocking threads, the shared runtime
        // where no runtime is active (see `bridge_concurrent`). The
        // serializing fallback stays only where single-flight is
        // load-bearing (the boot flag inside `ensure`); this bridge must
        // never serialize concurrent voice turns behind one boot.
        let url = crate::socket::bridge_concurrent(self.ensure()).map_err(|error| match error {
            VoiceError::Disabled
            | VoiceError::ShuttingDown
            | VoiceError::NoBinary(_)
            | VoiceError::Boot(_)
            | VoiceError::BootContention
            | VoiceError::Saturated(_)
            | VoiceError::Ejected(_)
            | VoiceError::NoAudio(_)
            | VoiceError::Overflow(_)
            | VoiceError::SlowPeer(_)
            | VoiceError::Aborted(_) => error,
            other => VoiceError::Backend(other.to_string()),
        })?;
        self.say_via(&url, text, session)
    }

    /// Speak `text` through `endpoint` (the hermetic seam: tests pass a
    /// scripted server; production passes the ensured URL). Records the
    /// outcome for ejection: transport failures and over-bound turns
    /// count, responsive turns (including voiceless and cancelled ones)
    /// break the streak.
    pub fn say_via(
        &self,
        endpoint: &str,
        text: &str,
        session: Option<&str>,
    ) -> Result<(Vec<f32>, u32), VoiceError> {
        // The bound lives at the turn entries so stub-driven turns and
        // production turns share one enforcement point.
        let _permit = self
            .gate
            .admit()
            .map_err(|refusal| VoiceError::Saturated(refusal.reason))?;
        let mut audio = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
            endpoint: endpoint.to_string(),
            ..self.config.s2s.clone()
        });
        // Every attempt names the probe target, success or failure —
        // ejection without a URL could never re-probe.
        common_core::sync::lock(&self.state).last_url = Some(endpoint.to_string());
        match audio.speak_blocking(text) {
            Ok(samples) => {
                let label = session.unwrap_or("daemon").to_string();
                let _ = self.log.append(&label, 0, "reply", text, wall_ms());
                if let Some(id) = session {
                    common_core::sync::lock(&self.sessions).touch(id);
                }
                self.record_voice_success(endpoint);
                Ok((samples, orator_s2s::AUDIO_OUT_RATE))
            }
            Err(orator_s2s::S2sError::NoAudio) => {
                self.record_voice_response();
                Err(VoiceError::NoAudio("server sent no audio".to_string()))
            }
            Err(orator_s2s::S2sError::Aborted) => {
                self.record_voice_response();
                Err(VoiceError::Aborted("voice turn aborted".to_string()))
            }
            Err(orator_s2s::S2sError::Overflow(detail)) => {
                self.record_voice_backend();
                Err(VoiceError::Overflow(detail))
            }
            Err(orator_s2s::S2sError::Backend(message)) => {
                self.record_voice_backend();
                Err(VoiceError::Backend(message))
            }
        }
    }

    /// Ask `question` through the configured LLM endpoint. Returns the
    /// reply text; both sides land in the event log.
    pub fn ask_blocking(
        &self,
        question: &str,
        session: Option<&str>,
    ) -> Result<String, VoiceError> {
        if question.trim().is_empty() {
            return Err(VoiceError::Backend("nothing to ask".to_string()));
        }
        if let Some(id) = session {
            let sessions = common_core::sync::lock(&self.sessions);
            if !sessions.contains(id) {
                return Err(VoiceError::UnknownSession(id.to_string()));
            }
        }
        let llm = self.config.llm.clone();
        let (base, model) = orator_cli_config_base(&llm);
        // Local-first text: the configured endpoint wins when it
        // answers; otherwise the ensured voice server takes the turn
        // (LFM3-Audio answers text as well as speech). A refused
        // connection fails the probe instantly, so the fallback costs
        // nothing when the gateway is simply down — and a live gateway
        // never pays for a voice boot it does not need.
        if crate::socket::bridge_concurrent(orator_llm::probe_models(
            &base,
            common_core::http::shared_http_client(),
        ))
        .is_ok()
        {
            return self.ask_via(
                &base,
                &model,
                usize::from(llm.ctx),
                usize::from(llm.n_batch),
                question,
                session,
            );
        }
        let url = crate::socket::bridge_concurrent(self.ensure()).map_err(|error| match error {
            VoiceError::Disabled
            | VoiceError::ShuttingDown
            | VoiceError::NoBinary(_)
            | VoiceError::Boot(_)
            | VoiceError::BootContention
            | VoiceError::Saturated(_)
            | VoiceError::Ejected(_)
            | VoiceError::NoAudio(_)
            | VoiceError::Overflow(_)
            | VoiceError::Aborted(_)
            | VoiceError::SlowPeer(_)
            | VoiceError::UnknownSession(_)
            | VoiceError::AutoBootDisabled(_)
            | VoiceError::Backend(_) => error,
        })?;
        // The readiness probe opens routes before weights finish
        // streaming in: hold the turn until `/health` answers (or the
        // budget burns and the turn reports honestly). Legacy builds
        // serve no `/health` (404 counts as ready — their OPTIONS
        // probe already gated load).
        wait_voice_loaded(&url);
        self.ask_via(
            &url,
            &model,
            usize::from(llm.ctx),
            usize::from(llm.n_batch),
            question,
            session,
        )
    }

    /// Transcribe 16 kHz mono `samples` through the ensured voice
    /// server. Empty input is silence, not a failure (`Ok("")`). The
    /// input bound lives here so stub-driven and production turns
    /// share one enforcement point.
    pub fn transcribe_blocking(
        &self,
        samples: &[f32],
        session: Option<&str>,
    ) -> Result<String, VoiceError> {
        if samples.len() > MAX_TRANSCRIBE_SAMPLES {
            return Err(VoiceError::Overflow(format!(
                "transcribe turn of {} samples exceeds the single-frame bound ({} samples); send shorter utterances",
                samples.len(),
                MAX_TRANSCRIBE_SAMPLES,
            )));
        }
        if samples.iter().all(|sample| *sample == 0.0) {
            return Ok(String::new());
        }
        if let Some(id) = session {
            let sessions = common_core::sync::lock(&self.sessions);
            if !sessions.contains(id) {
                return Err(VoiceError::UnknownSession(id.to_string()));
            }
        }
        let url = crate::socket::bridge_concurrent(self.ensure()).map_err(|error| match error {
            VoiceError::Disabled
            | VoiceError::ShuttingDown
            | VoiceError::NoBinary(_)
            | VoiceError::Boot(_)
            | VoiceError::BootContention
            | VoiceError::Saturated(_)
            | VoiceError::Ejected(_)
            | VoiceError::NoAudio(_)
            | VoiceError::Overflow(_)
            | VoiceError::Aborted(_)
            | VoiceError::SlowPeer(_)
            | VoiceError::UnknownSession(_)
            | VoiceError::AutoBootDisabled(_)
            | VoiceError::Backend(_) => error,
        })?;
        self.transcribe_via(&url, samples, session)
    }

    /// Transcribe `samples` through `endpoint` (the hermetic seam:
    /// tests pass a scripted server; production passes the ensured
    /// URL). Transport failures count toward ejection; responsive
    /// turns break the streak.
    pub fn transcribe_via(
        &self,
        endpoint: &str,
        samples: &[f32],
        session: Option<&str>,
    ) -> Result<String, VoiceError> {
        let _permit = self
            .gate
            .admit()
            .map_err(|refusal| VoiceError::Saturated(refusal.reason))?;
        let mut audio = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
            endpoint: endpoint.to_string(),
            ..self.config.s2s.clone()
        });
        common_core::sync::lock(&self.state).last_url = Some(endpoint.to_string());
        match audio.transcribe_blocking(samples) {
            Ok(text) => {
                let label = session.unwrap_or("daemon").to_string();
                let _ = self.log.append(&label, 0, "transcript", &text, wall_ms());
                if let Some(id) = session {
                    common_core::sync::lock(&self.sessions).touch(id);
                }
                self.record_voice_success(endpoint);
                Ok(text)
            }
            Err(orator_s2s::S2sError::Aborted) => {
                self.record_voice_response();
                Err(VoiceError::Aborted("voice turn aborted".to_string()))
            }
            Err(orator_s2s::S2sError::Backend(message)) => {
                self.record_voice_backend();
                Err(VoiceError::Backend(message))
            }
            Err(orator_s2s::S2sError::Overflow(detail)) => {
                self.record_voice_backend();
                Err(VoiceError::Overflow(detail))
            }
            Err(orator_s2s::S2sError::NoAudio) => {
                self.record_voice_response();
                Ok(String::new())
            }
        }
    }

    /// Ask `question` through `endpoint`/`model` (the hermetic seam).
    /// With a session id, past turns ride along as text context (oldest
    /// first, bounded), so voice and text chat build context turn by
    /// turn; without one the turn is stateless. Only completed turns
    /// extend history — errors and aborts never do.
    pub fn ask_via(
        &self,
        endpoint: &str,
        model: &str,
        ctx: usize,
        n_batch: usize,
        question: &str,
        session: Option<&str>,
    ) -> Result<String, VoiceError> {
        // Text shares the bound: one policy for every serving path.
        let _permit = self
            .gate
            .admit()
            .map_err(|refusal| VoiceError::Saturated(refusal.reason))?;
        let prompt = match session {
            Some(id) => self.prompt_with_history(id, question),
            None => question.to_string(),
        };
        let mut llm_session = orator_llm::HttpSession::open(endpoint, model, ctx, n_batch);
        llm_session.push_text(&prompt);
        match llm_session.step(&mut orator_core::Sampler::default()) {
            Ok(out) => {
                let reply = out.delta.trim_end().to_string();
                let label = session.unwrap_or("daemon").to_string();
                let _ = self
                    .log
                    .append(&label, 0, "transcript", question, wall_ms());
                let _ = self.log.append(&label, 0, "reply", &reply, wall_ms());
                if let Some(id) = session {
                    common_core::sync::lock(&self.sessions).touch(id);
                    self.record_history(id, question, &reply);
                }
                Ok(reply)
            }
            Err(error) => Err(VoiceError::Backend(error.to_string())),
        }
    }

    /// Build the prompt for a session turn: bounded past turns plus
    /// the new question. Oldest turns drop past [`MAX_HISTORY_CHARS`];
    /// the new question always survives intact.
    fn prompt_with_history(&self, id: &str, question: &str) -> String {
        let history = common_core::sync::lock(&self.llm_history);
        let Some(turns) = history.get(id) else {
            return question.to_string();
        };
        if turns.is_empty() {
            return question.to_string();
        }
        let mut kept = turns.as_slice();
        while !kept.is_empty() {
            let mut prompt = String::new();
            for (past_q, past_a) in kept {
                prompt.push_str("user: ");
                prompt.push_str(past_q);
                prompt.push_str("\nassistant: ");
                prompt.push_str(past_a);
                prompt.push('\n');
            }
            prompt.push_str("user: ");
            prompt.push_str(question);
            if prompt.chars().count() <= MAX_HISTORY_CHARS {
                return prompt;
            }
            kept = &kept[1..];
        }
        question.to_string()
    }

    /// Record one completed turn, capping stored turns.
    fn record_history(&self, id: &str, question: &str, reply: &str) {
        let mut history = common_core::sync::lock(&self.llm_history);
        let turns = history.entry(id.to_string()).or_default();
        turns.push((question.to_string(), reply.to_string()));
        while turns.len() > MAX_SESSION_TURNS {
            turns.remove(0);
        }
    }

    /// Drop a session's conversational context, if any.
    fn drop_history(&self, id: &str) {
        common_core::sync::lock(&self.llm_history).remove(id);
    }
}

fn orator_cli_config_base(llm: &orator_core::LlmEndpointConfig) -> (String, String) {
    let explicit = std::env::var("LLM_BASE_URL").unwrap_or_default();
    if explicit.trim().is_empty() {
        (format!("http://{}", llm.endpoint), llm.model.clone())
    } else {
        let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| llm.model.clone());
        (explicit, model)
    }
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

/// Hold a voice turn until the server's model answers `/health`.
/// Cold boots open routes before weights finish streaming in; the
/// first turn would otherwise fail a server seconds from serving.
/// Legacy builds serve no `/health` (404 counts as ready). A burnt
/// budget returns silently and the turn reports whatever the server
/// says — never a silent success, never an unbounded wait.
fn wait_voice_loaded(url: &str) {
    let budget = std::time::Duration::from_secs(90);
    let poll = std::time::Duration::from_secs(2);
    let deadline = Instant::now() + budget;
    loop {
        let ready = common_core::runtime::block_on(
            common_core::http::shared_http_client()
                .get(format!("{url}/health"))
                .timeout(std::time::Duration::from_secs(5))
                .send(),
        )
        .is_ok_and(|response| {
            response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND
        });
        if ready || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(poll);
    }
}

fn default_max_inflight() -> usize {
    usize::from(orator_core::OratorConfig::default().daemon.admission_max)
}

fn memory_log() -> std::sync::Arc<orator_store::EventLog> {
    orator_store::EventLog::in_memory().expect("memory event log")
}

fn build_meeting_pipeline(meeting: &str) -> Result<(), String> {
    let store =
        fluent_db::store::SqliteStore::open_in_memory().map_err(|error| error.to_string())?;
    store
        .with_conn(|conn| {
            fluent_db::migrate::migrate(conn, &orator_store::migrations())?;
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    let pipeline =
        orator_diary::MeetingPipeline::new(store, meeting).map_err(|error| error.to_string())?;
    if !pipeline.ready().contains(&"finalize_audio".to_string()) {
        return Err("meeting pipeline did not open at finalize_audio".to_string());
    }
    Ok(())
}

impl Default for VoiceOwner {
    fn default() -> Self {
        Self::new(&orator_core::OratorConfig::default())
    }
}

/// Whether `url` answers the readiness probe right now.
async fn is_alive(url: &str) -> bool {
    orator_llm::probe_voice_ready(
        common_core::http::shared_http_client(),
        url,
        orator_llm::HealthProbe::OptionsCompletions,
    )
    .await
    .is_ok()
}

#[cfg(test)]
#[path = "../tests/voice.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/leases.rs"]
mod leases_tests;

#[cfg(test)]
#[path = "../tests/export.rs"]
mod export_tests;
