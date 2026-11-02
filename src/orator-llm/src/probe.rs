//! Boot health probe: `GET {base}/models` with a 6 s bound and a
//! `+/v1` retry, merging nothing else. Unhealthy endpoints keep sessions
//! degraded instead of failing turns.

use std::time::Duration;

use thiserror::Error;

/// Bound per probe attempt. Loopback answers in milliseconds; a silent
/// endpoint is reported, never awaited.
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);

/// What the endpoint serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeInfo {
    /// Served model ids, in server order.
    pub models: Vec<String>,
    /// Whether the answer came from the `/v1` retry.
    pub via_v1_fallback: bool,
}

/// Probe failures. Refusals and timeouts are one variant: both mean the
/// endpoint is not there.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// Nothing answered (refused, timed out, transport failed).
    #[error("endpoint unreachable at {0}: {1}")]
    Unreachable(String, String),
    /// The endpoint answered but not with a model list.
    #[error("endpoint at {0} answered {1}")]
    BadStatus(String, String),
}

/// Probe `base` for its model list: `GET {base}/models`, then
/// `GET {base}/v1/models` when the first misses.
pub async fn probe_models(base: &str, client: &reqwest::Client) -> Result<ProbeInfo, ProbeError> {
    match probe_once(base, "/models", client).await {
        Ok(models) => Ok(ProbeInfo {
            models,
            via_v1_fallback: false,
        }),
        Err(ProbeError::Unreachable(_, _)) => {
            Err(ProbeError::Unreachable(base.to_string(), "connection failed".to_string()))
        }
        Err(ProbeError::BadStatus(_, _)) => match probe_once(base, "/v1/models", client).await {
            Ok(models) => Ok(ProbeInfo {
                models,
                via_v1_fallback: true,
            }),
            Err(error) => Err(error),
        },
    }
}

async fn probe_once(
    base: &str,
    path: &str,
    client: &reqwest::Client,
) -> Result<Vec<String>, ProbeError> {
    let url = format!("{base}{path}");
    let response = tokio::time::timeout(PROBE_TIMEOUT, client.get(&url).send())
        .await
        .map_err(|_| {
            ProbeError::Unreachable(base.to_string(), "probe timed out".to_string())
        })?
        .map_err(|error| ProbeError::Unreachable(base.to_string(), error.to_string()))?;
    if !response.status().is_success() {
        return Err(ProbeError::BadStatus(
            base.to_string(),
            format!("GET {path} → {}", response.status()),
        ));
    }
    let body: serde_json::Value = response.json().await.map_err(|error| {
        ProbeError::BadStatus(base.to_string(), format!("unparseable model list: {error}"))
    })?;
    Ok(model_ids(&body))
}

/// Model ids from an OpenAI-style `{data: [{id}]}` list. Entries without
/// an id are skipped, never fatal.
fn model_ids(body: &serde_json::Value) -> Vec<String> {
    body.get("data")
        .and_then(|data| data.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("id")?.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
