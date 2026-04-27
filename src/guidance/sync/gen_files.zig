//! sync/gen_files.zig — Gen command, file pipeline, and DB sync logic.
//!
//! Extracted from sync_engine.zig (M2.1) to keep file sizes navigable.
//! Public API: GenArgs, cmdGen, cmdGenImpl, and all internal helpers used by them.
//!
//! ## Memory Ownership
//!
//!   - GenArgs: Holds borrowed CLI string slices (no deinit needed); parsed from argv.
//!   - ResolvedGenPaths: Owns resolved absolute-path strings; free with deinit().
//!   - cmdGen()/cmdGenImpl(): Orchestrates file processing; creates an Enhancer internally
//!     (which owns the LlmClient) and tears it down at function exit.
//!   - processFiles(): Allocates per-file results; arena-backed for batch processing.
//!   - setupEnhancer()/teardownCspEnhancer(): Create/destroy an LlmClient-based Enhancer;
//!     caller owns the returned Enhancer and must call teardownCspEnhancer() to deinit.
//!   - CapabilitiesSyncFn: Function pointer to avoid circular imports; ownership of
//!     capability sync is delegated to sync_engine.zig.

const std = @import("std");
const types = @import("../types.zig");
const vector_db_mod = @import("vector");
const vector_mod = @import("vector");
const common = @import("common");
const enhancer_mod = @import("../enhancer.zig");
const config_mod = @import("../config.zig");
const provider_mod = @import("../provider_discovery.zig");
const comment_sync_mod = @import("../comments/sync.zig");
const json_store_mod = @import("json_store.zig");
const comment_inserter_mod = @import("../comments/inserter.zig");
const sync_mod = @import("../sync.zig");
const marker_mod = @import("marker.zig");
const llm = @import("llm");
const query_engine_mod = @import("../query_engine.zig");
const schema_validator_mod = @import("../schema_validator.zig");
const GuidanceDb = vector_db_mod.GuidanceDb;
const stepPrint = types.stepPrint;

/// Callback type for synchronizing capabilities during gen.
/// Avoids circular dependency: sync_engine.zig owns the actual implementation,
/// gen_files.zig calls it via this function pointer when provided.
pub const CapabilitiesSyncFn = *const fn (std.mem.Allocator, []const u8, []const u8, []const u8, bool) void;

// =============================================================================
// GenArgs — parsed CLI flags for the gen subcommand
// =============================================================================

pub const GenArgs = struct {
    file: ?[]const u8 = null, // single-file mode (--file)
    scan: ?[]const u8 = null, // directory scan mode (--scan)
    workspace: ?[]const u8 = null,
    json_dir: ?[]const u8 = null,
    /// Output path for the .guidance.db vector database.
    /// -o / --db sets this.  Defaults to config or DEFAULT_GUIDANCE_DB_PATH.
    db_path: ?[]const u8 = null,
    dry_run: bool = false,
    verbose: bool = false,
    api_url: []const u8 = config_mod.DEFAULT_API_URL,
    /// True when --api-url was explicitly passed on the CLI.
    api_url_set: bool = false,
    model: []const u8 = config_mod.DEFAULT_MODEL,
    /// True when -m was explicitly passed on the CLI, overriding config slots.
    model_override: bool = false,
    regen_comments: bool = false,
    /// Run CommentSyncProcessor before JSON generation to insert/update source comments.
    sync_comments: bool = false,
    /// Generate //! file headers for files that lack them (used with --sync-comments).
    sync_headers: bool = false,
    /// Disable all LLM calls (no_llm=true disables automatic comment sync).
    no_llm: bool = false,
    /// Set false via --no-db to skip database generation.
    compile_db: bool = true,
    /// Re-process all files even when guidance JSON is fresh.
    force: bool = false,
    /// Discover and invoke external providers for non-built-in extensions.
    all_languages: bool = false,
    /// Skip the test-suite phase (useful when tests were just run externally).
    skip_tests: bool = false,
    /// Skip the lint phase.
    skip_lint: bool = false,
    /// Skip the format phase.
    skip_fmt: bool = false,
    /// Validate generated GuidanceDoc against schema after each file.
    validate_schema: bool = false,
    /// Sleep duration (in seconds) after processing each file. Default: 2.
    /// Set to 0 to disable.
    timeout_seconds: u64 = 2,
    /// Show LLM prompts in debug output (separate from --debug).
    /// Use --show-prompts to see prompts; --debug shows metadata only.
    show_prompts: bool = false,
    /// Enable debug output (LLM metadata, HTTP requests).
    /// Separate from --verbose which shows file processing status.
    debug: bool = false,

    /// Parse gen subcommand arguments. Returns error.MissingValue when a
    /// flag-with-value is the last argument (fail fast; do not silently drop).
    pub fn parse(args: []const []const u8) error{MissingValue}!GenArgs {
        var ga: GenArgs = .{};
        var i: usize = 0;
        while (i < args.len) : (i += 1) {
            const arg = args[i];
            if (std.mem.eql(u8, arg, "--file")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.file = args[i];
            } else if (std.mem.eql(u8, arg, "--scan")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.scan = args[i];
            } else if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.workspace = args[i];
            } else if (std.mem.eql(u8, arg, "--json-dir") or std.mem.eql(u8, arg, "--output")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.json_dir = args[i];
            } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.db_path = args[i];
            } else if (std.mem.eql(u8, arg, "--dry-run")) {
                ga.dry_run = true;
            } else if (std.mem.eql(u8, arg, "--verbose")) {
                ga.verbose = true;
            } else if (std.mem.eql(u8, arg, "--debug")) {
                ga.debug = true;
            } else if (std.mem.eql(u8, arg, "--regen")) {
                ga.regen_comments = true;
            } else if (std.mem.eql(u8, arg, "--sync-comments")) {
                ga.sync_comments = true;
            } else if (std.mem.eql(u8, arg, "--sync-headers")) {
                ga.sync_headers = true;
            } else if (std.mem.eql(u8, arg, "--no-llm")) {
                ga.no_llm = true;
            } else if (std.mem.eql(u8, arg, "--no-db")) {
                ga.compile_db = false;
            } else if (std.mem.eql(u8, arg, "--force")) {
                ga.force = true;
            } else if (std.mem.eql(u8, arg, "--validate-schema")) {
                ga.validate_schema = true;
            } else if (std.mem.eql(u8, arg, "--all-languages")) {
                ga.all_languages = true;
            } else if (std.mem.eql(u8, arg, "--skip-tests")) {
                ga.skip_tests = true;
            } else if (std.mem.eql(u8, arg, "--skip-lint")) {
                ga.skip_lint = true;
            } else if (std.mem.eql(u8, arg, "--skip-fmt")) {
                ga.skip_fmt = true;
            } else if (std.mem.eql(u8, arg, "--api-url")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.api_url = args[i];
                ga.api_url_set = true;
            } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.model = args[i];
                ga.model_override = true;
            } else if (std.mem.eql(u8, arg, "--db-type=lance") or
                std.mem.eql(u8, arg, "--lance") or
                std.mem.startsWith(u8, arg, "--db-type="))
            {
                // Accepted but ignored — SQLite is always used.
            } else if (std.mem.eql(u8, arg, "--guidance-db")) {
                // Alias for -o when used with old scripts.
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.db_path = args[i];
            } else if (std.mem.eql(u8, arg, "--timeout")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.timeout_seconds = std.fmt.parseInt(u64, args[i], 10) catch {
                    std.debug.print("error: --timeout requires a valid u64 value\n", .{});
                    return error.MissingValue;
                };
            } else if (std.mem.eql(u8, arg, "--show-prompts")) {
                ga.show_prompts = true;
            }
        }
        return ga;
    }
};

// =============================================================================
// ResolvedGenPaths + resolveGenPaths
// =============================================================================

pub const ResolvedGenPaths = struct {
    workspace: []const u8,
    json_dir: []const u8,
    db_path: []const u8,

    pub fn deinit(self: *@This(), allocator: std.mem.Allocator) void {
        allocator.free(self.workspace);
        allocator.free(self.json_dir);
        allocator.free(self.db_path);
    }
};

/// Resolves generation paths using an allocator, GenArgs, and current directory, returning an owned path list.
pub fn resolveGenPaths(allocator: std.mem.Allocator, ga: GenArgs, cwd: []const u8) !ResolvedGenPaths {
    const workspace = try common.resolvePath(allocator, cwd, ga.workspace orelse cwd);
    errdefer allocator.free(workspace);

    const json_dir = try common.resolvePath(allocator, workspace, ga.json_dir orelse config_mod.DEFAULT_GUIDANCE_DIR);
    errdefer allocator.free(json_dir);

    const db_path = try common.resolvePath(allocator, workspace, ga.db_path orelse config_mod.DEFAULT_GUIDANCE_DB_PATH);
    return .{ .workspace = workspace, .json_dir = json_dir, .db_path = db_path };
}

// =============================================================================
// CSP enhancer setup / teardown (comment sync pre-pass)
// =============================================================================

/// Initializes a CSP enhancer configuration using provided allocator, arguments, and project settings.
pub fn setupCspEnhancer(
    allocator: std.mem.Allocator,
    ga: GenArgs,
    cfg: *const config_mod.ProjectConfig,
    csp: *comment_sync_mod.CommentSyncProcessor,
) void {
    const model = if (!std.mem.eql(u8, ga.model, config_mod.DEFAULT_MODEL) or ga.model_override)
        ga.model
    else
        cfg.infillModel();

    var resolved_url_to_free: ?[]const u8 = null;
    const llm_config = query_engine_mod.resolveLlmConfigForThinking(
        allocator,
        cfg,
        model,
        if (ga.api_url_set) ga.api_url else null,
    ) catch {
        const fallback_config: llm.LlmConfig = .{
            .api_url = ga.api_url,
            .model = model,
            .think = null,
            .debug = ga.debug,
            .show_prompts = ga.show_prompts,
        };
        const enh_ptr = allocator.create(enhancer_mod.Enhancer) catch return;
        enh_ptr.* = enhancer_mod.Enhancer.init(allocator, fallback_config) catch {
            allocator.destroy(enh_ptr);
            return;
        };
        csp.enhancer = enh_ptr;
        return;
    };
    resolved_url_to_free = llm_config.resolved_url;

    const final_config: llm.LlmConfig = .{
        .api_url = llm_config.api_url,
        .model = llm_config.model,
        .think = llm_config.think,
        .debug = ga.debug,
        .show_prompts = ga.show_prompts,
    };

    const enh_ptr = allocator.create(enhancer_mod.Enhancer) catch {
        if (resolved_url_to_free) |url| allocator.free(url);
        return;
    };
    enh_ptr.* = enhancer_mod.Enhancer.init(allocator, final_config) catch |err| {
        std.debug.print("warning: could not init LLM enhancer for comment sync: {any}\n", .{err});
        allocator.destroy(enh_ptr);
        return;
    };
    if (resolved_url_to_free) |url| allocator.free(url);
    csp.enhancer = enh_ptr;
}

/// Cleans up the CSP enhancer by releasing allocated resources.
pub fn teardownCspEnhancer(allocator: std.mem.Allocator, csp: *comment_sync_mod.CommentSyncProcessor) void {
    if (csp.enhancer) |enh_ptr| {
        enh_ptr.deinit();
        allocator.destroy(enh_ptr);
        csp.enhancer = null;
    }
}

// =============================================================================
// SyncProcessor enhancer setup (main gen pipeline)
// =============================================================================

/// Initializes a sync enhancer with allocator, configuration, and processor parameters.
pub fn setupEnhancer(
    allocator: std.mem.Allocator,
    ga: GenArgs,
    cfg: *const config_mod.ProjectConfig,
    processor: *sync_mod.SyncProcessor,
) void {
    // CLI -m flag overrides config; otherwise resolve from fast/default slots.
    const model = if (!std.mem.eql(u8, ga.model, config_mod.DEFAULT_MODEL) or ga.model_override)
        ga.model
    else
        cfg.infillModel();

    // Use centralized helper to resolve URL and thinking model settings
    var resolved_url_to_free: ?[]const u8 = null;
    const llm_config = query_engine_mod.resolveLlmConfigForThinking(
        allocator,
        cfg,
        model,
        if (ga.api_url_set) ga.api_url else null,
    ) catch {
        // Fallback to defaults
        const fallback_config: llm.LlmConfig = .{
            .api_url = ga.api_url,
            .model = model,
            .think = null,
            .debug = ga.debug,
            .show_prompts = ga.show_prompts,
        };
        processor.enhancer = enhancer_mod.Enhancer.init(allocator, fallback_config) catch |init_err| {
            std.debug.print("warning: could not init LLM enhancer: {any}\n", .{init_err});
            return;
        };
        processor.regen_comments = ga.regen_comments;
        return;
    };
    resolved_url_to_free = llm_config.resolved_url;

    // The returned api_url points to either resolved_url_to_free (if allocated) or a static string
    // Enhancer.init will dupe the api_url, so we can free resolved_url_to_free after init
    const api_url: []const u8 = llm_config.api_url;

    // Build final config with debug setting
    const final_config: llm.LlmConfig = .{
        .api_url = api_url,
        .model = llm_config.model,
        .think = llm_config.think,
        .debug = ga.debug,
        .show_prompts = ga.show_prompts,
    };

    if (ga.verbose) std.debug.print("DEBUG: LLM config - api_url: {s}, model: {s}, think: {?any}\n", .{ api_url, final_config.model, final_config.think });
    processor.enhancer = enhancer_mod.Enhancer.init(allocator, final_config) catch |err| {
        std.debug.print("warning: could not init LLM enhancer: {any}\n", .{err});
        if (resolved_url_to_free) |url| allocator.free(url);
        processor.regen_comments = ga.regen_comments;
        return;
    };
    // Enhancer.init makes its own copy of api_url, so we can free our temp copy now.
    if (resolved_url_to_free) |url| allocator.free(url);
    processor.regen_comments = ga.regen_comments;

    // --- Set up thinking enhancer for module detail generation ---
    const thinking_model = cfg.thinkingModel();
    if (thinking_model.len > 0) {
        const thinking_config = query_engine_mod.resolveLlmConfigForThinking(
            allocator,
            cfg,
            thinking_model,
            if (ga.api_url_set) ga.api_url else null,
        ) catch {
            if (ga.verbose) std.debug.print("warning: could not resolve thinking model config\n", .{});
            return;
        };

        // Thinking model should use Ollama /api/chat endpoint with think=true
        const thinking_llm_config: llm.LlmConfig = .{
            .api_url = thinking_config.api_url,
            .model = thinking_config.model,
            .think = true, // Always enable thinking for detail generation
            .debug = ga.debug,
            .show_prompts = ga.show_prompts,
        };
        processor.thinking_enhancer = enhancer_mod.Enhancer.init(allocator, thinking_llm_config) catch |err| {
            if (ga.verbose) std.debug.print("warning: could not init thinking enhancer: {any}\n", .{err});
            return;
        };

        // Free resolved URL if allocated
        if (thinking_config.resolved_url) |url| allocator.free(url);
    }
}

// =============================================================================
// processFiles — process single file, scan dir, or full workspace
// =============================================================================

/// Processes Zig source files using an allocator, synchronization processor, and resolved paths, returning processed count.
pub fn processFiles(
    allocator: std.mem.Allocator,
    processor: *sync_mod.SyncProcessor,
    ga: GenArgs,
    paths: ResolvedGenPaths,
) !usize {
    if (ga.file) |file_arg| {
        const full_path = try common.resolvePath(allocator, paths.workspace, file_arg);
        defer allocator.free(full_path);
        _ = try processor.processFile(full_path, ga.timeout_seconds);
        if (ga.verbose) std.debug.print("gen: processed {s}\n", .{full_path});
        return 1;
    }

    if (ga.scan) |scan_arg| {
        const scan_abs = try common.resolvePath(allocator, paths.workspace, scan_arg);
        defer allocator.free(scan_abs);
        const count = try processor.processDirectory(scan_abs, ga.timeout_seconds);
        std.debug.print("gen: {d} source files processed from {s}\n", .{ count, scan_abs });
        return count;
    }

    // Full workspace scan: read src_dirs from config, fail fast on any error.
    var cfg = try config_mod.loadConfig(allocator, paths.workspace);
    defer cfg.deinit();

    var total: usize = 0;
    for (cfg.src_dirs) |src_rel| {
        const src_abs = try common.resolvePath(allocator, paths.workspace, src_rel);
        defer allocator.free(src_abs);
        total += try processor.processDirectory(src_abs, ga.timeout_seconds);
    }
    std.debug.print("gen: {d} source files processed\n", .{total});
    return total;
}

// =============================================================================
// Database helpers
// =============================================================================

pub fn databaseHasTables(db_path: []const u8) bool {
    const c = vector_db_mod.sqlite;
    const db_path_z = std.fmt.allocPrintSentinel(std.heap.page_allocator, "{s}", .{db_path}, 0) catch return false;
    defer std.heap.page_allocator.free(db_path_z);

    var db: ?*c.sqlite3 = null;
    const rc = c.sqlite3_open(db_path_z.ptr, &db);
    if (rc != c.SQLITE_OK) {
        if (db) |d| _ = c.sqlite3_close(d);
        return false;
    }
    defer _ = c.sqlite3_close(db);

    var stmt: ?*c.sqlite3_stmt = null;
    const sql = "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='ast_nodes'";
    const prep_rc = c.sqlite3_prepare_v2(db, sql, -1, &stmt, null);
    if (prep_rc != c.SQLITE_OK) return false;
    defer _ = c.sqlite3_finalize(stmt);

    if (c.sqlite3_step(stmt) != c.SQLITE_ROW) return false;
    const table_count = c.sqlite3_column_int(stmt, 0);
    if (table_count == 0) return false;

    var stmt2: ?*c.sqlite3_stmt = null;
    const sql2 = "SELECT COUNT(*) FROM ast_nodes";
    const prep_rc2 = c.sqlite3_prepare_v2(db, sql2, -1, &stmt2, null);
    if (prep_rc2 != c.SQLITE_OK) return false;
    defer _ = c.sqlite3_finalize(stmt2);

    if (c.sqlite3_step(stmt2) != c.SQLITE_ROW) return false;
    const row_count = c.sqlite3_column_int(stmt2, 0);
    return row_count > 0;
}

/// Checks if the guidance database is up-to-date using provided storage, paths, and capabilities.
pub fn guidanceDbIsUpToDate(
    allocator: std.mem.Allocator,
    db_path: []const u8,
    json_dir: []const u8,
    capabilities_dir: []const u8,
) bool {
    const db_mtime = marker_mod.fileMtime(db_path) orelse return false;

    if (!databaseHasTables(db_path)) return false;

    // Top-level config and data files in json_dir.
    const top_level = [_][]const u8{
        "semantic-aliases.json",
        "capability-mapping.json",
        "capability-index.json",
        "guidance-config.json",
    };
    for (top_level) |name| {
        const p = std.fs.path.join(allocator, &.{ json_dir, name }) catch return false;
        defer allocator.free(p);
        const m = marker_mod.fileMtime(p) orelse continue; // absent → not a dep
        if (m > db_mtime) return false;
    }

    // Walk json_dir/src/ for newest JSON mtime.
    {
        const src_dir_path = std.fs.path.join(allocator, &.{ json_dir, "src" }) catch return false;
        defer allocator.free(src_dir_path);
        const io = std.Io.Threaded.global_single_threaded.io();
        var src_dir = std.Io.Dir.openDirAbsolute(io, src_dir_path, .{ .iterate = true }) catch return false;
        defer src_dir.close(io);
        var walker = src_dir.walk(allocator) catch return false;
        defer walker.deinit();
        while (walker.next(std.Io.Threaded.global_single_threaded.io()) catch return false) |entry| {
            if (entry.kind != .file) continue;
            if (!std.mem.endsWith(u8, entry.basename, ".json")) continue;
            const full = std.fs.path.join(allocator, &.{ src_dir_path, entry.path }) catch continue;
            defer allocator.free(full);
            const m = marker_mod.fileMtime(full) orelse continue;
            if (m > db_mtime) return false;
        }
    }

    // Walk capabilities_dir for newest mtime.
    {
        std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), capabilities_dir, .{}) catch return true; // absent → skip
        const io = std.Io.Threaded.global_single_threaded.io();
        var cap_dir = std.Io.Dir.openDirAbsolute(io, capabilities_dir, .{ .iterate = true }) catch return true;
        defer cap_dir.close(io);
        var walker = cap_dir.walk(allocator) catch return true;
        defer walker.deinit();
        while (walker.next(std.Io.Threaded.global_single_threaded.io()) catch return true) |entry| {
            if (entry.kind != .file) continue;
            const full = std.fs.path.join(allocator, &.{ capabilities_dir, entry.path }) catch continue;
            defer allocator.free(full);
            const m = marker_mod.fileMtime(full) orelse continue;
            if (m > db_mtime) return false;
        }
    }

    return true;
}

// =============================================================================
// syncGuidanceDb — rebuilds .guidance.db from JSON + capabilities
// =============================================================================

/// Synchronizes guidance database with Zig allocator and configuration parameters.
pub fn syncGuidanceDb(
    allocator: std.mem.Allocator,
    json_dir: []const u8,
    guidance_db_path: []const u8,
    cfg: *const config_mod.ProjectConfig,
    verbose: bool,
) void {
    if (guidanceDbIsUpToDate(allocator, guidance_db_path, json_dir, cfg.capabilities_dir)) {
        if (verbose) std.debug.print("gen: guidance.db is up to date, skipping\n", .{});
        return;
    }

    const embedder = vector_mod.createEmbeddingProvider(
        allocator,
        cfg.embedding_provider,
        null, // api_key — from environment, not config
        cfg.embedding_model,
        cfg.embedding_dims,
    ) catch |err| {
        std.debug.print("guidance.db: embedding provider init failed ({s}), using keyword-only\n", .{@errorName(err)});
        var noop = allocator.create(vector_mod.NoopEmbedding) catch return;
        noop.* = .{ .allocator = allocator };
        const p = noop.provider();
        vector_db_mod.syncDatabase(allocator, json_dir, guidance_db_path, p, null, null, cfg.embedding_cache_limit) catch |se| {
            std.debug.print("guidance.db: sync failed: {s}\n", .{@errorName(se)});
        };
        p.deinit();
        return;
    };
    defer embedder.deinit();

    stepPrint("gen: guidance.db ({s})\n", .{embedder.getName()});
    if (verbose) std.debug.print("gen: syncing guidance.db to {s}\n", .{guidance_db_path});

    // cfg.capabilities_dir is now an absolute path (resolved in config loader).
    const cap_dir_abs: ?[]const u8 = blk: {
        std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), cfg.capabilities_dir, .{}) catch break :blk null;
        break :blk allocator.dupe(u8, cfg.capabilities_dir) catch break :blk null;
    };
    defer if (cap_dir_abs) |p| allocator.free(p);

    if (verbose) {
        if (cap_dir_abs) |p| {
            std.debug.print("gen: capabilities_dir: {s}\n", .{p});
        } else {
            std.debug.print("gen: capabilities_dir: not found or not accessible\n", .{});
        }
    }

    // Load semantic aliases for embedding-based query steering
    const aliases_path = std.fs.path.join(allocator, &.{ json_dir, "semantic-aliases.json" }) catch |err| {
        std.debug.print("warning: failed to build aliases path: {s}\n", .{@errorName(err)});
        return;
    };
    defer allocator.free(aliases_path);

    var aliases: ?vector_db_mod.SemanticAliases = blk: {
        const loaded = vector_db_mod.loadSemanticAliases(allocator, aliases_path) catch |err| {
            std.debug.print("warning: failed to load semantic aliases from {s}: {s}\n", .{ aliases_path, @errorName(err) });
            break :blk null;
        };
        if (loaded) |ali| {
            std.debug.print("semantic-aliases: loaded {d} aliases from {s}\n", .{ ali.aliases.len, aliases_path });
            break :blk ali;
        }
        std.debug.print("semantic-aliases: no aliases found at {s}\n", .{aliases_path});
        break :blk null;
    };
    defer if (aliases) |*a| a.deinit();

    vector_db_mod.syncDatabase(allocator, json_dir, guidance_db_path, embedder, cap_dir_abs, aliases, cfg.embedding_cache_limit) catch |err| {
        std.debug.print("guidance.db: sync failed: {s}\n", .{@errorName(err)});
        return;
    };

    if (verbose) std.debug.print("gen: guidance.db written to {s}\n", .{guidance_db_path});
}

// =============================================================================
// clearSynthesisCacheAt
// =============================================================================

/// Clears the synthesis cache using the provided allocator and database path.
pub fn clearSynthesisCacheAt(allocator: std.mem.Allocator, db_path: []const u8) void {
    var noop: vector_mod.NoopEmbedding = .{};
    var db = GuidanceDb.init(allocator, db_path, noop.provider()) catch return;
    defer db.deinit();
    db.clearSynthesisCache();
    std.debug.print("gen: cleared llm synthesis cache (--force)\n", .{});
}

// =============================================================================
// generateSemanticAliases
// =============================================================================

/// Generates semantic aliases from a guidance directory, returning a Zig slice.
pub fn generateSemanticAliases(guidance_dir: []const u8, verbose: bool) !void {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    // Check if semantic-aliases.json already exists
    const aliases_path = try std.fs.path.join(allocator, &.{ guidance_dir, "semantic-aliases.json" });
    defer allocator.free(aliases_path);

    std.Io.Dir.cwd().access(aliases_path, .{}) catch |err| {
        if (err == error.FileNotFound) {
            if (verbose) std.debug.print("semantic-aliases: {s} not found, using default aliases\n", .{aliases_path});
            // Create a minimal default aliases file
            // Most projects should hand-curate their aliases
        }
        return;
    };

    if (verbose) std.debug.print("semantic-aliases: using existing {s}\n", .{aliases_path});
}

// =============================================================================
// cmdGen / cmdGenImpl — main entry points
// =============================================================================

/// Generates a Zig command string using provided allocator and arguments.
pub fn cmdGen(allocator: std.mem.Allocator, args: []const []const u8, caps_sync_fn: ?CapabilitiesSyncFn) !void {
    const ga = GenArgs.parse(args) catch |err| {
        std.debug.print("error: gen flag missing value ({s})\n", .{@errorName(err)});
        return err;
    };
    try cmdGenImpl(allocator, ga, caps_sync_fn);
}

/// Generates a Zig implementation for the sync engine using an allocator and provided generation arguments.
pub fn cmdGenImpl(allocator: std.mem.Allocator, ga: GenArgs, caps_sync_fn: ?CapabilitiesSyncFn) !void {
    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    var paths = try resolveGenPaths(allocator, ga, cwd);
    defer paths.deinit(allocator);

    if (ga.verbose) {
        std.debug.print("guidance gen:\n  workspace: {s}\n  json_dir:  {s}\n  db_path:   {s}\n", .{
            paths.workspace, paths.json_dir, paths.db_path,
        });
    }

    // Load config for test/lint/fmt commands.
    var cfg = config_mod.loadConfig(allocator, paths.workspace) catch
        try config_mod.loadConfig(allocator, cwd);
    defer cfg.deinit();

    // ── Optional comment sync pre-pass ────────────────────────────────────────
    // Run CommentSyncProcessor over the target file(s) before JSON generation.
    // This inserts/updates /// doc comments in source files so they are
    // captured in the subsequent JSON sync pass.
    //
    // Condition: run when --sync-comments is explicitly passed, OR when AI is
    // not explicitly disabled (--no-llm).  When the LLM is unreachable,
    // generateMemberComment returns null and no changes are made (no-op).
    if (ga.sync_comments or !ga.no_llm) {
        var csp = comment_sync_mod.CommentSyncProcessor.init(
            allocator,
            paths.workspace,
            paths.json_dir,
            ga.debug,
            ga.dry_run,
        );
        csp.generate_headers = ga.sync_headers;
        csp.incremental = !ga.force;
        setupCspEnhancer(allocator, ga, &cfg, &csp);
        defer teardownCspEnhancer(allocator, &csp);

        if (ga.file) |file_arg| {
            const src_abs = try common.resolvePath(allocator, paths.workspace, file_arg);
            defer allocator.free(src_abs);
            _ = csp.processFile(src_abs) catch |err| {
                if (ga.verbose) std.debug.print("[sync-comments] {s}: {s}\n", .{ src_abs, @errorName(err) });
            };
        } else {
            const src_scan_dir = try std.fs.path.join(allocator, &.{ paths.workspace, "src" });
            defer allocator.free(src_scan_dir);
            var dir = std.Io.Dir.openDirAbsolute(std.Io.Threaded.global_single_threaded.io(), src_scan_dir, .{ .iterate = true }) catch null;
            if (dir) |*d| {
                var walker = try d.walk(allocator);
                defer walker.deinit();
                while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
                    if (entry.kind != .file) continue;
                    if (!std.mem.endsWith(u8, entry.path, ".zig")) continue;
                    const abs = try std.fs.path.join(allocator, &.{ src_scan_dir, entry.path });
                    defer allocator.free(abs);
                    _ = csp.processFile(abs) catch continue;
                }
            }
        }
    }

    var processor = sync_mod.SyncProcessor.init(
        allocator,
        paths.workspace,
        paths.json_dir,
        ga.dry_run,
        ga.debug,
    );
    defer processor.deinit();
    setupEnhancer(allocator, ga, &cfg, &processor);

    // M4: Load capabilities from database before processing files.
    // This enables back-propagation of capabilities into guidance JSON.
    // Requires discover-capability-sources to have been run first.
    if (ga.compile_db) {
        processor.loadCapabilitiesFromDb(paths.db_path);
    }

    // ── Single-file mode ──────────────────────────────────────────────────────
    if (ga.file) |file_arg| {
        const src_abs = try common.resolvePath(allocator, paths.workspace, file_arg);
        defer allocator.free(src_abs);

        const json_path = try guidanceJsonPath(allocator, paths.workspace, paths.json_dir, src_abs);
        defer allocator.free(json_path);

        if (!ga.force and !marker_mod.fileNeedsProcessing(src_abs, json_path)) {
            if (ga.verbose) std.debug.print("gen: {s} is up to date\n", .{src_abs});
        } else {
            const ext = std.fs.path.extension(src_abs);
            // Only .zig files are handled by the built-in Zig AST pipeline.
            // .md and other files registered in the plugin registry go through
            // the provider/plugin path below, not through processFile (Zig-only).
            const is_zig_builtin = std.mem.eql(u8, ext, ".zig");
            if (is_zig_builtin) {
                const ok = try runBuiltinFilePipeline(
                    allocator,
                    &cfg,
                    &processor,
                    src_abs,
                    ga,
                );
                if (!ok) return error.LintFailed;
            } else if (ga.all_languages) {
                if (try provider_mod.discoverProvider(allocator, paths.workspace, ext)) |prov| {
                    defer prov.deinit(allocator);
                    _ = try provider_mod.invokeProviderFile(
                        allocator,
                        prov,
                        src_abs,
                        paths.json_dir,
                        &.{},
                    );
                }
            }
        }

        if (ga.dry_run) {
            std.debug.print("(dry-run — no files written)\n", .{});
            return;
        }
        if (ga.compile_db) {
            syncGuidanceDb(allocator, paths.json_dir, paths.db_path, &cfg, ga.verbose);
            if (ga.force) clearSynthesisCacheAt(allocator, paths.db_path);
        }
        return;
    }

    // ── Explicit scan-dir mode  (--scan) ─────────────────────────────────────
    if (ga.scan) |scan_arg| {
        const scan_abs = try common.resolvePath(allocator, paths.workspace, scan_arg);
        defer allocator.free(scan_abs);

        // Only collect .zig files for the built-in Zig AST pipeline.
        // .md files use the MarkdownPlugin path, not the Zig AST parser.
        const builtin_exts = [_][]const u8{".zig"};
        const zig_files = try collectFilesWithExts(allocator, scan_abs, &builtin_exts);
        defer {
            for (zig_files) |p| allocator.free(p);
            allocator.free(zig_files);
        }

        // Collect stale files only.
        var stale: std.ArrayList([]const u8) = .empty;
        defer stale.deinit(allocator);
        for (zig_files) |src_abs| {
            const json_path = try guidanceJsonPath(allocator, paths.workspace, paths.json_dir, src_abs);
            defer allocator.free(json_path);
            if (ga.force or marker_mod.fileNeedsProcessing(src_abs, json_path))
                try stale.append(allocator, src_abs);
        }

        if (stale.items.len > 0) {
            try runBuiltinLanguagePipeline(allocator, &cfg, &processor, "zig", stale.items, zig_files, paths.json_dir, ga);
        } else {
            if (ga.verbose) std.debug.print("gen: all {d} built-in file(s) up to date\n", .{zig_files.len});
        }

        if (ga.verbose) std.debug.print("gen: {d}/{d} file(s) processed from {s}\n", .{
            stale.items.len, zig_files.len, scan_abs,
        });

        if (ga.dry_run) {
            std.debug.print("(dry-run — no files written)\n", .{});
            return;
        }
        if (ga.compile_db) {
            syncGuidanceDb(allocator, paths.json_dir, paths.db_path, &cfg, ga.verbose);
            if (ga.force) clearSynthesisCacheAt(allocator, paths.db_path);
        }
        return;
    }

    // ── Full workspace scan (default) ─────────────────────────────────────────
    // Group source files by built-in vs. external so each language runs its
    // own test suite exactly once, before per-file lint/fmt/guidance.

    // Built-in language: Zig only. The Zig AST pipeline uses AstParser which
    // only understands Zig syntax. .md files are registered in the plugin
    // registry (MarkdownPlugin) and processed via the external-provider path.
    const builtin_exts = [_][]const u8{".zig"};
    {
        var all_builtin: std.ArrayList([]const u8) = .empty;
        defer {
            for (all_builtin.items) |p| allocator.free(p);
            all_builtin.deinit(allocator);
        }
        for (cfg.src_dirs) |src_rel| {
            const src_abs = try common.resolvePath(allocator, paths.workspace, src_rel);
            defer allocator.free(src_abs);
            const files = try collectFilesWithExts(allocator, src_abs, &builtin_exts);
            defer allocator.free(files);
            for (files) |p| try all_builtin.append(allocator, p);
            // Note: `p` is now owned by `all_builtin`; `files` slice freed above.
        }

        // Filter to stale only.
        var stale: std.ArrayList([]const u8) = .empty;
        defer stale.deinit(allocator);
        var missing_count: usize = 0;
        var newer_count: usize = 0;
        for (all_builtin.items) |src_abs| {
            const json_path = try guidanceJsonPath(
                allocator,
                paths.workspace,
                paths.json_dir,
                src_abs,
            );
            defer allocator.free(json_path);
            const needs_processing = ga.force or marker_mod.fileNeedsProcessing(src_abs, json_path);
            if (needs_processing) {
                try stale.append(allocator, src_abs);
                // Classify reason: missing JSON or source newer than JSON
                if (!ga.force) {
                    if (std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), json_path, .{})) {
                        // JSON exists, so file is stale because source is newer
                        newer_count += 1;
                    } else |_| {
                        // JSON missing
                        missing_count += 1;
                    }
                }
            }
        }

        if (stale.items.len > 0) {
            // Build concise reason string
            var reason_buf: [80]u8 = undefined;
            const reason = if (ga.force)
                " (forced)"
            else if (missing_count > 0 and newer_count > 0)
                std.fmt.bufPrint(&reason_buf, " ({d} newer, {d} missing)", .{ newer_count, missing_count }) catch "stale"
            else if (missing_count > 0)
                std.fmt.bufPrint(&reason_buf, " ({d} missing)", .{missing_count}) catch "stale"
            else if (newer_count > 0)
                std.fmt.bufPrint(&reason_buf, " ({d} newer)", .{newer_count}) catch "stale"
            else
                "stale";
            stepPrint("gen: {d}/{d} zig files{s}\n", .{ stale.items.len, all_builtin.items.len, reason });
            try runBuiltinLanguagePipeline(allocator, &cfg, &processor, "zig", stale.items, all_builtin.items, paths.json_dir, ga);
        } else {
            stepPrint("gen: all {d} zig files up to date\n", .{all_builtin.items.len});
        }
    }

    // External providers (e.g. guidance-py for .py files).
    if (ga.all_languages) {
        // Collect every distinct extension found in src_dirs that is NOT built-in.
        var foreign_exts: std.StringHashMapUnmanaged(void) = .empty;
        defer foreign_exts.deinit(allocator);

        for (cfg.src_dirs) |src_rel| {
            const src_abs = try common.resolvePath(allocator, paths.workspace, src_rel);
            defer allocator.free(src_abs);

            const io = std.Io.Threaded.global_single_threaded.io();
            var dir = std.Io.Dir.openDirAbsolute(io, src_abs, .{ .iterate = true }) catch continue;
            defer dir.close(io);
            var walker = try dir.walk(allocator);
            defer walker.deinit();

            while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
                if (entry.kind != .file) continue;
                const ext = std.fs.path.extension(entry.basename);
                if (ext.len == 0) continue;
                // Skip built-in extensions.
                const is_builtin = for (builtin_exts) |be| {
                    if (std.mem.eql(u8, ext, be)) break true;
                } else false;
                if (is_builtin) continue;
                // Check whether any file with this extension is stale before
                // recording the extension (avoids probing providers unnecessarily).
                const file_abs = try std.fs.path.join(allocator, &.{ src_abs, entry.path });
                defer allocator.free(file_abs);
                const json_path = try guidanceJsonPath(
                    allocator,
                    paths.workspace,
                    paths.json_dir,
                    file_abs,
                );
                defer allocator.free(json_path);
                if (ga.force or marker_mod.fileNeedsProcessing(file_abs, json_path)) {
                    if (!foreign_exts.contains(ext)) {
                        try foreign_exts.put(allocator, try allocator.dupe(u8, ext), {});
                    }
                }
            }
        }

        // Invoke one provider per stale extension group via --scan.
        var ext_it = foreign_exts.keyIterator();
        while (ext_it.next()) |ext_ptr| {
            const ext = ext_ptr.*;
            defer allocator.free(ext);
            const prov_opt = try provider_mod.discoverProvider(allocator, paths.workspace, ext);
            if (prov_opt == null) {
                if (ga.verbose) std.debug.print("gen: no provider found for {s} — skipping\n", .{ext});
                continue;
            }
            const prov = prov_opt.?;
            defer prov.deinit(allocator);

            // Invoke provider once per src_dir that contains stale files of this extension.
            for (cfg.src_dirs) |src_rel| {
                const src_abs = try common.resolvePath(allocator, paths.workspace, src_rel);
                defer allocator.free(src_abs);
                if (ga.verbose) std.debug.print("gen: invoking {s} provider for {s} in {s}\n", .{
                    prov.name, ext, src_abs,
                });
                _ = try provider_mod.invokeProviderScan(
                    allocator,
                    prov,
                    src_abs,
                    paths.json_dir,
                    &.{},
                );
            }
        }
    }

    if (ga.dry_run) {
        std.debug.print("(dry-run — no files written)\n", .{});
        return;
    }

    // ── Optional schema validation pass ─────────────────────────────────────
    if (ga.validate_schema) {
        validateAllJsonSchema(allocator, paths.json_dir, ga.verbose);
    }

    // ── Post-processing: sync generated comments to source, fmt, correct lines ─
    // This phase runs after all JSON files are generated and checks for members
    // with comment_generated=true. For those files, it writes comments to source,
    // runs fmt, and corrects line numbers.
    if (!ga.dry_run and !ga.no_llm) {
        _ = postProcessCommentSync(
            allocator,
            paths.json_dir,
            paths.workspace,
            &cfg,
            ga.dry_run,
            ga.verbose,
        ) catch |err| {
            std.debug.print("warning: post-process comment sync failed: {s}\n", .{@errorName(err)});
        };
    }

    if (ga.compile_db) {
        // M8.2: Auto-sync capabilities before DB sync so capability embeddings
        // and source mappings are always current.
        if (caps_sync_fn) |syncFn| syncFn(allocator, paths.json_dir, paths.db_path, cfg.capabilities_dir, ga.verbose);

        // Generate semantic aliases from keyword frequency analysis
        // json_dir is the .guidance directory
        generateSemanticAliases(paths.json_dir, ga.verbose) catch |err| {
            std.debug.print("warning: semantic alias generation failed: {s}\n", .{@errorName(err)});
        };
        syncGuidanceDb(allocator, paths.json_dir, paths.db_path, &cfg, ga.verbose);
        if (ga.force) clearSynthesisCacheAt(allocator, paths.db_path);
    }
}

// =============================================================================
// validateAllJsonSchema
// =============================================================================

/// Validates JSON schema files against specified allocator and directory, returning success or error details.
pub fn validateAllJsonSchema(allocator: std.mem.Allocator, json_dir: []const u8, verbose: bool) void {
    const src_dir_path = std.fs.path.join(allocator, &.{ json_dir, "src" }) catch return;
    defer allocator.free(src_dir_path);

    const io = std.Io.Threaded.global_single_threaded.io();
    var src_dir = std.Io.Dir.openDirAbsolute(io, src_dir_path, .{ .iterate = true }) catch return;
    defer src_dir.close(io);
    var walker = src_dir.walk(allocator) catch return;
    defer walker.deinit();

    var store = json_store_mod.JsonStore.init(allocator);
    var ok: usize = 0;
    var bad: usize = 0;

    while (walker.next(std.Io.Threaded.global_single_threaded.io()) catch null) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.basename, ".json")) continue;

        const full = std.fs.path.join(allocator, &.{ src_dir_path, entry.path }) catch continue;
        defer allocator.free(full);

        const doc = store.loadGuidance(full) catch continue orelse continue;
        defer store.freeGuidanceDoc(doc);

        schema_validator_mod.validateGuidanceDoc(allocator, &doc) catch |err| {
            std.debug.print("schema violation in {s}: {s}\n", .{ entry.path, @errorName(err) });
            bad += 1;
            continue;
        };
        ok += 1;
    }

    if (verbose or bad > 0) {
        std.debug.print("schema validation: {d} ok, {d} violations\n", .{ ok, bad });
    }
}

// =============================================================================
// postProcessCommentSync
// =============================================================================

/// Processes a sync engine Zig comment, updating state with allocator, directory, workspace, config, and verbosity flags.
pub fn postProcessCommentSync(
    allocator: std.mem.Allocator,
    json_dir: []const u8,
    workspace: []const u8,
    cfg: *const config_mod.ProjectConfig,
    dry_run: bool,
    verbose: bool,
) !usize {
    const json_src_dir = std.fs.path.join(allocator, &.{ json_dir, "src" }) catch return 0;
    defer allocator.free(json_src_dir);

    const io = std.Io.Threaded.global_single_threaded.io();
    var src_dir = std.Io.Dir.openDirAbsolute(io, json_src_dir, .{ .iterate = true }) catch return 0;
    defer src_dir.close(io);
    var walker = src_dir.walk(allocator) catch return 0;
    defer walker.deinit();

    var store = json_store_mod.JsonStore.init(allocator);
    // JsonStore has no deinit method - memory is managed per-call

    // Collect files that have generated comments.
    var modified_count: usize = 0;

    while (walker.next(std.Io.Threaded.global_single_threaded.io()) catch null) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.basename, ".json")) continue;

        const json_path = std.fs.path.join(allocator, &.{ json_src_dir, entry.path }) catch continue;
        defer allocator.free(json_path);

        var doc = store.loadGuidance(json_path) catch continue orelse continue;

        // Milestone 3.2: Extract member comments from source file.
        // Member comments are NOT stored in JSON per Milestone 3.1, so we need to
        // extract them from source to find members with generated comments.
        const src_rel = entry.path[0 .. entry.path.len - 5]; // strip .json
        const src_abs = std.fs.path.join(allocator, &.{ workspace, "src", src_rel }) catch {
            store.freeGuidanceDoc(doc);
            continue;
        };

        const source = std.Io.Dir.cwd().readFileAlloc(io, src_abs, allocator, .limited(10 * 1024 * 1024)) catch |err| {
            if (err == error.FileNotFound) {
                // Source file was deleted — the JSON is an orphan.  Remove it so
                // discover-capability-sources cannot emit stale AUTO-SOURCES entries.
                std.Io.Dir.deleteFileAbsolute(std.Io.Threaded.global_single_threaded.io(), json_path) catch {};
                if (verbose) std.debug.print(
                    "post-process: removed orphaned JSON for deleted source {s}\n",
                    .{src_abs},
                );
            } else {
                std.debug.print("post-process: WARN: cannot read {s}: {s}\n", .{ src_abs, @errorName(err) });
            }
            allocator.free(src_abs);
            store.freeGuidanceDoc(doc);
            continue;
        };
        // Note: source will be freed at the end of the loop iteration

        store.extractMemberCommentsFromSource(&doc, source);

        // Check if any member has comment_generated=true.
        var has_generated = false;
        for (doc.members) |member| {
            if (member.comment_generated) {
                has_generated = true;
                break;
            }
        }

        if (!has_generated) {
            allocator.free(src_abs);
            allocator.free(source);
            store.freeGuidanceDoc(doc);
            continue;
        }

        if (verbose) {
            std.debug.print("post-process: {s}: writing generated comments to source\n", .{src_abs});
        }

        if (dry_run) {
            allocator.free(src_abs);
            allocator.free(source);
            store.freeGuidanceDoc(doc);
            modified_count += 1;
            continue;
        }

        // Use the already-loaded source (avoid re-reading the file)
        // Note: can't defer source here, will be freed manually

        // Process members in descending line order to avoid shifting.
        const sorted_members = comment_sync_mod.sortMembersByLineDesc(allocator, doc.members) catch {
            store.freeGuidanceDoc(doc);
            continue;
        };
        defer allocator.free(sorted_members);

        var current_source: []const u8 = try allocator.dupe(u8, source);
        defer allocator.free(current_source);
        var source_changed = false;

        for (sorted_members) |member| {
            if (!member.comment_generated) continue;
            if (member.comment == null) continue;

            const decl_line = member.line orelse continue;
            const new_comment = member.comment.?;

            // Check if there's already a comment at this line.
            const existing = try comment_inserter_mod.extractCommentAtLine(allocator, current_source, decl_line);
            defer if (existing) |e| allocator.free(e);

            if (existing == null) {
                // No existing comment — insert.
                const insert_res = try comment_inserter_mod.insertComment(
                    allocator,
                    current_source,
                    decl_line,
                    new_comment,
                );
                if (insert_res.changed) {
                    allocator.free(current_source);
                    current_source = insert_res.new_source;
                    allocator.free(insert_res.line_adjustments);
                    source_changed = true;
                } else {
                    insert_res.deinit(allocator);
                }
            } else {
                // Existing comment — replace if different.
                if (!std.mem.eql(u8, existing.?, new_comment)) {
                    const replace_res = try comment_inserter_mod.replaceComment(
                        allocator,
                        current_source,
                        decl_line,
                        new_comment,
                    );
                    if (replace_res.changed) {
                        allocator.free(current_source);
                        current_source = replace_res.new_source;
                        allocator.free(replace_res.line_adjustments);
                        source_changed = true;
                    } else {
                        replace_res.deinit(allocator);
                    }
                }
            }
        }

        if (source_changed) {
            // Write modified source.
            const file = std.Io.Dir.createFileAbsolute(std.Io.Threaded.global_single_threaded.io(), src_abs, .{ .truncate = true }) catch |err| {
                std.debug.print("post-process: WARN: cannot write {s}: {s}\n", .{ src_abs, @errorName(err) });
                allocator.free(src_abs);
                allocator.free(source);
                store.freeGuidanceDoc(doc);
                continue;
            };
            defer file.close(io);
            try file.writeAll(current_source);
            modified_count += 1;

            // Run fmt on the modified file.
            const ext = std.fs.path.extension(src_abs);
            if (std.mem.eql(u8, ext, ".zig")) {
                if (cfg.fmtCommandForExt(ext)) |fmt_argv| {
                    if (verbose) std.debug.print("fmt:      {s}\n", .{src_abs});
                    _ = common.shell.runCommand(allocator, fmt_argv) catch {};
                }
            }

            // Correct line numbers in JSON.
            comment_sync_mod.correctLineNumbers(allocator, src_abs, json_dir, workspace) catch |err| {
                std.debug.print("post-process: WARN: failed to correct lines for {s}: {s}\n", .{ src_abs, @errorName(err) });
            };
        }

        allocator.free(src_abs);
        allocator.free(source);
        store.freeGuidanceDoc(doc);
    }

    if (modified_count > 0 and verbose) {
        std.debug.print("post-process: processed {d} file(s) with generated comments\n", .{modified_count});
    }

    return modified_count;
}

// =============================================================================
// guidanceJsonPath
// =============================================================================

/// Extracts a JSON path from a Zig source file using an allocator and workspace parameters.
pub fn guidanceJsonPath(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    json_dir: []const u8,
    src_abs: []const u8,
) ![]const u8 {
    const rel: []const u8 = if (std.mem.startsWith(u8, src_abs, workspace)) blk: {
        const stripped = src_abs[workspace.len..];
        break :blk if (stripped.len > 0 and stripped[0] == '/') stripped[1..] else stripped;
    } else src_abs;
    return std.fmt.allocPrint(allocator, "{s}/{s}.json", .{ json_dir, rel });
}

// =============================================================================
// Pipeline helpers
// =============================================================================

/// Executes a command using the provided allocator and file paths, returning success or error status.
pub fn runPhaseCommand(
    allocator: std.mem.Allocator,
    argv_template: []const []const u8,
    file_path: []const u8,
) !bool {
    var argv: std.ArrayList([]const u8) = .empty;
    defer argv.deinit(allocator);
    for (argv_template) |tok| {
        try argv.append(allocator, if (std.mem.eql(u8, tok, "{file}")) file_path else tok);
    }
    return common.shell.runCommand(allocator, argv.items);
}

/// Extracts files from a directory, filtering by specified extensions.
pub fn collectFilesWithExts(
    allocator: std.mem.Allocator,
    dir_abs: []const u8,
    exts: []const []const u8,
) ![][]const u8 {
    var results: std.ArrayList([]const u8) = .empty;
    errdefer {
        for (results.items) |p| allocator.free(p);
        results.deinit(allocator);
    }

    const io = std.Io.Threaded.global_single_threaded.io();
    var dir = std.Io.Dir.openDirAbsolute(io, dir_abs, .{ .iterate = true }) catch return results.toOwnedSlice(allocator);
    defer dir.close(io);

    var walker = try dir.walk(allocator);
    defer walker.deinit();

    while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
        if (entry.kind != .file) continue;
        if (std.mem.endsWith(u8, entry.basename, "_tests.zig")) continue;
        const ext = std.fs.path.extension(entry.basename);
        const matched = for (exts) |e| {
            if (std.mem.eql(u8, ext, e)) break true;
        } else false;
        if (!matched) continue;
        const full = try std.fs.path.join(allocator, &.{ dir_abs, entry.path });
        try results.append(allocator, full);
    }

    return results.toOwnedSlice(allocator);
}

// =============================================================================
// Built-in file/language pipelines
// =============================================================================

pub fn runBuiltinFilePipeline(
    allocator: std.mem.Allocator,
    cfg: *const config_mod.ProjectConfig,
    processor: *sync_mod.SyncProcessor,
    src_abs: []const u8,
    ga: GenArgs,
) !bool {
    const ext = std.fs.path.extension(src_abs);

    // ── 1. Lint (with one auto-fix attempt via fmt) ───────────────────────
    if (!ga.skip_lint) {
        if (cfg.lintCommandForExt(ext)) |lint_argv| {
            if (ga.verbose) std.debug.print("lint:     {s}\n", .{src_abs});
            const ok = try runPhaseCommand(allocator, lint_argv, src_abs);
            if (!ok) {
                // One attempt to auto-fix via the fmt command.
                const fixed = if (!ga.skip_fmt)
                    if (cfg.fmtCommandForExt(ext)) |fmt_argv| blk: {
                        if (ga.verbose) std.debug.print("lint-fix: {s}\n", .{src_abs});
                        _ = try runPhaseCommand(allocator, fmt_argv, src_abs);
                        break :blk try runPhaseCommand(allocator, lint_argv, src_abs);
                    } else false
                else
                    false;
                if (!fixed) {
                    std.debug.print("error: lint failed for {s}\n", .{src_abs});
                    return false;
                }
                if (ga.verbose) std.debug.print("lint-fix: fixed {s}\n", .{src_abs});
            }
        }
    }

    // ── 2. Format ─────────────────────────────────────────────────────────
    if (!ga.skip_fmt) {
        if (cfg.fmtCommandForExt(ext)) |fmt_argv| {
            if (ga.verbose) std.debug.print("fmt:      {s}\n", .{src_abs});
            _ = try runPhaseCommand(allocator, fmt_argv, src_abs);
        }
    }

    // ── 3. Guidance ───────────────────────────────────────────────────────
    // processFile writes the JSON unconditionally (merge + save), which
    // advances the file's mtime naturally — no separate touch needed.
    // Touching would truncate the file we just wrote.
    _ = processor.processFile(src_abs, ga.timeout_seconds) catch |err| {
        std.debug.print("warning: guidance failed for {s}: {s}\n", .{ src_abs, @errorName(err) });
        // Leave JSON stale on failure so the next run retries this file.
        return true;
    };

    return true;
}

/// Executes a built-in language pipeline using provided allocator, config, processor, and language data.
pub fn runBuiltinLanguagePipeline(
    allocator: std.mem.Allocator,
    cfg: *const config_mod.ProjectConfig,
    processor: *sync_mod.SyncProcessor,
    language: []const u8,
    stale_files: []const []const u8,
    all_files: []const []const u8,
    guidance_root: []const u8,
    ga: GenArgs,
) !void {
    if (stale_files.len == 0) return;

    // ── Test suite (once per language group, before any file is modified) ─
    // Derive extension from language (e.g. "zig" -> ".zig")
    var ext_buf: [32]u8 = undefined;
    const ext = std.fmt.bufPrint(&ext_buf, ".{s}", .{language}) catch language;

    if (!ga.skip_tests) {
        const test_argv = cfg.testCommandForExt(ext);
        if (test_argv) |argv| {
            // Check if we can skip tests (marker newer than ALL source files)
            const marker_path = marker_mod.testMarkerPath(allocator, guidance_root) catch
                return error.OutOfMemory;
            defer allocator.free(marker_path);

            const can_skip = !ga.force and marker_mod.testsCanBeSkipped(marker_path, all_files);
            if (can_skip) {
                if (ga.verbose) std.debug.print("test:     {s} skipped (test_passed marker is fresh)\n", .{language});
            } else {
                stepPrint("test: {s} ({d} changed)\n", .{ language, stale_files.len });
                const ok = try common.shell.runCommand(allocator, argv);
                if (!ok) {
                    std.debug.print("error: test suite failed for language '{s}'\n", .{language});
                    return error.TestFailed;
                }
                // Touch the marker to record successful test run
                marker_mod.touchTestMarker(marker_path) catch |err| {
                    std.debug.print("warning: could not create test_passed marker: {s}\n", .{@errorName(err)});
                };
                stepPrint("test: passed\n", .{});
            }
        } else {
            if (ga.verbose) std.debug.print("test:     skipped (no test command for {s})\n", .{language});
        }
    }

    // ── Per-file phases ───────────────────────────────────────────────────
    // Process every stale file so all lint failures are reported before exiting.
    var any_lint_failed = false;
    for (stale_files) |src_abs| {
        const ok = try runBuiltinFilePipeline(allocator, cfg, processor, src_abs, ga);
        if (!ok) any_lint_failed = true;
    }
    if (any_lint_failed) return error.LintFailed;
}
