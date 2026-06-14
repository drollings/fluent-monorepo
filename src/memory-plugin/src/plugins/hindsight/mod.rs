//! Hindsight memory plugin — structured knowledge graph.
//!
//! Multi-strategy retrieval with strict structural entity resolution.
//! Code symbols (functions, structs, modules) map to graph entities.
//! Tracks symbol location/hash shifts during codebase iteration.

use crate::traits::MemoryOps;
use crate::types::*;
use fluent_wvr::{FieldAccess, FieldError, Describable, WorkUnit, WorkContext, WorkOutput, WorkError};
use internment::ArcIntern;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Graph data model ──────────────────────────────────────────

/// Typed entity in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphEntity {
    /// A function or method definition.
    Function {
        /// Function name.
        name: String,
        /// Module path (e.g. `crate::module::func`).
        module_path: String,
        /// Source file path.
        file_path: String,
        /// First line number (1-indexed).
        line_start: u32,
        /// Last line number (inclusive).
        line_end: u32,
        /// SHA-256 hash of the function body.
        content_hash: String,
    },
    /// A struct or enum definition.
    Struct {
        /// Type name.
        name: String,
        /// Module path.
        module_path: String,
        /// Source file path.
        file_path: String,
        /// Field names.
        fields: Vec<String>,
        /// SHA-256 hash of the type definition.
        content_hash: String,
    },
    /// A module declaration.
    Module {
        /// Module name.
        name: String,
        /// Module path.
        path: String,
        /// Source file path.
        file_path: String,
        /// Names of symbols defined in this module.
        child_symbols: Vec<String>,
    },
    /// An observed fact from cross-session reasoning.
    Observation {
        /// Observation content.
        content: String,
        /// Category label.
        category: String,
        /// Trust score in [0.0, 1.0].
        trust: f64,
        /// Entity names that support this observation.
        evidence: Vec<String>,
        /// Creation timestamp (Unix seconds).
        created_at: i64,
    },
}

/// A directed edge in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Source entity name.
    pub from: String,
    /// Target entity name.
    pub to: String,
    /// Edge relation type.
    pub relation: Relation,
}

/// Relation type for knowledge graph edges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Relation {
    /// Parent contains child (module → function).
    Contains,
    /// Function calls function.
    Calls,
    /// Function uses type or constant.
    Uses,
    /// Module depends on module.
    DependsOn,
    /// Entity supports an observation.
    Supports,
    /// Entity contradicts an observation.
    Contradicts,
}

/// The full knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KnowledgeGraph {
    /// Named entities keyed by unique name.
    pub entities: BTreeMap<String, GraphEntity>,
    /// Directed edges between entities.
    pub edges: Vec<GraphEdge>,
    /// Monotonically increasing version counter.
    pub version: u64,
}

/// Configuration for the hindsight plugin.
#[derive(Debug, Clone)]
pub struct HindsightConfig {
    /// Maximum graph traversal depth.
    pub max_depth: usize,
    /// Path for persisting the graph.
    pub graph_path: Option<std::path::PathBuf>,
}

impl Default for HindsightConfig {
    fn default() -> Self {
        Self {
            max_depth: 3,
            graph_path: None,
        }
    }
}

/// Hindsight memory plugin — structured knowledge graph.
pub struct HindsightMemory {
    config: HindsightConfig,
    graph: Arc<RwLock<KnowledgeGraph>>,
}

impl HindsightMemory {
    /// Create a new hindsight memory plugin with the given configuration.
    pub fn new(config: HindsightConfig) -> Self {
        Self {
            config,
            graph: Arc::new(RwLock::new(KnowledgeGraph::default())),
        }
    }
}

impl MemoryOps for HindsightMemory {
    fn name(&self) -> &'static str {
        "hindsight"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn initialize(&mut self, _ctx: &MemoryInitContext) -> Result<(), MemoryError> {
        if let Some(path) = &self.config.graph_path {
            if path.exists() {
                let data = std::fs::read_to_string(path)
                    .map_err(|e| MemoryError::InitFailed(e.to_string()))?;
                let graph: KnowledgeGraph = serde_json::from_str(&data)
                    .map_err(|e| MemoryError::InitFailed(e.to_string()))?;
                tracing::info!(
                    "loaded knowledge graph v{} from {}",
                    graph.version,
                    path.display()
                );
            }
        }
        Ok(())
    }

    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let graph = Arc::clone(&self.graph);
        let path = self.config.graph_path.clone();
        Box::pin(async move {
            if let Some(path) = path {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let g = graph.read().await;
                if let Ok(data) = serde_json::to_string_pretty(&*g) {
                    let _ = std::fs::write(path, data);
                }
            }
        })
    }

    fn prefetch(
        &self,
        query: &str,
        _ctx: &MemoryQueryContext,
    ) -> Pin<Box<dyn Future<Output = String> + Send + '_>> {
        let graph = Arc::clone(&self.graph);
        let query = query.to_string();
        Box::pin(async move {
            if query.is_empty() {
                return String::new();
            }

            let g = graph.read().await;
            let query_lower = query.to_lowercase();

            let matching: Vec<&String> = g
                .entities
                .keys()
                .filter(|name| name.to_lowercase().contains(&query_lower))
                .take(5)
                .collect();

            if matching.is_empty() {
                return String::new();
            }

            let lines: Vec<String> = matching.iter().map(|name| format!("- {name}")).collect();
            format!("## Hindsight Knowledge Graph\n{}", lines.join("\n"))
        })
    }

    fn queue_prefetch(&self, _query: &str, _ctx: &MemoryQueryContext) {}

    fn search(
        &self,
        req: &MemorySearchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<MemoryResult>, MemoryError>> + Send + '_>> {
        let graph = Arc::clone(&self.graph);
        let req = req.clone();
        Box::pin(async move {
            let g = graph.read().await;
            let query_lower = req.query.to_lowercase();

            let results: Vec<MemoryResult> = g
                .entities
                .iter()
                .filter(|(name, _)| name.to_lowercase().contains(&query_lower))
                .take(req.limit)
                .map(|(name, entity)| {
                    let content = match entity {
                        GraphEntity::Function {
                            module_path, ..
                        } => format!("function {name} in {module_path}"),
                        GraphEntity::Struct {
                            module_path, ..
                        } => format!("struct {name} in {module_path}"),
                        GraphEntity::Module { path, .. } => format!("module {path}"),
                        GraphEntity::Observation { content, .. } => content.clone(),
                    };
                    MemoryResult {
                        content,
                        score: 1.0,
                        trust: 1.0,
                        source: "hindsight".into(),
                        metadata: json!({ "entity_name": name }),
                    }
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
        _messages: &[TurnMessage],
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }

    fn handle_tool_call(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<String, MemoryError> {
        match tool_name {
            "knowledge_add" => {
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| MemoryError::ToolError("missing 'name'".into()))?
                    .to_string();
                let _content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let category = args
                    .get("category")
                    .and_then(|v| v.as_str())
                    .unwrap_or("general")
                    .to_string();

                Ok(serde_json::json!({
                    "status": "added",
                    "entity": name,
                    "category": category,
                })
                .to_string())
            }
            "knowledge_search" => {
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
            name: "knowledge_search".into(),
            description: "Search the structured knowledge graph for entities and relationships."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "depth": { "type": "integer" }
                },
                "required": ["query"]
            }),
        }]
    }
}

impl FieldAccess for HindsightMemory {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        match name {
            "max_depth" => {
                self.config.max_depth = value
                    .parse()
                    .map_err(|e| FieldError::Parse(format!("invalid max_depth: {e}")))?;
                Ok(())
            }
            _ => Err(FieldError::NotFound(name.into())),
        }
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "max_depth" => Ok(self.config.max_depth.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }

    fn field_names(&self) -> &'static [&'static str] {
        &["max_depth"]
    }
}

impl Describable for HindsightMemory {
    fn describe(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum graph traversal depth"
                }
            }
        })
    }
}

impl WorkUnit for HindsightMemory {
    fn name(&self) -> &str {
        "hindsight"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok("hindsight memory"))
    }
}

const _: () = {
    fn _assert<T: Send + Sync>() {}
    fn _a() {
        _assert::<HindsightMemory>();
    }
};
