//! Audio endpoint assurance: use the configured endpoint when it
//! serves the audio model, else boot a supervised one (the `ask`
//! precedent). Supervised servers run the full audio bundle on a
//! free loopback port and stop with their caller.

use orator::AudioBundle;

use crate::s2s::session::S2sError;
use crate::s2s::voice::{AUDIO_SERVER_BIN, AUDIO_SERVER_ENV};

/// An assured audio endpoint: shared (use as-is) or owned (stop on
/// release by the caller).
pub enum AudioEndpoint {
    /// The configured endpoint serves the model; use it directly.
    Shared(String),
    /// A supervised server booted for this caller.
    Owned {
        /// Base URL of the fresh server.
        base_url: String,
        /// Supervisor handle; `stop()` releases the server.
        supervisor: Box<orator_agent::llm::Supervisor>,
    },
}

impl AudioEndpoint {
    /// Base URL to dial.
    #[must_use]
    pub fn base_url(&self) -> &str {
        match self {
            Self::Shared(url) => url,
            Self::Owned { base_url, .. } => base_url,
        }
    }

    /// Release an owned server. Shared endpoints need nothing.
    pub fn release(&self) {
        if let Self::Owned { supervisor, .. } = self {
            supervisor.stop();
        }
    }
}

/// Ensure `model` is served at `base`: shared when the endpoint
/// lists the model or answers the completions readiness probe (the
/// audio build serves no `/models`, so its readiness is the
/// assurance); otherwise boot a supervised audio server (backbone +
/// mmproj) and return it owned — but only when `auto_boot` opts in.
/// With auto-boot off, an unserved model fails loudly naming the
/// remedy (start the server, or set `s2s.auto_boot`).
pub async fn ensure_audio_server(
    base: &str,
    model: &str,
    bundle: &AudioBundle,
    auto_boot: bool,
    gpu: bool,
) -> Result<AudioEndpoint, S2sError> {
    if serves_model(base, model).await {
        return Ok(AudioEndpoint::Shared(base.to_string()));
    }
    if !auto_boot {
        return Err(S2sError::Backend(format!(
            "endpoint at {base} does not serve '{model}' (start a llama-server there, or set s2s.auto_boot to boot one automatically)"
        )));
    }
    if !bundle.present() {
        return Err(S2sError::Backend(format!(
            "endpoint at {base} does not serve '{model}' and the bundle is incomplete"
        )));
    }
    // Opt-in supervised boot: the operator asked for a child in the
    // file, so the candidate walk below is requested, never ambient.
    eprintln!("orator: supervised voice boot requested (s2s.auto_boot=on); trying candidates");
    boot_audio_server(bundle, model, auto_boot, gpu).await
}

/// Boot a supervised audio server unconditionally: the bundle over
/// the audio binary on a free loopback port, readied by the
/// completions `OPTIONS` probe. Every candidate binary is tried in
/// order until one serves: a pre-LFM3 install fails fast on the
/// bundle and the next serves. The caller owns the handle and
/// releases it when done. With `auto_boot` off this refuses before
/// spawning (the `ensure` gate above is the usual caller; direct
/// callers pass their own leave).
pub async fn boot_audio_server(
    bundle: &AudioBundle,
    model: &str,
    auto_boot: bool,
    gpu: bool,
) -> Result<AudioEndpoint, S2sError> {
    if !auto_boot {
        return Err(S2sError::Backend(
            "voice auto-boot is off (set s2s.auto_boot to boot a supervised server)".to_string(),
        ));
    }
    if gpu {
        orator::gpu::require_gpu("llama-server").map_err(S2sError::Backend)?;
    }
    if !bundle.present() {
        return Err(S2sError::Backend(format!(
            "audio bundle incomplete: {}",
            bundle
                .missing()
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let candidates = crate::s2s::voice::resolve_audio_server_candidates();
    if candidates.is_empty() {
        return Err(S2sError::Backend(format!(
            "no voice server (set {AUDIO_SERVER_ENV} or install {AUDIO_SERVER_BIN}; search hits that fail the ownership check are refused)"
        )));
    }
    match fluent_wvr::ladder::first_accept_in_order(
        candidates,
        |binary: std::path::PathBuf| {
            let mut config = crate::s2s::voice::audio_supervisor_config(bundle, 0);
            config.model = model.to_string();
            if gpu {
                config.extra_args.extend([
                    "--n-gpu-layers".to_string(),
                    orator_agent::llm::GPU_OFFLOAD_LAYERS.to_string(),
                ]);
            }
            async move {
                match orator_agent::llm::boot_with_binary(config, &binary).await {
                    Ok(supervisor) => {
                        let base_url = supervisor.base_url().to_string();
                        Ok(Some(AudioEndpoint::Owned {
                            base_url,
                            supervisor: Box::new(supervisor),
                        }))
                    }
                    Err(error) => {
                        eprintln!(
                            "orator: voice candidate {} failed ({error}); trying the next",
                            binary.display()
                        );
                        Err(error.to_string())
                    }
                }
            }
        },
        |_: &String| false,
    )
    .await
    {
        Ok(Some(endpoint)) => Ok(endpoint),
        Ok(None) => Err(S2sError::Backend(
            "every voice server candidate failed (last: )".to_string(),
        )),
        Err(last_error) => Err(S2sError::Backend(format!(
            "every voice server candidate failed (last: {last_error})"
        ))),
    }
}

/// Model ids listed at `{base}/models`, then `{base}/v1/models`
/// (unioned). Empty when the endpoint won't say. One parser for the
/// assurance check and the session's error hint, fronted by a bounded
/// cache: at most one `/models` fetch per endpoint per TTL. Staleness
/// is bounded by TTL *or* one failed turn, whichever comes first —
/// every turn still verifies by task-value, so a stale hint costs at
/// most one owned boot, never a wrong success.
///
/// M12-S4 audit (keep-decision): this stays a hand-rolled TTL map
/// instead of `common_core::cache::LoadCache`. `LoadCache` has no TTL
/// expiry — its staleness bound is LRU capacity, not time — so a
/// migration would trade "stale ≤ TTL" for "stale until evicted" and
/// change the serving-hint contract; its `load` is also sync while
/// the fetch is two async HTTP calls. Revisit if `LoadCache` gains
/// TTL expiry.
pub(crate) async fn listed_model_ids(base: &str) -> Vec<String> {
    if let Some(cached) = hint_cache_get(base) {
        return cached;
    }
    let ids = fetch_listed_model_ids(base).await;
    hint_cache_put(base, ids.clone());
    ids
}

/// Whether `base` serves audio-capable turns for `model`: a `/models`
/// listing wins first (text gateways name their models); otherwise the
/// completions `OPTIONS` readiness decides (the audio build serves no
/// `/models`, so readiness is the assurance). Listing is producer
/// confidence for gateways; a served turn stays the task-value proof.
pub async fn serves_model(base: &str, model: &str) -> bool {
    if listed_model_ids(base).await.iter().any(|id| id == model) {
        return true;
    }
    audio_build_ready(base).await
}

/// Readiness of an appliance endpoint: 2xx on `OPTIONS
/// /v1/chat/completions`, the same probe the supervisor boots against.
/// Single attempt, short timeout; never cached (readiness is cheap
/// and changes with the server).
async fn audio_build_ready(base: &str) -> bool {
    let url = format!("{}/v1/chat/completions", base.trim_end_matches('/'));
    common_core::http::shared_http_client()
        .request(reqwest::Method::OPTIONS, &url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

/// Whether `base` currently serves turns for `model` from a loaded
/// instance: the `/v1/models` entry exists with a loaded status.
/// Listing presence alone is NOT enough — the preset lists every
/// discovered GGUF, including architectures this build cannot load
/// (kokoro, moonshine); routing role heads at merely-listed models
/// would capture turns for instances that can never serve.
/// Unreachable endpoints and unknown shapes answer false (fall
/// through to the configured backend, never a boot).
pub async fn model_loaded(base: &str, model: &str) -> bool {
    let url = format!("{}/v1/models", base.trim_end_matches('/'));
    let Ok(response) = common_core::http::shared_http_client()
        .get(&url)
        .timeout(std::time::Duration::from_secs(6))
        .send()
        .await
    else {
        return false;
    };
    let Ok(body) = response.json::<serde_json::Value>().await else {
        return false;
    };
    let Some(entries) = body.get("data").and_then(|data| data.as_array()) else {
        return false;
    };
    entries.iter().any(|entry| {
        entry.get("id").and_then(|id| id.as_str()) == Some(model)
            && model_status_loaded(entry.get("status"))
    })
}

/// Whether a `/v1/models` status shape reads as loaded: the router
/// object shape (`{"value": "loaded", ...}`) or a bare `"loaded"`
/// string. Anything else (unloaded, loading, absent) is not serving.
fn model_status_loaded(status: Option<&serde_json::Value>) -> bool {
    match status {
        Some(serde_json::Value::String(state)) => state == "loaded",
        Some(serde_json::Value::Object(map)) => {
            map.get("value").and_then(|value| value.as_str()) == Some("loaded")
        }
        _ => false,
    }
}

/// Best-effort serving hint for error messages: which model ids the
/// endpoint lists. Empty when the endpoint won't say.
pub(crate) async fn served_models_hint(base: &str) -> String {
    let ids = listed_model_ids(base).await;
    if ids.is_empty() {
        String::new()
    } else {
        format!("; endpoint serves: [{}]", ids.join(", "))
    }
}

/// Drop the cached hint for `base`. Call on any turn failure so a
/// swapped model is detected at most one failed turn after the swap.
pub(crate) fn invalidate_hint_cache(base: &str) {
    let mut cache = common_core::sync::lock(&HINT_CACHE);
    cache.remove(base);
}

#[cfg(test)]
const HINT_TTL: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(not(test))]
const HINT_TTL: std::time::Duration = std::time::Duration::from_secs(300);

static HINT_CACHE: std::sync::LazyLock<HintCache> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

type HintCache =
    std::sync::Mutex<std::collections::HashMap<String, (Vec<String>, std::time::Instant)>>;

fn hint_cache_get(base: &str) -> Option<Vec<String>> {
    let cache = common_core::sync::lock(&HINT_CACHE);
    let (ids, at) = cache.get(base)?;
    if at.elapsed() < HINT_TTL {
        Some(ids.clone())
    } else {
        None
    }
}

fn hint_cache_put(base: &str, ids: Vec<String>) {
    let mut cache = common_core::sync::lock(&HINT_CACHE);
    cache.insert(base.to_string(), (ids, std::time::Instant::now()));
}

async fn fetch_listed_model_ids(base: &str) -> Vec<String> {
    let client = common_core::http::shared_http_client();
    let mut ids = Vec::new();
    for path in ["/models", "/v1/models"] {
        let url = format!("{}{path}", base.trim_end_matches('/'));
        let response = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(6))
            .send()
            .await;
        let Ok(response) = response else {
            continue;
        };
        let Ok(body) = response.json::<serde_json::Value>().await else {
            continue;
        };
        let listed = fluent_llm::openai::model_ids(&body);
        for id in listed {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

#[cfg(test)]
#[path = "../../tests/s2s_ensure.rs"]
mod tests;
