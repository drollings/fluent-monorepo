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

pub const version = "0.1.0";

const Command = enum {
    gen,
    status,
    clean,
    structure,
    deps,
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
