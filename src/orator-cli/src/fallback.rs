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
use orator_core::LlmSession;

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
    let Some(dir) = orator_core::models_dir() else {
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
    let mut backend = orator_stt::WhisperBackend::new(path.to_path_buf(), language.to_string());
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
pub fn resolve_asr_language(config: &orator_core::OratorConfig, tag: &str) -> String {
    orator_core::compose_role_params(&config.models, &config.roles, "asr", tag)
        .get("language")
        .and_then(|value| value.as_str())
        .map_or_else(|| config.stt.language.clone(), str::to_string)
}

/// Voice for `tag` serving the TTS role: the composed `voice`
/// override wins, else the global TTS leaf. Pure (no weights, no
/// I/O) so the override is unit-testable.
#[must_use]
pub fn resolve_tts_voice(config: &orator_core::OratorConfig, tag: &str) -> String {
    orator_core::compose_role_params(&config.models, &config.roles, "tts", tag)
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
    config: &orator_core::OratorConfig,
) -> Result<String, String> {
    fallback_if_unavailable(
        crate::tui::live_ask(text, session, transport, abort),
        || local_ask(text, config),
    )
}

/// Voice one reply with daemon-first, local-second routing: the
/// daemon voices through its server; on voice-unavailable refusals
/// the reply synthesizes locally (kokoro, the TTS role default).
pub fn say_dial(
    text: &str,
    transport: Transport,
    abort: &StreamAbort,
    config: &orator_core::OratorConfig,
) -> Result<(Vec<f32>, u32), String> {
    fallback_if_unavailable(crate::tui::live_say(text, transport, abort), || {
        local_say(text, config)
    })
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
    config: &orator_core::OratorConfig,
) -> Result<String, String> {
    fallback_if_unavailable(
        crate::tui::transcribe_samples(samples, transport, abort),
        || local_transcribe(samples, config, abort),
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
    config: &orator_core::OratorConfig,
) -> Result<(std::path::PathBuf, String), String> {
    resolve_local_asr_with(config, &[LocalBackend::Whisper]).map(|found| (found.path, found.tag))
}

/// Resolve the moonshine weights file to decode with, same walk as
/// [`resolve_whisper_model`] but serving moonshine-backed tags.
/// Nothing servable fails loudly naming the file.
pub fn resolve_moonshine_model(
    config: &orator_core::OratorConfig,
) -> Result<(std::path::PathBuf, String), String> {
    resolve_local_asr_with(config, &[LocalBackend::Moonshine]).map(|found| (found.path, found.tag))
}

/// Resolve the local ASR model to decode with: the role side first
/// (heads in order, then the configured `default` tag), then the
/// explicitly configured `stt.model` leaf as the legacy fallback.
/// Role order decides across backends — a present moonshine head
/// beats a whisper head listed later. Returns path, tag, and the
/// decoding backend so callers name what actually served.
pub fn resolve_local_asr(config: &orator_core::OratorConfig) -> Result<LocalAsr, String> {
    resolve_local_asr_with(config, &[LocalBackend::Whisper, LocalBackend::Moonshine])
}

fn resolve_local_asr_with(
    config: &orator_core::OratorConfig,
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
            if found.tag != config.stt.model {
                eprintln!(
                    "orator: {} serves ASR locally instead of {}",
                    found.tag, config.stt.model
                );
            }
            return Ok(found);
        }
    }
    // Legacy fallback: the explicitly configured tag serves when the
    // whole role side serves nothing (skipped when already listed —
    // every candidate is tried exactly once).
    if !leaf_listed {
        let backend = config.models.models.get(&config.stt.model).map_or(
            config.stt.backend.as_str(),
            |spec| {
                spec.primary_backend()
                    .unwrap_or(config.stt.backend.as_str())
            },
        );
        if let Some(found) = evaluate(&config.stt.model.clone(), Some(backend), &mut tried) {
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
    config: &orator_core::OratorConfig,
    tag: &str,
) -> Result<std::path::PathBuf, String> {
    match orator_core::resolve_model_weights(&config.models, &config.gguf_dir, tag) {
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
    let mut backend = orator_stt::MoonshineBackend::new(path.to_path_buf());
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

/// Transcribe 16 kHz samples through the local backend selected by
/// the roles table: the first `asr` entry served locally with
/// present weights wins, whisper or moonshine in role order.
/// audio-server entries are skipped (the daemon path refused, which
/// is why this runs). Nothing servable fails loudly naming the
/// fetch remedy.
pub fn local_transcribe(
    samples: &[f32],
    config: &orator_core::OratorConfig,
    abort: &StreamAbort,
) -> Result<String, String> {
    match resolve_local_asr(config) {
        Ok(found) => {
            // Owned up front: the budgeted closure runs on a worker
            // thread, so nothing borrowed crosses the spawn.
            let LocalAsr { path, tag, backend } = found;
            eprintln!(
                "orator: daemon voice unavailable; transcribing locally ({} {})",
                backend.name(),
                tag
            );
            let language = resolve_asr_language(config, &tag);
            let gpu = config.stt.gpu;
            let samples = samples.to_vec();
            let abort = abort.clone();
            budgeted("ASR", LOCAL_OP_BUDGET, move || match backend {
                LocalBackend::Whisper => {
                    decode_whisper_samples(&path, &language, gpu, &samples, &abort)
                }
                LocalBackend::Moonshine => decode_moonshine_samples(&path, gpu, &samples, &abort),
            })
        }
        Err(error) => Err(error),
    }
}

/// Ask one question directly against the configured LLM endpoint (no
/// supervised boot: the fallback never surprises with a child).
/// Absence fails loudly naming the endpoint. The session opens
/// inside the shared runtime bridge (like `ask`): opening outside
/// would force the shared queue's worker spawn with no reactor.
pub fn local_ask(question: &str, config: &orator_core::OratorConfig) -> Result<String, String> {
    let llm = crate::ask::effective_llm_config(config)?;
    let (base, model) = resolve_llm_base(&llm);
    let ctx = usize::from(llm.ctx);
    let n_batch = usize::from(llm.n_batch);
    let question = question.to_string();
    common_core::runtime::block_on_shared(async move {
        if let Err(error) =
            orator_llm::probe_models(&base, common_core::http::shared_http_client()).await
        {
            return Err(format!(
                "orator: no LLM server answers at {base} ({error}; start a llama-server there)"
            ));
        }
        let mut session = orator_llm::HttpSession::open(&base, &model, ctx, n_batch);
        session.push_text(&question);
        match session.step(&mut orator_core::Sampler::default()) {
            Ok(out) => Ok(out.delta.trim_end().to_string()),
            Err(error) => Err(format!("orator: local ask failed: {error}")),
        }
    })
}

/// Synthesize text through the local kokoro backend. Missing weights
/// fail loudly naming the fetch; errors never read as silence.
pub fn local_say(
    text: &str,
    config: &orator_core::OratorConfig,
) -> Result<(Vec<f32>, u32), String> {
    let Some((model, voices)) = orator_tts::default_paths() else {
        return Err(
            "orator: no managed model directory (HOME and XDG_DATA_HOME are unset)".to_string(),
        );
    };
    eprintln!("orator: daemon voice unavailable; synthesizing locally (kokoro)");
    // Voice rides the composed TTS params (role override wins), else
    // the global leaf — the composition chain with a visible effect.
    let tts_tag = orator_core::resolve_role_model(&config.roles, "tts")
        .filter(|tag| config.models.models.contains_key(tag))
        .unwrap_or_default();
    let voice = resolve_tts_voice(config, &tts_tag);
    let backend = if config.tts.gpu {
        orator_tts::KokoroBackend::new(model, voices, voice).with_gpu()
    } else {
        orator_tts::KokoroBackend::new(model, voices, voice)
    };
    let text = text.to_string();
    budgeted("TTS", LOCAL_OP_BUDGET, move || {
        backend
            .synth_blocking(&text)
            .map(|samples| (samples, orator_tts::KOKORO_SAMPLE_RATE))
            .map_err(|error| format!("orator: local voice failed: {error}"))
    })
}

#[cfg(test)]
#[path = "../tests/fallback.rs"]
mod tests;
