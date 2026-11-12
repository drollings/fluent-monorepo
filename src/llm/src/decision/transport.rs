//! Remote opt-in boundary: redaction hook + audit.
//!
//! The default transport is localhost-only. A remote endpoint requires an
//! explicit `allow_remote` flag plus a caller-supplied redaction hook
//! (composing the single `anonymize` entry, never a second scrubber) and
//! emits one audit record per remote call (call id, model, host, sizes —
//! never raw state bytes). Refusals are typed `Rejected`: loud
//! misconfiguration, never a silent send and never a retryable.
//!
//! The hook, once configured, applies to every send (defense in depth);
//! localhost sends without a hook are byte-identical to the pre-boundary
//! behavior. This is the only change in send behavior, and
//! only for the previously-impossible remote path.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::decision::DecisionError;

/// `RedactHook`: scrubbed text out for raw text in.
pub type RedactHook = Arc<dyn Fn(&str) -> String + Send + Sync>;
/// `AuditSink`: one record per remote call.
pub type AuditSink = Arc<dyn Fn(DecisionAudit) + Send + Sync>;

/// The canonical redaction hook over the single `anonymize` entry.
///
/// ```rust
/// use fluent_llm::decision::anonymize_hook;
///
/// assert_eq!(anonymize_hook()("server 192.168.1.1"), "server [IP_ADDRESS]");
/// ```
#[must_use]
/// The canonical redaction hook over the single `anonymize` entry.
/// 
/// ```rust
/// use fluent_llm::decision::anonymize_hook;
/// 
/// assert_eq!(anonymize_hook()("server 192.168.1.1"), "server [IP_ADDRESS]");
/// ```
pub fn anonymize_hook() -> RedactHook {
    Arc::new(|text: &str| crate::anonymize::anonymize(text))
}

/// Endpoint capability report: does this endpoint return logprobs and honor
/// the no-think setting, or would every decision silently degrade to the
/// text fallback? Call at startup or first use after a model swap; a
/// degrading endpoint must surface here, never as quiet fallback traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Endpoint capability report: does this endpoint return logprobs and honor
/// the no-think setting, or would every decision silently degrade to the
/// text fallback? Call at startup or first use after a model swap; a
/// degrading endpoint must surface here, never as quiet fallback traffic.
pub struct DecisionProbe {
    /// `top_logprobs` entries present on the single token.
    pub logprobs: bool,
    /// How many top entries were observed.
    pub top_entries: usize,
    /// Content empty while `reasoning_content` present: the no-think
    /// setting was not honored and the token went to reasoning.
    pub reasoning_hijack: bool,
}

impl DecisionProbe {
    /// True when decisions against this endpoint would fall back to text.
    #[must_use]
    pub fn degrades_to_text(&self) -> bool {
        !self.logprobs || self.reasoning_hijack
    }

    /// Read a probe from a full chat-completion response value.
    ///
    /// ```rust
    /// use fluent_llm::decision::DecisionProbe;
    ///
    /// let body = serde_json::json!({
    ///     "choices": [{
    ///         "message": {"role": "assistant", "content": "A"},
    ///         "logprobs": {"content": [{
    ///             "token": "A", "logprob": -0.02,
    ///             "top_logprobs": [{"token": "A", "logprob": -0.02}],
    ///         }]},
    ///     }],
    /// });
    /// let probe = DecisionProbe::read(&body);
    /// assert!(probe.logprobs && !probe.degrades_to_text());
    /// ```
    #[must_use]
    pub fn read(response: &serde_json::Value) -> Self {
        let choice = response
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first());
        let content_empty = choice
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .is_none_or(|s| s.trim().is_empty());
        let reasoning = choice
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("reasoning_content"))
            .and_then(|c| c.as_str())
            .is_some_and(|s| !s.trim().is_empty());
        let top_entries = choice
            .and_then(|c| c.get("logprobs"))
            .and_then(|l| l.get("content"))
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|e| e.get("top_logprobs"))
            .and_then(|t| t.as_array())
            .map_or(0, Vec::len);
        Self {
            logprobs: top_entries > 0,
            top_entries,
            reasoning_hijack: content_empty && reasoning,
        }
    }
}

/// One audit record per remote decision call (`DecisionAudit`). Metadata only: the prompt
/// bytes never appear here, so PII cannot leak through observability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One audit record per remote decision call (`DecisionAudit`). Metadata only: the prompt
/// bytes never appear here, so PII cannot leak through observability.
pub struct DecisionAudit {
    pub call_id: String,
    pub model: String,
    pub host: String,
    pub sent_bytes: usize,
    pub redaction_applied: bool,
}

/// What `prepare` decided about one prompt (`PreparedPrompt`).
#[derive(Debug, Clone, PartialEq, Eq)]
/// What `prepare` decided about one prompt (`PreparedPrompt`).
pub struct PreparedPrompt {
    pub text: String,
    pub audited: bool,
}

/// Host of an endpoint URL, bracket-aware for IPv6 literals (the shared
/// extractor is bracket-naive by documented contract and would cut
/// `[::1]` down to `[`).
fn endpoint_host(base_url: &str) -> &str {
    if let Some(rest) = base_url.split("://").nth(1) {
        if let Some(after) = rest.strip_prefix('[') {
            if let Some(end) = after.find(']') {
                return &after[..end];
            }
        }
    }
    crate::url::extract_host(base_url)
}

/// Exact-loopback identity for the send boundary: only canonical loopback
/// forms (plus numeric `127/8` quads) send unredacted. The shared
/// SSRF-range class treats any `127.`-prefixed name as loopback, so a DNS
/// name like `127.0.0.1.evil.example` (attacker-resolved, not loopback)
/// would pass — the decision boundary must not trust DNS structure.
fn is_exact_loopback(host: &str) -> bool {
    let normalized = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_lowercase();
    if normalized == "localhost" || normalized == "127.0.0.1" || normalized == "::1" {
        return true;
    }
    let parts: Vec<&str> = normalized.split('.').collect();
    parts.len() == 4
        && parts[0] == "127"
        && parts[1..]
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Localhost-only by default; remote requires explicit opt-in.
#[derive(Clone, Default)]
/// Localhost-only by default; remote requires explicit opt-in.
pub struct DecisionTransport {
    allow_remote: bool,
    redact: Option<RedactHook>,
    audit: Option<AuditSink>,
}

impl DecisionTransport {
    /// Localhost-only, no hook, no audit sink.
    #[must_use]
    pub fn local_only() -> Self {
        Self::default()
    }

    /// Opt in to remote endpoints (still requires a redaction hook to send).
    ///
    /// ```rust
    /// use fluent_llm::decision::DecisionTransport;
    ///
    /// let local = DecisionTransport::local_only();
    /// assert!(local.prepare("http://127.0.0.1:8080", "m", "hi".to_string()).is_ok());
    /// assert!(local.prepare("https://llm.example.invalid", "m", "hi".to_string()).is_err());
    /// ```
    #[must_use]
    pub fn with_allow_remote(mut self, allow: bool) -> Self {
        self.allow_remote = allow;
        self
    }

    /// Redaction hook applied to every send once configured.
    #[must_use]
    pub fn with_redact(mut self, hook: RedactHook) -> Self {
        self.redact = Some(hook);
        self
    }

    /// Audit sink for remote calls.
    #[must_use]
    pub fn with_audit(mut self, sink: AuditSink) -> Self {
        self.audit = Some(sink);
        self
    }

    /// Redact without auditing: the caller audits once per whole send.
    ///
    /// ```rust
    /// use fluent_llm::decision::{anonymize_hook, DecisionTransport};
    ///
    /// let transport = DecisionTransport::local_only().with_redact(anonymize_hook());
    /// assert_eq!(transport.redact_text("call 10.0.0.1"), "call [IP_ADDRESS]");
    /// ```
    #[must_use]
    pub fn redact_text(&self, text: &str) -> String {
        match &self.redact {
            Some(hook) => hook(text),
            None => text.to_string(),
        }
    }

    /// Gate one prompt for `base_url`: refuse remote without opt-in (and
    /// without a hook), redact when a hook is configured, audit remote sends.
    pub fn prepare(
        &self,
        base_url: &str,
        model: &str,
        prompt: String,
    ) -> Result<PreparedPrompt, DecisionError> {
        let host = endpoint_host(base_url).to_string();
        let remote = !is_exact_loopback(&host);
        if remote && !self.allow_remote {
            return Err(DecisionError::Rejected(format!(
                "remote decision endpoint refused without explicit opt-in: {host}"
            )));
        }
        if remote && self.redact.is_none() {
            return Err(DecisionError::Rejected(format!(
                "remote decision endpoint refused without a redaction hook: {host}"
            )));
        }
        let text = match &self.redact {
            Some(hook) => hook(&prompt),
            None => prompt,
        };
        if remote {
            let record = DecisionAudit {
                call_id: common_core::hash::uuid_v4(),
                model: model.to_string(),
                host,
                sent_bytes: text.len(),
                redaction_applied: self.redact.is_some(),
            };
            if let Some(sink) = &self.audit {
                sink(record);
            }
            Ok(PreparedPrompt {
                text,
                audited: true,
            })
        } else {
            Ok(PreparedPrompt {
                text,
                audited: false,
            })
        }
    }
}