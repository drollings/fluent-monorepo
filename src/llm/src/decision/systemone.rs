//! Native `/v1/systemone` decision transport.
//!
//! The endpoint is a bounded classifier, not a text generator: it accepts a
//! shared `state` plus named `questions` of type `choice`, `score`, or
//! `noul`, and returns probabilities/confidence. This backend maps that
//! wire shape onto the existing `DecideBackend` seam so decision points never
//! send chat-completion JSON to a non-generative model.

use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use serde_json::{json, Value};

use crate::decision::backend::{BatchFuture, DecideBackend, DecideRequest};
use crate::decision::transport::DecisionTransport;
use crate::decision::{Decision, DecisionError, DecisionKind};

/// Native systemone decision backend.
#[derive(Clone)]
/// Native systemone decision backend.
pub struct SystemOneBackend {
    base_url: String,
    transport: DecisionTransport,
}

impl std::fmt::Debug for SystemOneBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemOneBackend")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl SystemOneBackend {
    /// Build a backend for one endpoint. The endpoint must be loopback unless
    /// the caller installs an explicit remote transport.
    #[must_use]
    pub fn new(base_url: impl Into<String>, transport: DecisionTransport) -> Self {
        Self {
            base_url: base_url.into(),
            transport,
        }
    }

    /// Native endpoint URL, accepting both `host` and `host/v1` bases.
    #[must_use]
    pub fn endpoint(base_url: &str) -> String {
        let base = base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/systemone")
        } else {
            format!("{base}/v1/systemone")
        }
    }

    async fn post(&self, requests: &[DecideRequest]) -> Result<Value, DecisionError> {
        if requests.is_empty() {
            return Err(DecisionError::Rejected("empty decision batch".to_string()));
        }
        let first = &requests[0];
        let prepared = self.transport.prepare(
            &self.base_url,
            &first.model,
            format!("{}\n{}", first.prompt, first.state),
        )?;
        let state = if first.state.is_empty() {
            prepared.text.clone()
        } else {
            prepared.text
        };
        let body = batch_body(&state, requests);
        let url = Self::endpoint(&self.base_url);
        let response = common_core::http::shared_http_client()
            .post(&url)
            .timeout(std::time::Duration::from_millis(first.timeout_ms.max(1)))
            .json(&body)
            .send()
            .await
            .map_err(|error| DecisionError::Unavailable(error.to_string()))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_IMPLEMENTED
            || status == reqwest::StatusCode::METHOD_NOT_ALLOWED
        {
            return Err(DecisionError::Rejected(
                "systemone endpoint unsupported (model is not a decision model)".to_string(),
            ));
        }
        if status.is_server_error() {
            return Err(DecisionError::Unavailable(format!(
                "systemone HTTP {status}"
            )));
        }
        if !status.is_success() {
            return Err(DecisionError::Rejected(format!("systemone HTTP {status}")));
        }
        response
            .json::<Value>()
            .await
            .map_err(|error| DecisionError::Decode(format!("systemone body: {error}")))
    }
}

impl DecideBackend for SystemOneBackend {
    fn decide(
        &self,
        request: DecideRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Decision, DecisionError>> + Send + '_>> {
        Box::pin(async move {
            let started = Instant::now();
            let body = self.post(std::slice::from_ref(&request)).await?;
            let answer = body
                .get("answers")
                .and_then(|answers| answers.get("q1"))
                .ok_or_else(|| DecisionError::Decode("missing answers.q1".to_string()))?;
            decision_from_answer(answer, &request, started.elapsed().as_millis() as u64)
        })
    }

    fn decide_batch(&self, requests: Vec<DecideRequest>) -> BatchFuture<'_> {
        Box::pin(async move {
            if requests.len() <= 1 {
                let mut out = Vec::with_capacity(requests.len());
                for request in requests {
                    out.push(self.decide(request).await);
                }
                return out;
            }
            // The endpoint shares one state across questions. Preserve the
            // sequential path when states differ rather than merging
            // unrelated contexts into one prompt.
            let first_state = requests[0].state.clone();
            if requests.iter().any(|request| request.state != first_state) {
                let mut out = Vec::with_capacity(requests.len());
                for request in requests {
                    out.push(self.decide(request).await);
                }
                return out;
            }
            let started = Instant::now();
            let body = match self.post(&requests).await {
                Ok(body) => body,
                Err(error) => return requests.iter().map(|_| Err(error.clone())).collect(),
            };
            let Some(answers) = body.get("answers").and_then(Value::as_object) else {
                return requests
                    .iter()
                    .map(|_| Err(DecisionError::Decode("missing answers object".to_string())))
                    .collect();
            };
            requests
                .iter()
                .enumerate()
                .map(|(index, request)| {
                    let answer = answers.get(&format!("q{}", index + 1)).ok_or_else(|| {
                        DecisionError::Decode(format!("missing answers.q{}", index + 1))
                    })?;
                    decision_from_answer(answer, request, started.elapsed().as_millis() as u64)
                })
                .collect()
        })
    }
}

/// Native endpoint support, reported by [`SystemOneBackend::probe`].
/// A 501/405 means the model behind the endpoint cannot judge; that is
/// `Unsupported`, never a decode error. Transport failures are
/// `Unreachable` so callers do not cache a transient outage as permanent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemOneSupport {
    Supported,
    Unsupported { reason: String },
    Unreachable { reason: String },
}

impl SystemOneBackend {
    /// Probe native decision support with one minimal `noul` question.
    /// Reuses the guarded POST path; no second HTTP client.
    pub async fn probe(&self, model: &str) -> SystemOneSupport {
        let request = DecideRequest {
            prompt: "probe".to_string(),
            state: "probe".to_string(),
            question: String::new(),
            options: Vec::new(),
            kind: DecisionKind::Noul,
            version: 0,
            timeout_ms: 2000,
            model: model.to_string(),
        };
        match self.post(std::slice::from_ref(&request)).await {
            Ok(_) => SystemOneSupport::Supported,
            Err(DecisionError::Rejected(reason)) => SystemOneSupport::Unsupported { reason },
            Err(error) => SystemOneSupport::Unreachable {
                reason: error.to_string(),
            },
        }
    }
}

/// Build the batched wire body. Pure and testable.
#[must_use]
/// Build the batched wire body. Pure and testable.
pub fn batch_body(state: &str, requests: &[DecideRequest]) -> Value {
    let questions: serde_json::Map<String, Value> = requests
        .iter()
        .enumerate()
        .map(|(index, request)| (format!("q{}", index + 1), question_body(request)))
        .collect();
    json!({
        "model": requests.first().map_or("", |request| request.model.as_str()),
        "state": state,
        "questions": questions,
    })
}

fn question_body(request: &DecideRequest) -> Value {
    let instructions = if request.question.trim().is_empty() {
        request.prompt.clone()
    } else {
        request.question.clone()
    };
    match request.kind {
        DecisionKind::Choice => {
            let options: Vec<&str> = request
                .options
                .iter()
                .map(|option| option.label.as_str())
                .collect();
            let criteria: serde_json::Map<String, Value> = request
                .options
                .iter()
                .map(|option| {
                    let description = if option.description.trim().is_empty() {
                        option.label.clone()
                    } else {
                        option.description.clone()
                    };
                    (option.label.clone(), Value::String(description))
                })
                .collect();
            json!({
                "type": "choice",
                "instructions": instructions,
                "options": options,
                "criteria": criteria,
            })
        }
        DecisionKind::Score => {
            let criteria: Vec<Value> = if request.options.len() >= 2 {
                request
                    .options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| {
                        let score = option.label.parse::<f64>().unwrap_or(index as f64);
                        json!({
                            "score": score,
                            "description": if option.description.trim().is_empty() {
                                option.label.clone()
                            } else {
                                option.description.clone()
                            },
                        })
                    })
                    .collect()
            } else {
                vec![
                    json!({"score": 0, "description": "irrelevant"}),
                    json!({"score": 50, "description": "partial"}),
                    json!({"score": 100, "description": "relevant"}),
                ]
            };
            json!({
                "type": "score",
                "instructions": instructions,
                "criteria": criteria,
            })
        }
        DecisionKind::Noul => json!({
            "type": "noul",
            "instructions": instructions,
        }),
    }
}

/// Map one native answer to the shared `Decision`. Pure.
pub fn decision_from_answer(
    answer: &Value,
    request: &DecideRequest,
    latency_ms: u64,
) -> Result<Decision, DecisionError> {
    let kind = answer
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(request.kind.as_str());
    match kind {
        "choice" => {
            let label = answer
                .get("choice")
                .and_then(Value::as_str)
                .ok_or_else(|| DecisionError::Decode("choice has no label".to_string()))?;
            let distribution = probabilities(answer)?;
            Decision::from_distribution(
                label,
                distribution,
                request.model.clone(),
                request.version,
                latency_ms,
                false,
            )
        }
        "score" => {
            let label = answer
                .get("score")
                .and_then(Value::as_f64)
                .map(|score| score.to_string())
                .ok_or_else(|| DecisionError::Decode("score has no value".to_string()))?;
            let mut distribution = Vec::new();
            if let Some(probabilities) = answer.get("probabilities").and_then(Value::as_object) {
                let legend = answer.get("legend").and_then(Value::as_object);
                for (key, probability) in probabilities {
                    let Some(probability) = probability.as_f64() else {
                        continue;
                    };
                    let label = legend
                        .and_then(|legend| legend.get(key))
                        .and_then(|entry| entry.get("score"))
                        .and_then(Value::as_f64)
                        .map_or_else(|| key.clone(), |score| score.to_string());
                    distribution.push((label, probability));
                }
            }
            if !distribution
                .iter()
                .any(|(candidate, _)| candidate == &label)
            {
                distribution.push((
                    label.clone(),
                    answer
                        .get("confidence")
                        .and_then(Value::as_f64)
                        .unwrap_or(1.0),
                ));
            }
            Decision::from_distribution(
                &label,
                distribution,
                request.model.clone(),
                request.version,
                latency_ms,
                false,
            )
        }
        "noul" => {
            let p_no = answer
                .get("noul")
                .and_then(Value::as_f64)
                .ok_or_else(|| DecisionError::Decode("noul has no probability".to_string()))?;
            let label = if p_no >= 0.5 { "No" } else { "Yes" };
            Decision::from_distribution(
                label,
                vec![
                    ("No".to_string(), p_no.clamp(0.0, 1.0)),
                    ("Yes".to_string(), (1.0 - p_no).clamp(0.0, 1.0)),
                ],
                request.model.clone(),
                request.version,
                latency_ms,
                false,
            )
        }
        other => Err(DecisionError::Decode(format!(
            "unknown systemone answer type: {other}"
        ))),
    }
}

fn probabilities(answer: &Value) -> Result<Vec<(String, f64)>, DecisionError> {
    let object = answer
        .get("probabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| DecisionError::Decode("choice has no probabilities".to_string()))?;
    let distribution: Vec<(String, f64)> = object
        .iter()
        .filter_map(|(label, probability)| {
            probability
                .as_f64()
                .map(|probability| (label.clone(), probability))
        })
        .collect();
    if distribution.is_empty() {
        return Err(DecisionError::Decode(
            "choice probabilities are empty".to_string(),
        ));
    }
    Ok(distribution)
}

#[cfg(test)]
#[path = "../../tests/decision_systemone.rs"]
mod systemone_tests;
