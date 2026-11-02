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

/// How long `shutdown` waits for an in-flight boot to finish before
/// giving up the wait. A late boot still checks the shutdown flag and
/// releases instead of publishing, so the bound only shortens the
/// shutdown path — it never leaks.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);

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
            Self::Boot(_) => "Boot",
            Self::BootContention => "BootContention",
            Self::Backend(_) => "Backend",
            Self::Saturated(_) => "Saturated",
            Self::Ejected(_) => "Ejected",
            Self::NoAudio(_) => "NoAudio",
            Self::Overflow(_) => "Overflow",
            Self::Aborted(_) => "Aborted",
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
        }
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
    pub async fn ensure(&self) -> Result<String, VoiceError> {
        if self.disabled {
            return Err(VoiceError::Disabled);
        }
        if self
            .shutting_down
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(VoiceError::ShuttingDown);
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
                            if self
                                .shutting_down
                                .load(std::sync::atomic::Ordering::SeqCst)
                            {
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
    /// leases), waits boundedly for an in-flight boot, then clears. A
    /// boot that lands after the wait releases instead of publishing.
    pub fn shutdown(&self) {
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        for id in self.list_sessions() {
            let _ = self.stop_session(&id);
        }
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        while Instant::now() < deadline {
            {
                let state = common_core::sync::lock(&self.state);
                if !state.booting {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.clear();
    }

    async fn boot(&self) -> Result<orator_llm::Supervisor, VoiceError> {
        let bundle = orator_core::default_bundle();
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
        let config = orator_s2s::audio_supervisor_config(&bundle, 0);
        let booted = if let Some(binary) = self.binary.as_ref() {
            orator_llm::boot_with_binary(config, binary).await
        } else {
            match orator_s2s::resolve_audio_server() {
                Some(binary) => orator_llm::boot_with_binary(config, &binary).await,
                None => {
                    return Err(VoiceError::NoBinary(format!(
                        "no voice server (set {} or install {}; search hits that fail the ownership check are refused)",
                        orator_s2s::AUDIO_SERVER_ENV,
                        orator_s2s::AUDIO_SERVER_BIN
                    )));
                }
            }
        };
        booted.map_err(|error| VoiceError::Boot(error.to_string()))
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
        let _ = self.log.append(&id, 0, "note", &format!("session {id} started ({kind})"), wall_ms());
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
        let target = target.expect("single live session");
        {
            let mut sessions = common_core::sync::lock(&self.sessions);
            sessions.stop(&target).map_err(VoiceError::UnknownSession)?;
        }
        // Dropping the entry releases its lease.
        let mut state = common_core::sync::lock(&self.state);
        state.leases = state.leases.saturating_sub(1);
        let _ = self.log.append(&target, 0, "note", &format!("session {target} stopped"), wall_ms());
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
    /// voiced sample count and rate; audio bytes stay out of the
    /// socket envelope (the CLI plays locally).
    pub fn say_blocking(&self, text: &str, session: Option<&str>) -> Result<(usize, u32), VoiceError> {
        if text.trim().is_empty() {
            return Err(VoiceError::Backend("nothing to speak".to_string()));
        }
        if let Some(id) = session {
            let sessions = common_core::sync::lock(&self.sessions);
            if !sessions.contains(id) {
                return Err(VoiceError::UnknownSession(id.to_string()));
            }
        }
        // Serving path, concurrent callers: the shared runtime from
        // blocking threads, the in-place bridge from runtime workers
        // (tests). The serializing fallback stays only where
        // single-flight is load-bearing (the boot flag inside `ensure`);
        // this bridge must never serialize concurrent voice turns behind
        // one boot.
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
    pub fn say_via(&self, endpoint: &str, text: &str, session: Option<&str>) -> Result<(usize, u32), VoiceError> {
        // The bound lives at the turn entries so stub-driven turns and
        // production turns share one enforcement point.
        let _permit = self
            .gate
            .admit()
            .map_err(|refusal| VoiceError::Saturated(refusal.reason))?;
        let mut audio = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
            endpoint: endpoint.to_string(),
            ..orator_s2s::S2sConfig::default()
        });
        // Every attempt names the probe target, success or failure —
        // ejection without a URL could never re-probe.
        common_core::sync::lock(&self.state).last_url = Some(endpoint.to_string());
        match audio.speak_blocking(text) {
            Ok(samples) => {
                let count = samples.len();
                let label = session.unwrap_or("daemon").to_string();
                let _ = self.log.append(&label, 0, "reply", text, wall_ms());
                if let Some(id) = session {
                    common_core::sync::lock(&self.sessions).touch(id);
                }
                self.record_voice_success(endpoint);
                Ok((count, orator_s2s::AUDIO_OUT_RATE))
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
    pub fn ask_blocking(&self, question: &str, session: Option<&str>) -> Result<String, VoiceError> {
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
        self.ask_via(&base, &model, usize::from(llm.ctx), usize::from(llm.n_batch), question, session)
    }

    /// Ask `question` through `endpoint`/`model` (the hermetic seam).
    pub fn ask_via(&self, endpoint: &str, model: &str, ctx: usize, n_batch: usize, question: &str, session: Option<&str>) -> Result<String, VoiceError> {
        // Text shares the bound: one policy for every serving path.
        let _permit = self
            .gate
            .admit()
            .map_err(|refusal| VoiceError::Saturated(refusal.reason))?;
        let mut llm_session = orator_llm::HttpSession::open(endpoint, model, ctx, n_batch);
        llm_session.push_text(question);
        match llm_session.step(&mut orator_core::Sampler::default()) {
            Ok(out) => {
                let reply = out.delta.trim_end().to_string();
                let label = session.unwrap_or("daemon").to_string();
                let _ = self.log.append(&label, 0, "transcript", question, wall_ms());
                let _ = self.log.append(&label, 0, "reply", &reply, wall_ms());
                if let Some(id) = session {
                    common_core::sync::lock(&self.sessions).touch(id);
                }
                Ok(reply)
            }
            Err(error) => Err(VoiceError::Backend(error.to_string())),
        }
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

fn default_max_inflight() -> usize {
    usize::from(orator_core::OratorConfig::default().daemon.admission_max)
}

fn memory_log() -> std::sync::Arc<orator_store::EventLog> {
    orator_store::EventLog::in_memory().expect("memory event log")
}

fn build_meeting_pipeline(meeting: &str) -> Result<(), String> {
    let store = fluent_db::store::SqliteStore::open_in_memory().map_err(|error| error.to_string())?;
    store
        .with_conn(|conn| {
            fluent_db::migrate::migrate(conn, &orator_store::migrations())?;
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    let pipeline = orator_diar::MeetingPipeline::new(store, meeting).map_err(|error| error.to_string())?;
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
