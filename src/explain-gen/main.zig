//! explain-gen — AST-guided SQLite FTS5 database generator for NullClaw.
//!
//! Produces:
//!   .explain-gen/src/**/*.json  — Per-file structured metadata mirror
//!   .explain.db                 — SQLite FTS5 database consumed by NullClaw's explain tool
//!
//! Usage:
//!   explain-gen gen      [options]   Generate JSON + compile .explain.db
//!   explain-gen status   [options]   Report generation status
//!   explain-gen clean    [options]   Remove .explain-gen/ and .explain.db
//!   explain-gen structure [options]  Update STRUCTURE.md from guidance JSON
//!   explain-gen deps     [options]   Generate Makefile .depend file

const std = @import("std");
const types = @import("types.zig");
const ast_parser = @import("ast_parser.zig");
const sync_mod = @import("sync.zig");
const structure_mod = @import("structure.zig");
const deps_mod = @import("deps.zig");
const db_mod = @import("db.zig");
const llm = @import("common");
const enhancer_mod = @import("enhancer.zig");
const config_mod = @import("config.zig");
const plugin_mod = @import("plugin.zig");
const plugin_registry = @import("plugin_registry.zig");
const staged_mod = @import("staged.zig");
const llm_filter_mod = @import("llm_filter.zig");
const synthesize_mod = @import("synthesize.zig");

pub const version = "0.1.0";

const Command = enum {
    gen,
    status,
    clean,
    structure,
    deps,
    query,
    explain,
};

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const args = try std.process.argsAlloc(allocator);
    defer std.process.argsFree(allocator, args);

    if (args.len < 2) {
        try printHelp();
        return;
    }

    if (std.mem.eql(u8, args[1], "-h") or std.mem.eql(u8, args[1], "--help")) {
        try printHelp();
        return;
    }
    if (std.mem.eql(u8, args[1], "-v") or std.mem.eql(u8, args[1], "--version")) {
        std.debug.print("explain-gen v{s}\n", .{version});
        return;
    }

    const subcmd = std.meta.stringToEnum(Command, args[1]) orelse {
        std.debug.print("Unknown subcommand: {s}\n\n", .{args[1]});
        try printHelp();
        return;
    };

    switch (subcmd) {
        .gen => try cmdGen(allocator, args[2..]),
        .status => try cmdStatus(allocator, args[2..]),
        .clean => try cmdClean(allocator, args[2..]),
        .structure => try cmdStructure(allocator, args[2..]),
        .deps => try cmdDeps(allocator, args[2..]),
        .query => try cmdQuery(allocator, args[2..]),
        .explain => try cmdExplain(allocator, args[2..]),
    }
}

fn printHelp() !void {
    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.writeAll(
        \\explain-gen v0.1.0 — AST-guided SQLite FTS5 database generator
        \\
        \\Produces .explain-gen/src/**/*.json and .explain.db for NullClaw.
        \\
        \\Usage:
        \\  explain-gen <command> [options]
        \\  explain-gen --help | --version
        \\
        \\Commands:
        \\  gen        Generate .explain-gen/ JSON mirror and .explain.db
        \\  status     Show generation status (synced, stale, missing)
        \\  clean      Remove .explain-gen/src and .explain.db
        \\  structure  Regenerate STRUCTURE.md from guidance JSON
        \\  deps       Generate Makefile .depend file from Zig imports
        \\  query      Search .explain.db with BM25 (no LLM)
        \\  explain    Search with LLM-synthesized summary
        \\
        \\Gen options:
        \\  --file FILE           Process a single source file (incremental)
        \\  --scan DIR            Process all source files under DIR
        \\  -w, --workspace DIR   Source root directory (default: current directory)
        \\  --json-dir DIR        JSON output directory (default: .explain-gen)
        \\  -o, --db PATH         SQLite database path (default: .explain.db)
        \\  --no-db               Skip database compilation step
        \\  --infill              LLM-fill blank comment fields
        \\  --regen               LLM-regenerate all comments
        \\  --dry-run             Show what would change without writing
        \\  --verbose             Print LLM prompts and raw responses
        \\  --api-url URL         LLM API endpoint (default: http://localhost:11434/api/chat)
        \\  -m, --model NAME      Model name (default: code:latest)
        \\
        \\Query/Explain options:
        \\  <query>              Search query (required)
        \\  -l, --limit N         Max results (default: 10)
        \\  --json                Output JSON (query only)
        \\  -o, --db PATH         Database path (default: .explain.db)
        \\  -w, --workspace DIR   Workspace root (default: current directory)
        \\  --guidance DIR        Guidance directory (default: .explain-gen)
        \\  --no-llm              Skip LLM synthesis (explain only)
        \\  --staged=false        Use legacy output format (rollback safety)
        \\  --filter=auto|force|skip  LLM relevance filter mode (default: auto)
        \\                          auto  = LLM filter only for long queries (5+ words)
        \\                          force = always apply LLM filter
        \\                          skip  = never apply LLM filter (fast path)
        \\  --api-url URL         LLM endpoint
        \\  -m, --model NAME      Model for synthesis
        \\
        \\Structure options:
        \\  --json-dir DIR        Guidance JSON directory (default: .explain-gen)
        \\  --no-ai               Skip AI infill pre-pass
        \\  --api-url URL         LLM endpoint
        \\  -m, --model NAME      Model for AI infill
        \\
        \\Deps options:
        \\  --src DIR             Source directory to scan (default: src)
        \\
        \\Examples:
        \\  explain-gen gen
        \\  explain-gen gen --file src/main.zig --infill
        \\  explain-gen gen --file src/main.zig --json-dir .explain-gen --db .explain.db
        \\  explain-gen gen --scan src --infill -m fast:latest
        \\  explain-gen gen -o /tmp/project.explain.db
        \\  explain-gen query "hash function"
        \\  explain-gen explain "how does the sync processor work" --limit 5
        \\  explain-gen status
        \\  explain-gen clean
        \\  explain-gen structure
        \\  explain-gen deps --src src > zig.depend
        \\
    );
    try stdout.flush();
}

// =============================================================================
// gen
// =============================================================================

const GenArgs = struct {
    file: ?[]const u8 = null, // single-file mode (--file)
    scan: ?[]const u8 = null, // directory scan mode (--scan)
    workspace: ?[]const u8 = null,
    json_dir: ?[]const u8 = null,
    db_path: ?[]const u8 = null,
    dry_run: bool = false,
    verbose: bool = false,
    api_url: []const u8 = config_mod.DEFAULT_API_URL,
    model: []const u8 = config_mod.DEFAULT_MODEL,
    infill_comments: bool = false,
    regen_comments: bool = false,
    compile_db: bool = true,
};

fn cmdGen(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var ga: GenArgs = .{};
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--file")) {
            i += 1;
            if (i >= args.len) return;
            ga.file = args[i];
        } else if (std.mem.eql(u8, arg, "--scan")) {
            i += 1;
            if (i >= args.len) return;
            ga.scan = args[i];
        } else if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) return;
            ga.workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--json-dir") or std.mem.eql(u8, arg, "--output")) {
            i += 1;
            if (i >= args.len) return;
            ga.json_dir = args[i];
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) return;
            ga.db_path = args[i];
        } else if (std.mem.eql(u8, arg, "--dry-run")) {
            ga.dry_run = true;
        } else if (std.mem.eql(u8, arg, "--verbose") or std.mem.eql(u8, arg, "--debug")) {
            ga.verbose = true;
        } else if (std.mem.eql(u8, arg, "--infill")) {
            ga.infill_comments = true;
        } else if (std.mem.eql(u8, arg, "--regen")) {
            ga.regen_comments = true;
        } else if (std.mem.eql(u8, arg, "--no-db")) {
            ga.compile_db = false;
        } else if (std.mem.eql(u8, arg, "--api-url")) {
            i += 1;
            if (i >= args.len) return;
            ga.api_url = args[i];
        } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
            i += 1;
            if (i >= args.len) return;
            ga.model = args[i];
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    // Resolve workspace (default: cwd)
    const workspace = if (ga.workspace) |w|
        if (std.fs.path.isAbsolute(w)) try allocator.dupe(u8, w) else try std.fs.path.join(allocator, &.{ cwd, w })
    else
        try allocator.dupe(u8, cwd);
    defer allocator.free(workspace);

    // Resolve json_dir (default: .explain-gen in workspace)
    const json_dir = if (ga.json_dir) |jd|
        if (std.fs.path.isAbsolute(jd)) try allocator.dupe(u8, jd) else try std.fs.path.join(allocator, &.{ workspace, jd })
    else
        try std.fs.path.join(allocator, &.{ workspace, config_mod.DEFAULT_GUIDANCE_DIR });
    defer allocator.free(json_dir);

    // Resolve db_path (default: .explain.db in workspace)
    const db_path = if (ga.db_path) |dp|
        if (std.fs.path.isAbsolute(dp)) try allocator.dupe(u8, dp) else try std.fs.path.join(allocator, &.{ workspace, dp })
    else
        try std.fs.path.join(allocator, &.{ workspace, config_mod.DEFAULT_DB_PATH });
    defer allocator.free(db_path);

    if (ga.verbose) {
        std.debug.print("explain-gen gen:\n  workspace: {s}\n  json_dir:  {s}\n  db_path:   {s}\n", .{
            workspace, json_dir, db_path,
        });
    }

    // ── Build SyncProcessor ──────────────────────────────────────────────────
    var processor = sync_mod.SyncProcessor.init(allocator, workspace, json_dir, ga.dry_run, ga.verbose);
    defer processor.deinit();

    if (ga.infill_comments or ga.regen_comments) {
        const llm_config: llm.LlmConfig = .{
            .api_url = ga.api_url,
            .model = ga.model,
            .debug = ga.verbose,
        };
        processor.enhancer = enhancer_mod.Enhancer.init(allocator, llm_config) catch |err| blk: {
            std.debug.print("warning: could not init LLM enhancer: {}\n", .{err});
            break :blk null;
        };
        processor.infill_comments = ga.infill_comments;
        processor.regen_comments = ga.regen_comments;
    }

    // ── Step 1: Process source files → JSON ─────────────────────────────────

    if (ga.file) |file_arg| {
        // ── Single-file mode (used by per-file Makefile rule) ────────────────
        const full_path = if (std.fs.path.isAbsolute(file_arg))
            try allocator.dupe(u8, file_arg)
        else
            try std.fs.path.join(allocator, &.{ workspace, file_arg });
        defer allocator.free(full_path);

        _ = processor.processFile(full_path) catch |err| {
            std.debug.print("error processing {s}: {}\n", .{ full_path, err });
        };
        std.debug.print("gen: processed {s}\n", .{full_path});
    } else if (ga.scan) |scan_arg| {
        // ── Explicit --scan DIR mode ─────────────────────────────────────────
        const scan_abs = if (std.fs.path.isAbsolute(scan_arg))
            try allocator.dupe(u8, scan_arg)
        else
            try std.fs.path.join(allocator, &.{ workspace, scan_arg });
        defer allocator.free(scan_abs);

        const count = processor.processDirectory(scan_abs) catch |err| {
            std.debug.print("error scanning {s}: {}\n", .{ scan_abs, err });
            return;
        };
        std.debug.print("gen: {d} source files processed from {s}\n", .{ count, scan_abs });
    } else {
        // ── Full workspace scan (default) ────────────────────────────────────
        var cfg = config_mod.loadConfig(allocator, workspace) catch
            try config_mod.loadConfig(allocator, workspace);
        defer cfg.deinit();

        var total: usize = 0;
        for (cfg.src_dirs) |src_rel| {
            const src_abs = if (std.fs.path.isAbsolute(src_rel))
                try allocator.dupe(u8, src_rel)
            else
                try std.fs.path.join(allocator, &.{ workspace, src_rel });
            defer allocator.free(src_abs);

            const count = processor.processDirectory(src_abs) catch |err| {
                std.debug.print("warning: processDirectory({s}): {}\n", .{ src_abs, err });
                continue;
            };
            total += count;
        }
        std.debug.print("gen: {d} source files processed\n", .{total});
    }

    if (ga.dry_run) {
        std.debug.print("(dry-run — no files written)\n", .{});
        return;
    }

    // ── Step 2: Compile JSON → .explain.db ──────────────────────────────────
    if (ga.compile_db) {
        db_mod.syncDatabase(allocator, json_dir, db_path) catch |err| {
            std.debug.print("error: database compilation failed: {}\n", .{err});
            return;
        };
        std.debug.print("gen: .explain.db written to {s}\n", .{db_path});
    }
}

// =============================================================================
// status
// =============================================================================

fn cmdStatus(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var json_dir_arg: ?[]const u8 = null;
    var db_path_arg: ?[]const u8 = null;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--json-dir")) {
            i += 1;
            if (i >= args.len) return;
            json_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) return;
            db_path_arg = args[i];
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const json_dir = if (json_dir_arg) |jd|
        if (std.fs.path.isAbsolute(jd)) try allocator.dupe(u8, jd) else try std.fs.path.join(allocator, &.{ cwd, jd })
    else
        try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR });
    defer allocator.free(json_dir);

    const db_path = if (db_path_arg) |dp|
        if (std.fs.path.isAbsolute(dp)) try allocator.dupe(u8, dp) else try std.fs.path.join(allocator, &.{ cwd, dp })
    else
        try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_DB_PATH });
    defer allocator.free(db_path);

    // Count JSON files in json_dir/src/.
    const json_src_dir = try std.fs.path.join(allocator, &.{ json_dir, "src" });
    defer allocator.free(json_src_dir);

    var json_count: usize = 0;
    if (std.fs.openDirAbsolute(json_src_dir, .{ .iterate = true })) |*jdir_ptr| {
        var jdir = jdir_ptr.*;
        defer jdir.close();
        var walker = try jdir.walk(allocator);
        defer walker.deinit();
        while (try walker.next()) |entry| {
            if (entry.kind == .file and std.mem.endsWith(u8, entry.path, ".json"))
                json_count += 1;
        }
    } else |_| {}

    const db_exists = if (std.fs.openFileAbsolute(db_path, .{})) |f| blk: {
        f.close();
        break :blk true;
    } else |_| false;

    std.debug.print("explain-gen status:\n", .{});
    std.debug.print("  json_dir:   {s}\n", .{json_dir});
    std.debug.print("  json files: {d}\n", .{json_count});
    std.debug.print("  db_path:    {s}\n", .{db_path});
    std.debug.print("  db_exists:  {}\n", .{db_exists});
}

// =============================================================================
// clean
// =============================================================================

fn cmdClean(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var json_dir_arg: ?[]const u8 = null;
    var db_path_arg: ?[]const u8 = null;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--json-dir")) {
            i += 1;
            if (i >= args.len) return;
            json_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) return;
            db_path_arg = args[i];
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const json_dir = if (json_dir_arg) |jd|
        if (std.fs.path.isAbsolute(jd)) try allocator.dupe(u8, jd) else try std.fs.path.join(allocator, &.{ cwd, jd })
    else
        try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR });
    defer allocator.free(json_dir);

    const db_path = if (db_path_arg) |dp|
        if (std.fs.path.isAbsolute(dp)) try allocator.dupe(u8, dp) else try std.fs.path.join(allocator, &.{ cwd, dp })
    else
        try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_DB_PATH });
    defer allocator.free(db_path);

    // Remove the database.
    std.fs.deleteFileAbsolute(db_path) catch |err| {
        if (err != error.FileNotFound)
            std.debug.print("warning: could not remove {s}: {}\n", .{ db_path, err });
    };
    std.debug.print("clean: removed {s}\n", .{db_path});

    // Remove the generated JSON src tree only (preserve config and skills).
    const json_src = try std.fs.path.join(allocator, &.{ json_dir, "src" });
    defer allocator.free(json_src);
    std.fs.deleteTreeAbsolute(json_src) catch |err| {
        if (err != error.FileNotFound)
            std.debug.print("warning: could not remove {s}: {}\n", .{ json_src, err });
    };
    std.debug.print("clean: removed {s}\n", .{json_src});
}

// =============================================================================
// structure
// =============================================================================

fn cmdStructure(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var json_dir_arg: ?[]const u8 = null;
    var no_ai: bool = false;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--json-dir") or std.mem.eql(u8, arg, "--guidance")) {
            i += 1;
            if (i >= args.len) return;
            json_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "--no-ai")) {
            no_ai = true;
        }
    }
    const _no_ai = no_ai; // LLM infill for structure is a future enhancement
    _ = _no_ai;

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const json_dir = if (json_dir_arg) |jd|
        if (std.fs.path.isAbsolute(jd)) try allocator.dupe(u8, jd) else try std.fs.path.join(allocator, &.{ cwd, jd })
    else
        try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR });
    defer allocator.free(json_dir);

    var gen = structure_mod.StructureGenerator.init(allocator, cwd, json_dir, false);
    defer gen.deinit();
    try gen.generate();
}

// =============================================================================
// deps
// =============================================================================

fn cmdDeps(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var src_dir: []const u8 = "src";
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--src")) {
            i += 1;
            if (i >= args.len) return;
            src_dir = args[i];
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    var deps_gen = deps_mod.DepsGenerator.init(allocator, cwd);
    try deps_gen.generateDependencies(src_dir);
}

// =============================================================================
// query
// =============================================================================

const QueryArgs = struct {
    query_str: ?[]const u8 = null,
    limit: usize = 10,
    json_mode: bool = false,
    db_path: ?[]const u8 = null,
    workspace: ?[]const u8 = null,
};

fn cmdQuery(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var qa: QueryArgs = .{};
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "-l") or std.mem.eql(u8, arg, "--limit")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --limit requires a number\n", .{});
                return;
            }
            qa.limit = std.fmt.parseInt(usize, args[i], 10) catch 10;
        } else if (std.mem.eql(u8, arg, "--json")) {
            qa.json_mode = true;
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) return;
            qa.db_path = args[i];
        } else if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) return;
            qa.workspace = args[i];
        } else if (!std.mem.startsWith(u8, arg, "-")) {
            qa.query_str = arg;
        }
    }

    const query_text = qa.query_str orelse {
        std.debug.print("Error: query string required\n", .{});
        return;
    };

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const workspace = if (qa.workspace) |w|
        if (std.fs.path.isAbsolute(w)) try allocator.dupe(u8, w) else try std.fs.path.join(allocator, &.{ cwd, w })
    else
        try allocator.dupe(u8, cwd);
    defer allocator.free(workspace);

    const db_path = if (qa.db_path) |dp|
        if (std.fs.path.isAbsolute(dp)) try allocator.dupe(u8, dp) else try std.fs.path.join(allocator, &.{ workspace, dp })
    else
        try std.fs.path.join(allocator, &.{ workspace, config_mod.DEFAULT_DB_PATH });
    defer allocator.free(db_path);

    // Check database exists
    std.fs.accessAbsolute(db_path, .{}) catch {
        std.debug.print("Error: No .explain.db found at {s}\n", .{db_path});
        std.debug.print("Run 'explain-gen gen' to generate it.\n", .{});
        return;
    };

    var db = db_mod.ExplainDb.init(allocator, db_path) catch |err| {
        std.debug.print("Error opening database: {s}\n", .{@errorName(err)});
        return;
    };
    defer db.deinit();

    const results = db.search(allocator, query_text, qa.limit) catch |err| {
        std.debug.print("Search failed: {s}\n", .{@errorName(err)});
        return;
    };
    defer {
        for (results) |r| db_mod.ExplainDb.freeSearchResult(allocator, r);
        allocator.free(results);
    }

    if (results.len == 0) {
        std.debug.print("No results found for: {s}\n", .{query_text});
        return;
    }

    if (qa.json_mode) {
        try printQueryJson(allocator, query_text, results);
    } else {
        try printQueryText(allocator, query_text, results);
    }
}

fn printQueryText(allocator: std.mem.Allocator, query_text: []const u8, results: []db_mod.ExplainDb.SearchResult) !void {
    _ = allocator;
    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.print("# Query: {s}\n\n", .{query_text});

    for (results, 1..) |r, idx| {
        try stdout.print("{d}. **{s}** ({s}) — {s}", .{ idx, r.name, r.node_type, r.module });
        if (r.line) |l| try stdout.print(":{d}", .{l});
        try stdout.print("\n", .{});

        if (r.comment) |c| {
            const nl = std.mem.indexOfScalar(u8, c, '\n') orelse c.len;
            const snippet = c[0..@min(nl, 120)];
            try stdout.print("   {s}\n", .{snippet});
        }
        if (r.signature) |s| {
            try stdout.print("   `{s}`\n", .{s});
        }
        try stdout.print("\n", .{});
    }

    try stdout.flush();
}

fn printQueryJson(allocator: std.mem.Allocator, query_text: []const u8, results: []db_mod.ExplainDb.SearchResult) !void {
    _ = allocator;
    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.print("{{\"query\":\"{s}\",\"results\":[\n", .{query_text});

    for (results, 0..) |r, i| {
        try stdout.print("  {{\"module\":\"{s}\",\"name\":\"{s}\",\"type\":\"{s}\"", .{ r.module, r.name, r.node_type });
        if (r.signature) |s| {
            try stdout.print(",\"signature\":\"{s}\"", .{s});
        }
        if (r.comment) |c| {
            const nl = std.mem.indexOfScalar(u8, c, '\n') orelse c.len;
            const snippet = c[0..@min(nl, 200)];
            try stdout.print(",\"comment\":\"{s}\"", .{snippet});
        }
        if (r.line) |l| {
            try stdout.print(",\"line\":{d}", .{l});
        }
        try stdout.print(",\"language\":\"{s}\",\"score\":{d:.4}}}", .{ r.language, r.score });
        if (i < results.len - 1) try stdout.print(",\n", .{});
    }

    try stdout.print("]}}\n", .{});
    try stdout.flush();
}

// =============================================================================
// explain — shared types
// =============================================================================

const SkillExcerpt = struct { name: []const u8, excerpt: []const u8 };
const ExcerptEntry = struct {
    file_path: []const u8, // borrowed from SearchResult
    label: []const u8, // owned: "src/foo.zig:42"
    code: []const u8, // owned: pruned source block
    lang: []const u8, // borrowed constant
};

// =============================================================================
// explain
// =============================================================================

/// Whether LLM relevance filtering should be applied on the staged path.
const FilterMode = enum {
    /// Auto-detect: apply LLM filter only for long queries (5+ words).
    auto,
    /// Always apply LLM filter (even for short queries).
    force,
    /// Never apply LLM filter (always fast path).
    skip,
};

const ExplainArgs = struct {
    query_str: ?[]const u8 = null,
    limit: usize = 10,
    db_path: ?[]const u8 = null,
    workspace: ?[]const u8 = null,
    guidance: ?[]const u8 = null,
    api_url: []const u8 = config_mod.DEFAULT_API_URL,
    model: []const u8 = config_mod.DEFAULT_MODEL,
    /// Skip LLM synthesis; emit structural output only.
    no_llm: bool = false,
    verbose: bool = false,
    /// Use new staged pipeline (default: true).  --staged=false → legacy path.
    staged: bool = true,
    /// LLM relevance filtering mode.
    filter: FilterMode = .auto,
};

/// Return true when the query has 4 or fewer whitespace-separated words.
/// Short queries use the fast path (no LLM calls).
fn isShortQuery(query: []const u8) bool {
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    var count: usize = 0;
    var in_word = false;
    for (trimmed) |c| {
        if (c == ' ' or c == '\t') {
            in_word = false;
        } else {
            if (!in_word) {
                count += 1;
                in_word = true;
            }
        }
    }
    return count <= 4;
}

fn cmdExplain(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var ea: ExplainArgs = .{};
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "-l") or std.mem.eql(u8, arg, "--limit")) {
            i += 1;
            if (i >= args.len) return;
            ea.limit = std.fmt.parseInt(usize, args[i], 10) catch 10;
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) return;
            ea.db_path = args[i];
        } else if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) return;
            ea.workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--api-url")) {
            i += 1;
            if (i >= args.len) return;
            ea.api_url = args[i];
        } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
            i += 1;
            if (i >= args.len) return;
            ea.model = args[i];
        } else if (std.mem.eql(u8, arg, "--no-llm")) {
            ea.no_llm = true;
        } else if (std.mem.eql(u8, arg, "--guidance")) {
            i += 1;
            if (i >= args.len) return;
            ea.guidance = args[i];
        } else if (std.mem.startsWith(u8, arg, "--staged=")) {
            const val = arg["--staged=".len..];
            ea.staged = !std.mem.eql(u8, val, "false");
        } else if (std.mem.eql(u8, arg, "--staged")) {
            ea.staged = true;
        } else if (std.mem.startsWith(u8, arg, "--filter=")) {
            const val = arg["--filter=".len..];
            ea.filter = std.meta.stringToEnum(FilterMode, val) orelse .auto;
        } else if (!std.mem.startsWith(u8, arg, "-")) {
            ea.query_str = arg;
        }
    }

    const query_text = ea.query_str orelse {
        std.debug.print("Error: query string required\n", .{});
        return;
    };

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const workspace = if (ea.workspace) |w|
        if (std.fs.path.isAbsolute(w)) try allocator.dupe(u8, w) else try std.fs.path.join(allocator, &.{ cwd, w })
    else
        try allocator.dupe(u8, cwd);
    defer allocator.free(workspace);

    const db_path = if (ea.db_path) |dp|
        if (std.fs.path.isAbsolute(dp)) try allocator.dupe(u8, dp) else try std.fs.path.join(allocator, &.{ workspace, dp })
    else
        try std.fs.path.join(allocator, &.{ workspace, config_mod.DEFAULT_DB_PATH });
    defer allocator.free(db_path);

    std.fs.accessAbsolute(db_path, .{}) catch {
        std.debug.print("Error: No .explain.db found at {s}\n", .{db_path});
        std.debug.print("Run 'explain-gen gen' to generate it.\n", .{});
        return;
    };

    var db = db_mod.ExplainDb.init(allocator, db_path) catch |err| {
        std.debug.print("Error opening database: {s}\n", .{@errorName(err)});
        return;
    };
    defer db.deinit();

    // ── Staged pipeline (default) ──────────────────────────────────────────────
    if (ea.staged) {
        const guidance_dir_staged = if (ea.guidance) |g|
            if (std.fs.path.isAbsolute(g)) try allocator.dupe(u8, g) else try std.fs.path.join(allocator, &.{ workspace, g })
        else
            try std.fs.path.join(allocator, &.{ workspace, config_mod.DEFAULT_GUIDANCE_DIR });
        defer allocator.free(guidance_dir_staged);

        const llm_config_staged: llm.LlmConfig = .{
            .api_url = ea.api_url,
            .model = ea.model,
            .debug = ea.verbose,
        };

        staged_path: {
            cmdExplainStaged(allocator, &db, query_text, workspace, guidance_dir_staged, llm_config_staged, ea) catch |err| {
                if (ea.verbose) std.debug.print("staged explain failed ({s}), falling back to legacy\n", .{@errorName(err)});
                break :staged_path;
            };
            return; // staged path completed successfully
        }
    }

    // ── Legacy path (--staged=false) ──────────────────────────────────────────
    const results = db.search(allocator, query_text, ea.limit) catch |err| {
        std.debug.print("Search failed: {s}\n", .{@errorName(err)});
        return;
    };
    defer {
        for (results) |r| db_mod.ExplainDb.freeSearchResult(allocator, r);
        allocator.free(results);
    }

    if (results.len == 0) {
        const lower_q = try std.ascii.allocLowerString(allocator, query_text);
        defer allocator.free(lower_q);
        std.debug.print("# Explain: {s}\n\nNot indexed for '{s}'. Search the source directly:\n\n", .{ query_text, query_text });
        std.debug.print("    grep -ri '{s}' src/ | head -n 20\n\n", .{lower_q});
        std.debug.print("Run 'explain-gen gen' after finding the file to index it.\n", .{});
        return;
    }

    // Build normalised search terms (lowercase tokens).
    var search_terms: std.ArrayList([]const u8) = .{};
    defer {
        for (search_terms.items) |t| allocator.free(t);
        search_terms.deinit(allocator);
    }
    {
        var tok = std.mem.tokenizeAny(u8, query_text, " \t_");
        while (tok.next()) |word| {
            if (word.len == 0) continue;
            try search_terms.append(allocator, try std.ascii.allocLowerString(allocator, word));
        }
        if (search_terms.items.len == 0)
            try search_terms.append(allocator, try std.ascii.allocLowerString(allocator, query_text));
    }

    // Guidance dir (for skill excerpts and inbox bullets).
    const guidance_dir = if (ea.guidance) |g|
        if (std.fs.path.isAbsolute(g)) try allocator.dupe(u8, g) else try std.fs.path.join(allocator, &.{ workspace, g })
    else
        try std.fs.path.join(allocator, &.{ workspace, config_mod.DEFAULT_GUIDANCE_DIR });
    defer allocator.free(guidance_dir);

    // ── PHASE A: Skill excerpts ───────────────────────────────────────────────
    var skill_excerpts: std.ArrayList(SkillExcerpt) = .{};
    defer {
        for (skill_excerpts.items) |se| {
            allocator.free(se.name);
            allocator.free(se.excerpt);
        }
        skill_excerpts.deinit(allocator);
    }
    // For the top result, load skills from its JSON guidance file.
    {
        var seen_skills: std.StringHashMapUnmanaged(void) = .{};
        defer seen_skills.deinit(allocator);

        const top = results[0];
        // top.file_path is the absolute path to the JSON guidance file.
        const json_path = top.file_path;

        if (loadSkillsFromJson(allocator, json_path)) |skills_json| {
            defer allocator.free(skills_json);
            var sp = std.mem.splitScalar(u8, skills_json, '\n');
            while (sp.next()) |skill_name_raw| {
                const skill_name = std.mem.trim(u8, skill_name_raw, " \t\r");
                if (skill_name.len == 0) continue;
                if (seen_skills.contains(skill_name)) continue;
                try seen_skills.put(allocator, skill_name, {});
                if (loadSkillPara(allocator, guidance_dir, workspace, skill_name)) |para| {
                    try skill_excerpts.append(allocator, .{
                        .name = try allocator.dupe(u8, skill_name),
                        .excerpt = para,
                    });
                }
            }
        }
    }

    // ── PHASE B: Source excerpts ──────────────────────────────────────────────
    var excerpts: std.ArrayList(ExcerptEntry) = .{};
    defer {
        for (excerpts.items) |e| {
            allocator.free(e.label);
            allocator.free(e.code);
        }
        excerpts.deinit(allocator);
    }

    // Collect up to 3 excerpts. Prefer exact name matches over test_decls.
    // Re-sort results slice by: exact-name-match first, then non-test, then score.
    var sorted_results: std.ArrayList(db_mod.ExplainDb.SearchResult) = .{};
    defer sorted_results.deinit(allocator);
    for (results) |r| try sorted_results.append(allocator, r);
    std.sort.insertion(db_mod.ExplainDb.SearchResult, sorted_results.items, search_terms.items, struct {
        fn lessThan(terms: []const []const u8, a: db_mod.ExplainDb.SearchResult, b: db_mod.ExplainDb.SearchResult) bool {
            const a_exact = isExactNameMatch(a.name, terms);
            const b_exact = isExactNameMatch(b.name, terms);
            if (a_exact != b_exact) return a_exact; // exact comes first
            const a_test = std.mem.eql(u8, a.node_type, "test_decl");
            const b_test = std.mem.eql(u8, b.node_type, "test_decl");
            if (a_test != b_test) return !a_test; // non-test comes first
            return a.score > b.score;
        }
    }.lessThan);

    var seen_excerpt_files: std.StringHashMapUnmanaged(void) = .{};
    defer seen_excerpt_files.deinit(allocator);

    for (sorted_results.items) |r| {
        if (excerpts.items.len >= 3) break;
        if (seen_excerpt_files.contains(r.source)) continue;
        if (r.source.len == 0) continue;

        const start_line = r.line orelse continue;
        const src_abs = try std.fs.path.join(allocator, &.{ workspace, r.source });
        defer allocator.free(src_abs);

        const src_opt: ?[]const u8 = blk: {
            const f = std.fs.openFileAbsolute(src_abs, .{}) catch break :blk null;
            defer f.close();
            break :blk f.readToEndAlloc(allocator, 10 * 1024 * 1024) catch null;
        };
        if (src_opt == null) continue;
        const src = src_opt.?;
        defer allocator.free(src);

        const end_line = start_line + 79;
        const code = try explainExtractExcerpt(allocator, src, start_line, end_line);
        if (code.len == 0) {
            allocator.free(code);
            continue;
        }
        const lang: []const u8 = if (std.mem.endsWith(u8, r.source, ".zig")) "zig" else if (std.mem.endsWith(u8, r.source, ".py")) "python" else "text";
        const label = try std.fmt.allocPrint(allocator, "{s}:{d}", .{ r.source, start_line });
        try excerpts.append(allocator, .{ .file_path = r.source, .label = label, .code = code, .lang = lang });
        try seen_excerpt_files.put(allocator, r.source, {});
    }

    // ── PHASE C: Grep for files with most matches ─────────────────────────────
    const FileMatchItem = struct { path: []const u8, count: usize, lines: []usize };
    var file_grep: std.ArrayList(FileMatchItem) = .{};
    defer {
        for (file_grep.items) |fm| allocator.free(fm.lines);
        file_grep.deinit(allocator);
    }
    var grep_seen: std.StringHashMapUnmanaged(void) = .{};
    defer grep_seen.deinit(allocator);

    for (results[0..@min(5, results.len)]) |r| {
        if (r.source.len == 0) continue;
        if (grep_seen.contains(r.source)) continue;
        try grep_seen.put(allocator, r.source, {});

        const abs = try std.fs.path.join(allocator, &.{ workspace, r.source });
        defer allocator.free(abs);

        const matches = try explainGrepFile(allocator, abs, search_terms.items, 10);
        if (matches.len > 0) {
            var lines_list: std.ArrayList(usize) = .{};
            for (matches) |ln| try lines_list.append(allocator, ln);
            allocator.free(matches);
            try file_grep.append(allocator, .{
                .path = r.source,
                .count = lines_list.items.len,
                .lines = try lines_list.toOwnedSlice(allocator),
            });
        } else {
            allocator.free(matches);
        }
    }
    // Sort descending by match count.
    std.sort.insertion(FileMatchItem, file_grep.items, {}, struct {
        fn less(_: void, a: FileMatchItem, b: FileMatchItem) bool {
            return a.count > b.count;
        }
    }.less);

    // ── PHASE D: LLM synthesis ────────────────────────────────────────────────
    var ai_summary: ?[]const u8 = null;
    defer if (ai_summary) |s| allocator.free(s);

    if (!ea.no_llm) {
        const llm_config: llm.LlmConfig = .{
            .api_url = ea.api_url,
            .model = ea.model,
            .debug = ea.verbose,
        };
        var client_opt = llm.LlmClient.init(allocator, llm_config) catch null;
        defer if (client_opt) |*c| c.deinit();

        if (client_opt) |*client| {
            ai_summary = buildLlmSummary(allocator, client, query_text, results, skill_excerpts.items, excerpts.items) catch null;
        }
    }

    // ── PHASE E: Output ───────────────────────────────────────────────────────
    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.print("# Explain: {s}\n\n", .{query_text});

    if (ai_summary) |s| {
        const trimmed_s = std.mem.trim(u8, s, " \t\n\r");
        if (trimmed_s.len > 0) try stdout.print("{s}\n\n", .{trimmed_s});
    }

    try stdout.print("---\n", .{});

    // Primary source reference.
    try stdout.print("**Source**: `{s}`\n", .{results[0].source});

    // Pattern / skill line.
    for (skill_excerpts.items[0..@min(2, skill_excerpts.items.len)]) |se| {
        const first_nl = std.mem.indexOfScalar(u8, se.excerpt, '\n') orelse se.excerpt.len;
        const first_line = se.excerpt[0..@min(first_nl, 120)];
        try stdout.print("**Pattern**: `{s}` — {s}\n", .{ se.name, first_line });
    }
    try stdout.print("\n", .{});

    // Code excerpts.
    for (excerpts.items) |e| {
        try stdout.print("```{s}\n// {s}\n{s}\n```\n\n", .{ e.lang, e.label, e.code });
    }

    // Keywords: public non-test members from the primary source file's JSON,
    // excluding names that are identical to a search term.
    {
        const top_src = results[0].source;
        var kw_buf: std.ArrayList(u8) = .{};
        defer kw_buf.deinit(allocator);
        var kw_count: usize = 0;

        if (top_src.len > 0) {
            // results[0].file_path is the absolute JSON guidance path.
            const kw_json_path = results[0].file_path;

            if (loadPublicMemberNames(allocator, kw_json_path)) |names| {
                defer {
                    for (names) |n| allocator.free(n);
                    allocator.free(names);
                }
                for (names) |mname| {
                    if (kw_count >= 8) break;
                    const mname_lower = try std.ascii.allocLowerString(allocator, mname);
                    defer allocator.free(mname_lower);
                    var is_term = false;
                    for (search_terms.items) |term| {
                        if (std.mem.eql(u8, mname_lower, term)) {
                            is_term = true;
                            break;
                        }
                    }
                    if (is_term) continue;
                    if (kw_count > 0) try kw_buf.appendSlice(allocator, ", ");
                    try kw_buf.writer(allocator).print("`{s}`", .{mname});
                    kw_count += 1;
                }
            }
        }
        if (kw_count > 0) try stdout.print("**Keywords**: {s}\n\n", .{kw_buf.items});
    }

    // See also: used_by from top result (or from JSON if member row), + secondary paths.
    {
        var see_buf: std.ArrayList(u8) = .{};
        defer see_buf.deinit(allocator);
        var see_count: usize = 0;

        // Gather used_by: prefer the result's own slice, fall back to loading from JSON.
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
        // Secondary results' file paths if still room.
        for (results[1..@min(results.len, 6)]) |r| {
            if (see_count >= 6) break;
            // Skip if same file as primary.
            if (std.mem.eql(u8, r.source, results[0].source)) continue;
            if (r.source.len == 0) continue;
            if (see_count > 0) try see_buf.appendSlice(allocator, ", ");
            try see_buf.writer(allocator).print("`{s}`", .{r.source});
            see_count += 1;
        }
        if (see_count > 0) try stdout.print("**See also**: {s}\n\n", .{see_buf.items});
    }

    // Files with most matches.
    if (file_grep.items.len > 0) {
        try stdout.print("### Files with most matches\n\n", .{});
        for (file_grep.items[0..@min(3, file_grep.items.len)]) |fm| {
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

// ---------------------------------------------------------------------------
// explain helpers
// ---------------------------------------------------------------------------

/// Load `used_by` array from a guidance JSON file.
/// Returns an owned slice of owned strings, or null on failure / empty.
fn loadUsedByFromJson(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    const f = std.fs.openFileAbsolute(json_path, .{}) catch return null;
    defer f.close();
    const content = f.readToEndAlloc(allocator, 8 * 1024 * 1024) catch return null;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    const ub_val = parsed.value.object.get("used_by") orelse return null;
    if (ub_val != .array) return null;

    var out: std.ArrayList([]const u8) = .{};
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

/// Return true when `name` (case-insensitive) exactly equals any search term.
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

/// Load skills listed in a guidance JSON file as a newline-separated string.
/// Returns an owned allocation or null if the file is absent or has no skills.
fn loadSkillsFromJson(allocator: std.mem.Allocator, json_path: []const u8) ?[]const u8 {
    const f = std.fs.openFileAbsolute(json_path, .{}) catch return null;
    defer f.close();
    const content = f.readToEndAlloc(allocator, 8 * 1024 * 1024) catch return null;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    const skills_val = parsed.value.object.get("skills") orelse return null;
    if (skills_val != .array) return null;

    var out: std.ArrayList(u8) = .{};
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
        // e.g. ".skills/gof-patterns/SKILL.md" → "gof-patterns"
        const base = std.fs.path.basename(ref);
        const skill_name: []const u8 = if (std.mem.eql(u8, base, "SKILL.md")) blk: {
            const dir = std.fs.path.dirname(ref) orelse break :blk base;
            break :blk std.fs.path.basename(dir);
        } else base;
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

/// Load public non-test member names from a guidance JSON file.
/// Returns an owned slice of owned strings, or null on failure.
fn loadPublicMemberNames(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    const f = std.fs.openFileAbsolute(json_path, .{}) catch return null;
    defer f.close();
    const content = f.readToEndAlloc(allocator, 8 * 1024 * 1024) catch return null;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    const members_val = parsed.value.object.get("members") orelse return null;
    if (members_val != .array) return null;

    var out: std.ArrayList([]const u8) = .{};
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

/// Load the first paragraph (or `description:` front-matter value) of a SKILL.md.
/// Searches `<guidance_dir>/.skills/<name>/SKILL.md` and `<cwd>/doc/skills/<name>/SKILL.md`.
/// Returns an owned allocation or null if not found.
fn loadSkillPara(
    allocator: std.mem.Allocator,
    guidance_dir: []const u8,
    cwd: []const u8,
    skill_name: []const u8,
) ?[]const u8 {
    const SearchPath = struct { base: []const u8, rel: []const u8 };
    const paths = [_]SearchPath{
        .{ .base = guidance_dir, .rel = ".skills" },
        .{ .base = cwd, .rel = "doc/skills" },
    };
    for (paths) |sp| {
        const path = std.fs.path.join(allocator, &.{ sp.base, sp.rel, skill_name, "SKILL.md" }) catch continue;
        defer allocator.free(path);
        const sf = std.fs.openFileAbsolute(path, .{}) catch continue;
        defer sf.close();
        const content = sf.readToEndAlloc(allocator, 512 * 1024) catch continue;
        defer allocator.free(content);

        if (std.mem.startsWith(u8, content, "---\n")) {
            // YAML front matter — look for `description:`.
            const fm_close = std.mem.indexOf(u8, content[4..], "\n---\n") orelse {
                var lines = std.mem.splitScalar(u8, content[4..], '\n');
                while (lines.next()) |line| {
                    const t = std.mem.trim(u8, line, " \t\r");
                    if (t.len > 0 and !std.mem.eql(u8, t, "---"))
                        return allocator.dupe(u8, t[0..@min(t.len, 200)]) catch null;
                }
                return null;
            };
            const fm_body = content[4 .. 4 + fm_close];
            var fm_lines = std.mem.splitScalar(u8, fm_body, '\n');
            while (fm_lines.next()) |fl| {
                if (std.mem.startsWith(u8, fl, "description:")) {
                    const val = std.mem.trim(u8, fl["description:".len..], " \t\r");
                    if (val.len > 0) return allocator.dupe(u8, val[0..@min(val.len, 200)]) catch null;
                }
            }
            // No description: — return first non-empty body line.
            const after_fm = content[4 + fm_close + 5 ..];
            var body = std.mem.splitScalar(u8, after_fm, '\n');
            while (body.next()) |bl| {
                const t = std.mem.trim(u8, bl, " \t\r");
                if (t.len > 0) return allocator.dupe(u8, t[0..@min(t.len, 200)]) catch null;
            }
            return null;
        }
        // No front matter — first paragraph (up to blank line), max 600 chars.
        const para_end = std.mem.indexOf(u8, content, "\n\n") orelse content.len;
        return allocator.dupe(u8, content[0..@min(para_end, 600)]) catch null;
    }
    return null;
}

/// Extract the source block starting at `start_line` (1-based).
/// Stops at the next col-0 top-level declaration or after MAX_LINES, whichever
/// comes first.  Then prunes trailing blank and comment-only lines.
/// Returns an owned allocation; caller must free.
fn explainExtractExcerpt(
    allocator: std.mem.Allocator,
    src: []const u8,
    start_line: u32,
    _end_line: u32, // advisory upper bound; real stop is next col-0 decl
) ![]const u8 {
    _ = _end_line;
    const MAX_LINES: usize = 80;

    var lines: std.ArrayList([]const u8) = .{};
    defer lines.deinit(allocator);

    var iter = std.mem.splitScalar(u8, src, '\n');
    var line_no: u32 = 0;
    var saw_close_brace = false;

    while (iter.next()) |raw| {
        line_no += 1;
        if (line_no < start_line) continue;
        if (lines.items.len >= MAX_LINES) break;

        const trimmed = std.mem.trimRight(u8, raw, "\r");
        const is_first = line_no == start_line;

        // Stop at next col-0 top-level declaration (not on the very first line).
        if (!is_first and trimmed.len > 0 and trimmed[0] != ' ' and trimmed[0] != '\t') {
            if (std.mem.startsWith(u8, trimmed, "pub ") or
                std.mem.startsWith(u8, trimmed, "fn ") or
                std.mem.startsWith(u8, trimmed, "const ") or
                std.mem.startsWith(u8, trimmed, "var ") or
                std.mem.startsWith(u8, trimmed, "test ") or
                std.mem.startsWith(u8, trimmed, "// =") or
                std.mem.startsWith(u8, trimmed, "// -") or
                (saw_close_brace and std.mem.startsWith(u8, trimmed, "//")) or
                (saw_close_brace and trimmed.len == 0))
            {
                break;
            }
        }
        // Track col-0 closing brace.
        if (!is_first and std.mem.eql(u8, std.mem.trim(u8, trimmed, " \t"), "};")) {
            saw_close_brace = true;
        }
        // Skip separator banners.
        if (std.mem.startsWith(u8, std.mem.trimLeft(u8, trimmed, " \t"), "// ---")) continue;

        try lines.append(allocator, trimmed);
    }
    if (lines.items.len == 0) return allocator.dupe(u8, "");

    // Prune trailing blank then trailing comment-only lines (stable loop).
    var changed = true;
    while (changed) {
        changed = false;
        while (lines.items.len > 0 and std.mem.trim(u8, lines.items[lines.items.len - 1], " \t\r").len == 0) {
            _ = lines.pop();
            changed = true;
        }
        while (lines.items.len > 0 and std.mem.startsWith(u8, std.mem.trimLeft(u8, lines.items[lines.items.len - 1], " \t"), "//")) {
            _ = lines.pop();
            changed = true;
        }
    }
    if (lines.items.len == 0) return allocator.dupe(u8, "");

    var buf: std.ArrayList(u8) = .{};
    errdefer buf.deinit(allocator);
    for (lines.items, 0..) |line, idx| {
        if (idx > 0) try buf.append(allocator, '\n');
        try buf.appendSlice(allocator, line);
    }
    return buf.toOwnedSlice(allocator);
}

/// Grep a file for search terms (case-insensitive substring, skipping comment lines).
/// Returns owned slice of matching line numbers (caller frees).
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

    var line_numbers: std.ArrayList(usize) = .{};
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

/// Build LLM synthesis: skill context + member index + excerpts → prompt → summary.
/// Strips "absence" sentences.  Returns owned string or null on failure.
fn buildLlmSummary(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query_text: []const u8,
    results: []const db_mod.ExplainDb.SearchResult,
    skill_excerpts_in: []const SkillExcerpt,
    excerpts_in: []const ExcerptEntry,
) !?[]const u8 {
    var kb: std.ArrayList(u8) = .{};
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
    var skill_names_buf: std.ArrayList(u8) = .{};
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

    // Strip absence sentences.
    const absence_kws = [_][]const u8{
        "no other",       "not present",  "only has", "does not contain",
        "does not exist", "nothing else", "none are", "none were",
    };
    var out_buf: std.ArrayList(u8) = .{};
    var line_it = std.mem.splitScalar(u8, raw, '\n');
    while (line_it.next()) |line| {
        const lower = try std.ascii.allocLowerString(allocator, line);
        defer allocator.free(lower);
        var is_absence = false;
        for (absence_kws) |kw| {
            if (std.mem.indexOf(u8, lower, kw) != null) {
                is_absence = true;
                break;
            }
        }
        if (!is_absence) try out_buf.writer(allocator).print("{s}\n", .{line});
    }
    const result = out_buf.toOwnedSlice(allocator) catch return null;
    const trimmed_result = std.mem.trim(u8, result, " \t\n\r");
    if (trimmed_result.len == 0) {
        allocator.free(result);
        return null;
    }
    // Return a dupe of the trimmed slice (result may have trailing whitespace).
    const final = try allocator.dupe(u8, trimmed_result);
    allocator.free(result);
    return final;
}

// =============================================================================
// Staged explain implementation  (M3/M5-M9)
// =============================================================================

/// Full staged explain pipeline.  Called when `--staged` is active (default).
///
/// Pipeline:
///   Short query (≤4 words) or --no-llm or --filter=skip:
///     executeStaged() → formatStaged() → output
///   Long query (5+ words) with LLM:
///     executeStaged() → llmFilter() → expandFollowUps() → synthesize() → formatStaged() → output
fn cmdExplainStaged(
    allocator: std.mem.Allocator,
    db: *db_mod.ExplainDb,
    query_text: []const u8,
    workspace: []const u8,
    guidance_dir: []const u8,
    llm_config: llm.LlmConfig,
    ea: ExplainArgs,
) !void {
    const skills_dir = try std.fs.path.join(allocator, &.{ guidance_dir, ".skills" });
    defer allocator.free(skills_dir);

    // ── Stage collection ──────────────────────────────────────────────────────
    const stages_raw = try staged_mod.executeStaged(
        allocator,
        db,
        query_text,
        workspace,
    );
    defer {
        types.freeStages(allocator, stages_raw);
        allocator.free(stages_raw);
    }

    if (stages_raw.len == 0) {
        const lower_q = try std.ascii.allocLowerString(allocator, query_text);
        defer allocator.free(lower_q);
        std.debug.print("# Explain: {s}\n\nNot indexed for '{s}'. Search the source directly:\n\n", .{ query_text, query_text });
        std.debug.print("    grep -ri '{s}' src/ | head -n 20\n\n", .{lower_q});
        std.debug.print("Run 'explain-gen gen' after finding the file to index it.\n", .{});
        return;
    }

    // Determine whether to use LLM.
    const use_llm = !ea.no_llm and switch (ea.filter) {
        .skip => false,
        .force => true,
        .auto => !isShortQuery(query_text),
    };

    var summary: ?[]const u8 = null;
    defer if (summary) |s| allocator.free(s);

    var stages_final: []types.Stage = undefined;
    var stages_filtered_alloc: ?[]types.Stage = null;
    defer if (stages_filtered_alloc) |sf| {
        types.freeStages(allocator, sf);
        allocator.free(sf);
    };
    var stages_expanded_alloc: ?[]types.Stage = null;
    defer if (stages_expanded_alloc) |se| {
        types.freeStages(allocator, se);
        allocator.free(se);
    };

    if (use_llm) {
        // ── LLM path ─────────────────────────────────────────────────────────
        var client_opt = llm.LlmClient.init(allocator, llm_config) catch null;
        defer if (client_opt) |*c| c.deinit();

        if (client_opt) |*client| {
            // M6: LLM relevance filter.
            const filtered = llm_filter_mod.filterStages(allocator, client, query_text, stages_raw) catch blk: {
                if (ea.verbose) std.debug.print("llm_filter failed, using unfiltered stages\n", .{});
                break :blk null;
            };

            const working_stages: []const types.Stage = if (filtered) |f| blk: {
                stages_filtered_alloc = f;
                break :blk f;
            } else stages_raw;

            // M7: Follow-up expansion.
            // Collect inputs: file_paths, sources, used_by from current working stages.
            // We re-search the db briefly to get used_by lists.
            const expansion_results = db.search(allocator, query_text, 5) catch &.{};
            defer {
                for (expansion_results) |r| db_mod.ExplainDb.freeSearchResult(allocator, r);
                allocator.free(expansion_results);
            }

            var fp_list: std.ArrayList([]const u8) = .{};
            defer fp_list.deinit(allocator);
            var src_list: std.ArrayList([]const u8) = .{};
            defer src_list.deinit(allocator);
            var ub_list: std.ArrayList([]const []const u8) = .{};
            defer ub_list.deinit(allocator);

            for (expansion_results) |r| {
                try fp_list.append(allocator, r.file_path);
                try src_list.append(allocator, r.source);
                try ub_list.append(allocator, r.used_by);
            }

            // Collect already-seen sources to avoid duplicates.
            var existing_srcs: std.ArrayList([]const u8) = .{};
            defer existing_srcs.deinit(allocator);
            for (working_stages) |s| {
                if (s.kind == .code or s.kind == .prose) {
                    try existing_srcs.append(allocator, s.source);
                }
            }

            const extra_stages = staged_mod.expandFollowUps(
                allocator,
                fp_list.items,
                src_list.items,
                ub_list.items,
                workspace,
                guidance_dir,
                skills_dir,
                existing_srcs.items,
                6, // limit extra stages
            ) catch &.{};
            stages_expanded_alloc = @constCast(extra_stages);

            // Build combined working + extra stages view (no alloc, just a concatenated list).
            var combined: std.ArrayList(types.Stage) = .{};
            defer combined.deinit(allocator);
            for (working_stages) |s| try combined.append(allocator, s);
            for (extra_stages) |s| try combined.append(allocator, s);

            // M8: LLM synthesis.
            summary = synthesize_mod.synthesize(allocator, client, query_text, combined.items) catch null;

            // Use combined as final display set.
            // We can't deinit combined here since we need it for formatStaged;
            // so transfer ownership to a separate slice.
            const combined_slice = try combined.toOwnedSlice(allocator);
            stages_final = combined_slice;
            // Note: combined_slice borrows from working_stages + extra_stages (which are deferred-freed above),
            // so we must NOT free individual Stage strings in combined_slice. We just free the slice itself.
            defer allocator.free(combined_slice);

            const output = try staged_mod.formatStaged(allocator, query_text, stages_final, summary, workspace);
            defer allocator.free(output);

            var ws: llm.WriterState = .{};
            ws.initStdout();
            const stdout = ws.writer();
            try stdout.writeAll(output);
            try stdout.flush();
            return;
        }
        // LLM client init failed — fall through to fast path.
        if (ea.verbose) std.debug.print("LLM unavailable, using fast path\n", .{});
    }

    // ── Fast path: format stages directly without LLM ─────────────────────────
    const output = try staged_mod.formatStaged(allocator, query_text, stages_raw, null, workspace);
    defer allocator.free(output);

    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();
    try stdout.writeAll(output);
    try stdout.flush();
}

// =============================================================================
// Tests
// =============================================================================

test "main compiles" {
    // Compilation smoke test — ensures all imports resolve.
    _ = types.GuidanceDoc;
    _ = types.FileType;
    _ = sync_mod.SyncProcessor;
    _ = db_mod.ExplainDb;
    _ = plugin_mod.LanguagePlugin;
    _ = plugin_registry.PluginRegistry;
}
