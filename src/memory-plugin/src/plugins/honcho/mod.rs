//! Honcho memory plugin — cross-session reasoning analyzer.
//!
//! Behavioral memory: compiles cross-session summaries, tracks reasoning
//! paths, maintains persistent conclusions about developer iterations.

use crate::traits::MemoryOps;
use crate::types::*;
use fluent_wvr::{FieldAccess, FieldError, Describable, WorkUnit, WorkContext, WorkOutput, WorkError};
use internment::ArcIntern;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

/// A compiled summary of one interaction session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Unique session identifier.
    pub session_id: String,
    /// Session start time (Unix seconds).
    pub started_at: i64,
    /// Session end time (Unix seconds), `None` if still active.
    pub ended_at: Option<i64>,
    /// Number of turns in the session.
    pub turn_count: usize,
    /// Extracted topic labels.
    pub topics: Vec<String>,
    /// Conclusions reached during this session.
    pub decisions: Vec<Decision>,
    /// Reasoning paths taken across turns.
    pub reasoning_paths: Vec<ReasoningPath>,
}

/// A conclusion reached during a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    /// The conclusion statement.
    pub statement: String,
    /// Confidence score in [0.0, 1.0].
    pub confidence: f64,
    /// Session IDs that provide evidence for this decision.
    pub evidence_sessions: Vec<String>,
    /// Session IDs with conflicting evidence.
    pub contradicted_by: Vec<String>,
}

/// A reasoning path tracked across turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningPath {
    /// Human-readable description of the reasoning.
    pub description: String,
    /// Ordered steps taken.
    pub steps: Vec<String>,
    /// Final outcome of the reasoning path.
    pub outcome: String,
}

/// Configuration for the honcho plugin.
#[derive(Debug, Clone)]
pub struct HonchoConfig {
    /// Maximum sessions to retain in memory.
    pub max_sessions: usize,
    /// Minimum confidence threshold for decisions.
    pub min_confidence: f64,
}

impl Default for HonchoConfig {
    fn default() -> Self {
        Self {
            max_sessions: 100,
            min_confidence: 0.5,
        }
    }
}

/// Honcho memory plugin — cross-session reasoning analyzer.
pub struct HonchoMemory {
    config: HonchoConfig,
    sessions: Arc<Mutex<Vec<SessionSummary>>>,
}

impl HonchoMemory {
    /// Create a new honcho memory plugin with the given configuration.
    pub fn new(config: HonchoConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl MemoryOps for HonchoMemory {
    fn name(&self) -> &'static str {
        "honcho"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn initialize(&mut self, _ctx: &MemoryInitContext) -> Result<(), MemoryError> {
        Ok(())
    }

    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }

    fn prefetch(
        &self,
        query: &str,
        _ctx: &MemoryQueryContext,
    ) -> Pin<Box<dyn Future<Output = String> + Send + '_>> {
        let sessions = Arc::clone(&self.sessions);
        let query = query.to_string();
        Box::pin(async move {
            if query.is_empty() {
                return String::new();
            }

            let sessions = sessions.lock().await;
            let query_lower = query.to_lowercase();

            let matching: Vec<&Decision> = sessions
                .iter()
                .flat_map(|s| &s.decisions)
                .filter(|d| d.statement.to_lowercase().contains(&query_lower))
                .take(3)
                .collect();

            if matching.is_empty() {
                return String::new();
            }

            let lines: Vec<String> = matching
                .iter()
                .map(|d| format!("- [{:.0}%] {}", d.confidence * 100.0, d.statement))
                .collect();

            format!("## Honcho Cross-Session Reasoning\n{}", lines.join("\n"))
        })
    }

    fn queue_prefetch(&self, _query: &str, _ctx: &MemoryQueryContext) {}

    fn search(
        &self,
        req: &MemorySearchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<MemoryResult>, MemoryError>> + Send + '_>> {
        let sessions = Arc::clone(&self.sessions);
        let req = req.clone();
        Box::pin(async move {
            let sessions = sessions.lock().await;
            let query_lower = req.query.to_lowercase();

            let results: Vec<MemoryResult> = sessions
                .iter()
                .flat_map(|s| &s.decisions)
                .filter(|d| d.statement.to_lowercase().contains(&query_lower))
                .take(req.limit)
                .map(|d| MemoryResult {
                    content: d.statement.clone(),
                    score: d.confidence,
                    trust: d.confidence,
                    source: "honcho".into(),
                    metadata: json!({
                        "evidence_sessions": d.evidence_sessions,
                        "contradicted_by": d.contradicted_by,
                    }),
                })
                .collect();

            Ok(results)
        })
    }

    fn sync_turn(
        &self,
        _user_content: &str,
        _assistant_content: &str,
        _ctx: &MemoryQueryContext,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }

    fn on_session_end(
        &self,
        messages: &[TurnMessage],
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let sessions = Arc::clone(&self.sessions);
        let max_sessions = self.config.max_sessions;
        let messages: Vec<TurnMessage> = messages.to_vec();
        Box::pin(async move {
            if messages.is_empty() {
                return;
            }

            let session_id = format!("sess_{}", simple_now());
            let turn_count = messages.len();

            let topics: Vec<String> = messages
                .iter()
                .filter(|m| m.role == "user")
                .filter(|m| m.content.len() > 20)
                .take(5)
                .map(|m| {
                    let words: Vec<&str> = m.content.split_whitespace().take(4).collect();
                    words.join(" ")
                })
                .collect();

            let summary = SessionSummary {
                session_id,
                started_at: simple_now(),
                ended_at: Some(simple_now()),
                turn_count,
                topics,
                decisions: Vec::new(),
                reasoning_paths: Vec::new(),
            };

            let mut sessions = sessions.lock().await;
            sessions.push(summary);

            if sessions.len() > max_sessions {
                let drain_count = sessions.len() - max_sessions;
                sessions.drain(..drain_count);
            }
        })
    }

    fn handle_tool_call(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<String, MemoryError> {
        match tool_name {
            "reasoning_search" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                Ok(serde_json::json!({
                    "results": [],
                    "query": query,
                })
                .to_string())
            }
            _ => Err(MemoryError::ToolError(format!(
                "unknown tool: {tool_name}"
            ))),
        }
    }

    fn tool_schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "reasoning_search".into(),
            description: "Search cross-session reasoning and decisions.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"]
            }),
        }]
    }
}

impl FieldAccess for HonchoMemory {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        match name {
            "max_sessions" => {
                self.config.max_sessions = value
                    .parse()
                    .map_err(|e| FieldError::Parse(format!("invalid max_sessions: {e}")))?;
                Ok(())
            }
            "min_confidence" => {
                self.config.min_confidence = value
                    .parse()
                    .map_err(|e| FieldError::Parse(format!("invalid min_confidence: {e}")))?;
                Ok(())
            }
            _ => Err(FieldError::NotFound(name.into())),
        }
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "max_sessions" => Ok(self.config.max_sessions.to_string()),
            "min_confidence" => Ok(self.config.min_confidence.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }

    fn field_names(&self) -> &'static [&'static str] {
        &["max_sessions", "min_confidence"]
    }
}

impl Describable for HonchoMemory {
    fn describe(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "max_sessions": {
                    "type": "integer",
                    "description": "Maximum sessions to retain"
                },
                "min_confidence": {
                    "type": "number",
                    "description": "Minimum confidence for decisions"
                }
            }
        })
    }
}

impl WorkUnit for HonchoMemory {
    fn name(&self) -> &str {
        "honcho"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok("honcho memory"))
    }
}

/// Simple timestamp without chrono dependency.
fn simple_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

const _: () = {
    fn _assert<T: Send + Sync>() {}
    fn _a() {
        _assert::<HonchoMemory>();
    }
};
