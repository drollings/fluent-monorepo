//! MCP (Model Context Protocol) server for guidance.
//!
//! JSON-RPC 2.0 over STDIO, exposing guidance's search capabilities
//! as MCP tools for AI coding assistants.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common_core::jsonrpc::{JsonRpcError, JsonRpcHandler, JsonRpcRequest, JsonRpcResponse};
use guidance_core::memory::MemoryBridge;
use guidance_core::query::db_storage::GuidanceDbStorage;
use guidance_core::query::hybrid::plan_from_query;
use guidance_core::query::ingest::LazyNlp;
use guidance_core::query::recall::run_recall;
use guidance_core::query::strategy::FsmEngine;
use guidance_core::search_types::{
    CodeSymbolType, FragmentContent, FragmentSpan, SearchHit, SearchMatchedBy, SearchPlan,
    SearchPlanRoute, SearchPlanRouteMode,
};
use guidance_core::sync_engine::SyncEngine;
use search_vector::GuidanceDb;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum McpError {
    #[error("IO error: {0}")]
    Io(#[from] common_core::error::IoError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database error: {0}")]
    Db(String),
}

impl From<std::io::Error> for McpError {
    fn from(e: std::io::Error) -> Self {
        McpError::Io(common_core::error::IoError(e))
    }
}

pub struct McpServer {
    db: Arc<GuidanceDb>,
    memory: Option<MemoryBridge>,
    workspace: Option<PathBuf>,
    json_dir: Option<PathBuf>,
    toolset: Toolset,
}

/// MCP toolset: `agent` is search-only, `full` adds lifecycle/status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Toolset {
    /// Search-only (`guidance_explain`).
    Agent,
    /// Search + status/lifecycle (`guidance_explain`, `guidance_status`).
    #[default]
    Full,
}

impl Toolset {
    /// Parse the `--toolset` flag value.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "agent" => Ok(Self::Agent),
            "full" => Ok(Self::Full),
            _ => Err(format!("unknown toolset '{value}' (agent|full)")),
        }
    }
}

/// Freshness contract: `eventual` serves the index as-is (background
/// default); `wait_for_fresh` re-checks staleness first and refreshes when
/// `auto_update` is set and the workspace is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreshnessMode {
    /// Serve as-is; report the observed state.
    #[default]
    Eventual,
    /// Re-check (and optionally refresh) before serving.
    WaitForFresh,
}

/// Observed index freshness, rendered as the `freshness:` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// No stale files observed.
    Fresh,
    /// Stale files observed (served as-is).
    PossiblyStale,
    /// No workspace configured — staleness is unknowable.
    Unknown,
}

impl Freshness {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::PossiblyStale => "possibly_stale",
            Self::Unknown => "unknown",
        }
    }
}

/// Coverage vocabulary: how the result set was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Ranked recall sample (fused index search).
    RankedSample,
    /// Exhaustive `rg` sweep (every match returned).
    RgExhaustive,
    /// `rg` sweep cut at the limit (more matches may exist).
    RgTruncated,
}

impl Coverage {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RankedSample => "ranked_sample",
            Self::RgExhaustive => "rg_exhaustive",
            Self::RgTruncated => "rg_truncated",
        }
    }
}

/// Input bounds (P5 toolset contract).
pub const MAX_QUERY_CHARS: usize = 4000;
pub const MAX_PATH_FILTERS: usize = 128;
pub const MAX_GROUPS: usize = 32;
pub const MAX_LIMIT: usize = 50;

/// Parsed `guidance_explain` arguments.
#[derive(Debug)]
pub struct ExplainParams {
    /// One or more queries (each ≤ [`MAX_QUERY_CHARS`], at most [`MAX_GROUPS`]).
    pub queries: Vec<String>,
    /// Lexical-only route.
    pub fts: bool,
    /// Vector-only route.
    pub vector: bool,
    /// Result limit after clamping to [`MAX_LIMIT`].
    pub limit: usize,
    /// Glob filters.
    pub globs: Vec<String>,
    /// Workspace-relative path filters.
    pub paths: Vec<String>,
    /// File-type filters.
    pub file_types: Vec<String>,
    /// Symbol-type filters (closed 6-set).
    pub symbol_types: Vec<CodeSymbolType>,
    /// Prefer symbol definitions.
    pub prefer_symbol: bool,
    /// Modification-time floor/ceiling (ms epoch).
    pub mtime_after: Option<i64>,
    /// Modification-time ceiling (ms epoch).
    pub mtime_before: Option<i64>,
    /// Freshness contract.
    pub freshness: FreshnessMode,
    /// Refresh stale files before serving (only with `wait_for_fresh`).
    pub auto_update: bool,
    /// Attach depth-1 graph context lines (explicit user intent; off
    /// by default so the response shape is unchanged unless asked).
    pub context: bool,
}

fn arg_str(arguments: &serde_json::Value, name: &str) -> Option<String> {
    arguments
        .get(name)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn arg_bool(arguments: &serde_json::Value, name: &str) -> bool {
    arguments
        .get(name)
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn arg_str_list(arguments: &serde_json::Value, name: &str) -> Vec<String> {
    arguments
        .get(name)
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse and bound `guidance_explain` arguments (pure; no I/O).
pub fn parse_explain_params(arguments: &serde_json::Value) -> Result<ExplainParams, String> {
    let mut queries = arg_str_list(arguments, "queries");
    if let Some(query) = arg_str(arguments, "query") {
        queries.insert(0, query);
    }
    if queries.is_empty() {
        return Err("query is required".to_string());
    }
    if queries.len() > MAX_GROUPS {
        return Err(format!("at most {MAX_GROUPS} queries per call"));
    }
    for query in &queries {
        if query.trim().is_empty() {
            return Err("query is required".to_string());
        }
        if query.len() > MAX_QUERY_CHARS {
            return Err(format!("query exceeds {MAX_QUERY_CHARS} characters"));
        }
    }
    let fts = arg_bool(arguments, "fts");
    let vector = arg_bool(arguments, "vector");
    let fuse = arg_bool(arguments, "fuse");
    if [fts, vector, fuse].iter().filter(|flag| **flag).count() > 1 {
        return Err("only one of fts, vector, fuse may be set".to_string());
    }
    let limit = arguments
        .get("limit")
        .and_then(|value| value.as_u64())
        .unwrap_or(10) as usize;
    let limit = limit.clamp(1, MAX_LIMIT);
    let globs = arg_str_list(arguments, "globs");
    let paths = arg_str_list(arguments, "paths");
    if globs.len() + paths.len() > MAX_PATH_FILTERS {
        return Err(format!("at most {MAX_PATH_FILTERS} path filters per call"));
    }
    let file_types = arg_str_list(arguments, "fileTypes");
    let mut symbol_types = Vec::new();
    for name in arg_str_list(arguments, "symbolTypes") {
        symbol_types.push(parse_mcp_symbol_type(&name)?);
    }
    let prefer_symbol = arg_bool(arguments, "preferSymbol");
    let mtime_after = arguments.get("mtimeAfter").and_then(|value| value.as_i64());
    let mtime_before = arguments
        .get("mtimeBefore")
        .and_then(|value| value.as_i64());
    if mtime_after.is_some_and(|value| value < 0) || mtime_before.is_some_and(|value| value < 0) {
        return Err("mtime filters must be non-negative ms epoch".to_string());
    }
    let freshness = match arg_str(arguments, "freshness").as_deref() {
        None | Some("eventual") => FreshnessMode::Eventual,
        Some("wait_for_fresh") => FreshnessMode::WaitForFresh,
        Some(other) => {
            return Err(format!(
                "unknown freshness '{other}' (eventual|wait_for_fresh)"
            ))
        }
    };
    Ok(ExplainParams {
        queries,
        fts,
        vector,
        limit,
        globs,
        paths,
        file_types,
        symbol_types,
        prefer_symbol,
        mtime_after,
        mtime_before,
        freshness,
        auto_update: arg_bool(arguments, "autoUpdate"),
        context: arg_bool(arguments, "context"),
    })
}

/// Parse an MCP `symbolTypes` entry against the closed 6-set.
pub fn parse_mcp_symbol_type(value: &str) -> Result<CodeSymbolType, String> {
    match value {
        "module" => Ok(CodeSymbolType::Module),
        "class" => Ok(CodeSymbolType::Class),
        "interface" => Ok(CodeSymbolType::Interface),
        "function" => Ok(CodeSymbolType::Function),
        "value" => Ok(CodeSymbolType::Value),
        "alias" => Ok(CodeSymbolType::Alias),
        _ => Err(format!(
            "unknown symbol type '{value}' (module|class|interface|function|value|alias)"
        )),
    }
}

/// Build one recall plan per query (fused by default; single-route with
/// `fts`/`vector`). Pure.
pub fn build_explain_plans(params: &ExplainParams) -> Vec<SearchPlan> {
    params
        .queries
        .iter()
        .map(|query| {
            if params.fts || params.vector {
                SearchPlan {
                    routes: vec![SearchPlanRoute {
                        mode: if params.fts {
                            SearchPlanRouteMode::Fts
                        } else {
                            SearchPlanRouteMode::Vector
                        },
                        query: query.clone(),
                    }],
                    limit: Some(params.limit),
                    trace: false,
                    prefer_symbol: Some(params.prefer_symbol),
                    symbol_types: (!params.symbol_types.is_empty())
                        .then(|| params.symbol_types.clone()),
                    include_paths: (!params.paths.is_empty()).then(|| params.paths.clone()),
                    globs: (!params.globs.is_empty()).then(|| params.globs.clone()),
                    file_types: (!params.file_types.is_empty()).then(|| params.file_types.clone()),
                    modified_after: params.mtime_after,
                    modified_before: params.mtime_before,
                    ..Default::default()
                }
            } else {
                let mut fsm = FsmEngine::new();
                let intent = fsm.run(query).intent;
                let mut plan = plan_from_query(query, intent, params.limit);
                plan.prefer_symbol = Some(params.prefer_symbol);
                plan.symbol_types =
                    (!params.symbol_types.is_empty()).then(|| params.symbol_types.clone());
                plan.include_paths = (!params.paths.is_empty()).then(|| params.paths.clone());
                plan.globs = (!params.globs.is_empty()).then(|| params.globs.clone());
                plan.file_types =
                    (!params.file_types.is_empty()).then(|| params.file_types.clone());
                plan.modified_after = params.mtime_after;
                plan.modified_before = params.mtime_before;
                plan
            }
        })
        .collect()
}

fn matched_by_name(matched_by: &SearchMatchedBy) -> &'static str {
    match matched_by {
        SearchMatchedBy::Fts => "fts",
        SearchMatchedBy::Vector => "vector",
        SearchMatchedBy::FtsAndVector => "fts+vector",
    }
}

fn range_suffix(range: &FragmentSpan) -> Option<String> {
    match range {
        FragmentSpan::Text {
            start_line,
            end_line,
            ..
        } => Some(format!("{start_line}-{end_line}")),
        FragmentSpan::File
        | FragmentSpan::Byte { .. }
        | FragmentSpan::Page { .. }
        | FragmentSpan::PageText { .. }
        | FragmentSpan::PageRegion { .. } => None,
    }
}

fn evidence_text(content: &FragmentContent) -> Option<String> {
    match content {
        FragmentContent::Text { text } => {
            let first = text.lines().next().unwrap_or("").trim();
            (!first.is_empty()).then(|| truncate_chars(first, 200))
        }
        FragmentContent::Image { .. } => None,
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

/// Display title for a hit: symbol name, section heading, or entity id.
/// Shared with the CLI explain table (one title rule, two renderers).
#[must_use]
pub fn hit_title(hit: &SearchHit) -> String {
    for evidence in &hit.evidence {
        if let Some(metadata) = &evidence.metadata {
            match metadata {
                guidance_core::search_types::EntityMetadata::Code { symbol_name, .. } => {
                    if let Some(name) = symbol_name {
                        return name.clone();
                    }
                }
                guidance_core::search_types::EntityMetadata::Markdown { heading, .. } => {
                    if let Some(heading) = heading {
                        return heading.clone();
                    }
                }
            }
        }
    }
    hit.entity.id.clone()
}

/// Render fused hits with `path:start-end` ranges, `matched:` lines, and
/// `matchedBy` provenance (pure). Every fused hit carries its `file:line`
/// citation — search-then-verify is taught by the format and enforced by
/// the grounding layer before synthesis.
pub fn render_explain(
    hits: &[SearchHit],
    limit: usize,
    freshness: Freshness,
    coverage: Coverage,
    note: Option<&str>,
) -> String {
    let mut out = format!(
        "freshness: {}\ncoverage: {}\n",
        freshness.as_str(),
        coverage.as_str()
    );
    if let Some(note) = note {
        out.push_str(&format!("note: {note}\n"));
    }
    if hits.is_empty() {
        out.push_str("No results found.\n");
        return out;
    }
    for (index, hit) in hits.iter().take(limit).enumerate() {
        let path = &hit.file.relative_path;
        let location = hit
            .evidence
            .iter()
            .find_map(|evidence| range_suffix(&evidence.range))
            .or_else(|| range_suffix(&hit.entity.range))
            .map_or_else(|| path.clone(), |range| format!("{path}:{range}"));
        out.push_str(&format!(
            "\n## {} — {} (rank {}, score {:.2}, matched_by {})\n",
            location,
            hit_title(hit),
            index + 1,
            hit.score.value(),
            matched_by_name(&hit.matched_by)
        ));
        for evidence in hit.evidence.iter().take(3) {
            if let Some(matched) = evidence_text(&evidence.content) {
                out.push_str(&format!("matched: {matched}\n"));
            }
        }
    }
    out
}

impl McpServer {
    pub fn new(db: Arc<GuidanceDb>) -> Self {
        Self {
            db,
            memory: None,
            workspace: None,
            json_dir: None,
            toolset: Toolset::Full,
        }
    }

    pub fn with_memory(db: Arc<GuidanceDb>, memory: MemoryBridge) -> Self {
        Self {
            db,
            memory: Some(memory),
            workspace: None,
            json_dir: None,
            toolset: Toolset::Full,
        }
    }

    /// Bind the workspace index + toolset (freshness checks, `autoUpdate`).
    pub fn with_workspace(
        mut self,
        workspace: Option<PathBuf>,
        json_dir: Option<PathBuf>,
        toolset: Toolset,
    ) -> Self {
        self.workspace = workspace;
        self.json_dir = json_dir;
        self.toolset = toolset;
        self
    }

    fn dispatch(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request),
            "tools/list" => self.handle_tools_list(request),
            "tools/call" => self.handle_tools_call(request),
            _ => common_core::jsonrpc::method_not_found(request.id.clone(), &request.method),
        }
    }

    fn handle_initialize(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id.clone(),
            result: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "guidance",
                    "version": "0.1.0"
                }
            })),
            error: None,
        }
    }

    fn handle_tools_list(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let mut tools: Vec<serde_json::Value> = vec![serde_json::json!({
            "name": "guidance_explain",
            "description": "Search the codebase knowledge graph for identifiers, functions, modules, and patterns. Returns ranked results with file:line citations, matched lines, and hybrid-route provenance.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query (identifier, keyword, or natural language question, max 4000 chars)"
                    },
                    "queries": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Additional queries (max 32 groups per call)"
                    },
                    "fts": {
                        "type": "boolean",
                        "description": "Lexical FTS route only (default fuses all routes)"
                    },
                    "vector": {
                        "type": "boolean",
                        "description": "Vector route only (default fuses all routes)"
                    },
                    "fuse": {
                        "type": "boolean",
                        "description": "Explicit RRF fusion across FTS + vector + lemma routes"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results (default: 10, max: 50)"
                    },
                    "globs": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Glob filters (at most 128 path filters per call)"
                    },
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Workspace-relative path filters"
                    },
                    "fileTypes": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "File-type filters"
                    },
                    "symbolTypes": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Symbol-type filters (module|class|interface|function|value|alias)"
                    },
                    "preferSymbol": {
                        "type": "boolean",
                        "description": "Prefer symbol definitions over prose matches"
                    },
                    "mtimeAfter": {
                        "type": "integer",
                        "description": "Modification-time floor (ms epoch)"
                    },
                    "mtimeBefore": {
                        "type": "integer",
                        "description": "Modification-time ceiling (ms epoch)"
                    },
                    "freshness": {
                        "type": "string",
                        "description": "eventual (serve as-is, default) or wait_for_fresh"
                    },
                    "autoUpdate": {
                        "type": "boolean",
                        "description": "Refresh stale files before serving (with wait_for_fresh)"
                    },
                    "context": {
                        "type": "boolean",
                        "description": "Include depth-1 graph context lines (provenance, same format as the explain output) as a separate context array beside the hits (default: false)"
                    }
                },
                "required": ["query"]
            }
        })];
        if self.toolset == Toolset::Full {
            tools.push(serde_json::json!({
                "name": "guidance_status",
                "description": "Get the status of the guidance database (node count, embedding count).",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            }));
        }

        if let Some(ref memory) = self.memory {
            let schemas = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(memory.tool_schemas())
            });
            for schema in schemas {
                tools.push(serde_json::json!({
                    "name": schema.name,
                    "description": schema.description,
                    "inputSchema": schema.parameters,
                }));
            }
        }

        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id.clone(),
            result: Some(serde_json::json!({ "tools": tools })),
            error: None,
        }
    }

    fn handle_tools_call(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let tool_name = request
            .params
            .as_ref()
            .and_then(|p| p.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let arguments = request
            .params
            .as_ref()
            .and_then(|p| p.get("arguments"))
            .cloned()
            .unwrap_or(serde_json::json!({}));

        match tool_name {
            "guidance_explain" => self.handle_guidance_explain(request, &arguments),
            "guidance_status" if self.toolset == Toolset::Full => {
                self.handle_guidance_status(request)
            }
            other if self.memory.is_some() => {
                let memory = self.memory.as_ref().unwrap();
                match tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(memory.handle_tool_call(other, &arguments))
                }) {
                    Ok(result) => JsonRpcResponse {
                        jsonrpc: "2.0".into(),
                        id: request.id.clone(),
                        result: Some(serde_json::json!({
                            "content": [{"type": "text", "text": result}]
                        })),
                        error: None,
                    },
                    Err(e) => JsonRpcResponse {
                        jsonrpc: "2.0".into(),
                        id: request.id.clone(),
                        error: Some(JsonRpcError {
                            code: -32000,
                            message: e.to_string(),
                        }),
                        result: None,
                    },
                }
            }
            _ => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                error: Some(JsonRpcError {
                    code: -32602,
                    message: format!("unknown tool: {tool_name}"),
                }),
                result: None,
            },
        }
    }

    fn handle_guidance_explain(
        &self,
        request: &JsonRpcRequest,
        arguments: &serde_json::Value,
    ) -> JsonRpcResponse {
        let params = match parse_explain_params(arguments) {
            Ok(params) => params,
            Err(message) => {
                return JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id.clone(),
                    error: Some(JsonRpcError {
                        code: -32602,
                        message,
                    }),
                    result: None,
                };
            }
        };
        let (freshness, note) = self.observe_freshness(&params);
        let storage = GuidanceDbStorage::new(&self.db);
        let plans = build_explain_plans(&params);
        // Rule-lemmatizer pipeline (no model): L2 inflections collapse at
        // zero embedding cost; `None` degrades to L1-only. Built lazily
        // on first lemma need, shared across the merged plans.
        let nlp = LazyNlp::new();
        // Multi-query merge: best rank wins per entity id, provenance kept.
        let mut merged: Vec<SearchHit> = Vec::new();
        for plan in &plans {
            match run_recall(plan, &storage, None, nlp.get()) {
                Ok(output) => {
                    for hit in output.hits {
                        if let Some(existing) = merged
                            .iter_mut()
                            .find(|known| known.entity.id == hit.entity.id)
                        {
                            if hit.score > existing.score {
                                *existing = hit;
                            }
                        } else {
                            merged.push(hit);
                        }
                    }
                }
                Err(error) => {
                    // An empty recall (e.g. no FTS rows) is an empty result,
                    // never a tool error — unless every query failed loudly.
                    let empty = error.to_string();
                    if !empty.contains("No results") && !empty.contains("no rows") {
                        return JsonRpcResponse {
                            jsonrpc: "2.0".into(),
                            id: request.id.clone(),
                            error: Some(JsonRpcError {
                                code: -32000,
                                message: error.to_string(),
                            }),
                            result: None,
                        };
                    }
                }
            }
        }
        merged.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let text = render_explain(
            &merged,
            params.limit,
            freshness,
            Coverage::RankedSample,
            note.as_deref(),
        );
        // Optional graph context through the shared context assembly
        // (same builder the CLI explain uses): anchors from the merged
        // hits, provenance lines verbatim, no fabricated hits, no score
        // invention — the renderer above never sees them. Off by
        // default; hydration failure degrades to no lines.
        let mut result = serde_json::json!({
            "content": [{
                "type": "text",
                "text": text
            }]
        });
        if params.context {
            let lines = match self.workspace.as_ref() {
                Some(workspace) => crate::search::context_lines_for_hits(
                    &merged,
                    &self.db,
                    &workspace.to_string_lossy(),
                )
                .unwrap_or_default(),
                None => Vec::new(),
            };
            result["context"] = serde_json::json!(lines);
        }
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id.clone(),
            result: Some(result),
            error: None,
        }
    }

    /// Observe index freshness for the `freshness:` header. With
    /// `wait_for_fresh` + `autoUpdate` and a configured workspace, stale
    /// files are refreshed before serving; otherwise the observed state is
    /// reported and stale results are served as-is.
    fn observe_freshness(&self, params: &ExplainParams) -> (Freshness, Option<String>) {
        let (Some(workspace), Some(json_dir)) = (self.workspace.as_ref(), self.json_dir.as_ref())
        else {
            return (
                Freshness::Unknown,
                Some("no workspace configured".to_string()),
            );
        };
        let mut engine = SyncEngine::new(json_dir.clone(), workspace.clone());
        let stale_now = |engine: &SyncEngine| {
            engine
                .status()
                .map(|status| status.stale_files)
                .unwrap_or(0)
        };
        let stale = stale_now(&engine);
        if params.freshness == FreshnessMode::WaitForFresh && stale > 0 && params.auto_update {
            if engine.gen_scoped(workspace).is_ok() {
                let restaled = stale_now(&engine);
                return (
                    if restaled == 0 {
                        Freshness::Fresh
                    } else {
                        Freshness::PossiblyStale
                    },
                    None,
                );
            }
            return (
                Freshness::PossiblyStale,
                Some("refresh failed; served as-is".to_string()),
            );
        }
        (
            if stale == 0 {
                Freshness::Fresh
            } else {
                Freshness::PossiblyStale
            },
            None,
        )
    }

    fn handle_guidance_status(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let node_count = self.db.get_node_count().unwrap_or(0);
        let embedding_count = self.db.get_embedding_count().unwrap_or(0);

        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id.clone(),
            result: Some(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::json!({
                        "node_count": node_count,
                        "embedding_count": embedding_count,
                        "hnsw_active": self.db.has_hnsw(),
                        "hnsw_points": self.db.hnsw_len(),
                    }).to_string()
                }]
            })),
            error: None,
        }
    }
}

impl JsonRpcHandler for McpServer {
    fn handle_request(&self, raw: &str) -> Result<String, JsonRpcError> {
        let request: JsonRpcRequest = serde_json::from_str(raw)?;
        let response = self.dispatch(&request);
        Ok(serde_json::to_string(&response)?)
    }
}

/// Serve MCP over STDIO with workspace binding (freshness) and toolset.
pub fn serve_stdio_with_options(
    db_path: &Path,
    workspace: Option<PathBuf>,
    json_dir: Option<PathBuf>,
    toolset: Toolset,
) -> Result<(), McpError> {
    let db = if db_path.exists() {
        GuidanceDb::open(db_path).map_err(|e| McpError::Db(e.to_string()))?
    } else {
        GuidanceDb::open_in_memory().map_err(|e| McpError::Db(e.to_string()))?
    };

    let memory = guidance_core::memory::init_memory_bridge();
    let server = match memory {
        Some(m) => McpServer::with_memory(Arc::new(db), m),
        None => McpServer::new(Arc::new(db)),
    }
    .with_workspace(workspace, json_dir, toolset);
    common_core::jsonrpc::serve_stdio(&server).map_err(McpError::Io)
}

#[cfg(test)]
#[path = "../tests/mcp_explain.rs"]
mod explain_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_server() -> McpServer {
        let db = Arc::new(GuidanceDb::open_in_memory().expect("db"));
        McpServer::new(db)
    }

    #[test]
    fn test_method_not_found() {
        let server = make_server();
        let req = r#"{"jsonrpc":"2.0","method":"unknown","id":1}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: JsonRpcResponse = serde_json::from_str(&response).expect("parse");
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn test_initialize() {
        let server = make_server();
        let req = r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{}}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: serde_json::Value = serde_json::from_str(&response).expect("parse");
        assert_eq!(resp["result"]["serverInfo"]["name"], "guidance");
    }

    #[test]
    fn test_tools_list() {
        let server = make_server();
        let req = r#"{"jsonrpc":"2.0","method":"tools/list","id":1,"params":{}}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: serde_json::Value = serde_json::from_str(&response).expect("parse");
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert!(tools.len() >= 2);
    }

    #[test]
    fn test_guidance_explain_empty_db() {
        let server = make_server();
        let req = r#"{"jsonrpc":"2.0","method":"tools/call","id":1,"params":{"name":"guidance_explain","arguments":{"query":"hello"}}}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: serde_json::Value = serde_json::from_str(&response).expect("parse");
        // Should return empty results, not an error
        assert!(resp["result"].is_object());
    }

    #[test]
    fn test_guidance_status() {
        let server = make_server();
        let req = r#"{"jsonrpc":"2.0","method":"tools/call","id":1,"params":{"name":"guidance_status","arguments":{}}}"#;
        let response = server.handle_request(req).expect("handle");
        let resp: serde_json::Value = serde_json::from_str(&response).expect("parse");
        assert!(resp["result"].is_object());
    }
}
