//! Local-first voice fallback: the daemon serves first; when it has
//! no voice to serve, the CLI falls back to local specialized
//! inference driven by the roles table instead of failing the turn.
//!
//! The order is load-bearing: daemon first (shared fleet, supervised
//! servers), local second (this machine's weights), loud error last.
//! Only *unavailable* refusals fall through (nothing serving, transport
//! down, ejected endpoint). Busy/transient/usage failures stay
//! terminal so retry, shed, and admission discipline keep working.
//! Moonshine tags resolve through the local transcribe.cpp backend;
//! unservable entries are skipped with the reason named, never
//! silently become something else.

use fluent_concurrency::stream::StreamAbort;
use orator::LlmSession;

use crate::config::resolve_llm_base;
use crate::socket::Transport;

/// Budget for one local ASR/TTS operation: this assistant is
/// full-duplex, so a local decode or synthesis that cannot finish in
/// ten seconds fails loudly instead of stalling the turn behind it.
/// LLM turns carry their own (longer) budgets and are out of scope.
pub const LOCAL_OP_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Run `op` to completion within `budget`, on a worker thread so the
/// caller never blocks past the deadline. A slow operation reports a
/// named timeout instead of hanging the turn; its worker runs to
/// completion detached (decodes always finish, so no thread
/// accumulates — at most one stray per timed-out turn, reaped by
/// process exit at the latest).
///
/// Shared execution home: `common_core::retry::run_with_budget` — this
/// is the domain wrapper that keeps the `orator: local …` refusal line
/// verbatim. (The abort-aware sibling, s2s `sleep_abort_aware`, stays
/// separate: it polls a cancellation signal, this one does not.)
pub fn budgeted<T>(
    label: &str,
    budget: std::time::Duration,
    op: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String>
where
    T: Send + 'static,
{
    match common_core::retry::run_with_budget(label, budget, op) {
        Ok(outcome) => outcome,
        Err(exceeded) => Err(format!(
            "orator: local {} exceeded its {} s budget; turn stays interactive instead of stalling",
            exceeded.label(),
            exceeded.budget_secs()
        )),
    }
}

/// Daemon refusal shapes meaning "no voice serving here". Matched
/// case-insensitively against the daemon's error line.
fn unavailable_markers() -> [&'static str; 8] {
    [
        "auto-boot is off",           // nothing serving, boot declined
        "no voice server",            // missing binary / no answers variants
        "voice server disabled",      // serving compiled out here
        "voice server shutting down", // the daemon is going away
        "voice endpoint ejected",     // the shared endpoint failed out
        "daemon unavailable",         // transport down
        "no socket path",             // no runtime dir at all
        "connection refused",         // transport down mid-dial
    ]
}

/// Whether a daemon refusal means "nothing serving" (fall through to
/// local inference) rather than busy/transient/usage (stay terminal).
/// Shared scan home: `common_core::string` (one lowered haystack over
/// the marker table — the folded-scan shape wanted here, already
/// present under that name); the table itself stays here and separate
/// from the chain classifier's wording legs (merging them would change
/// behavior — see `shared_corpus_pins_classifier_disagreements`).
#[must_use]
pub fn voice_unavailable(detail: &str) -> bool {
    common_core::string::LoweredHaystack::new(detail).contains_any(&unavailable_markers())
}

/// Compose a daemon dial with its local fallback: voice-unavailable
/// refusals try `fallback`, everything else returns as-is. The
/// fallback narrates its own hop; the daemon error is dropped only
/// when the fallback is attempted.
pub fn fallback_if_unavailable<T>(
    result: Result<T, String>,
    fallback: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if voice_unavailable(&error) {
                fallback()
            } else {
                Err(error)
            }
        }
    }
}

/// Resolve the managed ggml whisper file for `tag`. Missing homes and
/// missing files fail loudly naming the pull remedy.
pub fn whisper_model_path(tag: &str) -> Result<std::path::PathBuf, String> {
    let Some(dir) = orator::models_dir() else {
        return Err(
            "orator: no managed model directory (HOME and XDG_DATA_HOME are unset)".to_string(),
        );
    };
    let path = dir.join(format!("ggml-{tag}.bin"));
    if !path.exists() {
        return Err(format!(
            "orator: model missing at {} (fetch it with `orator models pull`)",
            path.display()
        ));
    }
    Ok(path)
}

/// Decode 16 kHz samples through the whisper file at `path`, joining
/// committed segments with single spaces. Single home for every
/// file/turn whisper decode that needs plain text.
pub fn decode_whisper_samples(
    path: &std::path::Path,
    language: &str,
    gpu: bool,
    samples: &[f32],
    abort: &StreamAbort,
) -> Result<String, String> {
    let mut backend = orator_voice::stt::WhisperBackend::new(path.to_path_buf(), language.to_string());
    if gpu {
        backend = backend.with_gpu();
    }
    backend
        .decode_blocking(samples, abort)
        .map(|segments| {
            segments
                .iter()
                .map(|segment| segment.text.clone())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .map_err(|error| error.to_string())
}

/// Recognition language for `tag` serving the ASR role: the
/// composed `language` override wins, else the global STT leaf.
/// Pure (no weights, no I/O) so the override is unit-testable.
#[must_use]
pub fn resolve_asr_language(config: &orator::OratorConfig, tag: &str) -> String {
    orator::compose_role_params(&config.models, &config.roles, "asr", tag)
        .get("language")
        .and_then(|value| value.as_str())
        .map_or_else(|| config.stt.language.clone(), str::to_string)
}

/// Voice for `tag` serving the TTS role: the composed `voice`
/// override wins, else the global TTS leaf. Pure (no weights, no
/// I/O) so the override is unit-testable.
#[must_use]
pub fn resolve_tts_voice(config: &orator::OratorConfig, tag: &str) -> String {
    orator::compose_role_params(&config.models, &config.roles, "tts", tag)
        .get("voice")
        .and_then(|value| value.as_str())
        .map_or_else(|| config.tts.voice.clone(), str::to_string)
}

/// Ask one question with daemon-first, local-second routing for the
/// TUI voice worker: the daemon serves shared inference; on
/// voice-unavailable refusals the question goes straight at the
/// configured endpoint (llama-server :8080 serves non-specialized
/// inference) instead of failing the turn.
pub fn ask_dial(
    text: &str,
    session: Option<&str>,
    transport: Transport,
    abort: &StreamAbort,
    config: &orator::OratorConfig,
) -> Result<String, String> {
    fallback_if_unavailable(
        crate::tui::live_ask(text, session, transport, abort),
        || local_ask(text, config),
    )
}

/// Voice one reply with daemon-first, local-second routing: the
/// daemon voices through its server; on voice-unavailable refusals
/// the reply synthesizes locally (kokoro, the TTS role default).
/// Before either, the TTS role head runs through the shared endpoint
/// when it is loaded there (router `/tts` selects the model) — the
/// shared server is the default backend, the configured model
/// backends serve past it.
pub fn say_dial(
    text: &str,
    transport: Transport,
    abort: &StreamAbort,
    config: &orator::OratorConfig,
) -> Result<(Vec<f32>, u32), String> {
    if let Some(Ok(direct)) = shared_head_say(text, config, abort) {
        return Ok(direct);
    }
    // Configured-local TTS before the fleet: the head names another
    // model than the daemon voices, so daemon-first would shadow the
    // pipeline with the wrong voice. A failing synth still falls
    // through — absence of serving, not of trying, is what the daemon
    // leg is for.
    if let Some(head) = tts_local_head(config) {
        eprintln!("orator: voicing with the configured TTS model ({head} kokoro)");
        match synth_kokoro(text, config) {
            ok @ Ok(_) => return ok,
            Err(error) => {
                eprintln!("orator: configured TTS voice failed ({error}); trying the daemon");
            }
        }
    }
    match crate::tui::live_say(text, transport, abort) {
        ok @ Ok(_) => ok,
        Err(error) => {
            if voice_unavailable(&error) {
                eprintln!("orator: daemon voice unavailable ({error}); synthesizing locally");
                local_say(text, config)
            } else {
                Err(error)
            }
        }
    }
}

/// One reply through the shared endpoint under the TTS role head:
/// when the head differs from the daemon's voice model and the
/// shared `s2s` endpoint has it loaded, voice there directly. `None`
/// keeps the daemon-first chain unchanged (same model, not loaded,
/// unreachable). `Err` also falls through — the next legs narrate —
/// so this stays best-effort role routing, never a new failure.
fn shared_head_say(
    text: &str,
    config: &orator::OratorConfig,
    abort: &StreamAbort,
) -> Option<Result<(Vec<f32>, u32), String>> {
    let head = orator::resolve_role_model(&config.roles, "tts")?;
    if head == config.s2s.model {
        return None; // the daemon leg already serves this model
    }
    let base = crate::config::s2s_base_url(&config.s2s);
    if !common_core::runtime::block_on_shared(orator_voice::s2s::model_loaded(&base, &head)) {
        return None; // configured backends (daemon, then local) serve
    }
    eprintln!("orator: voicing through the shared server ({head} audio-server)");
    let session = orator_voice::s2s::AudioSession::new(orator_voice::s2s::S2sConfig {
        endpoint: base,
        model: head,
        ..config.s2s.clone()
    });
    Some(
        session
            .speak_text_via_tts(text, abort)
            .map(|samples| (samples, orator_voice::s2s::AUDIO_OUT_RATE))
            .map_err(|error| error.to_string()),
    )
}

/// Transcribe one utterance with daemon-first, local-second routing:
/// the daemon serves shared and supervised STT; on voice-unavailable
/// refusals the audio decodes locally through the roles-table ASR
/// pick (moonshine first when a backend serves it, whisper where
/// weights are present).
pub fn transcribe_dial(
    samples: &[f32],
    transport: Transport,
    abort: &StreamAbort,
    config: &orator::OratorConfig,
) -> Result<String, String> {
    transcribe_dial_leg(samples, transport, abort, config).0
}

/// Which leg served a daemon-first transcribe dial: the shared voice
/// server or local inference. `Shared` is the direct role-head leg:
/// the ASR head decoded on the shared endpoint with no daemon hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialLeg {
    /// The daemon's voice server decoded (the configured `s2s`
    /// model over the audio-server protocol).
    Daemon,
    /// Local inference decoded (the role-walk pick, already logged
    /// by [`local_transcribe`] itself).
    Local,
    /// The shared endpoint decoded with the ASR role head (direct
    /// role routing over the audio-server protocol, no daemon hop).
    Shared,
}

/// Transcribe with daemon-first routing, reporting which leg served.
/// Same routing as [`transcribe_dial`]; the leg lets callers label
/// the served model honestly.
pub fn transcribe_dial_leg(
    samples: &[f32],
    transport: Transport,
    abort: &StreamAbort,
    config: &orator::OratorConfig,
) -> (Result<String, String>, DialLeg) {
    if let Some(Ok(text)) = shared_head_transcribe(samples, config) {
        return (Ok(text), DialLeg::Shared);
    }
    // Configured-local ASR before the fleet: same shadowing shape as
    // TTS — the daemon would decode with another model. A failing
    // decode still falls through to the daemon-first chain.
    if let Some(found) = asr_local_head(config) {
        eprintln!(
            "orator: transcribing with the configured ASR model ({} {})",
            found.tag,
            found.backend.name()
        );
        if let ok @ Ok(_) = decode_local_asr(found, samples, config, abort) {
            return (ok, DialLeg::Local);
        }
    }
    match crate::tui::transcribe_samples(samples, transport, abort) {
        ok @ Ok(_) => (ok, DialLeg::Daemon),
        Err(error) => {
            if voice_unavailable(&error) {
                eprintln!("orator: daemon voice unavailable ({error}); transcribing locally");
                (local_transcribe(samples, config, abort), DialLeg::Local)
            } else {
                (Err(error), DialLeg::Daemon)
            }
        }
    }
}

/// One utterance through the shared endpoint under the ASR role head:
/// when the head differs from the daemon's voice model and the
/// shared `s2s` endpoint has it loaded, decode there directly
/// (audio-server protocol with the head model). `None` keeps the
/// daemon-first chain unchanged; `Err` falls through the same way
/// (the next legs narrate), so this stays best-effort role routing.
fn shared_head_transcribe(
    samples: &[f32],
    config: &orator::OratorConfig,
) -> Option<Result<String, String>> {
    let head = orator::resolve_role_model(&config.roles, "asr")?;
    if head == config.s2s.model {
        return None; // the daemon leg already serves this model
    }
    let base = crate::config::s2s_base_url(&config.s2s);
    if !common_core::runtime::block_on_shared(orator_voice::s2s::model_loaded(&base, &head)) {
        return None; // configured backends (daemon, then local) serve
    }
    eprintln!("orator: transcribing through the shared server ({head} audio-server)");
    let mut session = orator_voice::s2s::AudioSession::new(orator_voice::s2s::S2sConfig {
        endpoint: base,
        model: head,
        ..config.s2s.clone()
    });
    Some(
        session
            .transcribe_blocking(samples)
            .map_err(|error| error.to_string()),
    )
}

/// Local ASR backend selected by the role walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalBackend {
    /// whisper.cpp GGUF (`ggml-<tag>.bin` from the managed directory).
    Whisper,
    /// Moonshine GGUF through transcribe.cpp (weights from the models
    /// table, resolved under `gguf_dir`).
    Moonshine,
}

impl LocalBackend {
    /// Backend name as the models table spells it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            LocalBackend::Whisper => "whisper",
            LocalBackend::Moonshine => "moonshine",
        }
    }
}

/// One servable local model: weights path, serving tag, and which
/// backend decodes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAsr {
    /// Resolved weights file.
    pub path: std::path::PathBuf,
    /// Serving tag (names the remedy and the log line).
    pub tag: String,
    /// Backend that decodes it.
    pub backend: LocalBackend,
}

/// Resolve the whisper weights file to decode with: the role side
/// first (heads in order, then the configured `default` tag), then
/// the explicitly configured `stt.model` leaf as the legacy
/// fallback. Returns the serving tag alongside the path so callers
/// name what actually served. Nothing servable fails loudly naming
/// the pull remedy.
pub fn resolve_whisper_model(
    config: &orator::OratorConfig,
) -> Result<(std::path::PathBuf, String), String> {
    resolve_local_asr_with(config, &[LocalBackend::Whisper]).map(|found| (found.path, found.tag))
}

/// Resolve the moonshine weights file to decode with, same walk as
/// [`resolve_whisper_model`] but serving moonshine-backed tags.
/// Nothing servable fails loudly naming the file.
pub fn resolve_moonshine_model(
    config: &orator::OratorConfig,
) -> Result<(std::path::PathBuf, String), String> {
    resolve_local_asr_with(config, &[LocalBackend::Moonshine]).map(|found| (found.path, found.tag))
}

/// Notice when local ASR serves past an unservable role side: the
/// heads were consulted and skipped, so the legacy leaf closes the
/// turn on words the roles did not pick. Same swap message as the
/// historical head-over-leaf notice when the tags differ; `None`
/// when nothing was bypassed (no role guidance, or the head serves
/// as listed). Pure so the decision pins without stderr capture.
#[must_use]
pub fn local_asr_serve_note(
    found_tag: &str,
    leaf: &str,
    heads_skipped: &[String],
) -> Option<String> {
    if found_tag != leaf {
        Some(format!(
            "orator: {found_tag} serves ASR locally instead of {leaf}"
        ))
    } else if heads_skipped.is_empty() {
        None
    } else {
        Some(format!(
            "orator: {found_tag} serves ASR locally (role side unservable: {})",
            heads_skipped.join(", ")
        ))
    }
}

/// Resolve the local ASR model to decode with: the role side first
/// (heads in order, then the configured `default` tag), then the
/// explicitly configured `stt.model` leaf as the legacy fallback.
/// Role order decides across backends — a present moonshine head
/// beats a whisper head listed later. Returns path, tag, and the
/// decoding backend so callers name what actually served.
pub fn resolve_local_asr(config: &orator::OratorConfig) -> Result<LocalAsr, String> {
    resolve_local_asr_with(config, &[LocalBackend::Whisper, LocalBackend::Moonshine])
}

fn resolve_local_asr_with(
    config: &orator::OratorConfig,
    allowed: &[LocalBackend],
) -> Result<LocalAsr, String> {
    let allows = |name: &str| match name {
        "whisper" => allowed.contains(&LocalBackend::Whisper),
        "moonshine" => allowed.contains(&LocalBackend::Moonshine),
        _ => false,
    };
    let mut tried = Vec::new();
    // Role preference order, then the configured default model    // (the near-complete fallback serves when a role list is empty
    // or resolves to nothing servable). A leaf listed among the
    // heads serves at its head position; an unlisted leaf is
    // deferred to the legacy block below so heads always decide
    // first.
    let mut ordered: Vec<&str> = config.roles.asr.models.iter().map(String::as_str).collect();
    if !config.default.is_empty() && !ordered.iter().any(|tag| *tag == config.default) {
        ordered.push(config.default.as_str());
    }
    let leaf_listed = ordered.iter().any(|tag| *tag == config.stt.model);
    // One candidate evaluation: the table backend decides the path
    // scheme (whisper's managed ggml vs the table weights), and only
    // allowed backends are tried — the rest are named skips.
    let evaluate = |tag: &str, backend: Option<&str>, tried: &mut Vec<String>| {
        let backend = backend.map_or_else(|| "unknown".to_string(), str::to_string);
        match backend.as_str() {
            name if allows(name) => {
                let resolved = if name == "whisper" {
                    match whisper_model_path(tag) {
                        Ok(path) => {
                            return Some(LocalAsr {
                                path,
                                tag: tag.to_string(),
                                backend: LocalBackend::Whisper,
                            })
                        }
                        Err(_) => format!("{tag} (weights missing)"),
                    }
                } else {
                    match moonshine_model_path(config, tag) {
                        Ok(path) => {
                            return Some(LocalAsr {
                                path,
                                tag: tag.to_string(),
                                backend: LocalBackend::Moonshine,
                            })
                        }
                        Err(note) => note,
                    }
                };
                tried.push(resolved);
                None
            }
            "unknown" => {
                tried.push(format!("{tag} (no spec)"));
                None
            }
            other => {
                tried.push(format!("{tag} (no local {other} backend)"));
                None
            }
        }
    };
    for tag in ordered {
        let backend = config
            .models
            .models
            .get(tag)
            .and_then(|spec| spec.primary_backend());
        if let Some(found) = evaluate(tag, backend, &mut tried) {
            if let Some(note) = local_asr_serve_note(&found.tag, &config.stt.model, &[]) {
                eprintln!("{note}");
            }
            return Ok(found);
        }
    }
    // Legacy fallback: the explicitly configured tag serves when the
    // whole role side serves nothing (skipped when already listed —
    // every candidate is tried exactly once). A consulted-but-empty
    // role side names itself so the operator sees the leaf closing
    // past bypassed heads instead of a silent swap.
    if !leaf_listed {
        let backend = config.models.models.get(&config.stt.model).map_or(
            config.stt.backend.as_str(),
            |spec| {
                spec.primary_backend()
                    .unwrap_or(config.stt.backend.as_str())
            },
        );
        if let Some(found) = evaluate(&config.stt.model.clone(), Some(backend), &mut tried) {
            if let Some(note) = local_asr_serve_note(&found.tag, &config.stt.model, &tried) {
                eprintln!("{note}");
            }
            return Ok(found);
        }
    }
    let only = |backend: LocalBackend| allowed.len() == 1 && allowed[0] == backend;
    let (missing, remedy) = if only(LocalBackend::Moonshine) {
        (
            "moonshine",
            format!(
                "place the moonshine weights under {}/handy-computer/ or start the voice server",
                config.gguf_dir
            ),
        )
    } else if only(LocalBackend::Whisper) {
        (
            "whisper",
            "fetch one with `orator models pull` or start the voice server".to_string(),
        )
    } else {
        (
            "ASR",
            "fetch one with `orator models pull` or start the voice server".to_string(),
        )
    };
    Err(format!(
        "orator: no local {missing} weights serve ASR (tried: {}); {remedy}",
        tried.join(", ")
    ))
}

/// Resolve the moonshine weights file for `tag` through the models
/// table: the declared `weights` joined under `gguf_dir` (absolute
/// passes through), present on disk. Absence names the file with
/// the placement remedy; an undeclared tag fails the same way.
///
/// The Ok path returns the file; the Err path returns the exact
/// tried-list note so the role walk names every candidate.
pub fn moonshine_model_path(
    config: &orator::OratorConfig,
    tag: &str,
) -> Result<std::path::PathBuf, String> {
    match orator::resolve_model_weights(&config.models, &config.gguf_dir, tag) {
        Some(path) if path.exists() => Ok(path),
        Some(path) => Err(format!("{tag} (weights missing at {})", path.display())),
        None => Err(format!("{tag} (no weights declared)")),
    }
}

/// Decode 16 kHz samples through the moonshine backend at `path`,
/// joining committed text with single spaces. Moonshine is
/// English-only: there is no language to set. GPU is explicit
/// opt-in and fails loudly without a device.
pub fn decode_moonshine_samples(
    path: &std::path::Path,
    gpu: bool,
    samples: &[f32],
    abort: &StreamAbort,
) -> Result<String, String> {
    let mut backend = orator_voice::stt::MoonshineBackend::new(path.to_path_buf());
    if gpu {
        backend = backend.with_gpu();
    }
    backend
        .decode_blocking(samples, abort)
        .map(|segments| {
            segments
                .iter()
                .map(|segment| segment.text.clone())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .map_err(|error| error.to_string())
}

/// ASR head the daemon leg must not shadow: the head differs from
/// the voice model and resolves through the local whisper/moonshine
/// backends with present weights. `None` keeps the daemon-first
/// chain (the full role walk still serves the head when the daemon
/// is down). The weights check is existence only — the decode owns
/// failure naming, same as the role walk.
pub(crate) fn asr_local_head(config: &orator::OratorConfig) -> Option<LocalAsr> {
    let head = orator::resolve_role_model(&config.roles, "asr")?;
    if head == config.s2s.model {
        return None;
    }
    let backend = config.models.models.get(&head)?.primary_backend()?;
    match backend {
        "whisper" => whisper_model_path(&head).ok().map(|path| LocalAsr {
            path,
            tag: head.clone(),
            backend: LocalBackend::Whisper,
        }),
        "moonshine" => moonshine_model_path(config, &head)
            .ok()
            .map(|path| LocalAsr {
                path,
                tag: head.clone(),
                backend: LocalBackend::Moonshine,
            }),
        _ => None,
    }
}

/// Transcribe 16 kHz samples through the local backend selected by
/// the roles table: the first `asr` entry served locally with
/// present weights wins, whisper or moonshine in role order.
/// audio-server entries are skipped (the daemon path refused, which
/// is why this runs). Nothing servable fails loudly naming the
/// fetch remedy.
pub fn local_transcribe(
    samples: &[f32],
    config: &orator::OratorConfig,
    abort: &StreamAbort,
) -> Result<String, String> {
    match resolve_local_asr(config) {
        Ok(found) => {
            eprintln!(
                "orator: daemon voice unavailable; transcribing locally ({} {})",
                found.backend.name(),
                found.tag
            );
            decode_local_asr(found, samples, config, abort)
        }
        Err(error) => Err(error),
    }
}

/// Decode through one resolved local ASR backend within the
/// interactive budget. Shared by the head leg (configured model
/// first) and the fallback leg (role walk after a daemon refusal).
fn decode_local_asr(
    found: LocalAsr,
    samples: &[f32],
    config: &orator::OratorConfig,
    abort: &StreamAbort,
) -> Result<String, String> {
    // Owned up front: the budgeted closure runs on a worker
    // thread, so nothing borrowed crosses the spawn.
    let LocalAsr { path, tag, backend } = found;
    let language = resolve_asr_language(config, &tag);
    let gpu = config.stt.gpu;
    let samples = samples.to_vec();
    let abort = abort.clone();
    budgeted("ASR", LOCAL_OP_BUDGET, move || match backend {
        LocalBackend::Whisper => decode_whisper_samples(&path, &language, gpu, &samples, &abort),
        LocalBackend::Moonshine => decode_moonshine_samples(&path, gpu, &samples, &abort),
    })
}

/// Ask one question directly against the configured LLM endpoint (no
/// supervised boot: the fallback never surprises with a child).
/// Absence fails loudly naming the endpoint. The session opens
/// inside the shared runtime bridge (like `ask`): opening outside
/// would force the shared queue's worker spawn with no reactor.
pub fn local_ask(question: &str, config: &orator::OratorConfig) -> Result<String, String> {
    let llm = crate::ask::effective_llm_config(config)?;
    let (base, model) = resolve_llm_base(&llm);
    let ctx = usize::from(llm.ctx);
    let n_batch = usize::from(llm.n_batch);
    let question = question.to_string();
    common_core::runtime::block_on_shared(async move {
        if let Err(error) =
            orator_agent::llm::probe_models(&base, common_core::http::shared_http_client()).await
        {
            return Err(format!(
                "orator: no LLM server answers at {base} ({error}; start a llama-server there)"
            ));
        }
        let mut session = orator_agent::llm::HttpSession::open(&base, &model, ctx, n_batch);
        session.push_text(&question);
        match session.step(&mut orator::Sampler::default()) {
            Ok(out) => Ok(out.delta.trim_end().to_string()),
            Err(error) => Err(format!("orator: local ask failed: {error}")),
        }
    })
}

/// Synthesize text through the local kokoro backend. Missing weights
/// fail loudly naming the fetch; errors never read as silence. The
/// caller narrates the hop (head leg vs daemon fallback); this stays
/// quiet so one synth never logs twice.
pub fn local_say(
    text: &str,
    config: &orator::OratorConfig,
) -> Result<(Vec<f32>, u32), String> {
    synth_kokoro(text, config)
}

/// TTS head the daemon leg must not shadow: the head differs from the
/// voice model and its table entry serves through kokoro. The shared
/// leg already passed on it (not loaded there), so this is the
/// configured-local leg — weights presence is decided by the synth,
/// whose failure falls through to the daemon instead of failing the
/// turn outright.
pub(crate) fn tts_local_head(config: &orator::OratorConfig) -> Option<String> {
    let head = orator::resolve_role_model(&config.roles, "tts")?;
    if head == config.s2s.model {
        return None;
    }
    config
        .models
        .models
        .get(&head)
        .filter(|spec| spec.backends.iter().any(|backend| backend == "kokoro"))
        .map(|_| head)
}

/// Process-wide kokoro session cache: one resident session per
/// (weights, voices, voice, gpu). Synth runs per chunk under a
/// 10 s budget, but a fresh backend reloads hundreds of megabytes
/// of weights every time — without reuse every chunk and every leg
/// pays the load, and long chunks time out that never would on a
/// warm session. Insertion is cheap (backends load lazily on first
/// synth); the limiter inside each backend serializes its own
/// inference, and dropping the cache entry never kills in-flight
/// clones. Capped small: voices rarely switch, and a cleared entry
/// simply reloads on next use.
pub fn cached_kokoro(
    model: std::path::PathBuf,
    voices: std::path::PathBuf,
    voice: String,
    gpu: bool,
) -> std::sync::Arc<orator_voice::tts::KokoroBackend> {
    use std::collections::HashMap;
    type Key = (std::path::PathBuf, std::path::PathBuf, String, bool);
    static CACHE: std::sync::LazyLock<
        std::sync::Mutex<HashMap<Key, std::sync::Arc<orator_voice::tts::KokoroBackend>>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
    let mut cache = common_core::sync::lock(&CACHE);
    if cache.len() > 8 {
        cache.clear();
    }
    std::sync::Arc::clone(cache.entry((model, voices, voice, gpu)).or_insert_with_key(
        |(model, voices, voice, gpu)| {
            std::sync::Arc::new(if *gpu {
                orator_voice::tts::KokoroBackend::new(model.clone(), voices.clone(), voice.clone())
                    .with_gpu()
            } else {
                orator_voice::tts::KokoroBackend::new(model.clone(), voices.clone(), voice.clone())
            })
        },
    ))
}

/// Shared kokoro synth behind both local legs: the head leg voices
/// the configured model when the daemon would voice another; the
/// fallback leg voices after a daemon refusal. One synth, two logs —
/// each caller narrates its own hop.
fn synth_kokoro(text: &str, config: &orator::OratorConfig) -> Result<(Vec<f32>, u32), String> {
    let Some((model, voices)) = orator_voice::tts::default_paths() else {
        return Err(
            "orator: no managed model directory (HOME and XDG_DATA_HOME are unset)".to_string(),
        );
    };
    // Voice rides the composed TTS params (role override wins), else
    // the global leaf — the composition chain with a visible effect.
    let tts_tag = orator::resolve_role_model(&config.roles, "tts")
        .filter(|tag| config.models.models.contains_key(tag))
        .unwrap_or_default();
    let voice = resolve_tts_voice(config, &tts_tag);
    let backend = cached_kokoro(model, voices, voice, config.tts.gpu);
    let text = text.to_string();
    budgeted("TTS", LOCAL_OP_BUDGET, move || {
        backend
            .synth_blocking(&text)
            .map(|samples| (samples, orator_voice::tts::KOKORO_SAMPLE_RATE))
            .map_err(|error| format!("orator: local voice failed: {error}"))
    })
}

#[cfg(test)]
#[path = "../tests/fallback.rs"]
mod tests;
