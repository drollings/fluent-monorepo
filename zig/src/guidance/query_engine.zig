//! query_engine.zig — explain, staged, show, test, check commands.
//!
//! Extracted from main.zig (M1.6) to keep individual file sizes navigable.
//! All public functions are called from main.zig's command dispatch switch.
//!
//! ## Memory Ownership
//!
//!   - cmdExplain(): Creates short-lived LlmClient for the explain pipeline; deinit at
//!     function exit. Returns void; all output goes to stdout.
//!   - LlmClient instances created within command functions are ephemeral — init/deinit
//!     within the function scope. The Enhancer (when used) owns the LlmClient.
//!   - ExplainArgs: Borrowed CLI string slices — no deinit needed.
//!   - QueryContext: Owns workspace, guidance_dir, db_path strings; call deinit() to release.
//!
//! ## Hot Path Optimizations
//!
//! A session-scoped QueryCache (FNV-1a64 keyed) is initialised at the top of
//! cmdExplainStaged and checked before the expensive search pipeline. Bypassed
//! by --no-cache. See ROADMAP_20260420_QUALITY.md for rationale.

const std = @import("std");
const types = @import("types.zig");
const ast_parser = @import("ast_parser.zig");
const vector_db_mod = @import("vector");
const vector_mod = @import("vector");
const common = @import("common");
const enhancer_mod = @import("enhancer.zig");
const config_mod = @import("config.zig");
const plugin_mod = @import("plugin.zig");
const plugin_registry = @import("plugin_registry.zig");
const staged_mod = @import("staged.zig");
const query_strategy_mod = @import("query/strategy.zig");
const llm_filter_mod = @import("query/llm_filter.zig");
const synthesize_mod = @import("query/synthesize.zig");
const marker_mod = @import("sync/marker.zig");
const provider_mod = @import("provider_discovery.zig");
const json_store_mod = @import("sync/json_store.zig");
const sync_mod = @import("sync.zig");
const llm = @import("llm");
const GuidanceDb = vector_db_mod.GuidanceDb;
const SearchResult = GuidanceDb.SearchResult;
const freeSearchResult = GuidanceDb.freeSearchResult;
const stepPrint = types.stepPrint;
const StringInterner = common.interner.StringInterner;
const BitSetDrift = common.drift.BitSetDrift;
const hash_mod = @import("hash.zig");
const skeleton_mod = @import("skeleton.zig");
const core_intent = @import("core/intent.zig");
const core_drift = @import("core/drift.zig");
const query_args_mod = @import("query/args.zig");

const FilterMode = query_args_mod.FilterMode;
const ExplainArgs = query_args_mod.ExplainArgs;
const QueryContext = query_args_mod.QueryContext;
const parseExplainArgs = query_args_mod.parseExplainArgs;

/// Delegates to core/intent.isShortQuery.
const isShortQuery = core_intent.isShortQuery;

// =============================================================================
// explain — small path/config helpers
// =============================================================================

/// Creates an embedding provider using an allocator and configuration, returning a vector slice.
fn createEmbedderWithFallback(
    allocator: std.mem.Allocator,
    cfg: *const config_mod.ProjectConfig,
) !vector_mod.EmbeddingProvider {
    return vector_mod.createEmbeddingProvider(
        allocator,
        cfg.embedding_provider,
        null,
        cfg.embedding_model,
        cfg.embedding_dims,
    ) catch {
        var noop = try allocator.create(vector_mod.NoopEmbedding);
        noop.* = .{ .allocator = allocator };
        return noop.provider();
    };
}

/// Creates an LLM configuration object from explanation arguments.
fn makeLlmConfig(ea: ExplainArgs) llm.LlmConfig {
    return .{ .api_url = ea.api_url, .model = ea.model, .debug = ea.debug };
}

/// Resolves LLM config for thinking with allocator, config, model, and URL parameters.
pub fn resolveLlmConfigForThinking(
    allocator: std.mem.Allocator,
    cfg: *const config_mod.ProjectConfig,
    model_ref: []const u8,
    explicit_api_url: ?[]const u8,
) !struct { api_url: []const u8, model: []const u8, think: ?bool, resolved_url: ?[]const u8 } {
    const is_thinking_slot = cfg.isThinkingModelRef(model_ref);

    // Use explicit API URL if provided
    if (explicit_api_url) |url| {
        return .{
            .api_url = url,
            .model = model_ref,
            .think = if (is_thinking_slot) true else null,
            .resolved_url = null,
        };
    }

    // Thinking model: must use Ollama /api/chat endpoint
    if (is_thinking_slot) {
        // Get ollama provider (uses /api/chat)
        if (cfg.getProvider("ollama")) |ollama| {
            const url = try std.fmt.allocPrint(allocator, "{s}{s}", .{ ollama.base_url, ollama.chat_endpoint });
            return .{
                .api_url = url,
                .model = model_ref,
                .think = true,
                .resolved_url = url,
            };
        }
        // No ollama provider: construct URL with /api/chat endpoint
        const parsed = config_mod.ProjectConfig.parseModelRef(model_ref) orelse {
            // Fallback to localhost
            const url = try allocator.dupe(u8, "http://localhost:11434/api/chat");
            return .{ .api_url = url, .model = model_ref, .think = true, .resolved_url = url };
        };
        if (cfg.getProvider(parsed.provider)) |provider| {
            // Use provider's base_url with /api/chat
            const scheme_end = std.mem.indexOf(u8, provider.base_url, "://") orelse 0;
            const host_start: usize = if (scheme_end > 0) scheme_end + 3 else 0;
            const path_start = std.mem.indexOfScalarPos(u8, provider.base_url, host_start, '/') orelse provider.base_url.len;
            const base = provider.base_url[0..path_start];
            const url = try std.fmt.allocPrint(allocator, "{s}/api/chat", .{base});
            return .{
                .api_url = url,
                .model = model_ref,
                .think = true,
                .resolved_url = url,
            };
        }
        // Final fallback
        const url = try allocator.dupe(u8, "http://localhost:11434/api/chat");
        return .{ .api_url = url, .model = model_ref, .think = true, .resolved_url = url };
    }

    // Non-thinking model: use configured provider
    const parsed = config_mod.ProjectConfig.parseModelRef(model_ref) orelse {
        return .{
            .api_url = config_mod.DEFAULT_API_URL,
            .model = model_ref,
            .think = null,
            .resolved_url = null,
        };
    };

    const provider = cfg.getProvider(parsed.provider) orelse {
        return .{
            .api_url = config_mod.DEFAULT_API_URL,
            .model = model_ref,
            .think = null,
            .resolved_url = null,
        };
    };

    const url = try std.fmt.allocPrint(allocator, "{s}{s}", .{ provider.base_url, provider.chat_endpoint });
    return .{
        .api_url = url,
        .model = model_ref,
        .think = null,
        .resolved_url = url,
    };
}

/// Resolves workspace, config, db_path, and guidance_dir. Caller must call ctx.deinit().
fn openQueryContext(allocator: std.mem.Allocator, ea: ExplainArgs) !QueryContext {
    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const workspace = if (ea.workspace) |w|
        try common.resolvePath(allocator, cwd, w)
    else
        try allocator.dupe(u8, cwd);
    errdefer allocator.free(workspace);

    var cfg = config_mod.loadConfig(allocator, workspace) catch
        try config_mod.loadConfig(allocator, cwd);
    errdefer cfg.deinit();

    const db_path = try common.resolvePath(allocator, workspace, ea.db_path orelse cfg.db_path);
    errdefer allocator.free(db_path);

    const guidance_dir = try common.resolvePath(allocator, workspace, ea.guidance orelse cfg.guidance_dir);

    return .{ .workspace = workspace, .guidance_dir = guidance_dir, .db_path = db_path, .cfg = cfg };
}

/// Routes to tier 1/2/3 handler (capability/file/struct). Returns true if handled.
fn routeToTierHandler(
    allocator: std.mem.Allocator,
    ctx: *const QueryContext,
    query_text: []const u8,
) !bool {
    const tier_match = try skeleton_mod.classifyQuery(
        allocator,
        query_text,
        ctx.cfg.capabilities_dir,
        ctx.workspace,
        ctx.guidance_dir,
    );
    defer {
        switch (tier_match) {
            .capability => |cap| allocator.free(cap),
            .file_path => |fp| allocator.free(fp),
            .struct_name => |sn| allocator.free(sn),
            .disambiguate => |paths| {
                for (paths) |p| allocator.free(p);
                allocator.free(paths);
            },
            .none => {},
        }
    }

    const has_nl = blk: {
        const trimmed = std.mem.trim(u8, query_text, " \t\n\r");
        if (trimmed.len > 0 and trimmed[trimmed.len - 1] == '?') break :blk true;
        var tok = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
        var count: usize = 0;
        while (tok.next()) |_| count += 1;
        break :blk count > 2;
    };

    switch (tier_match) {
        .capability => |cap_name| {
            try handleCapabilityQuery(allocator, cap_name, ctx.cfg.capabilities_dir, query_text, has_nl, &ctx.cfg);
            return true;
        },
        .file_path => |file_path| {
            try handleFileSkeletonQuery(allocator, file_path, ctx.workspace, ctx.guidance_dir, query_text);
            return true;
        },
        .struct_name => |struct_name| {
            try handleStructSkeletonQuery(allocator, struct_name, ctx.guidance_dir, query_text);
            return true;
        },
        .disambiguate => |paths| {
            try handleDisambiguationQuery(allocator, paths, query_text);
            return true;
        },
        .none => return false,
    }
}

/// Opens .guidance.db, builds LLM config, and dispatches through the staged pipeline.
fn openDbAndRunStaged(
    allocator: std.mem.Allocator,
    ctx: *const QueryContext,
    ea: ExplainArgs,
    query_text: []const u8,
) !void {
    std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), ctx.db_path, .{}) catch {
        std.debug.print("Error: No .guidance.db found at {s}\n", .{ctx.db_path});
        std.debug.print("Run 'guidance gen' to generate it.\n", .{});
        return;
    };

    const embedder = try createEmbedderWithFallback(allocator, &ctx.cfg);
    defer embedder.deinit();

    var db = GuidanceDb.init(allocator, ctx.db_path, embedder) catch |err| {
        std.debug.print("Error opening database: {s}\n", .{@errorName(err)});
        return;
    };
    defer db.deinit();

    if (!ea.staged) std.debug.print("warning: --staged=false is deprecated; staged pipeline is now the only search path\n", .{});

    var resolved_url_to_free: ?[]const u8 = null;
    defer if (resolved_url_to_free) |url| allocator.free(url);

    const model = if (std.mem.eql(u8, ea.model, config_mod.DEFAULT_MODEL)) ctx.cfg.model_default else ea.model;
    const llm_config = blk: {
        const resolved = resolveLlmConfigForThinking(
            allocator,
            &ctx.cfg,
            model,
            if (std.mem.eql(u8, ea.api_url, config_mod.DEFAULT_API_URL)) null else ea.api_url,
        ) catch {
            break :blk llm.LlmConfig{ .api_url = ea.api_url, .model = ea.model, .debug = ea.debug };
        };
        resolved_url_to_free = resolved.resolved_url;
        break :blk llm.LlmConfig{ .api_url = resolved.api_url, .model = resolved.model, .think = resolved.think, .debug = ea.debug };
    };

    try cmdExplainStaged(allocator, &db, query_text, ctx.workspace, ctx.guidance_dir, llm_config, ctx.cfg.infillModel(), ea);
}

/// Routes explain command invocations to the appropriate query handler.
pub fn cmdExplain(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var ea = parseExplainArgs(args) catch return;
    const query_text = ea.query_str orelse "";

    var ctx = try openQueryContext(allocator, ea);
    defer ctx.deinit(allocator);
    ea.capabilities_dir = ctx.cfg.capabilities_dir;

    if (std.mem.trim(u8, query_text, " \t\n\r").len == 0) {
        try showIndexIntro(allocator, ctx.workspace, ctx.guidance_dir, ctx.cfg.capabilities_dir, &ctx.cfg);
        return;
    }

    if (try routeToTierHandler(allocator, &ctx, query_text)) return;
    try openDbAndRunStaged(allocator, &ctx, ea, query_text);
}

// =============================================================================
// Staged explain implementation  (M3/M5-M9)
// =============================================================================

/// Loads semantic aliases from a guidance directory into a vector_db_mod structure.
fn loadAliases(allocator: std.mem.Allocator, guidance_dir: []const u8) ?vector_db_mod.SemanticAliases {
    const alias_path = std.fs.path.join(allocator, &.{ guidance_dir, "semantic-aliases.json" }) catch return null;
    defer allocator.free(alias_path);
    return vector_db_mod.loadSemanticAliases(allocator, alias_path) catch null;
}

/// Extracts key terms from a query string into structured slices for LLM processing.
fn llmExtractKeyTerms(allocator: std.mem.Allocator, client: *llm.LlmClient, query: []const u8) !?[][]const u8 {
    const prompt = try std.fmt.allocPrint(allocator,
        \\Extract 3-5 key technical terms from this query. Return only a comma-separated list, no other text.
        \\Query: {s}
        \\
    , .{query});
    defer allocator.free(prompt);

    const response_opt = client.complete(prompt, 50, 0.0, null) catch return null;
    const response = response_opt orelse return null;
    defer client.allocator.free(response);

    const stripped = llm.stripThinkBlock(response);
    const trimmed = std.mem.trim(u8, stripped, " \t\n\r");
    if (trimmed.len == 0) return null;

    var terms: std.ArrayList([]const u8) = .empty;
    errdefer {
        for (terms.items) |t| allocator.free(t);
        terms.deinit(allocator);
    }

    var it = std.mem.splitAny(u8, trimmed, ",\n");
    var count: usize = 0;
    while (it.next()) |term| {
        if (count >= 4) break;
        const t = std.mem.trim(u8, term, " \t\n\r\"");
        if (t.len == 0) continue;
        try terms.append(allocator, try allocator.dupe(u8, t));
        count += 1;
    }

    if (terms.items.len == 0) return null;
    return try terms.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// DRIFT helpers — Phase 7
// ---------------------------------------------------------------------------

/// Delegates to core/drift.tokenizeCapabilityWords.
const tokenizeCapabilityWords = core_drift.tokenizeCapabilityWords;

/// Delegates to core/drift.computeDriftFollowUps.
const computeDriftFollowUps = core_drift.computeDriftFollowUps;

/// Processes a query text to generate explanation data using the provided LLM configuration.
fn cmdExplainStaged(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query_text: []const u8,
    workspace: []const u8,
    guidance_dir: []const u8,
    llm_config: llm.LlmConfig,
    fast_model_ref: []const u8,
    ea: ExplainArgs,
) !void {
    if (ea.debug) {
        // Debug output at function entry - always print to stderr
        std.debug.print("[DEBUG] cmdExplainStaged called\n", .{});
        std.debug.print("[DEBUG]   ea.debug = {any}\n", .{ea.debug});
        std.debug.print("[DEBUG]   ea.no_llm = {any}\n", .{ea.no_llm});
        std.debug.print("[DEBUG]   query_text = \"{s}\"\n", .{query_text});
    }

    // Per-query arena: owns all temporary allocations for the duration of this query.
    // session_cache, client_opt, and fast_client_opt stay on `allocator` because they
    // either accumulate across queries (session_cache) or hold external resources (clients).
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const a = arena.allocator();

    const skills_dir = try std.fs.path.join(a, &.{ guidance_dir, "skills" });

    // Session-level in-memory cache: avoids repeating synthesis for identical queries
    // within the same process lifetime (e.g. guidance serve). Bypassed by --no-cache.
    var session_cache = common.QueryCache.init(allocator);
    defer session_cache.deinit();

    // aliases_opt internal strings are arena-allocated; no explicit deinit needed.
    const aliases_opt: ?vector_db_mod.SemanticAliases = loadAliases(a, guidance_dir);

    // use_llm: always on unless --no-llm is specified
    // use_filter: depends on --filter mode (auto enables filter for long queries only)
    const use_llm = !ea.no_llm;
    const use_filter = !ea.no_llm and switch (ea.filter) {
        .skip => false,
        .force => true,
        .auto => !isShortQuery(query_text),
    };

    if (ea.debug) {
        const trimmed = std.mem.trim(u8, query_text, " \t\n\r");
        const word_count = blk: {
            var tok = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
            var count: usize = 0;
            while (tok.next()) |_| count += 1;
            break :blk count;
        };
        std.debug.print("[DEBUG] Query classification:\n", .{});
        std.debug.print("[DEBUG]   query: \"{s}\"\n", .{query_text});
        std.debug.print("[DEBUG]   word_count: {d}\n", .{word_count});
        std.debug.print("[DEBUG]   isShortQuery: {any}\n", .{isShortQuery(query_text)});
        std.debug.print("[DEBUG] Pipeline settings:\n", .{});
        std.debug.print("[DEBUG]   use_llm: {any}\n", .{use_llm});
        std.debug.print("[DEBUG]   use_filter: {any}\n", .{use_filter});
        std.debug.print("[DEBUG]   filter_mode: {s}\n", .{@tagName(ea.filter)});
        std.debug.print("[DEBUG]   staged: {any}\n", .{ea.staged});
    }

    // Create the LLM client for filtering (default model)
    var client_opt: ?llm.LlmClient = if (use_llm) llm.LlmClient.init(allocator, llm_config) catch |err| blk: {
        if (ea.verbose) std.debug.print("DEBUG: LLM client init failed: {any}\n", .{err});
        break :blk null;
    } else null;
    defer if (client_opt) |*c| c.deinit();

    // Create separate client for synthesis (fast model)
    var fast_client_opt: ?llm.LlmClient = null;
    defer if (fast_client_opt) |*c| c.deinit();

    if (use_llm and fast_model_ref.len > 0) {
        const fast_config = llm.LlmConfig{
            .api_url = llm_config.api_url,
            .model = fast_model_ref,
            .think = null, // fast model never uses thinking
            .debug = ea.debug,
        };
        fast_client_opt = llm.LlmClient.init(allocator, fast_config) catch null;
    }

    if (ea.verbose) {
        if (client_opt) |_| {
            std.debug.print("DEBUG: LLM client initialized - api_url: {s}, model: {s}, think: {?any}\n", .{ llm_config.api_url, llm_config.model, llm_config.think });
        } else {
            std.debug.print("DEBUG: LLM client is null, synthesis will be skipped\n", .{});
        }
        if (fast_client_opt) |_| {
            std.debug.print("DEBUG: Fast client initialized - model: {s}\n", .{fast_model_ref});
        }
    }

    // For long queries, extract key terms to improve search recall.
    var expanded_query: ?[]const u8 = null;

    if (use_filter) {
        if (client_opt) |*client| {
            if (llmExtractKeyTerms(a, client, query_text) catch null) |terms| {
                if (ea.debug) {
                    std.debug.print("[DEBUG] Key term extraction:\n", .{});
                    std.debug.print("[DEBUG]   terms extracted: {d}\n", .{terms.len});
                    for (terms, 0..) |t, i| {
                        std.debug.print("[DEBUG]   [{d}] \"{s}\"\n", .{ i, t });
                    }
                }
                var buf: std.ArrayList(u8) = .empty;
                try buf.appendSlice(a, query_text);
                for (terms) |t| {
                    try buf.append(a, ' ');
                    try buf.appendSlice(a, t);
                }
                expanded_query = try buf.toOwnedSlice(a);
            }
        }
    }

    const effective_query = expanded_query orelse query_text;

    if (ea.debug) {
        std.debug.print("[DEBUG] Query processing:\n", .{});
        std.debug.print("[DEBUG]   original: \"{s}\"\n", .{query_text});
        std.debug.print("[DEBUG]   effective: \"{s}\"\n", .{effective_query});
        if (expanded_query) |_| {
            std.debug.print("[DEBUG]   was_expanded: true\n", .{});
        } else {
            std.debug.print("[DEBUG]   was_expanded: false\n", .{});
        }
    }

    // Session cache check — short-circuit before the expensive search pipeline.
    if (!ea.no_cache) {
        if (session_cache.get(effective_query)) |cached_summary| {
            if (ea.debug) std.debug.print("[DEBUG] session_cache: HIT for \"{s}\"\n", .{effective_query});
            return emitStagedOutput(allocator, query_text, &.{}, cached_summary, workspace);
        }
        if (ea.debug) std.debug.print("[DEBUG] session_cache: MISS for \"{s}\"\n", .{effective_query});
    }

    const matches = query_strategy_mod.buildDefaultStrategies();

    // Pass original query for deterministic matching, effective query for vector search
    const stages_raw = try query_strategy_mod.executeQueryWithMatch(
        a,
        db,
        effective_query,
        query_text,
        workspace,
        aliases_opt,
        &matches,
    );

    if (stages_raw.len == 0) {
        const lower_q = try std.ascii.allocLowerString(a, effective_query);
        std.debug.print("# Explain: {s}\n\nNot indexed for '{s}'. Search the source directly:\n\n", .{ query_text, effective_query });
        std.debug.print("    grep -ri '{s}' src/ | head -n 20\n\n", .{lower_q});
        std.debug.print("Run 'guidance gen' after finding the file to index it.\n", .{});
        return;
    }

    if (ea.debug) {
        std.debug.print("[DEBUG] Search results (stages_raw):\n", .{});
        std.debug.print("[DEBUG]   total_count: {d}\n", .{stages_raw.len});
        var idx: usize = 0;
        for (stages_raw) |s| {
            if (idx >= 5) break;
            std.debug.print("[DEBUG]   [{d}] kind={s} source=\"{s}\"\n", .{ idx, @tagName(s.kind), s.source });
            idx += 1;
        }
    }

    // M7: not_found sentinel — emit directly, skip synthesis and cache.
    if (stages_raw[0].kind == .not_found) {
        return emitStagedOutput(allocator, query_text, stages_raw, null, workspace);
    }

    // ── Fast path: no LLM ─────────────────────────────────────────────────────
    if (!use_llm or client_opt == null) {
        if (use_llm and ea.verbose) std.debug.print("LLM unavailable, using fast path\n", .{});
        for (stages_raw) |s| db.incrementQueryCountForFile(s.source);
        return emitStagedOutput(allocator, query_text, stages_raw, null, workspace);
    }

    // ── LLM path ─────────────────────────────────────────────────────────────
    const client = &client_opt.?;

    // M6: LLM relevance filter (only when filter mode enables it).
    const stages_filtered: ?[]types.Stage = if (use_filter) llm_filter_mod.filterStages(a, client, query_text, stages_raw) catch blk: {
        if (ea.verbose) std.debug.print("llm_filter failed, using unfiltered stages\n", .{});
        break :blk null;
    } else null;

    const working_stages: []const types.Stage = stages_filtered orelse stages_raw;

    if (ea.debug) {
        std.debug.print("[DEBUG] LLM filter:\n", .{});
        std.debug.print("[DEBUG]   filter_applied: {}\n", .{stages_filtered != null});
        std.debug.print("[DEBUG]   stages_before: {d}\n", .{stages_raw.len});
        std.debug.print("[DEBUG]   stages_after: {d}\n", .{working_stages.len});
    }

    // M7: Follow-up expansion — re-search to gather used_by for expansion inputs.
    // M4: Expand to top-3 results with score filtering (score >= 0.35).
    const SEE_ALSO_TOP_N: usize = 3;
    const SEE_ALSO_MIN_SCORE: f64 = 0.35;

    if (ea.debug) {
        std.debug.print("[DEBUG] Expansion search:\n", .{});
        std.debug.print("[DEBUG]   query: \"{s}\"\n", .{effective_query});
        std.debug.print("[DEBUG]   aliases_loaded: {any}\n", .{aliases_opt != null});
    }

    const expansion_results = db.searchWithAliases(a, effective_query, 5, aliases_opt) catch &.{};

    if (ea.debug) {
        std.debug.print("[DEBUG] Expansion results:\n", .{});
        std.debug.print("[DEBUG]   total_count: {d}\n", .{expansion_results.len});
        var idx: usize = 0;
        for (expansion_results) |r| {
            if (idx >= 5) break;
            std.debug.print("[DEBUG]   [{d}] source=\"{s}\" score={d:.3}\n", .{ idx, r.source, r.score });
            idx += 1;
        }
    }

    // Increment query_count for files returned in search results (hot files tracking).
    for (expansion_results) |r| db.incrementQueryCountForFile(r.source);

    var fp_list: std.ArrayList([]const u8) = .empty;
    var src_list: std.ArrayList([]const u8) = .empty;
    var ub_list: std.ArrayList([]const []const u8) = .empty;

    // M4: Only include results with score >= SEE_ALSO_MIN_SCORE, up to SEE_ALSO_TOP_N
    for (expansion_results[0..@min(SEE_ALSO_TOP_N, expansion_results.len)]) |r| {
        if (r.score < SEE_ALSO_MIN_SCORE) continue;
        try fp_list.append(a, r.file_path);
        try src_list.append(a, r.source);
        try ub_list.append(a, r.used_by);
    }

    var existing_srcs: std.ArrayList([]const u8) = .empty;
    for (working_stages) |s| {
        if (s.kind == .code or s.kind == .prose) try existing_srcs.append(a, s.source);
    }

    const extra_stages: ?[]types.Stage = staged_mod.expandFollowUps(
        a,
        fp_list.items,
        src_list.items,
        ub_list.items,
        workspace,
        guidance_dir,
        skills_dir,
        existing_srcs.items,
        6,
    ) catch null;

    // Combine working + extra stages (borrows — no new string copies).
    var combined: std.ArrayList(types.Stage) = .empty;
    for (working_stages) |s| try combined.append(a, s);
    if (extra_stages) |es| for (es) |s| try combined.append(a, s);

    // M8: LLM synthesis (use fast model if available, else default).
    // Check LLM synthesis cache before calling the model.
    const query_hash = common.sha256Hex(a, query_text) catch null;

    if (ea.debug) {
        std.debug.print("[DEBUG] Synthesis:\n", .{});
        std.debug.print("[DEBUG]   query_hash: {s}\n", .{query_hash orelse "(null)"});
    }

    const cached_summary: ?[]const u8 = if (query_hash) |qh|
        db.loadCachedSynthesis(a, qh) catch null
    else
        null;

    if (ea.debug) {
        std.debug.print("[DEBUG]   cache_hit: {any}\n", .{cached_summary != null});
    }

    const synth_client = if (fast_client_opt) |*fc| fc else &client_opt.?;
    if (ea.debug) {
        std.debug.print("[DEBUG]   using_fast_model: {any}\n", .{fast_client_opt != null});
    }

    const synth_result = if (cached_summary == null)
        synthesize_mod.synthesize(a, synth_client, query_text, combined.items) catch {
            return emitStagedOutput(allocator, query_text, combined.items, null, workspace);
        }
    else
        synthesize_mod.SynthesisResult{ .summary = null, .followup_keywords = null };

    // Store successful synthesis in cache (best-effort, no error propagation).
    // sig_buf and sig_hash are temporaries; arena owns them.
    if (cached_summary == null) {
        if (synth_result.summary) |summary| {
            if (query_hash) |qh| {
                // Compute signature_hash from stage file paths for future invalidation.
                var sig_buf_aw: std.Io.Writer.Allocating = .init(a);
                const sig_writer = &sig_buf_aw.writer;
                for (combined.items) |s| {
                    sig_writer.writeAll(s.source) catch {};
                    sig_writer.writeAll(&.{0}) catch {};
                }
                const sig_hash = common.sha256Hex(a, sig_buf_aw.written()) catch null;
                db.storeSynthesisCache(qh, summary, sig_hash orelse qh);
            }
        }
    }

    // Use cached summary if available, otherwise use synthesis result.
    const effective_summary = cached_summary orelse synth_result.summary;

    // M8.5: DRIFT follow-ups — deterministic, no LLM required.
    const drift_followups: []const []const u8 = if (!ea.no_drift)
        computeDriftFollowUps(a, query_text, expansion_results) catch &.{}
    else
        &.{};

    // Merge LLM-generated and DRIFT follow-ups into a single slice.
    // The merged slice borrows string pointers from both sources; only its
    // spine needs to be freed.
    const merged_followups: ?[]const []const u8 = if (drift_followups.len == 0)
        synth_result.followup_keywords
    else blk: {
        const synth_len = if (synth_result.followup_keywords) |sk| sk.len else 0;
        var all = try a.alloc([]const u8, synth_len + drift_followups.len);
        if (synth_result.followup_keywords) |sk| @memcpy(all[0..synth_len], sk);
        @memcpy(all[synth_len..], drift_followups);
        break :blk all;
    };

    // merged_followups is computed for future wiring (see-also / follow-up display);
    // not yet passed to emitStagedOutput. Arena owns the spine; no explicit free needed.
    _ = merged_followups;

    // Store synthesis result in session cache for future repeated queries (best-effort).
    if (!ea.no_cache) {
        if (effective_summary) |summary| {
            session_cache.put(effective_query, summary) catch {};
        }
    }

    return emitStagedOutput(allocator, query_text, combined.items, effective_summary, workspace);
}

/// Processes query stages and outputs results using an allocator, handling Zig-specific data structures.
fn emitStagedOutput(
    allocator: std.mem.Allocator,
    query_text: []const u8,
    stages: []const types.Stage,
    summary: ?[]const u8,
    workspace: []const u8,
) !void {
    const output = try staged_mod.formatStaged(allocator, query_text, stages, summary, workspace);
    defer allocator.free(output);
    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();
    try stdout.writeAll(output);
    try stdout.flush();
}

// =============================================================================
// INDEX.md introduction (empty query)
// =============================================================================

const IndexCapability = struct {
    name: []const u8,
    description: []const u8,
};

/// Converts a Zig source snippet into an IndexCapability type, handling allocator and path inputs.
fn parseCapabilityFrontmatter(allocator: std.mem.Allocator, cap_path: []const u8) ?IndexCapability {
    const io = std.Io.Threaded.global_single_threaded.io();
    const content = std.Io.Dir.cwd().readFileAlloc(io, cap_path, allocator, .limited(64 * 1024)) catch return null;
    defer allocator.free(content);

    if (!std.mem.startsWith(u8, content, "---")) return null;

    const end_frontmatter = std.mem.indexOf(u8, content[3..], "---") orelse return null;
    const frontmatter = content[3 .. 3 + end_frontmatter];

    var name: ?[]const u8 = null;
    var description: ?[]const u8 = null;

    var lines = std.mem.splitScalar(u8, frontmatter, '\n');
    while (lines.next()) |line| {
        if (std.mem.startsWith(u8, line, "name:")) {
            name = std.mem.trim(u8, line["name:".len..], " \t\r");
        } else if (std.mem.startsWith(u8, line, "description:")) {
            description = std.mem.trim(u8, line["description:".len..], " \t\r");
        }
    }

    const n = name orelse return null;
    const d = description orelse return null;

    const name_owned = allocator.dupe(u8, n) catch return null;
    const desc_owned = allocator.dupe(u8, d) catch {
        allocator.free(name_owned);
        return null;
    };

    return .{ .name = name_owned, .description = desc_owned };
}

/// Checks if an index path is outdated based on allocator and directory constraints.
fn isIndexStale(allocator: std.mem.Allocator, index_path: []const u8, capabilities_dir: []const u8, structure_path: []const u8) bool {
    const index_mtime = marker_mod.fileMtime(index_path);

    if (index_mtime == null) return true;
    const idx_mtime = index_mtime.?;

    if (marker_mod.fileMtime(structure_path)) |smt| {
        if (smt > idx_mtime) return true;
    }

    const io = std.Io.Threaded.global_single_threaded.io();
    std.Io.Dir.accessAbsolute(io, capabilities_dir, .{}) catch return false;
    var cap_dir = std.Io.Dir.openDirAbsolute(io, capabilities_dir, .{ .iterate = true }) catch return false;
    defer cap_dir.close(io);

    var walker = cap_dir.walk(allocator) catch return false;
    defer walker.deinit();

    while (walker.next(std.Io.Threaded.global_single_threaded.io()) catch null) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.basename, "CAPABILITY.md")) continue;
        const full = std.fs.path.join(allocator, &.{ capabilities_dir, entry.path }) catch continue;
        defer allocator.free(full);
        if (marker_mod.fileMtime(full)) |cap_mt| {
            if (cap_mt > idx_mtime) return true;
        }
    }

    return false;
}

/// Summarizes a capability description to 120 characters or less using the thinking model.
fn summarizeCapabilityDescription(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    name: []const u8,
    description: []const u8,
    intended_len: u16,
) anyerror!?[]const u8 {
    const prompt = std.fmt.allocPrint(allocator,
        \\Summarize this capability in intended_len characters or less. Be precise and concise.
        \\Capability: {s}
        \\Description: {s}
        \\Respond with only the summarized text, no explanation.
    , .{ name, description }) catch return null;
    defer allocator.free(prompt);

    const raw = client.complete(prompt, 100, 0.0, null) catch return null;
    const response = raw orelse return null;
    defer client.allocator.free(response);

    const stripped = llm.stripThinkBlock(response);
    const trimmed = std.mem.trim(u8, stripped, " \t\n\r");
    if (trimmed.len == 0) return null;

    // Ensure intended_len chars or less
    if (trimmed.len <= intended_len) return try allocator.dupe(u8, trimmed);

    // Find a good break point near intended_len chars
    const trunc = trimmed[0..@min(intended_len, trimmed.len)];
    const last_space = std.mem.lastIndexOfScalar(u8, trunc, ' ') orelse intended_len - 3;
    const summary = trunc[0..last_space];
    return try std.fmt.allocPrint(allocator, "{s}...", .{summary});
}

/// Generates a metadata index structure using provided allocator, guidance and capabilities data.
fn generateIndexMd(
    allocator: std.mem.Allocator,
    guidance_dir: []const u8,
    capabilities_dir: []const u8,
    cfg: *const config_mod.ProjectConfig,
) !void {
    const index_path = try std.fs.path.join(allocator, &.{ capabilities_dir, "INDEX.md" });
    defer allocator.free(index_path);

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);
    const structure_path = try std.fs.path.join(allocator, &.{ cwd, "STRUCTURE.md" });
    defer allocator.free(structure_path);

    if (!isIndexStale(allocator, index_path, capabilities_dir, structure_path)) {
        return;
    }

    var capabilities: std.ArrayList(IndexCapability) = .empty;
    defer {
        for (capabilities.items) |cap| {
            allocator.free(cap.name);
            allocator.free(cap.description);
        }
        capabilities.deinit(allocator);
    }

    const io = std.Io.Threaded.global_single_threaded.io();
    std.Io.Dir.accessAbsolute(io, capabilities_dir, .{}) catch return;
    var cap_dir = std.Io.Dir.openDirAbsolute(io, capabilities_dir, .{ .iterate = true }) catch return;
    defer cap_dir.close(io);

    var walker = cap_dir.walk(allocator) catch return;
    defer walker.deinit();

    while (walker.next(std.Io.Threaded.global_single_threaded.io()) catch null) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.basename, "CAPABILITY.md")) continue;
        const full = try std.fs.path.join(allocator, &.{ capabilities_dir, entry.path });
        if (parseCapabilityFrontmatter(allocator, full)) |cap| {
            try capabilities.append(allocator, cap);
        }
        allocator.free(full);
    }

    std.mem.sort(IndexCapability, capabilities.items, {}, struct {
        fn lessThan(_: void, a: IndexCapability, b: IndexCapability) bool {
            return std.mem.lessThan(u8, a.name, b.name);
        }
    }.lessThan);

    // Try to create LLM client for summarization
    var llm_api_url_to_free: ?[]const u8 = null;
    defer if (llm_api_url_to_free) |url| allocator.free(url);

    var llm_client_opt: ?llm.LlmClient = blk: {
        const resolved = resolveLlmConfigForThinking(
            allocator,
            cfg,
            cfg.model_thinking,
            null,
        ) catch break :blk null;
        llm_api_url_to_free = resolved.resolved_url;
        const llm_config = llm.LlmConfig{
            .api_url = resolved.api_url,
            .model = resolved.model,
            .think = resolved.think,
            .debug = false,
        };
        break :blk llm.LlmClient.init(allocator, llm_config) catch null;
    };
    defer if (llm_client_opt) |*c| c.deinit();

    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(allocator);

    try out.appendSlice(allocator, "# guidance — AST-guided Vector Search\n\n");
    try out.appendSlice(allocator, "`guidance explain \"<query>\"` is the first stop to gain relevant context about this codebase.\n\n");
    try out.appendSlice(allocator, "A single keyword like `cmdExplain` triggers a deterministic search without LLM synthesis. Queries with spaces use the LLM for synthesis.\n\n");
    try out.appendSlice(allocator, "**Example:**\n\n");
    try out.appendSlice(allocator, "```\nguidance explain \"cmdExplain\"\n```\n\n");
    try out.appendSlice(allocator, "Look up suggested search terms from results to discover related features. Use regular file tools once you're confident about the implementation.\n\n");
    try out.appendSlice(allocator, "**Important:** Run `guidance explain` to check for existing features before writing duplicate code.\n\n");
    try out.appendSlice(allocator, "---\n\n");
    try out.appendSlice(allocator, "## Capabilities\n\n");

    for (capabilities.items) |cap| {
        const desc = if (llm_client_opt) |*client| blk: {
            const summary = summarizeCapabilityDescription(allocator, client, cap.name, cap.description, 240) catch null;
            break :blk summary;
        } else null;

        const effective_desc = if (desc) |d| d else blk: {
            if (cap.description.len <= 120) break :blk allocator.dupe(u8, cap.description) catch break :blk cap.description;
            const trunc = cap.description[0..77];
            break :blk std.fmt.allocPrint(allocator, "{s}...", .{trunc}) catch cap.description;
        };
        defer if (desc != null) allocator.free(effective_desc);

        try out.appendSlice(allocator, "- **");
        try out.appendSlice(allocator, cap.name);
        try out.appendSlice(allocator, "**: ");
        try out.appendSlice(allocator, effective_desc);
        try out.appendSlice(allocator, "\n");
    }

    try out.appendSlice(allocator, "\n---\n\n");
    try out.appendSlice(allocator, "Run `guidance explain \"<keyword>\"` to explore any capability.\n");

    const index_content = try out.toOwnedSlice(allocator);
    defer allocator.free(index_content);

    const index_dir = std.fs.path.dirname(index_path) orelse guidance_dir;
    std.Io.Dir.createDirAbsolute(io, index_dir, .default_dir) catch {};
    const file = try std.Io.Dir.createFileAbsolute(io, index_path, .{ .truncate = true });
    defer file.close(io);
    {
        var wbuf: [4096]u8 = undefined;
        var writer = file.writer(io, &wbuf);
        try writer.interface.writeAll(index_content);
        try writer.interface.flush();
    }
}

/// Displays an introductory index section using allocator, guidance, and configuration data.
fn showIndexIntro(
    allocator: std.mem.Allocator,
    _: []const u8,
    guidance_dir: []const u8,
    capabilities_dir: []const u8,
    cfg: *const config_mod.ProjectConfig,
) !void {
    try generateIndexMd(allocator, guidance_dir, capabilities_dir, cfg);

    const index_path = try std.fs.path.join(allocator, &.{ capabilities_dir, "INDEX.md" });
    defer allocator.free(index_path);

    const content = std.Io.Dir.cwd().readFileAlloc(std.Io.Threaded.global_single_threaded.io(), index_path, allocator, .limited(64 * 1024)) catch |err| {
        std.debug.print("Error reading INDEX.md: {s}\n", .{@errorName(err)});
        return;
    };
    defer allocator.free(content);

    var ws: common.WriterState = .{};
    ws.initStdout();
    const w = ws.writer();
    try w.writeAll(content);
    try w.flush();
}

// =============================================================================
// TIER 1/2/3 handlers — capability, file skeleton, struct skeleton
// =============================================================================

/// Handles TIER 1: capability name match → show CAPABILITY.md content.
fn handleCapabilityQuery(
    allocator: std.mem.Allocator,
    cap_name: []const u8,
    capabilities_dir: []const u8,
    query_text: []const u8,
    natural_lang: bool,
    cfg: *const config_mod.ProjectConfig,
) !void {
    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    // Try to load and render capability document
    const cap_path = try std.fs.path.join(allocator, &.{ capabilities_dir, cap_name, "CAPABILITY.md" });
    defer allocator.free(cap_path);

    const content = std.Io.Dir.cwd().readFileAlloc(std.Io.Threaded.global_single_threaded.io(), cap_path, allocator, .limited(256 * 1024)) catch |err| {
        try stdout.print("# Capability: {s}\n\nError reading capability file: {s}\n", .{ cap_name, @errorName(err) });
        try stdout.flush();
        return;
    };
    defer allocator.free(content);

    // Strip frontmatter
    const display_content = blk: {
        if (std.mem.startsWith(u8, content, "---")) {
            if (std.mem.indexOf(u8, content[3..], "---")) |end_fm| {
                const start = 3 + end_fm + 3;
                if (start < content.len and content[start] == '\n') {
                    break :blk content[start + 1 ..];
                }
                break :blk content[start..];
            }
        }
        break :blk content;
    };

    // If natural language query, use LLM to summarize relevant parts
    if (natural_lang) {
        var resolved_url_to_free: ?[]const u8 = null;
        defer if (resolved_url_to_free) |url| allocator.free(url);
        var llm_client_opt: ?llm.LlmClient = blk: {
            const resolved = resolveLlmConfigForThinking(allocator, cfg, cfg.model_thinking, null) catch break :blk null;
            resolved_url_to_free = resolved.resolved_url;
            const llm_config: llm.LlmConfig = .{
                .api_url = resolved.api_url,
                .model = resolved.model,
                .think = resolved.think,
                .debug = false,
            };
            break :blk llm.LlmClient.init(allocator, llm_config) catch null;
        };
        defer if (llm_client_opt) |*c| c.deinit();

        if (llm_client_opt) |*client| {
            const prompt = try std.fmt.allocPrint(allocator,
                \\Extract and summarize ONLY the sections relevant to this query. Be concise.
                \\Include file:line references where available.
                \\
                \\Query: {s}
                \\Capability: {s}
                \\
                \\Content:
                \\{s}
                \\
                \\Return a focused summary with relevant code snippets.
            , .{ query_text, cap_name, display_content });
            defer allocator.free(prompt);

            const response = client.complete(prompt, 800, 0.1, null) catch null;
            if (response) |raw| {
                defer client.allocator.free(raw);
                const stripped = llm.stripThinkBlock(raw);
                try stdout.print("# {s}\n\n{s}\n", .{ cap_name, std.mem.trim(u8, stripped, " \t\n\r") });
                try stdout.print("\n---\n\nSource: `{s}`\n", .{cap_path});
                try stdout.flush();
                return;
            }
        }
    }

    // Default: show full content
    try stdout.print("# {s}\n\n{s}\n", .{ cap_name, display_content });
    try stdout.flush();
}

/// Handles TIER 2: file path match → show file skeleton.
fn handleFileSkeletonQuery(
    allocator: std.mem.Allocator,
    file_path: []const u8,
    workspace: []const u8,
    guidance_dir: []const u8,
    query_text: []const u8,
) !void {
    _ = query_text;
    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    const skeleton = skeleton_mod.generateFileSkeleton(allocator, file_path, workspace, guidance_dir);

    if (skeleton) |skel| {
        defer allocator.free(skel);

        const title = try std.fmt.allocPrint(allocator, "File: `{s}`", .{file_path});
        defer allocator.free(title);

        const output = skeleton_mod.formatSkeletonOutput(allocator, skel, .{
            .title = title,
            .source_path = file_path,
        }) catch |err| {
            try stdout.print("Error formatting skeleton: {any}\n", .{err});
            try stdout.flush();
            return;
        };
        defer allocator.free(output);

        try stdout.print("{s}", .{output});
    } else {
        try stdout.print("# File: `{s}`\n\nUnable to generate skeleton. File may not be indexed.\n\nRun `guidance gen` to index this file.\n", .{file_path});
    }
    try stdout.flush();
}

/// Handles disambiguation when basename matches multiple files.
fn handleDisambiguationQuery(
    allocator: std.mem.Allocator,
    paths: []const []const u8,
    query_text: []const u8,
) !void {
    _ = allocator;
    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.print("# Multiple files match: `{s}`\n\n", .{query_text});
    try stdout.print("Found {d} files. Use the full path:\n\n", .{paths.len});

    for (paths) |path| {
        try stdout.print("  guidance explain \"{s}\"\n", .{path});
    }

    try stdout.flush();
}

/// Handles TIER 3: struct name match → show struct skeleton.
fn handleStructSkeletonQuery(
    allocator: std.mem.Allocator,
    struct_name: []const u8,
    guidance_dir: []const u8,
    query_text: []const u8,
) !void {
    _ = query_text;
    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    const info = findStructInfo(allocator, struct_name, guidance_dir) catch null;
    if (info) |struct_info| {
        defer {
            allocator.free(struct_info.source_path);
            allocator.free(struct_info.comment);
        }

        const end_line = blk: {
            const workspace = std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator) catch break :blk struct_info.line orelse 1;
            defer allocator.free(workspace);
            const abs_path = std.fs.path.join(allocator, &.{ workspace, struct_info.source_path }) catch break :blk struct_info.line orelse 1;
            defer allocator.free(abs_path);

            const src = common.readFileAlloc(allocator, abs_path, 10 * 1024 * 1024) orelse break :blk struct_info.line orelse 1;
            defer allocator.free(src);

            const start = struct_info.line orelse break :blk 1;
            break :blk findStructEndLine(src, start);
        };

        const skeleton = skeleton_mod.generateStructSkeleton(allocator, struct_name, guidance_dir);

        if (skeleton) |skel| {
            defer allocator.free(skel);

            const title = try std.fmt.allocPrint(allocator, "Explain: {s}", .{struct_name});
            defer allocator.free(title);

            const output = skeleton_mod.formatSkeletonOutput(allocator, skel, .{
                .title = title,
                .source_path = struct_info.source_path,
                .start_line = struct_info.line,
                .end_line = end_line,
            }) catch |err| {
                try stdout.print("Error formatting skeleton: {any}\n", .{err});
                try stdout.flush();
                return;
            };
            defer allocator.free(output);

            try stdout.print("{s}", .{output});
        } else {
            try stdout.print("# Struct: `{s}`\n\nStruct found but skeleton could not be generated.\n", .{struct_name});
        }
    } else {
        const skeleton = skeleton_mod.generateStructSkeleton(allocator, struct_name, guidance_dir);
        if (skeleton) |skel| {
            defer allocator.free(skel);

            const title = try std.fmt.allocPrint(allocator, "Struct: `{s}`", .{struct_name});
            defer allocator.free(title);

            const output = skeleton_mod.formatSkeletonOutput(allocator, skel, .{
                .title = title,
                .source_path = "unknown",
            }) catch |err| {
                try stdout.print("Error formatting skeleton: {any}\n", .{err});
                try stdout.flush();
                return;
            };
            defer allocator.free(output);

            try stdout.print("{s}", .{output});
        } else {
            try stdout.print("# Struct: `{s}`\n\nStruct not found in indexed files.\n\nRun `guidance gen` to index your source files.\n", .{struct_name});
        }
    }
    try stdout.flush();
}

/// Finds the end line of a struct by counting braces starting from start_line.
fn findStructEndLine(src: []const u8, start_line: u32) u32 {
    var lines = std.mem.splitScalar(u8, src, '\n');
    var line_no: u32 = 0;
    var brace_depth: isize = 0;
    var found_open: bool = false;
    var open_line: u32 = 0;

    while (lines.next()) |line| {
        line_no += 1;
        if (line_no < start_line) continue;

        for (line) |ch| {
            if (ch == '{') {
                if (!found_open) {
                    found_open = true;
                    open_line = line_no;
                }
                brace_depth += 1;
            } else if (ch == '}') {
                brace_depth -= 1;
                if (found_open and brace_depth == 0) {
                    return line_no;
                }
            }
        }
    }
    return open_line;
}

const StructInfo = struct {
    source_path: []const u8,
    line: ?u32,
    comment: []const u8,
};

fn findStructInfo(allocator: std.mem.Allocator, struct_name: []const u8, guidance_dir: []const u8) !?StructInfo {
    const json_dir = std.fs.path.join(allocator, &.{ guidance_dir, "src" }) catch return null;
    defer allocator.free(json_dir);

    const io = std.Io.Threaded.global_single_threaded.io();
    var dir = std.Io.Dir.cwd().openDir(io, json_dir, .{ .iterate = true }) catch return null;
    defer dir.close(io);

    var walker = dir.walk(allocator) catch return null;
    defer walker.deinit();

    while (walker.next(std.Io.Threaded.global_single_threaded.io()) catch null) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.basename, ".json")) continue;

        const full_path = std.fs.path.join(allocator, &.{ json_dir, entry.path }) catch continue;
        defer allocator.free(full_path);

        const content = std.Io.Dir.cwd().readFileAlloc(io, full_path, allocator, .limited(2 * 1024 * 1024)) catch continue;
        defer allocator.free(content);

        var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch continue;
        defer parsed.deinit();

        if (parsed.value != .object) continue;
        const root = parsed.value.object;
        const members = root.get("members") orelse continue;
        if (members != .array) continue;

        const meta = root.get("meta") orelse continue;
        if (meta != .object) continue;
        const src_val = meta.object.get("source") orelse continue;
        if (src_val != .string) continue;
        const source_path = src_val.string;

        for (members.array.items) |m| {
            if (m != .object) continue;
            const type_val = m.object.get("type") orelse continue;
            if (type_val != .string) continue;
            if (!std.mem.eql(u8, type_val.string, "struct")) continue;

            const name_val = m.object.get("name") orelse continue;
            if (name_val != .string) continue;

            if (std.ascii.eqlIgnoreCase(name_val.string, struct_name)) {
                const line = if (m.object.get("line")) |lv| blk: {
                    if (lv != .integer) break :blk null;
                    break :blk @as(u32, @intCast(lv.integer));
                } else null;

                const comment = if (m.object.get("comment")) |cv| blk: {
                    if (cv != .string) break :blk "";
                    break :blk cv.string;
                } else "";

                return .{
                    .source_path = try allocator.dupe(u8, source_path),
                    .line = line,
                    .comment = if (comment.len > 0) try allocator.dupe(u8, comment) else try allocator.dupe(u8, ""),
                };
            }
        }
    }

    return null;
}

// =============================================================================
// test command
// =============================================================================

/// Manages query keywords with ownership model; ensures invariants are preserved during initialization and cleanup.
const TestQuery = struct {
    query: []const u8,
    rubric: []const u8,
    accuracy: u8 = 0,
    relevance: u8 = 0,
    completeness: u8 = 0,
    observations: []const u8 = "",
};

const BenchmarkResult = struct {
    query: common.SharedString.Ref,
    acc: u8,
    rel: u8,
    cmpl: u8,
    nav: u8,
};

/// Validates command arguments and processes them in the Zig engine.
pub fn cmdBenchmark(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var no_llm = false;
    var db_path: ?[]const u8 = null;
    var workspace: ?[]const u8 = null;
    var guidance_dir: ?[]const u8 = null;
    var api_url: ?[]const u8 = null;
    var model: ?[]const u8 = null;
    var single_query: ?[]const u8 = null;
    var num_limit: ?usize = null;
    var verbose = false;
    var debug = false;

    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--no-llm")) {
            no_llm = true;
        } else if (std.mem.eql(u8, arg, "-v") or std.mem.eql(u8, arg, "--verbose")) {
            verbose = true;
        } else if (std.mem.eql(u8, arg, "--debug")) {
            debug = true;
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --db requires a value\n", .{});
                return;
            }
            db_path = args[i];
        } else if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --workspace requires a value\n", .{});
                return;
            }
            workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--guidance")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --guidance requires a value\n", .{});
                return;
            }
            guidance_dir = args[i];
        } else if (std.mem.eql(u8, arg, "--api-url")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --api-url requires a value\n", .{});
                return;
            }
            api_url = args[i];
        } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --model requires a value\n", .{});
                return;
            }
            model = args[i];
        } else if (std.mem.eql(u8, arg, "--num") or std.mem.eql(u8, arg, "-n")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --num requires a value\n", .{});
                return;
            }
            num_limit = std.fmt.parseInt(usize, args[i], 10) catch {
                std.debug.print("Error: --num must be a positive integer\n", .{});
                return;
            };
        } else if (!std.mem.startsWith(u8, arg, "-")) {
            // First non-flag argument is the query
            single_query = arg;
        }
    }

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const ws = if (workspace) |w|
        try common.resolvePath(allocator, cwd, w)
    else
        try allocator.dupe(u8, cwd);
    defer allocator.free(ws);

    const cfg = config_mod.loadConfig(allocator, ws) catch
        try config_mod.loadConfig(allocator, cwd);
    defer @constCast(&cfg).deinit();

    const db = db_path orelse cfg.db_path;
    const db_abs = try common.resolvePath(allocator, ws, db);
    defer allocator.free(db_abs);

    const gdir = guidance_dir orelse cfg.guidance_dir;
    const gdir_abs = try common.resolvePath(allocator, ws, gdir);
    defer allocator.free(gdir_abs);

    // Initialize LLM client for evaluation (when not --no-llm)
    var llm_client_opt: ?llm.LlmClient = null;
    var resolved_url_buf: ?[]const u8 = null;
    defer if (resolved_url_buf) |buf| allocator.free(buf);

    if (!no_llm) {
        const model_ref = model orelse (if (cfg.model_fast.len > 0) cfg.model_fast else cfg.model_default);

        const resolved = resolveLlmConfigForThinking(
            allocator,
            &cfg,
            model_ref,
            api_url,
        ) catch {
            const fallback_config: llm.LlmConfig = .{
                .api_url = api_url orelse config_mod.DEFAULT_API_URL,
                .model = model_ref,
                .think = false,
            };
            llm_client_opt = llm.LlmClient.init(allocator, fallback_config) catch null;
            return;
        };
        resolved_url_buf = resolved.resolved_url;
        const llm_config: llm.LlmConfig = .{
            .api_url = resolved.api_url,
            .model = resolved.model,
            .think = false,
            .debug = debug,
        };
        llm_client_opt = llm.LlmClient.init(allocator, llm_config) catch null;
    }
    defer if (llm_client_opt) |*c| c.deinit();

    // Load queries: from benchmarks.txt, single query arg, or generate from module comments
    const all_queries = if (single_query) |sq| blk: {
        var single: std.ArrayList(TestQuery) = .empty;
        try single.append(allocator, .{ .query = try allocator.dupe(u8, sq), .rubric = &.{} });
        break :blk try single.toOwnedSlice(allocator);
    } else blk: {
        const from_file = loadBenchmarkQueries(allocator, gdir_abs) catch null;
        break :blk from_file orelse try generateTestQueries(allocator, gdir_abs);
    };
    const queries = if (num_limit) |n| all_queries[0..@min(n, all_queries.len)] else all_queries;
    defer {
        for (all_queries) |q| {
            allocator.free(q.query);
            if (q.rubric.len > 0) allocator.free(q.rubric);
            if (q.observations.len > 0) allocator.free(q.observations);
        }
        allocator.free(all_queries);
    }

    std.debug.print("# Explain Benchmark Results\n\n", .{});
    std.debug.print("Testing {d} queries (LLM evaluation: {s})\n\n", .{ queries.len, if (llm_client_opt != null) "enabled" else "disabled" });

    var total_acc: u32 = 0;
    var total_rel: u32 = 0;
    var total_cmpl: u32 = 0;
    var total_nav: u32 = 0;
    var excellent_count: usize = 0;
    var good_count: usize = 0;
    var weak_count: usize = 0;

    var benchmark_results: std.ArrayList(BenchmarkResult) = .empty;
    defer {
        for (benchmark_results.items) |*result| {
            result.query.release(allocator);
        }
        benchmark_results.deinit(allocator);
    }

    // Run each query
    for (queries) |tq| {
        std.debug.print("## Query: `{s}`\n\n", .{tq.query});

        const query_text = try allocator.dupe(u8, tq.query);
        defer allocator.free(query_text);

        // Capture results via the same strategy pipeline as `guidance explain`
        const stages = stages_blk: {
            const embedder = try createEmbedderWithFallback(allocator, &cfg);
            defer embedder.deinit();

            var gdb = GuidanceDb.init(allocator, db_abs, embedder) catch |err| {
                std.debug.print("Error opening database: {s}\n", .{@errorName(err)});
                return;
            };
            defer gdb.deinit();

            var aliases_opt = loadAliases(allocator, gdir_abs);
            defer if (aliases_opt) |*a| a.deinit();

            const matches = query_strategy_mod.buildDefaultStrategies();

            break :stages_blk try query_strategy_mod.executeQueryWithMatch(
                allocator,
                &gdb,
                query_text,
                query_text,
                ws,
                aliases_opt,
                &matches,
            );
        };
        defer {
            types.freeStages(allocator, stages);
            allocator.free(stages);
        }

        // Score results using LLM evaluation when available
        var acc: ?u8 = null;
        var rel: ?u8 = null;
        var cmpl: ?u8 = null;
        var nav: ?u8 = null;
        var obs_buf: [512]u8 = undefined;
        var obs_len: usize = 0;
        var llm_evaluated = false;

        if (llm_client_opt) |*client| {
            // Build stages summary for LLM evaluation (mirrors actual explain output)
            var results_buf_aw: std.Io.Writer.Allocating = .init(allocator);
            defer results_buf_aw.deinit();
            const rw = &results_buf_aw.writer;
            try rw.print("Query: \"{s}\"\n\n", .{query_text});
            if (stages.len > 0) {
                try rw.print("Found {d} stages:\n\n", .{stages.len});
                for (stages[0..@min(5, stages.len)]) |s| {
                    try rw.print("- {s} ({s})\n", .{ s.source, @tagName(s.kind) });
                    const ctrimmed = std.mem.trim(u8, s.content, " \t\n\r");
                    if (ctrimmed.len > 0) {
                        const first_line = std.mem.indexOfScalar(u8, ctrimmed, '\n') orelse ctrimmed.len;
                        try rw.print("  Content: {s}\n", .{ctrimmed[0..@min(first_line, 100)]});
                    }
                }
            } else {
                try rw.print("No results found.\n", .{});
            }

            const eval_prompt = try std.fmt.allocPrint(allocator,
                \\You are a code intelligence evaluator for AI subagent workflows. Assess whether search results provide actionable code navigation for an AI assistant that needs to understand, modify, or extend the codebase.
                \\
                \\Query: "{s}"
                \\Rubric (expected answer criteria): {s}
                \\
                \\Query and results:
                \\{s}
                \\
                \\Rate each dimension (0-10):
                \\- Accuracy: Results directly match what the query asks for. No false positives. CRITICAL: Check if results satisfy the rubric criteria.
                \\- Relevance: Top results are the most important/defining code for the query. First result is the best entry point.
                \\- Completeness: All critical code locations, types, and functions needed to understand the topic are found. No major gaps.
                \\- Navigation Quality: Results provide file paths, line numbers, function signatures, and context that enable an AI to immediately read and understand the relevant code.
                \\
                \\Score 9-10: Excellent code intelligence — AI can navigate directly to implementation with confidence. Rubric criteria satisfied.
                \\Score 7-8: Good results with minor gaps or noise. Rubric mostly satisfied.
                \\Score 5-6: Partial coverage, significant noise, or missing critical locations. Rubric partially satisfied.
                \\Score 3-4: Mostly irrelevant or incomplete for subagent use. Rubric not satisfied.
                \\Score 0-2: No useful results or wrong topic entirely. Rubric not satisfiable — query is about something not in codebase.
                \\
                \\Respond EXACTLY in this format (no other text):
                \\Accuracy: <0-10>
                \\Relevance: <0-10>
                \\Completeness: <0-10>
                \\Navigation: <0-10>
                \\Observation: <one sentence assessing subagent utility>
            , .{ query_text, tq.rubric, results_buf_aw.written() });
            defer allocator.free(eval_prompt);

            const response_opt = client.complete(eval_prompt, 400, 0.1, null) catch |err| blk: {
                std.debug.print("Warning: LLM complete() failed: {s}\n", .{@errorName(err)});
                break :blk null;
            };
            if (response_opt) |response| {
                defer client.allocator.free(response);
                const stripped = llm.stripThinkBlock(response);

                // Parse scores from response
                var lines = std.mem.splitScalar(u8, stripped, '\n');
                while (lines.next()) |line| {
                    const trimmed = std.mem.trim(u8, line, "\t\r");

                    // Check for score lines like "### Accuracy: 8/10", "Accuracy: 8", "- **Accuracy:** 8/10"
                    if (std.mem.indexOf(u8, trimmed, "Accuracy") != null) {
                        if (acc == null) acc = parseScoreFromLine(trimmed);
                    } else if (std.mem.indexOf(u8, trimmed, "Relevance") != null) {
                        if (rel == null) rel = parseScoreFromLine(trimmed);
                    } else if (std.mem.indexOf(u8, trimmed, "Completeness") != null) {
                        if (cmpl == null) cmpl = parseScoreFromLine(trimmed);
                    } else if (std.mem.indexOf(u8, trimmed, "Navigation") != null) {
                        if (nav == null) nav = parseScoreFromLine(trimmed);
                    } else if (std.mem.indexOf(u8, trimmed, "Observation")) |_| {
                        if (std.mem.indexOf(u8, trimmed, ":")) |colon| {
                            var obs_start = colon + 1;
                            while (obs_start < trimmed.len and (trimmed[obs_start] == ' ' or trimmed[obs_start] == '*')) {
                                obs_start += 1;
                            }
                            const obs_text = trimmed[obs_start..];
                            obs_len = @min(obs_text.len, obs_buf.len - 1);
                            @memcpy(obs_buf[0..obs_len], obs_text[0..obs_len]);
                            obs_buf[obs_len] = 0;
                        }
                    }
                }
                llm_evaluated = (acc != null and rel != null and cmpl != null and nav != null);
            }
        }

        // If LLM evaluation failed, use "-" for scores
        const eval_status = if (llm_evaluated) "LLM" else "FALLBACK";

        // Get actual values for display (use "-" if not evaluated)
        const acc_val = acc orelse 0;
        const rel_val = rel orelse 0;
        const cmpl_val = cmpl orelse 0;
        const nav_val = nav orelse 0;
        const acc_display = if (acc) |v| try std.fmt.allocPrint(allocator, "{d}", .{v}) else try std.fmt.allocPrint(allocator, "-", .{});
        defer allocator.free(acc_display);
        const rel_display = if (rel) |v| try std.fmt.allocPrint(allocator, "{d}", .{v}) else try std.fmt.allocPrint(allocator, "-", .{});
        defer allocator.free(rel_display);
        const cmpl_display = if (cmpl) |v| try std.fmt.allocPrint(allocator, "{d}", .{v}) else try std.fmt.allocPrint(allocator, "-", .{});
        defer allocator.free(cmpl_display);
        const nav_display = if (nav) |v| try std.fmt.allocPrint(allocator, "{d}", .{v}) else try std.fmt.allocPrint(allocator, "-", .{});
        defer allocator.free(nav_display);

        // Only count toward statistics if actually evaluated
        if (llm_evaluated) {
            total_acc += acc_val;
            total_rel += rel_val;
            total_cmpl += cmpl_val;
            total_nav += nav_val;

            try benchmark_results.append(allocator, .{
                .query = try common.SharedString.Ref.init(allocator, tq.query),
                .acc = acc_val,
                .rel = rel_val,
                .cmpl = cmpl_val,
                .nav = nav_val,
            });

            if (acc_val >= 9) {
                excellent_count += 1;
            } else if (acc_val >= 7) {
                good_count += 1;
            } else {
                weak_count += 1;
            }
        }

        std.debug.print("| Metric | Score |\n", .{});
        std.debug.print("|--------|-------|\n", .{});
        std.debug.print("| Accuracy | {s}/10 |\n", .{acc_display});
        std.debug.print("| Relevance | {s}/10 |\n", .{rel_display});
        std.debug.print("| Completeness | {s}/10 |\n", .{cmpl_display});
        std.debug.print("| Navigation | {s}/10 |\n", .{nav_display});
        std.debug.print("| Results | {d} |\n", .{stages.len});
        std.debug.print("| Evaluation | {s} |\n\n", .{eval_status});

        // Show top 3 stages
        std.debug.print("**Top Stages:**\n", .{});
        for (stages[0..@min(3, stages.len)]) |s| {
            std.debug.print("- `{s}` ({s})\n", .{ s.source, @tagName(s.kind) });
        }
        if (obs_len > 0) {
            std.debug.print("\n**Observation:** {s}\n", .{obs_buf[0..obs_len]});
        }
        std.debug.print("\n---\n\n", .{});
    }

    // Summary
    const n = queries.len;
    const evaluated_count = excellent_count + good_count + weak_count;
    if (n > 0 and evaluated_count > 0) {
        std.debug.print("# Benchmark Results\n\n", .{});

        std.mem.sort(BenchmarkResult, benchmark_results.items, {}, struct {
            fn less(_: void, a: BenchmarkResult, b: BenchmarkResult) bool {
                const avg_a = @as(f32, @floatFromInt(a.rel + a.acc + a.cmpl)) / 3.0;
                const avg_b = @as(f32, @floatFromInt(b.rel + b.acc + b.cmpl)) / 3.0;
                return avg_a > avg_b;
            }
        }.less);

        std.debug.print("| Query | Relevance | Accuracy | Completeness | Avg |\n", .{});
        std.debug.print("|-------|-----------|----------|--------------|-----|\n", .{});

        var total_row_rel: f32 = 0;
        var total_row_acc: f32 = 0;
        var total_row_cmpl: f32 = 0;
        for (benchmark_results.items) |result| {
            const avg = @as(f32, @floatFromInt(result.rel + result.acc + result.cmpl)) / 3.0;
            total_row_rel += @as(f32, @floatFromInt(result.rel));
            total_row_acc += @as(f32, @floatFromInt(result.acc));
            total_row_cmpl += @as(f32, @floatFromInt(result.cmpl));
            std.debug.print("| {s} | {d} | {d} | {d} | {d:.1} |\n", .{
                result.query.slice(),
                result.rel,
                result.acc,
                result.cmpl,
                avg,
            });
        }

        const final_avg_rel = total_row_rel / @as(f32, @floatFromInt(evaluated_count));
        const final_avg_acc = total_row_acc / @as(f32, @floatFromInt(evaluated_count));
        const final_avg_cmpl = total_row_cmpl / @as(f32, @floatFromInt(evaluated_count));
        const final_avg = (final_avg_rel + final_avg_acc + final_avg_cmpl) / 3.0;
        std.debug.print("| **Average** | **{d:.1}** | **{d:.1}** | **{d:.1}** | **{d:.1}** |\n", .{
            final_avg_rel,
            final_avg_acc,
            final_avg_cmpl,
            final_avg,
        });
    }
}

/// Converts a Zig line string into a numeric score value.
fn parseScoreFromLine(line: []const u8) ?u8 {
    const colon = std.mem.indexOfScalar(u8, line, ':') orelse return null;
    var i = colon + 1;
    // Skip spaces and markdown decoration (* -)
    while (i < line.len and (line[i] == ' ' or line[i] == '*' or line[i] == '-')) i += 1;
    // Collect consecutive ASCII digits
    const start = i;
    while (i < line.len and line[i] >= '0' and line[i] <= '9') i += 1;
    if (i == start) return null;
    const v = std.fmt.parseInt(u8, line[start..i], 10) catch return null;
    return @min(10, v);
}

/// Loads benchmark query data from benchmarks.md into a Zig test query slice.
/// Format: query on its own line, then ---, then rubric lines until next --- or EOF.
fn loadBenchmarkQueries(allocator: std.mem.Allocator, guidance_dir: []const u8) ![]TestQuery {
    const io = std.Io.Threaded.global_single_threaded.io();
    const path = try std.fs.path.join(allocator, &.{ guidance_dir, "benchmarks.md" });
    defer allocator.free(path);

    var file = std.Io.Dir.cwd().openFile(io, path, .{}) catch |err| return err;
    defer file.close(io);

    const content = std.Io.Dir.cwd().readFileAlloc(io, path, allocator, .limited(512 * 1024)) catch |err| return err;
    defer allocator.free(content);

    var queries: std.ArrayList(TestQuery) = .empty;
    errdefer {
        for (queries.items) |q| {
            allocator.free(q.query);
            allocator.free(q.rubric);
        }
        queries.deinit(allocator);
    }

    var lines = std.mem.splitScalar(u8, content, '\n');
    var current_query: ?[]const u8 = null;
    var rubric_lines: std.ArrayList([]const u8) = .empty;
    defer {
        for (rubric_lines.items) |s| allocator.free(s);
        rubric_lines.deinit(allocator);
    }

    while (lines.next()) |line| {
        const trimmed = std.mem.trim(u8, line, " \t\r");
        if (trimmed.len == 0) continue;

        if (std.mem.startsWith(u8, trimmed, "#")) {
            if (current_query != null) {
                try queries.append(allocator, .{
                    .query = current_query.?,
                    .rubric = try std.mem.join(allocator, "\n", rubric_lines.items),
                });
                current_query = null;
                for (rubric_lines.items) |s| allocator.free(s);
                rubric_lines.clearRetainingCapacity();
            }
            continue;
        }

        if (std.mem.eql(u8, trimmed, "---")) {
            if (current_query != null and rubric_lines.items.len > 0) {
                try queries.append(allocator, .{
                    .query = current_query.?,
                    .rubric = try std.mem.join(allocator, "\n", rubric_lines.items),
                });
                current_query = null;
                for (rubric_lines.items) |s| allocator.free(s);
                rubric_lines.clearRetainingCapacity();
            }
            continue;
        }

        if (current_query == null) {
            current_query = try allocator.dupe(u8, trimmed);
        } else {
            try rubric_lines.append(allocator, try allocator.dupe(u8, trimmed));
        }
    }

    if (current_query != null) {
        try queries.append(allocator, .{
            .query = current_query.?,
            .rubric = try std.mem.join(allocator, "\n", rubric_lines.items),
        });
        for (rubric_lines.items) |s| allocator.free(s);
        rubric_lines.clearRetainingCapacity();
    }

    return queries.toOwnedSlice(allocator);
}

/// Generates test queries for the guidance engine using provided allocator and directory data.
fn generateTestQueries(allocator: std.mem.Allocator, guidance_dir: []const u8) ![]TestQuery {
    var queries: std.ArrayList(TestQuery) = .empty;
    errdefer {
        for (queries.items) |q| {
            allocator.free(q.query);
            if (q.rubric.len > 0) allocator.free(q.rubric);
            if (q.observations.len > 0) allocator.free(q.observations);
        }
        queries.deinit(allocator);
    }

    // Scan .guidance/src/**/*.json for module-level comments
    const io = std.Io.Threaded.global_single_threaded.io();
    const src_dir = try std.fs.path.join(allocator, &.{ guidance_dir, "src" });
    defer allocator.free(src_dir);

    var dir = std.Io.Dir.cwd().openDir(io, src_dir, .{ .iterate = true }) catch |err| {
        std.debug.print("Warning: cannot open guidance src dir: {s}\n", .{@errorName(err)});
        return queries.toOwnedSlice(allocator);
    };
    defer dir.close(io);

    var walker = try dir.walk(allocator);
    defer walker.deinit();

    while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, ".json")) continue;

        const json_path = try std.fs.path.join(allocator, &.{ src_dir, entry.path });
        defer allocator.free(json_path);

        const file = std.Io.Dir.cwd().openFile(std.Io.Threaded.global_single_threaded.io(), json_path, .{}) catch continue;
        defer file.close(std.Io.Threaded.global_single_threaded.io());

        const content = std.Io.Dir.cwd().readFileAlloc(std.Io.Threaded.global_single_threaded.io(), json_path, allocator, .limited(1024 * 1024)) catch continue;
        defer allocator.free(content);

        var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch continue;
        defer parsed.deinit();

        if (parsed.value != .object) continue;
        const root = parsed.value.object;

        // Get module-level comment
        const comment_val = root.get("comment") orelse continue;
        if (comment_val != .string) continue;
        const comment = comment_val.string;
        if (comment.len < 20) continue;

        // Get module name
        const meta_val = root.get("meta") orelse continue;
        if (meta_val != .object) continue;
        const module_val = meta_val.object.get("module") orelse continue;
        if (module_val != .string) continue;
        const module = module_val.string;

        // Extract module basename (last component)
        const module_basename = std.mem.lastIndexOfScalar(u8, module, '.');
        const basename = if (module_basename) |idx| module[idx + 1 ..] else module;

        // Generate simple query from module name
        const query1 = try std.fmt.allocPrint(allocator, "{s}", .{basename});
        try queries.append(allocator, .{ .query = query1, .rubric = &.{} });

        // Generate question-style query
        const query2 = try std.fmt.allocPrint(allocator, "How does {s} work?", .{basename});
        try queries.append(allocator, .{ .query = query2, .rubric = &.{} });

        // Limit to 20 queries
        if (queries.items.len >= 20) break;
    }

    return queries.toOwnedSlice(allocator);
}

// =============================================================================
// guidance telemetry — query frequency stats
// =============================================================================

/// Processes telemetry data using an allocator and returns no value on success.
pub fn cmdTelemetry(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var limit: usize = 10;
    var j: usize = 0;
    while (j < args.len) : (j += 1) {
        const arg = args[j];
        if (std.mem.eql(u8, arg, "--top-queries") or std.mem.eql(u8, arg, "--slowest")) {
            j += 1;
            if (j < args.len) limit = std.fmt.parseInt(usize, args[j], 10) catch limit;
        }
        // --tier-breakdown: future work
    }

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);
    const cfg = config_mod.loadConfig(allocator, cwd) catch
        try config_mod.loadConfig(allocator, cwd);
    defer @constCast(&cfg).deinit();

    const db_path = try common.resolvePath(allocator, cwd, cfg.db_path);
    defer allocator.free(db_path);

    std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), db_path, .{}) catch {
        std.debug.print("No .guidance.db found at {s}\n", .{db_path});
        return;
    };

    const embedder = try createEmbedderWithFallback(allocator, &cfg);
    defer embedder.deinit();
    var db = GuidanceDb.init(allocator, db_path, embedder) catch return;
    defer db.deinit();

    const entries = try db.topQueries(allocator, limit);
    defer {
        for (entries) |e| {
            allocator.free(e.query);
            allocator.free(e.tier);
        }
        allocator.free(entries);
    }

    var ws: common.WriterState = .{};
    ws.initStdout();
    const w = ws.writer();
    try w.print("# Top Queries\n\n", .{});
    if (entries.len == 0) {
        try w.print("No queries logged yet.\n", .{});
    } else {
        try w.print("| Rank | Query | Count | Avg Latency (ms) | Tier |\n", .{});
        try w.print("|------|-------|-------|-----------------|------|\n", .{});
        for (entries, 0..) |e, idx| {
            try w.print("| {d} | `{s}` | {d} | {d:.1} | {s} |\n", .{
                idx + 1, e.query, e.count, e.avg_latency_ms, e.tier,
            });
        }
    }
    try w.flush();
}

// =============================================================================
// guidance serve — MCP server (STDIO JSON-RPC 2.0)
// =============================================================================

/// Handles allocation and execution of the Zig command with specified arguments.
pub fn cmdServe(allocator: std.mem.Allocator, args: []const []const u8) !void {
    const mcp_mod = @import("mcp.zig");
    try mcp_mod.serve(allocator, args);
}

const testing = std.testing;

test "isShortQuery: empty string is short" {
    try testing.expect(isShortQuery(""));
    try testing.expect(isShortQuery("   "));
}

test "isShortQuery: one word is short" {
    try testing.expect(isShortQuery("foo"));
    try testing.expect(isShortQuery("  bar  "));
}

test "isShortQuery: two words is not short" {
    try testing.expect(!isShortQuery("foo bar"));
    try testing.expect(!isShortQuery("one two"));
}

test "isShortQuery: three words is not short" {
    try testing.expect(!isShortQuery("foo bar baz"));
    try testing.expect(!isShortQuery("one two three"));
}

test "isShortQuery: question mark makes it not short" {
    try testing.expect(!isShortQuery("foo?"));
    try testing.expect(!isShortQuery("foo bar?"));
}

test "isShortQuery: question word prefixes make it not short" {
    try testing.expect(!isShortQuery("how does this work"));
    try testing.expect(!isShortQuery("what is foo"));
    try testing.expect(!isShortQuery("where is bar"));
    try testing.expect(!isShortQuery("when does it run"));
    try testing.expect(!isShortQuery("why is this happening"));
    try testing.expect(!isShortQuery("if this happens"));
    try testing.expect(!isShortQuery("does it work"));
}

test "isShortQuery: question words case insensitive" {
    try testing.expect(!isShortQuery("How does this work"));
    try testing.expect(!isShortQuery("WHAT is foo"));
    try testing.expect(!isShortQuery("Where IS bar"));
}

test "isShortQuery: regular two-word queries are not short" {
    try testing.expect(!isShortQuery("sync json"));
    try testing.expect(!isShortQuery("parse file"));
    try testing.expect(!isShortQuery("load config"));
}
