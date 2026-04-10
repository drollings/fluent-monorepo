//! query_engine.zig — explain, staged, show, test, check commands.
//!
//! Extracted from main.zig (M1.6) to keep individual file sizes navigable.
//! All public functions are called from main.zig's command dispatch switch.

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

// =============================================================================
// explain — shared types
// =============================================================================

const SkillExcerpt = struct { name: []const u8, excerpt: []const u8 };
/// Manages structured excerpt data for query processing; owned by the engine; key invariant is consistent structure.
const ExcerptEntry = struct {
    file_path: []const u8, // borrowed from SearchResult
    label: []const u8, // owned: "src/foo.zig:42"
    code: []const u8, // owned: pruned source block
    lang: []const u8, // borrowed constant
};
/// File match metadata with path, count, and line numbers.
const FileMatchItem = struct { path: []const u8, count: usize, lines: []usize };

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

/// LLM filter mode for query results (auto, force, skip).
const FilterMode = enum {
    /// Auto-detect: apply LLM filter only for long queries (5+ words).
    auto,
    /// Always apply LLM filter (even for short queries).
    force,
    /// Never apply LLM filter (always fast path).
    skip,
};

/// Command-line arguments for the explain command.
const ExplainArgs = struct {
    query_str: ?[]const u8 = null,
    limit: usize = 10,
    /// Path to .guidance.db. Defaults to config db_path or DEFAULT_GUIDANCE_DB_PATH.
    db_path: ?[]const u8 = null,
    workspace: ?[]const u8 = null,
    guidance: ?[]const u8 = null,
    api_url: []const u8 = config_mod.DEFAULT_API_URL,
    model: []const u8 = config_mod.DEFAULT_MODEL,
    /// Skip LLM synthesis; emit structural output only.
    no_llm: bool = false,
    verbose: bool = false,
    debug: bool = false,
    /// Use new staged pipeline (default: true).  --staged=false → legacy path.
    staged: bool = true,
    /// LLM relevance filtering mode.
    filter: FilterMode = .auto,
    /// Disable deterministic DRIFT follow-up generation.
    no_drift: bool = false,
    /// Absolute path to the capabilities tree; sourced from cfg.capabilities_dir.
    capabilities_dir: []const u8 = "",
};

/// Checks if a query string is short enough to be considered valid.
fn isShortQuery(query: []const u8) bool {
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    if (trimmed.len == 0) return true;

    // Question mark at end triggers LLM filter
    if (trimmed[trimmed.len - 1] == '?') return false;

    // Check for question word prefixes (case-insensitive, with trailing space)
    const question_prefixes = [_][]const u8{ "if ", "how ", "where ", "when ", "does ", "why ", "what " };
    for (question_prefixes) |prefix| {
        if (trimmed.len >= prefix.len) {
            const candidate = trimmed[0..prefix.len];
            var i: usize = 0;
            while (i < prefix.len) : (i += 1) {
                if (std.ascii.toLower(candidate[i]) != std.ascii.toLower(prefix[i])) break;
            }
            if (i == prefix.len) return false;
        }
    }

    // Word count: 1 or fewer = short (no LLM filter)
    var tok = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
    var count: usize = 0;
    while (tok.next()) |_| {
        count += 1;
        if (count > 1) return false;
    }
    return true;
}

// =============================================================================
// explain — phase helpers
// =============================================================================

/// Collects skill excerpts from JSON paths using an allocator, returning a slice of SkillExcerpt objects.
fn collectSkillExcerpts(
    allocator: std.mem.Allocator,
    top_json_path: []const u8,
    guidance_dir: []const u8,
    workspace: []const u8,
) ![]SkillExcerpt {
    var out: std.ArrayList(SkillExcerpt) = .empty;
    errdefer {
        for (out.items) |se| {
            allocator.free(se.name);
            allocator.free(se.excerpt);
        }
        out.deinit(allocator);
    }

    const skills_str = loadSkillsFromJson(allocator, top_json_path) orelse return out.toOwnedSlice(allocator);
    defer allocator.free(skills_str);

    var seen: std.StringHashMapUnmanaged(void) = .{};
    defer seen.deinit(allocator);

    var sp = std.mem.splitScalar(u8, skills_str, '\n');
    while (sp.next()) |raw| {
        const name = std.mem.trim(u8, raw, " \t\r");
        if (name.len == 0 or seen.contains(name)) continue;
        try seen.put(allocator, name, {});
        if (loadSkillPara(allocator, guidance_dir, workspace, name)) |para| {
            try out.append(allocator, .{
                .name = try allocator.dupe(u8, name),
                .excerpt = para,
            });
        }
    }
    return out.toOwnedSlice(allocator);
}

/// Collects source excerpts matching search terms from memory allocator results.
fn collectSourceExcerpts(
    allocator: std.mem.Allocator,
    results: []const SearchResult,
    search_terms: []const []const u8,
    workspace: []const u8,
) ![]ExcerptEntry {
    var out: std.ArrayList(ExcerptEntry) = .empty;
    errdefer {
        for (out.items) |e| {
            allocator.free(e.label);
            allocator.free(e.code);
        }
        out.deinit(allocator);
    }

    // Re-sort: exact-name-match first, then non-test, then score.
    var sorted: std.ArrayList(SearchResult) = .empty;
    defer sorted.deinit(allocator);
    for (results) |r| try sorted.append(allocator, r);
    std.sort.insertion(SearchResult, sorted.items, search_terms, struct {
        fn lessThan(terms: []const []const u8, a: SearchResult, b: SearchResult) bool {
            const a_exact = isExactNameMatch(a.name, terms);
            const b_exact = isExactNameMatch(b.name, terms);
            if (a_exact != b_exact) return a_exact;
            const a_test = std.mem.eql(u8, a.node_type, "test_decl");
            const b_test = std.mem.eql(u8, b.node_type, "test_decl");
            if (a_test != b_test) return !a_test;
            return a.score > b.score;
        }
    }.lessThan);

    var seen_files: std.StringHashMapUnmanaged(void) = .{};
    defer seen_files.deinit(allocator);

    for (sorted.items) |r| {
        if (out.items.len >= 3) break;
        if (r.source.len == 0 or seen_files.contains(r.source)) continue;
        const start_line = r.line orelse continue;

        const src_abs = try std.fs.path.join(allocator, &.{ workspace, r.source });
        defer allocator.free(src_abs);

        const src_opt: ?[]const u8 = blk: {
            const f = std.fs.openFileAbsolute(src_abs, .{}) catch break :blk null;
            defer f.close();
            break :blk f.readToEndAlloc(allocator, 10 * 1024 * 1024) catch null;
        };
        const src = src_opt orelse continue;
        defer allocator.free(src);

        const code = try explainExtractExcerpt(allocator, src, start_line, r.node_type);
        if (code.len == 0) {
            allocator.free(code);
            continue;
        }
        const lang = common.langFromPath(r.source);
        const label = try std.fmt.allocPrint(allocator, "{s}:{d}", .{ r.source, start_line });
        try out.append(allocator, .{ .file_path = r.source, .label = label, .code = code, .lang = lang });
        try seen_files.put(allocator, r.source, {});
    }
    return out.toOwnedSlice(allocator);
}

/// Filters search results to return top matching files based on provided terms.
fn grepTopFiles(
    allocator: std.mem.Allocator,
    results: []const SearchResult,
    search_terms: []const []const u8,
    workspace: []const u8,
) ![]FileMatchItem {
    var out: std.ArrayList(FileMatchItem) = .empty;
    errdefer {
        for (out.items) |fm| allocator.free(fm.lines);
        out.deinit(allocator);
    }

    var seen: std.StringHashMapUnmanaged(void) = .{};
    defer seen.deinit(allocator);

    for (results[0..@min(5, results.len)]) |r| {
        if (r.source.len == 0 or seen.contains(r.source)) continue;
        try seen.put(allocator, r.source, {});

        const abs = try std.fs.path.join(allocator, &.{ workspace, r.source });
        defer allocator.free(abs);

        const matches = try explainGrepFile(allocator, abs, search_terms, 10);
        if (matches.len > 0) {
            try out.append(allocator, .{ .path = r.source, .count = matches.len, .lines = matches });
        } else {
            allocator.free(matches);
        }
    }

    std.sort.insertion(FileMatchItem, out.items, {}, struct {
        fn less(_: void, a: FileMatchItem, b: FileMatchItem) bool {
            return a.count > b.count;
        }
    }.less);

    return out.toOwnedSlice(allocator);
}

/// Renders explanation output using provided data structures and allocator.
fn renderExplainOutput(
    allocator: std.mem.Allocator,
    query_text: []const u8,
    results: []const SearchResult,
    search_terms: []const []const u8,
    ai_summary: ?[]const u8,
    skill_excerpts: []const SkillExcerpt,
    excerpts: []const ExcerptEntry,
    file_matches: []const FileMatchItem,
) !void {
    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.print("# Explain: {s}\n\n", .{query_text});

    if (ai_summary) |s| {
        const trimmed_s = std.mem.trim(u8, s, " \t\n\r");
        if (trimmed_s.len > 0) try stdout.print("{s}\n\n", .{trimmed_s});
    }

    try stdout.print("---\n", .{});
    try stdout.print("**Source**: `{s}`\n", .{results[0].source});

    for (skill_excerpts[0..@min(2, skill_excerpts.len)]) |se| {
        const first_nl = std.mem.indexOfScalar(u8, se.excerpt, '\n') orelse se.excerpt.len;
        try stdout.print("**Pattern**: `{s}` — {s}\n", .{ se.name, se.excerpt[0..@min(first_nl, 120)] });
    }
    try stdout.print("\n", .{});

    for (excerpts) |e| {
        try stdout.print("```{s}\n// {s}\n{s}\n```\n\n", .{ e.lang, e.label, e.code });
    }

    // Keywords: public non-test members from primary source JSON, excluding search terms.
    {
        var kw_buf: std.ArrayList(u8) = .empty;
        defer kw_buf.deinit(allocator);
        var kw_count: usize = 0;

        if (loadPublicMemberNames(allocator, results[0].file_path)) |names| {
            defer {
                for (names) |n| allocator.free(n);
                allocator.free(names);
            }
            for (names) |mname| {
                if (kw_count >= 8) break;
                const mname_lower = try std.ascii.allocLowerString(allocator, mname);
                defer allocator.free(mname_lower);
                const is_term = for (search_terms) |term| {
                    if (std.mem.eql(u8, mname_lower, term)) break true;
                } else false;
                if (is_term) continue;
                if (kw_count > 0) try kw_buf.appendSlice(allocator, ", ");
                try kw_buf.writer(allocator).print("`{s}`", .{mname});
                kw_count += 1;
            }
        }
        if (kw_count > 0) try stdout.print("**See Also**: {s}\n\n", .{kw_buf.items});
    }

    // See also: used_by from top result + secondary file paths.
    {
        var see_buf: std.ArrayList(u8) = .empty;
        defer see_buf.deinit(allocator);
        var see_count: usize = 0;

        var ub_from_json: ?[][]const u8 = null;
        defer if (ub_from_json) |ub| {
            for (ub) |s| allocator.free(s);
            allocator.free(ub);
        };
        const top_used_by: [][]const u8 = if (results[0].used_by.len > 0)
            results[0].used_by
        else blk: {
            ub_from_json = loadUsedByFromJson(allocator, results[0].file_path);
            break :blk ub_from_json orelse &.{};
        };

        for (top_used_by[0..@min(4, top_used_by.len)]) |ub| {
            if (see_count > 0) try see_buf.appendSlice(allocator, ", ");
            try see_buf.writer(allocator).print("`{s}`", .{ub});
            see_count += 1;
        }
        for (results[1..@min(results.len, 6)]) |r| {
            if (see_count >= 6) break;
            if (r.source.len == 0 or std.mem.eql(u8, r.source, results[0].source)) continue;
            if (see_count > 0) try see_buf.appendSlice(allocator, ", ");
            try see_buf.writer(allocator).print("`{s}`", .{r.source});
            see_count += 1;
        }
        if (see_count > 0) try stdout.print("**See also**: {s}\n\n", .{see_buf.items});
    }

    if (file_matches.len > 0) {
        try stdout.print("### Files with most matches\n\n", .{});
        for (file_matches[0..@min(3, file_matches.len)]) |fm| {
            try stdout.print("- `{s}` ({d} matches): lines ", .{ fm.path, fm.count });
            for (fm.lines[0..@min(10, fm.lines.len)], 0..) |ln, li| {
                if (li > 0) try stdout.print(", ", .{});
                try stdout.print("{d}", .{ln});
            }
            try stdout.print("\n", .{});
        }
        try stdout.print("\n", .{});
    }

    try stdout.flush();
}

/// Processes allocation arguments to generate explanation output.
pub fn cmdExplain(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var ea: ExplainArgs = .{};
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "-l") or std.mem.eql(u8, arg, "--limit")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --limit requires a value\n", .{});
                return;
            }
            ea.limit = std.fmt.parseInt(usize, args[i], 10) catch 10;
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --db requires a value\n", .{});
                return;
            }
            ea.db_path = args[i];
        } else if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --workspace requires a value\n", .{});
                return;
            }
            ea.workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--api-url")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --api-url requires a value\n", .{});
                return;
            }
            ea.api_url = args[i];
        } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --model requires a value\n", .{});
                return;
            }
            ea.model = args[i];
        } else if (std.mem.eql(u8, arg, "--debug")) {
            ea.debug = true;
        } else if (std.mem.eql(u8, arg, "--no-llm")) {
            ea.no_llm = true;
        } else if (std.mem.eql(u8, arg, "--no-drift")) {
            ea.no_drift = true;
        } else if (std.mem.eql(u8, arg, "--guidance")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --guidance requires a value\n", .{});
                return;
            }
            ea.guidance = args[i];
        } else if (std.mem.startsWith(u8, arg, "--staged=")) {
            ea.staged = !std.mem.eql(u8, arg["--staged=".len..], "false");
        } else if (std.mem.eql(u8, arg, "--staged")) {
            ea.staged = true;
        } else if (std.mem.startsWith(u8, arg, "--filter=")) {
            ea.filter = std.meta.stringToEnum(FilterMode, arg["--filter=".len..]) orelse .auto;
        } else if (std.mem.eql(u8, arg, "--guidance-db")) {
            // Alias for -o / --db for backward compatibility.
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --guidance-db requires a value\n", .{});
                return;
            }
            ea.db_path = args[i];
        } else if (std.mem.eql(u8, arg, "--db-type=lance") or
            std.mem.eql(u8, arg, "--lance") or
            std.mem.startsWith(u8, arg, "--db-type="))
        {
            // Accepted but ignored — SQLite is always used.
        } else if (!std.mem.startsWith(u8, arg, "-")) {
            ea.query_str = arg;
        }
    }

    // TIER 0: empty query → hot files (no query_text required)
    const query_text = ea.query_str orelse "";
    const is_empty_query = std.mem.trim(u8, query_text, " \t\n\r").len == 0;

    // ── Resolve paths ─────────────────────────────────────────────────────────
    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const workspace = if (ea.workspace) |w|
        try common.resolvePath(allocator, cwd, w)
    else
        try allocator.dupe(u8, cwd);
    defer allocator.free(workspace);

    // Load config for embedding provider and db path defaults.
    const cfg = config_mod.loadConfig(allocator, workspace) catch
        try config_mod.loadConfig(allocator, cwd);
    defer @constCast(&cfg).deinit();
    ea.capabilities_dir = cfg.capabilities_dir;

    const db_path = try common.resolvePath(
        allocator,
        workspace,
        ea.db_path orelse cfg.db_path,
    );
    defer allocator.free(db_path);

    const guidance_dir = try common.resolvePath(allocator, workspace, ea.guidance orelse cfg.guidance_dir);
    defer allocator.free(guidance_dir);

    // ── TIER 0: Empty query → INDEX.md introduction ─────────────────────────────
    // Check before database open - no DB needed for INDEX display
    if (is_empty_query) {
        try showIndexIntro(allocator, workspace, guidance_dir, cfg.capabilities_dir, &cfg);
        return;
    }

    // ── TIER 1/2/3: Capability, file, and struct skeleton routing ────────────────
    // Check for capability name, file path, or struct name before database lookup
    const tier_match = try skeleton_mod.classifyQuery(
        allocator,
        query_text,
        cfg.capabilities_dir,
        workspace,
        guidance_dir,
    );
    defer {
        switch (tier_match) {
            .capability => |cap| allocator.free(cap),
            .file_path => |fp| allocator.free(fp),
            .struct_name => |sn| allocator.free(sn),
            .none => {},
        }
    }

    // Determine if query has natural language (requires summarization)
    const has_natural_language = blk: {
        const trimmed = std.mem.trim(u8, query_text, " \t\n\r");
        // Question mark or multi-word query indicates natural language
        if (trimmed.len > 0 and trimmed[trimmed.len - 1] == '?') break :blk true;
        var tok = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
        var count: usize = 0;
        while (tok.next()) |_| count += 1;
        if (count > 2) break :blk true; // Multi-word query likely natural language
        break :blk false;
    };

    switch (tier_match) {
        .capability => |cap_name| {
            try handleCapabilityQuery(allocator, cap_name, cfg.capabilities_dir, query_text, has_natural_language, &cfg);
            return;
        },
        .file_path => |file_path| {
            try handleFileSkeletonQuery(allocator, file_path, workspace, guidance_dir, query_text);
            return;
        },
        .struct_name => |struct_name| {
            try handleStructSkeletonQuery(allocator, struct_name, guidance_dir, query_text);
            return;
        },
        .none => {},
    }

    // ── Open .guidance.db ─────────────────────────────────────────────────────
    std.fs.accessAbsolute(db_path, .{}) catch {
        std.debug.print("Error: No .guidance.db found at {s}\n", .{db_path});
        std.debug.print("Run 'guidance gen' to generate it.\n", .{});
        return;
    };

    const embedder = try createEmbedderWithFallback(allocator, &cfg);
    defer embedder.deinit();

    var db = GuidanceDb.init(allocator, db_path, embedder) catch |err| {
        std.debug.print("Error opening database: {s}\n", .{@errorName(err)});
        return;
    };
    defer db.deinit();

    // ── Staged pipeline (default) ──────────────────────────────────────────────
    if (ea.staged) {
        staged_path: {
            // Resolve LLM config with thinking model support
            var resolved_url_to_free: ?[]const u8 = null;
            defer if (resolved_url_to_free) |url| allocator.free(url);

            const model = if (std.mem.eql(u8, ea.model, config_mod.DEFAULT_MODEL))
                cfg.model_default
            else
                ea.model;

            const llm_config = blk: {
                const resolved = resolveLlmConfigForThinking(
                    allocator,
                    &cfg,
                    model,
                    if (std.mem.eql(u8, ea.api_url, config_mod.DEFAULT_API_URL)) null else ea.api_url,
                ) catch {
                    // Fallback to direct args
                    break :blk llm.LlmConfig{
                        .api_url = ea.api_url,
                        .model = ea.model,
                        .debug = ea.debug,
                    };
                };
                resolved_url_to_free = resolved.resolved_url;
                break :blk llm.LlmConfig{
                    .api_url = resolved.api_url,
                    .model = resolved.model,
                    .think = resolved.think,
                    .debug = ea.debug,
                };
            };

            cmdExplainStaged(allocator, &db, query_text, workspace, guidance_dir, llm_config, cfg.infillModel(), ea) catch |err| {
                if (ea.verbose) std.debug.print("staged explain failed ({s}), falling back to legacy\n", .{@errorName(err)});
                break :staged_path;
            };
            return;
        }
    }

    // ── Legacy path (--staged=false) ──────────────────────────────────────────
    const results = db.search(allocator, query_text, ea.limit) catch |err| {
        std.debug.print("Search failed: {s}\n", .{@errorName(err)});
        return;
    };
    defer {
        for (results) |r| freeSearchResult(allocator, r);
        allocator.free(results);
    }

    if (results.len == 0) {
        const lower_q = try std.ascii.allocLowerString(allocator, query_text);
        defer allocator.free(lower_q);
        std.debug.print("# Explain: {s}\n\nNot indexed for '{s}'. Search the source directly:\n\n", .{ query_text, query_text });
        std.debug.print("    grep -ri '{s}' src/ | head -n 20\n\n", .{lower_q});
        std.debug.print("Run 'guidance gen' after finding the file to index it.\n", .{});
        return;
    }

    // Build normalised search terms (lowercase tokens).
    var search_terms: std.ArrayList([]const u8) = .empty;
    defer {
        for (search_terms.items) |t| allocator.free(t);
        search_terms.deinit(allocator);
    }
    {
        var tok = std.mem.tokenizeAny(u8, query_text, " \t_");
        while (tok.next()) |word| {
            try search_terms.append(allocator, try std.ascii.allocLowerString(allocator, word));
        }
        if (search_terms.items.len == 0)
            try search_terms.append(allocator, try std.ascii.allocLowerString(allocator, query_text));
    }

    // ── Phase A: Skill excerpts ───────────────────────────────────────────────
    const skill_excerpts = try collectSkillExcerpts(allocator, results[0].file_path, guidance_dir, workspace);
    defer {
        for (skill_excerpts) |se| {
            allocator.free(se.name);
            allocator.free(se.excerpt);
        }
        allocator.free(skill_excerpts);
    }

    // ── Phase B: Source excerpts ──────────────────────────────────────────────
    const excerpts = try collectSourceExcerpts(allocator, results, search_terms.items, workspace);
    defer {
        for (excerpts) |e| {
            allocator.free(e.label);
            allocator.free(e.code);
        }
        allocator.free(excerpts);
    }

    // ── Phase C: Grep top files ───────────────────────────────────────────────
    const file_matches = try grepTopFiles(allocator, results, search_terms.items, workspace);
    defer {
        for (file_matches) |fm| allocator.free(fm.lines);
        allocator.free(file_matches);
    }

    // ── Phase D: LLM synthesis ────────────────────────────────────────────────
    var ai_summary: ?[]const u8 = null;
    defer if (ai_summary) |s| allocator.free(s);

    if (!ea.no_llm) {
        var client_opt: ?llm.LlmClient = llm.LlmClient.init(allocator, makeLlmConfig(ea)) catch null;
        defer if (client_opt) |*c| c.deinit();
        if (client_opt) |*client| {
            ai_summary = buildLlmSummary(allocator, client, query_text, results, skill_excerpts, excerpts) catch null;
        }
    }

    // ── Phase E: Render output ────────────────────────────────────────────────
    try renderExplainOutput(allocator, query_text, results, search_terms.items, ai_summary, skill_excerpts, excerpts, file_matches);
}

// ---------------------------------------------------------------------------
// explain helpers
// ---------------------------------------------------------------------------

/// Loads used data from a JSON path into a Zig array of slices.
fn loadUsedByFromJson(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    var parsed = common.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return null;
    defer parsed.deinit();

    const ub_val = parsed.value.object.get("used_by") orelse return null;
    if (ub_val != .array) return null;

    var out: std.ArrayList([]const u8) = .empty;
    for (ub_val.array.items) |item| {
        if (item != .string) continue;
        out.append(allocator, allocator.dupe(u8, item.string) catch continue) catch continue;
    }
    if (out.items.len == 0) {
        out.deinit(allocator);
        return null;
    }
    return out.toOwnedSlice(allocator) catch null;
}

/// Checks if a list of characters exactly matches a list of character lists in the query terms.
fn isExactNameMatch(name: []const u8, terms: []const []const u8) bool {
    // Fast path — avoid allocation for short names.
    var buf: [128]u8 = undefined;
    if (name.len > buf.len) return false;
    const lower = std.ascii.lowerString(buf[0..name.len], name);
    for (terms) |term| {
        if (std.mem.eql(u8, lower, term)) return true;
    }
    return false;
}

/// Loads skill data from a JSON string into a Zig array of byte slices.
fn loadSkillsFromJson(allocator: std.mem.Allocator, json_path: []const u8) ?[]const u8 {
    var parsed = common.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return null;
    defer parsed.deinit();

    const skills_val = parsed.value.object.get("skills") orelse return null;
    if (skills_val != .array) return null;

    var out: std.ArrayList(u8) = .empty;
    for (skills_val.array.items) |item| {
        // skills[] entries may be strings or objects with a "ref" field.
        const ref: []const u8 = switch (item) {
            .string => |s| s,
            .object => blk: {
                const rv = item.object.get("ref") orelse break :blk "";
                if (rv != .string) break :blk "";
                break :blk rv.string;
            },
            else => "",
        };
        if (ref.len == 0) continue;
        // Derive skill name: last path component before SKILL.md.
        // e.g. "skills/gof-patterns/SKILL.md" → "gof-patterns"
        const skill_name = common.skillNameFromRef(ref);
        if (skill_name.len == 0) continue;
        out.appendSlice(allocator, skill_name) catch continue;
        out.append(allocator, '\n') catch continue;
    }
    if (out.items.len == 0) {
        out.deinit(allocator);
        return null;
    }
    return out.toOwnedSlice(allocator) catch null;
}

/// Loads public member names from a JSON path into a Zig array of slices.
fn loadPublicMemberNames(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    var parsed = common.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return null;
    defer parsed.deinit();

    const members_val = parsed.value.object.get("members") orelse return null;
    if (members_val != .array) return null;

    var out: std.ArrayList([]const u8) = .empty;
    for (members_val.array.items) |item| {
        if (item != .object) continue;
        // Skip non-public members.
        const is_pub: bool = blk: {
            const pv = item.object.get("is_pub") orelse break :blk false;
            if (pv != .bool) break :blk false;
            break :blk pv.bool;
        };
        if (!is_pub) continue;
        // Skip test declarations.
        const type_v = item.object.get("type") orelse continue;
        if (type_v != .string) continue;
        if (std.mem.eql(u8, type_v.string, "test_decl")) continue;
        // Get name.
        const name_v = item.object.get("name") orelse continue;
        if (name_v != .string) continue;
        if (name_v.string.len == 0) continue;
        out.append(allocator, allocator.dupe(u8, name_v.string) catch continue) catch continue;
    }
    if (out.items.len == 0) {
        out.deinit(allocator);
        return null;
    }
    return out.toOwnedSlice(allocator) catch null;
}

/// Loads a skill parameter slice from the guidance directory using the provided allocator and returns it.
fn loadSkillPara(
    allocator: std.mem.Allocator,
    guidance_dir: []const u8,
    cwd: []const u8,
    skill_name: []const u8,
) ?[]const u8 {
    const SearchPath = struct { base: []const u8, rel: []const u8 };
    const paths = [_]SearchPath{
        .{ .base = guidance_dir, .rel = "skills" },
        .{ .base = cwd, .rel = "doc/skills" },
    };
    for (paths) |sp| {
        const path = std.fs.path.join(allocator, &.{ sp.base, sp.rel, skill_name, "SKILL.md" }) catch continue;
        defer allocator.free(path);
        const sf = std.fs.openFileAbsolute(path, .{}) catch continue;
        defer sf.close();
        const content = sf.readToEndAlloc(allocator, 512 * 1024) catch continue;
        defer allocator.free(content);
        if (staged_mod.parseSkillDocContent(allocator, content) catch null) |doc| return doc;
    }
    return null;
}

/// Extracts and explains a specified excerpt from a Zig source file, returning its contents.
fn explainExtractExcerpt(
    allocator: std.mem.Allocator,
    src: []const u8,
    start_line: u32,
    node_type: []const u8,
) ![]const u8 {
    const node_type_enum = common.NodeType.fromString(node_type);
    return common.extractExcerpt(allocator, src, start_line, node_type_enum, 80);
}

/// Explains grep-like behavior for a file, returning matching result indices.
fn explainGrepFile(
    allocator: std.mem.Allocator,
    file_path: []const u8,
    terms: []const []const u8,
    max_results: usize,
) ![]usize {
    const f = std.fs.openFileAbsolute(file_path, .{}) catch return &.{};
    defer f.close();
    const content = f.readToEndAlloc(allocator, 10 * 1024 * 1024) catch return &.{};
    defer allocator.free(content);

    var line_numbers: std.ArrayList(usize) = .empty;
    errdefer line_numbers.deinit(allocator);
    var it = std.mem.splitScalar(u8, content, '\n');
    var line_no: usize = 0;
    while (it.next()) |line| {
        line_no += 1;
        if (line_numbers.items.len >= max_results) break;
        const trimmed = std.mem.trimLeft(u8, line, " \t");
        if (std.mem.startsWith(u8, trimmed, "//") or std.mem.startsWith(u8, trimmed, "#")) continue;
        const lower = try std.ascii.allocLowerString(allocator, line);
        defer allocator.free(lower);
        for (terms) |term| {
            if (std.mem.indexOf(u8, lower, term) != null) {
                try line_numbers.append(allocator, line_no);
                break;
            }
        }
    }
    return line_numbers.toOwnedSlice(allocator);
}

/// Constructs a summary slice from LLM query results, using allocator, client, and query data.
fn buildLlmSummary(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query_text: []const u8,
    results: []const SearchResult,
    skill_excerpts_in: []const SkillExcerpt,
    excerpts_in: []const ExcerptEntry,
) !?[]const u8 {
    var kb: std.ArrayList(u8) = .empty;
    defer kb.deinit(allocator);
    const kbw = kb.writer(allocator);

    // 1. Skill context first.
    if (skill_excerpts_in.len > 0) {
        try kbw.writeAll("=== Skill patterns ===\n");
        for (skill_excerpts_in[0..@min(2, skill_excerpts_in.len)]) |se| {
            try kbw.print("{s}: {s}\n\n", .{ se.name, se.excerpt });
        }
    }

    // 2. Module sections.
    var seen_files: std.StringHashMapUnmanaged(void) = .{};
    defer seen_files.deinit(allocator);
    for (results[0..@min(5, results.len)]) |r| {
        const src_key = if (r.source.len > 0) r.source else r.file_path;
        if (seen_files.contains(src_key)) {
            // Just add the member line.
            try kbw.print("  {s} (line {?})", .{ r.name, r.line });
            if (r.signature) |sig| try kbw.print(": {s}", .{sig});
            if (r.comment) |cm| {
                const nl = std.mem.indexOfScalar(u8, cm, '\n') orelse cm.len;
                try kbw.print(" — {s}", .{cm[0..@min(nl, 120)]});
            }
            try kbw.print("\n", .{});
            continue;
        }
        try seen_files.put(allocator, src_key, {});
        try kbw.print("=== {s} ===\n", .{src_key});
        if (r.comment) |cm| {
            const nl = std.mem.indexOfScalar(u8, cm, '\n') orelse cm.len;
            try kbw.print("{s}\n", .{cm[0..nl]});
        }
        if (r.used_by.len > 0) {
            try kbw.writeAll("Used by: ");
            for (r.used_by, 0..) |ub, ui| {
                if (ui > 0) try kbw.writeAll(", ");
                try kbw.writeAll(ub);
            }
            try kbw.writeByte('\n');
        }
        try kbw.print("\nMember: {s} (line {?})", .{ r.name, r.line });
        if (r.signature) |sig| try kbw.print(": {s}", .{sig});
        try kbw.writeByte('\n');
    }

    // 3. Source excerpts.
    for (excerpts_in[0..@min(2, excerpts_in.len)]) |e| {
        try kbw.print("\nSource excerpt ({s}):\n{s}\n\n", .{ e.label, e.code });
    }

    // 4. Build skill-name string for the instruction.
    var skill_names_buf: std.ArrayList(u8) = .empty;
    defer skill_names_buf.deinit(allocator);
    for (skill_excerpts_in, 0..) |se, si| {
        if (si > 0) try skill_names_buf.appendSlice(allocator, ", ");
        try skill_names_buf.appendSlice(allocator, se.name);
    }
    const skill_instruction: []const u8 = if (skill_names_buf.items.len > 0)
        try std.fmt.allocPrint(allocator, "SKILL PATTERNS APPLIED: {s}\nThe code implements these patterns — name them in your summary.\n", .{skill_names_buf.items})
    else
        try allocator.dupe(u8, "");
    defer allocator.free(skill_instruction);

    const prompt = try std.fmt.allocPrint(
        allocator,
        "You are a code navigation assistant for a Zig/Python codebase. Be precise and terse.\n{s}\nSummarise '{s}': what it is, what design pattern it implements (if any), key members/functions with line numbers, and who calls it. 3-5 sentences. Use only facts from KNOWLEDGE. STRICT RULE: Never write sentences about absence.\n\nKNOWLEDGE:\n{s}\n\nReturn only the summary.",
        .{ skill_instruction, query_text, kb.items },
    );
    defer allocator.free(prompt);

    const raw = (client.complete(prompt, 1500, 0.15, null) catch null) orelse return null;
    defer allocator.free(raw);

    const cleaned = try synthesize_mod.stripAbsenceSentences(allocator, llm.stripThinkBlock(raw));
    defer allocator.free(cleaned);

    const trimmed = std.mem.trim(u8, cleaned, " \t\n\r");
    if (trimmed.len == 0) return null;
    return try allocator.dupe(u8, trimmed);
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
    defer allocator.free(response);

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

const drift_stop_words = [_][]const u8{
    "the",  "a",    "an",   "is",   "in",  "of",   "to",   "for",  "and",   "or",
    "with", "from", "that", "this", "how", "does", "what", "when", "where", "why",
    "use",  "get",  "set",  "its",  "are", "not",
};

/// Converts a C string into a list of tokenized capability words using the allocator.
fn tokenizeCapabilityWords(
    allocator: std.mem.Allocator,
    text: []const u8,
    out: *std.ArrayList([]const u8),
) !void {
    var it = std.mem.tokenizeAny(u8, text, " \t\n\r_-./");
    while (it.next()) |raw| {
        if (raw.len < 3) continue;
        const lower = try std.ascii.allocLowerString(allocator, raw);
        var is_stop = false;
        for (drift_stop_words) |sw| {
            if (std.mem.eql(u8, lower, sw)) {
                is_stop = true;
                break;
            }
        }
        if (is_stop) {
            allocator.free(lower);
            continue;
        }
        try out.append(allocator, lower);
    }
}

/// Processes query results to generate drift follow-up indices using allocator and text data.
fn computeDriftFollowUps(
    allocator: std.mem.Allocator,
    query_text: []const u8,
    results: []const SearchResult,
) ![]const []const u8 {
    var interner = StringInterner.init(allocator);
    defer interner.deinit();

    // Collect needed words (from query)
    var needed_words: std.ArrayList([]const u8) = .empty;
    defer {
        for (needed_words.items) |w| allocator.free(w);
        needed_words.deinit(allocator);
    }
    try tokenizeCapabilityWords(allocator, query_text, &needed_words);

    if (needed_words.items.len == 0) return try allocator.alloc([]const u8, 0);

    // Collect available words (from result modules and symbol names)
    var avail_words: std.ArrayList([]const u8) = .empty;
    defer {
        for (avail_words.items) |w| allocator.free(w);
        avail_words.deinit(allocator);
    }
    for (results) |r| {
        try tokenizeCapabilityWords(allocator, r.module, &avail_words);
        try tokenizeCapabilityWords(allocator, r.name, &avail_words);
    }

    // Intern all words first to fix total capacity
    for (needed_words.items) |w| _ = try interner.intern(w);
    for (avail_words.items) |w| _ = try interner.intern(w);
    const cap = @max(1, interner.count());

    // Build needed bitset
    var needed_bs = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, cap);
    defer needed_bs.deinit(allocator);
    for (needed_words.items) |w| {
        if (interner.getIndex(w)) |idx| needed_bs.set(idx);
    }

    // Build available bitset
    var avail_bs = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, cap);
    defer avail_bs.deinit(allocator);
    for (avail_words.items) |w| {
        if (interner.getIndex(w)) |idx| avail_bs.set(idx);
    }

    const drift = BitSetDrift{ .interner = &interner };
    return try drift.generateFollowUps(allocator, &needed_bs, &avail_bs);
}

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
    // Debug output at function entry - always print to stderr
    std.debug.print("[DEBUG] cmdExplainStaged called\n", .{});
    std.debug.print("[DEBUG]   ea.debug = {}\n", .{ea.debug});
    std.debug.print("[DEBUG]   ea.no_llm = {}\n", .{ea.no_llm});
    std.debug.print("[DEBUG]   query_text = \"{s}\"\n", .{query_text});

    const skills_dir = try std.fs.path.join(allocator, &.{ guidance_dir, "skills" });
    defer allocator.free(skills_dir);

    var aliases_opt: ?vector_db_mod.SemanticAliases = loadAliases(allocator, guidance_dir);
    defer if (aliases_opt) |*a| a.deinit();

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
        std.debug.print("[DEBUG]   isShortQuery: {}\n", .{isShortQuery(query_text)});
        std.debug.print("[DEBUG] Pipeline settings:\n", .{});
        std.debug.print("[DEBUG]   use_llm: {}\n", .{use_llm});
        std.debug.print("[DEBUG]   use_filter: {}\n", .{use_filter});
        std.debug.print("[DEBUG]   filter_mode: {s}\n", .{@tagName(ea.filter)});
        std.debug.print("[DEBUG]   staged: {}\n", .{ea.staged});
    }

    // Create the LLM client for filtering (default model)
    var client_opt: ?llm.LlmClient = if (use_llm) llm.LlmClient.init(allocator, llm_config) catch |err| blk: {
        if (ea.verbose) std.debug.print("DEBUG: LLM client init failed: {}\n", .{err});
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
            std.debug.print("DEBUG: LLM client initialized - api_url: {s}, model: {s}, think: {?}\n", .{ llm_config.api_url, llm_config.model, llm_config.think });
        } else {
            std.debug.print("DEBUG: LLM client is null, synthesis will be skipped\n", .{});
        }
        if (fast_client_opt) |_| {
            std.debug.print("DEBUG: Fast client initialized - model: {s}\n", .{fast_model_ref});
        }
    }

    // For long queries, extract key terms to improve search recall.
    var expanded_query: ?[]const u8 = null;
    defer if (expanded_query) |q| allocator.free(q);

    if (use_filter) {
        if (client_opt) |*client| {
            if (llmExtractKeyTerms(allocator, client, query_text) catch null) |terms| {
                defer {
                    for (terms) |t| allocator.free(t);
                    allocator.free(terms);
                }
                if (ea.debug) {
                    std.debug.print("[DEBUG] Key term extraction:\n", .{});
                    std.debug.print("[DEBUG]   terms extracted: {d}\n", .{terms.len});
                    for (terms, 0..) |t, i| {
                        std.debug.print("[DEBUG]   [{d}] \"{s}\"\n", .{ i, t });
                    }
                }
                var buf: std.ArrayList(u8) = .empty;
                defer buf.deinit(allocator);
                try buf.appendSlice(allocator, query_text);
                for (terms) |t| {
                    try buf.append(allocator, ' ');
                    try buf.appendSlice(allocator, t);
                }
                expanded_query = try buf.toOwnedSlice(allocator);
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

    // M2: Route through QueryStrategy VTable for intent-based dispatch.
    var id_strategy: query_strategy_mod.IdentifierLookupStrategy = .{};
    var cap_strategy: query_strategy_mod.CapabilityQueryStrategy = .{};
    var concept_strategy: query_strategy_mod.ConceptQueryStrategy = .{};
    const strategies = query_strategy_mod.buildDefaultStrategies(&id_strategy, &cap_strategy, &concept_strategy);

    // Pass original query for deterministic matching, effective query for vector search
    const stages_raw = try query_strategy_mod.executeWithStrategy(
        allocator,
        db,
        effective_query,
        query_text,
        workspace,
        aliases_opt,
        &strategies,
    );
    defer {
        types.freeStages(allocator, stages_raw);
        allocator.free(stages_raw);
    }

    if (stages_raw.len == 0) {
        const lower_q = try std.ascii.allocLowerString(allocator, effective_query);
        defer allocator.free(lower_q);
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
    const stages_filtered: ?[]types.Stage = if (use_filter) llm_filter_mod.filterStages(allocator, client, query_text, stages_raw) catch blk: {
        if (ea.verbose) std.debug.print("llm_filter failed, using unfiltered stages\n", .{});
        break :blk null;
    } else null;
    defer if (stages_filtered) |sf| {
        types.freeStages(allocator, sf);
        allocator.free(sf);
    };

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
        std.debug.print("[DEBUG]   aliases_loaded: {}\n", .{aliases_opt != null});
    }

    const expansion_results = db.searchWithAliases(allocator, effective_query, 5, aliases_opt) catch &.{};
    defer {
        for (expansion_results) |r| freeSearchResult(allocator, r);
        allocator.free(expansion_results);
    }

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
    defer fp_list.deinit(allocator);
    var src_list: std.ArrayList([]const u8) = .empty;
    defer src_list.deinit(allocator);
    var ub_list: std.ArrayList([]const []const u8) = .empty;
    defer ub_list.deinit(allocator);

    // M4: Only include results with score >= SEE_ALSO_MIN_SCORE, up to SEE_ALSO_TOP_N
    for (expansion_results[0..@min(SEE_ALSO_TOP_N, expansion_results.len)]) |r| {
        if (r.score < SEE_ALSO_MIN_SCORE) continue;
        try fp_list.append(allocator, r.file_path);
        try src_list.append(allocator, r.source);
        try ub_list.append(allocator, r.used_by);
    }

    var existing_srcs: std.ArrayList([]const u8) = .empty;
    defer existing_srcs.deinit(allocator);
    for (working_stages) |s| {
        if (s.kind == .code or s.kind == .prose) try existing_srcs.append(allocator, s.source);
    }

    const extra_stages: ?[]types.Stage = staged_mod.expandFollowUps(
        allocator,
        fp_list.items,
        src_list.items,
        ub_list.items,
        workspace,
        guidance_dir,
        skills_dir,
        existing_srcs.items,
        6,
    ) catch null;
    defer if (extra_stages) |es| {
        types.freeStages(allocator, es);
        allocator.free(es);
    };

    // Combine working + extra stages (borrows — no new string copies).
    var combined: std.ArrayList(types.Stage) = .empty;
    defer combined.deinit(allocator); // only frees the ArrayList spine; strings owned by above slices
    for (working_stages) |s| try combined.append(allocator, s);
    if (extra_stages) |es| for (es) |s| try combined.append(allocator, s);

    // M8: LLM synthesis (use fast model if available, else default).
    // Check LLM synthesis cache before calling the model.
    const query_hash = common.sha256Hex(allocator, query_text) catch null;
    defer if (query_hash) |qh| allocator.free(qh);

    if (ea.debug) {
        std.debug.print("[DEBUG] Synthesis:\n", .{});
        std.debug.print("[DEBUG]   query_hash: {s}\n", .{query_hash orelse "(null)"});
    }

    const cached_summary: ?[]const u8 = if (query_hash) |qh|
        db.loadCachedSynthesis(allocator, qh) catch null
    else
        null;
    defer if (cached_summary) |cs| allocator.free(cs);

    if (ea.debug) {
        std.debug.print("[DEBUG]   cache_hit: {}\n", .{cached_summary != null});
    }

    const synth_client = if (fast_client_opt) |*fc| fc else &client_opt.?;
    if (ea.debug) {
        std.debug.print("[DEBUG]   using_fast_model: {}\n", .{fast_client_opt != null});
    }

    const synth_result = if (cached_summary == null)
        synthesize_mod.synthesize(allocator, synth_client, query_text, combined.items) catch {
            return emitStagedOutput(allocator, query_text, combined.items, null, workspace);
        }
    else
        synthesize_mod.SynthesisResult{ .summary = null, .followup_keywords = null };
    defer {
        if (cached_summary == null) {
            if (synth_result.summary) |s| allocator.free(s);
            if (synth_result.followup_keywords) |kw| {
                for (kw) |k| allocator.free(k);
                allocator.free(kw);
            }
        }
    }

    // Store successful synthesis in cache (best-effort, no error propagation).
    if (cached_summary == null) {
        if (synth_result.summary) |summary| {
            if (query_hash) |qh| {
                // Compute signature_hash from stage file paths for future invalidation.
                var sig_buf: std.ArrayList(u8) = .empty;
                defer sig_buf.deinit(allocator);
                const sig_writer = sig_buf.writer(allocator);
                for (combined.items) |s| {
                    sig_writer.writeAll(s.source) catch {};
                    sig_writer.writeByte(0) catch {};
                }
                const sig_hash = common.sha256Hex(allocator, sig_buf.items) catch null;
                defer if (sig_hash) |sh| allocator.free(sh);
                db.storeSynthesisCache(qh, summary, sig_hash orelse qh);
            }
        }
    }

    // Use cached summary if available, otherwise use synthesis result.
    const effective_summary = cached_summary orelse synth_result.summary;

    // M8.5: DRIFT follow-ups — deterministic, no LLM required.
    const drift_followups: []const []const u8 = if (!ea.no_drift)
        computeDriftFollowUps(allocator, query_text, expansion_results) catch &.{}
    else
        &.{};
    defer {
        for (drift_followups) |q| allocator.free(q);
        allocator.free(drift_followups);
    }

    // Merge LLM-generated and DRIFT follow-ups into a single slice.
    // The merged slice borrows string pointers from both sources; only its
    // spine needs to be freed.
    const merged_followups: ?[]const []const u8 = if (drift_followups.len == 0)
        synth_result.followup_keywords
    else blk: {
        const synth_len = if (synth_result.followup_keywords) |sk| sk.len else 0;
        var all = try allocator.alloc([]const u8, synth_len + drift_followups.len);
        if (synth_result.followup_keywords) |sk| @memcpy(all[0..synth_len], sk);
        @memcpy(all[synth_len..], drift_followups);
        break :blk all;
    };
    defer if (drift_followups.len > 0) {
        if (merged_followups) |mf| allocator.free(mf);
    };

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
/// Checks if a list of name terms exactly matches a query result, returning true if they align perfectly.
pub fn isExactNameMatchPub(name: []const u8, terms: []const []const u8) bool {
    return isExactNameMatch(name, terms);
}

/// Loads skill data from a JSON path into a Zig array of byte slices.
pub fn loadSkillsFromJsonPub(allocator: std.mem.Allocator, json_path: []const u8) ?[]const u8 {
    return loadSkillsFromJson(allocator, json_path);
}

/// Loads used data from a JSON path into a Zig array of slices.
pub fn loadUsedByFromJsonPub(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    return loadUsedByFromJson(allocator, json_path);
}

/// Loads public member names from a JSON path into a Zig array of arrays of bytes.
pub fn loadPublicMemberNamesPub(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    return loadPublicMemberNames(allocator, json_path);
}

/// Loads a skill parameter pack into a Zig array, returning the allocated slice.
pub fn loadSkillParaPub(allocator: std.mem.Allocator, guidance_dir: []const u8, cwd: []const u8, skill_name: []const u8) ?[]const u8 {
    return loadSkillPara(allocator, guidance_dir, cwd, skill_name);
}

/// Explains the extracted excerpt in the query engine, taking allocator, source slice, line number, and node type as inputs.
pub fn explainExtractExcerptPub(allocator: std.mem.Allocator, src: []const u8, start_line: u32, node_type: []const u8) ![]const u8 {
    return explainExtractExcerpt(allocator, src, start_line, node_type);
}

/// Explains grep results for file paths, returning matching indices.
pub fn explainGrepFilePub(allocator: std.mem.Allocator, file_path: []const u8, terms: []const []const u8, max_results: usize) ![]usize {
    return explainGrepFile(allocator, file_path, terms, max_results);
}

/// Checks if a query string is short enough for public use, returning true or false.
pub fn isShortQueryPub(query: []const u8) bool {
    return isShortQuery(query);
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
    const content = std.fs.cwd().readFileAlloc(allocator, cap_path, 64 * 1024) catch return null;
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

    std.fs.accessAbsolute(capabilities_dir, .{}) catch return false;
    var cap_dir = std.fs.openDirAbsolute(capabilities_dir, .{ .iterate = true }) catch return false;
    defer cap_dir.close();

    var walker = cap_dir.walk(allocator) catch return false;
    defer walker.deinit();

    while (walker.next() catch null) |entry| {
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
    defer allocator.free(response);

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

    const cwd = try std.process.getCwdAlloc(allocator);
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

    std.fs.accessAbsolute(capabilities_dir, .{}) catch return;
    var cap_dir = std.fs.openDirAbsolute(capabilities_dir, .{ .iterate = true }) catch return;
    defer cap_dir.close();

    var walker = cap_dir.walk(allocator) catch return;
    defer walker.deinit();

    while (walker.next() catch null) |entry| {
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
        llm_api_url_to_free = resolved.api_url;
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
    const w = out.writer(allocator);

    try w.print("# guidance — AST-guided Vector Search\n\n", .{});
    try w.print("`guidance explain \"<query>\"` is the first stop to gain relevant context about this codebase.\n\n", .{});
    try w.print("A single keyword like `cmdExplain` triggers a deterministic search without LLM synthesis. Queries with spaces use the LLM for synthesis.\n\n", .{});
    try w.print("**Example:**  \n", .{});
    try w.print("```\n", .{});
    try w.print("guidance explain \"cmdExplain\"\n", .{});
    try w.print("```\n\n", .{});
    try w.print("Look up suggested search terms from results to discover related features. Use regular file tools once you're confident about the implementation.\n\n", .{});
    try w.print("**Important:** Run `guidance explain` to check for existing features before writing duplicate code.\n\n", .{});
    try w.print("---\n\n", .{});
    try w.print("## Capabilities\n\n", .{});

    for (capabilities.items) |cap| {
        const desc = if (llm_client_opt) |*client| blk: {
            const summary = summarizeCapabilityDescription(allocator, client, cap.name, cap.description, 240) catch null;
            break :blk summary;
        } else null;

        const effective_desc = if (desc) |d| d else blk: {
            // Fallback: truncate at 120 chars
            if (cap.description.len <= 120) break :blk allocator.dupe(u8, cap.description) catch break :blk cap.description;
            const trunc = cap.description[0..77];
            break :blk std.fmt.allocPrint(allocator, "{s}...", .{trunc}) catch cap.description;
        };
        defer if (desc != null) allocator.free(effective_desc);

        try w.print("- **{s}**: {s}\n", .{ cap.name, effective_desc });
    }

    try w.print("\n---\n\n", .{});
    try w.print("Run `guidance explain \"<keyword>\"` to explore any capability.\n", .{});

    const index_content = try out.toOwnedSlice(allocator);
    defer allocator.free(index_content);

    const index_dir = std.fs.path.dirname(index_path) orelse guidance_dir;
    std.fs.makeDirAbsolute(index_dir) catch {};
    const file = try std.fs.createFileAbsolute(index_path, .{ .truncate = true });
    defer file.close();
    try file.writeAll(index_content);
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

    const content = std.fs.cwd().readFileAlloc(allocator, index_path, 64 * 1024) catch |err| {
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

    const content = std.fs.cwd().readFileAlloc(allocator, cap_path, 256 * 1024) catch |err| {
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
        var llm_client_opt: ?llm.LlmClient = blk: {
            var resolved_url_to_free: ?[]const u8 = null;
            defer if (resolved_url_to_free) |url| allocator.free(url);
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
                defer allocator.free(raw);
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
        try stdout.print("# File: `{s}`\n\n{s}\n", .{ file_path, skel });
        try stdout.print("\n---\n\nRun `guidance explain \"<function-name>\"` to see specific function documentation.\n", .{});
    } else {
        try stdout.print("# File: `{s}`\n\nUnable to generate skeleton. File may not be indexed.\n\nRun `guidance gen` to index this file.\n", .{file_path});
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

    const skeleton = skeleton_mod.generateStructSkeleton(allocator, struct_name, guidance_dir);

    if (skeleton) |skel| {
        defer allocator.free(skel);
        try stdout.print("# Struct: `{s}`\n\n{s}\n", .{ struct_name, skel });
        try stdout.print("\n---\n\nRun `guidance explain \"<method-name>\"` to see specific method documentation.\n", .{});
    } else {
        try stdout.print("# Struct: `{s}`\n\nStruct not found in indexed files.\n\nRun `guidance gen` to index your source files.\n", .{struct_name});
    }
    try stdout.flush();
}

// =============================================================================
// show command
// =============================================================================

/// Displays a specified query result using the allocator and argument list.
pub fn cmdShow(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var db_path_arg: ?[]const u8 = null;
    var filter: ?[]const u8 = null;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) return;
            db_path_arg = args[i];
        } else if (std.mem.eql(u8, arg, "--filter")) {
            i += 1;
            if (i >= args.len) return;
            filter = args[i];
        } else if (std.mem.startsWith(u8, arg, "--filter=")) {
            filter = arg["--filter=".len..];
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const db_path = try common.resolvePath(allocator, cwd, db_path_arg orelse config_mod.DEFAULT_GUIDANCE_DB_PATH);
    defer allocator.free(db_path);

    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    var noop: vector_mod.NoopEmbedding = .{};
    var db = GuidanceDb.init(allocator, db_path, noop.provider()) catch |err| {
        try stdout.print("Error: could not open db {s}: {}\n", .{ db_path, err });
        return;
    };
    defer db.deinit();

    const do_alias = filter == null or std.mem.eql(u8, filter.?, "alias") or std.mem.eql(u8, filter.?, "all");
    const do_keywords = filter == null or std.mem.eql(u8, filter.?, "keywords") or std.mem.eql(u8, filter.?, "all");
    const do_ast = filter == null or std.mem.eql(u8, filter.?, "ast") or std.mem.eql(u8, filter.?, "all");

    try stdout.print("# Vector Embeddings in {s}\n\n", .{db_path});

    if (do_alias) {
        const aliases = db.getAllAliasEmbeddings(allocator) catch |err| {
            try stdout.print("Error reading alias embeddings: {}\n", .{err});
            return;
        };
        defer {
            for (aliases) |a| {
                allocator.free(a.key);
                allocator.free(a.model);
            }
            allocator.free(aliases);
        }
        try stdout.print("## Alias Embeddings ({d})\n\n", .{aliases.len});
        try stdout.print("| Key | Model |\n|-----|-------|\n", .{});
        for (aliases) |a| {
            try stdout.print("| `{s}` | {s} |\n", .{ a.key, a.model });
        }
        try stdout.print("\n", .{});
    }

    if (do_keywords) {
        const keywords = db.getAllKeywordEmbeddings(allocator) catch |err| {
            try stdout.print("Error reading keyword embeddings: {}\n", .{err});
            return;
        };
        defer {
            for (keywords) |k| {
                allocator.free(k.keyword);
                allocator.free(k.model);
            }
            allocator.free(keywords);
        }
        try stdout.print("## Keyword Embeddings ({d})\n\n", .{keywords.len});
        try stdout.print("| Keyword | Model |\n|---------|-------|\n", .{});
        for (keywords) |k| {
            try stdout.print("| `{s}` | {s} |\n", .{ k.keyword, k.model });
        }
        try stdout.print("\n", .{});
    }

    if (do_ast) {
        const ast = db.getAllAstNodeEmbeddings(allocator) catch |err| {
            try stdout.print("Error reading AST node embeddings: {}\n", .{err});
            return;
        };
        defer {
            for (ast) |a| {
                allocator.free(a.name);
                allocator.free(a.node_type);
                allocator.free(a.module);
            }
            allocator.free(ast);
        }
        try stdout.print("## AST Node Embeddings ({d})\n\n", .{ast.len});
        try stdout.print("| Module | Name | Type |\n|--------|------|------|\n", .{});
        for (ast) |a| {
            try stdout.print("| {s} | `{s}` | {s} |\n", .{ a.module, a.name, a.node_type });
        }
        try stdout.print("\n", .{});
    }

    try stdout.print("---\n*Use `--filter=alias|keywords|ast|all` to show specific groups*\n", .{});
    try stdout.flush();
}

// =============================================================================
// test command
// =============================================================================

/// Manages query keywords with ownership model; ensures invariants are preserved during initialization and cleanup.
const TestQuery = struct {
    query: []const u8,
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
pub fn cmdTest(allocator: std.mem.Allocator, args: []const []const u8) !void {
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

    const cwd = try std.process.getCwdAlloc(allocator);
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
        try single.append(allocator, .{ .query = try allocator.dupe(u8, sq) });
        break :blk try single.toOwnedSlice(allocator);
    } else blk: {
        const from_file = loadBenchmarkQueries(allocator, gdir_abs) catch null;
        break :blk from_file orelse try generateTestQueries(allocator, gdir_abs);
    };
    const queries = if (num_limit) |n| all_queries[0..@min(n, all_queries.len)] else all_queries;
    defer {
        for (all_queries) |q| {
            allocator.free(q.query);
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

            var id_strategy: query_strategy_mod.IdentifierLookupStrategy = .{};
            var cap_strategy: query_strategy_mod.CapabilityQueryStrategy = .{};
            var concept_strategy: query_strategy_mod.ConceptQueryStrategy = .{};
            const strategies = query_strategy_mod.buildDefaultStrategies(&id_strategy, &cap_strategy, &concept_strategy);

            break :stages_blk try query_strategy_mod.executeWithStrategy(
                allocator,
                &gdb,
                query_text,
                query_text,
                ws,
                aliases_opt,
                &strategies,
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
            var results_buf: std.ArrayList(u8) = .empty;
            defer results_buf.deinit(allocator);
            const rw = results_buf.writer(allocator);
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
                \\Query and results:
                \\{s}
                \\
                \\Rate each dimension (0-10):
                \\- Accuracy: Results directly match what the query asks for. No false positives.
                \\- Relevance: Top results are the most important/defining code for the query. First result is the best entry point.
                \\- Completeness: All critical code locations, types, and functions needed to understand the topic are found. No major gaps.
                \\- Navigation Quality: Results provide file paths, line numbers, function signatures, and context that enable an AI to immediately read and understand the relevant code.
                \\
                \\Score 9-10: Excellent code intelligence — AI can navigate directly to implementation with confidence.
                \\Score 7-8: Good results with minor gaps or noise.
                \\Score 5-6: Partial coverage, significant noise, or missing critical locations.
                \\Score 3-4: Mostly irrelevant or incomplete for subagent use.
                \\Score 0-2: No useful results or wrong topic entirely.
                \\
                \\Respond EXACTLY in this format (no other text):
                \\Accuracy: <0-10>
                \\Relevance: <0-10>
                \\Completeness: <0-10>
                \\Navigation: <0-10>
                \\Observation: <one sentence assessing subagent utility>
            , .{results_buf.items});
            defer allocator.free(eval_prompt);

            const response_opt = client.complete(eval_prompt, 400, 0.1, null) catch |err| blk: {
                std.debug.print("Warning: LLM complete() failed: {s}\n", .{@errorName(err)});
                break :blk null;
            };
            if (response_opt) |response| {
                defer allocator.free(response);
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

/// Loads benchmark query data from a file into a Zig test query slice.
fn loadBenchmarkQueries(allocator: std.mem.Allocator, guidance_dir: []const u8) ![]TestQuery {
    const path = try std.fs.path.join(allocator, &.{ guidance_dir, "benchmarks.txt" });
    defer allocator.free(path);

    var file = std.fs.cwd().openFile(path, .{}) catch |err| return err;
    defer file.close();

    const content = try file.readToEndAlloc(allocator, 512 * 1024);
    defer allocator.free(content);

    var queries: std.ArrayList(TestQuery) = .empty;
    errdefer {
        for (queries.items) |q| allocator.free(q.query);
        queries.deinit(allocator);
    }

    var lines = std.mem.splitScalar(u8, content, '\n');
    while (lines.next()) |line| {
        const trimmed = std.mem.trim(u8, line, " \t\r");
        if (trimmed.len == 0) continue;
        if (std.mem.startsWith(u8, trimmed, "#")) continue;
        try queries.append(allocator, .{ .query = try allocator.dupe(u8, trimmed) });
    }

    return queries.toOwnedSlice(allocator);
}

/// Generates test queries for the guidance engine using provided allocator and directory data.
fn generateTestQueries(allocator: std.mem.Allocator, guidance_dir: []const u8) ![]TestQuery {
    var queries: std.ArrayList(TestQuery) = .empty;
    errdefer {
        for (queries.items) |q| {
            allocator.free(q.query);
            if (q.observations.len > 0) allocator.free(q.observations);
        }
        queries.deinit(allocator);
    }

    // Scan .guidance/src/**/*.json for module-level comments
    const src_dir = try std.fs.path.join(allocator, &.{ guidance_dir, "src" });
    defer allocator.free(src_dir);

    var dir = std.fs.cwd().openDir(src_dir, .{ .iterate = true }) catch |err| {
        std.debug.print("Warning: cannot open guidance src dir: {s}\n", .{@errorName(err)});
        return queries.toOwnedSlice(allocator);
    };
    defer dir.close();

    var walker = try dir.walk(allocator);
    defer walker.deinit();

    while (try walker.next()) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, ".json")) continue;

        const json_path = try std.fs.path.join(allocator, &.{ src_dir, entry.path });
        defer allocator.free(json_path);

        const file = std.fs.cwd().openFile(json_path, .{}) catch continue;
        defer file.close();

        const content = file.readToEndAlloc(allocator, 1024 * 1024) catch continue;
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
        try queries.append(allocator, .{ .query = query1 });

        // Generate question-style query
        const query2 = try std.fmt.allocPrint(allocator, "How does {s} work?", .{basename});
        try queries.append(allocator, .{ .query = query2 });

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

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);
    const cfg = config_mod.loadConfig(allocator, cwd) catch
        try config_mod.loadConfig(allocator, cwd);
    defer @constCast(&cfg).deinit();

    const db_path = try common.resolvePath(allocator, cwd, cfg.db_path);
    defer allocator.free(db_path);

    std.fs.accessAbsolute(db_path, .{}) catch {
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
}

// =============================================================================
// guidance cache-stats — LLM synthesis cache statistics
// =============================================================================

/// Processes cache statistics using an allocator and returns no value.
pub fn cmdCacheStats(allocator: std.mem.Allocator, args: []const []const u8) !void {
    _ = args;

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);
    const cfg = config_mod.loadConfig(allocator, cwd) catch
        try config_mod.loadConfig(allocator, cwd);
    defer @constCast(&cfg).deinit();

    const db_path = try common.resolvePath(allocator, cwd, cfg.db_path);
    defer allocator.free(db_path);

    std.fs.accessAbsolute(db_path, .{}) catch {
        std.debug.print("No .guidance.db found at {s}\n", .{db_path});
        return;
    };

    const embedder = try createEmbedderWithFallback(allocator, &cfg);
    defer embedder.deinit();
    var db = GuidanceDb.init(allocator, db_path, embedder) catch return;
    defer db.deinit();

    const stats = db.cacheStats();
    std.debug.print("LLM synthesis cache: {d} entries, {d} bytes\n", .{ stats.entries, stats.bytes });
}

// =============================================================================
// guidance serve — MCP server (STDIO JSON-RPC 2.0)
// =============================================================================

/// Handles allocation and execution of the Zig command with specified arguments.
pub fn cmdServe(allocator: std.mem.Allocator, args: []const []const u8) !void {
    const mcp_mod = @import("mcp.zig");
    try mcp_mod.serve(allocator, args);
}

// =============================================================================
// guidance ralph — M6: RALPH loop single-query runner
// =============================================================================

/// Processes a Zig command string, validating arguments and preparing execution context.
pub fn cmdRalph(allocator: std.mem.Allocator, args: []const []const u8) !void {
    const ralph_mod = @import("ralph.zig");

    if (args.len == 0) {
        std.debug.print("Usage: guidance ralph <query>\n", .{});
        return;
    }

    // Join args into the query (supports multi-word without quotes).
    var query_buf: std.ArrayList(u8) = .empty;
    defer query_buf.deinit(allocator);
    for (args, 0..) |a, i| {
        if (i > 0) try query_buf.append(allocator, ' ');
        try query_buf.appendSlice(allocator, a);
    }
    const query = query_buf.items;

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const cfg = config_mod.loadConfig(allocator, cwd) catch {
        std.debug.print("Error: Could not load guidance config. Run `guidance init` first.\n", .{});
        return;
    };
    defer @constCast(&cfg).deinit();

    const db_path = try common.resolvePath(allocator, cwd, cfg.db_path);
    defer allocator.free(db_path);

    const guidance_dir = try common.resolvePath(allocator, cwd, cfg.guidance_dir);
    defer allocator.free(guidance_dir);

    var noop_embed: vector_mod.NoopEmbedding = .{};
    const embedder = createEmbedderWithFallback(allocator, &cfg) catch noop_embed.provider();
    defer embedder.deinit();

    var db = GuidanceDb.init(allocator, db_path, embedder) catch {
        std.debug.print("Error: could not open guidance database at {s}\n", .{db_path});
        std.debug.print("Run `guidance gen` to create it first.\n", .{});
        return;
    };
    defer db.deinit();

    var aliases_opt = loadAliases(allocator, guidance_dir);
    defer if (aliases_opt) |*a| a.deinit();

    const stages = ralph_mod.runQuery(allocator, &db, query, cwd, aliases_opt) catch |err| {
        std.debug.print("RALPH error: {}\n", .{err});
        return;
    };
    defer {
        types.freeStages(allocator, stages);
        allocator.free(stages);
    }

    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();
    try stdout.print("# RALPH: {s}\n\n", .{query});
    for (stages) |s| {
        switch (s.kind) {
            .prose, .not_found => try stdout.print("{s}\n\n", .{s.content}),
            .metadata => try stdout.print("---\n{s}\n", .{s.content}),
            .code => try stdout.print("```\n{s}\n```\n\n", .{s.content}),
            else => {},
        }
    }
    try stdout.flush();
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
