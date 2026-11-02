//! Audio endpoint assurance: use the configured endpoint when it
//! serves the audio model, else boot a supervised one (the `ask`
//! precedent). Supervised servers run the full audio bundle on a
//! free loopback port and stop with their caller.

use orator_core::AudioBundle;

use crate::session::S2sError;
use crate::voice::{
    audio_supervisor_config, resolve_audio_server, AUDIO_SERVER_BIN, AUDIO_SERVER_ENV,
};

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
        supervisor: Box<orator_llm::Supervisor>,
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

/// Ensure `model` is served at `base`: probe `/models` (then
/// `/v1/models`); when absent and the bundle is complete, boot a
/// supervised audio server (backbone + mmproj) and return it owned.
pub async fn ensure_audio_server(
    base: &str,
    model: &str,
    bundle: &AudioBundle,
) -> Result<AudioEndpoint, S2sError> {
    if serves_model(base, model).await {
        return Ok(AudioEndpoint::Shared(base.to_string()));
    }
    if !bundle.present() {
        return Err(S2sError::Backend(format!(
            "endpoint at {base} does not serve '{model}' and the bundle is incomplete"
        )));
    }
    boot_audio_server(bundle, model).await
}

/// Boot a supervised audio server unconditionally: the bundle over
/// the audio binary on a free loopback port, readied by the
/// completions `OPTIONS` probe. The caller owns the handle and
/// releases it when done.
pub async fn boot_audio_server(
    bundle: &AudioBundle,
    model: &str,
) -> Result<AudioEndpoint, S2sError> {
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
    let binary = resolve_audio_server().ok_or_else(|| {
        S2sError::Backend(format!(
            "no voice server (set {AUDIO_SERVER_ENV} or install {AUDIO_SERVER_BIN}; search hits that fail the ownership check are refused)"
        ))
    })?;
    let mut config = audio_supervisor_config(bundle, 0);
    config.model = model.to_string();
    match orator_llm::boot_with_binary(config, &binary).await {
        Ok(supervisor) => {
            let base_url = supervisor.base_url().to_string();
            Ok(AudioEndpoint::Owned {
                base_url,
                supervisor: Box::new(supervisor),
            })
        }
        Err(error) => Err(S2sError::Backend(error.to_string())),
    }
}

/// Model ids listed at `{base}/models`, then `{base}/v1/models`
/// (unioned). Empty when the endpoint won't say. One parser for the
/// assurance check and the session's error hint, fronted by a bounded
/// cache: at most one `/models` fetch per endpoint per TTL. Staleness
/// is bounded by TTL *or* one failed turn, whichever comes first —
/// every turn still verifies by task-value, so a stale hint costs at
/// most one owned boot, never a wrong success.
pub(crate) async fn listed_model_ids(base: &str) -> Vec<String> {
    if let Some(cached) = hint_cache_get(base) {
        return cached;
    }
    let ids = fetch_listed_model_ids(base).await;
    hint_cache_put(base, ids.clone());
    ids
}

/// Whether `GET {base}/models` (or `/v1/models`) lists `model`.
pub(crate) async fn serves_model(base: &str, model: &str) -> bool {
    listed_model_ids(base).await.iter().any(|id| id == model)
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
        let listed: Vec<String> = body
            .get("data")
            .and_then(|data| data.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.get("id")?.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for id in listed {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

#[cfg(test)]
#[path = "../tests/ensure.rs"]
mod tests;
