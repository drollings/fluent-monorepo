//! guidance — AST-guided SQLite vector search database generator.
//!
//! Produces:
//!   .guidance/src/**/*.json  — Per-file structured metadata mirror
//!   .guidance.db              — SQLite cosine-similarity database consumed by NullClaw's explain tool
//!
//! Usage:
//!   guidance gen      [options]   Generate JSON + compile .guidance.db
//!   guidance status   [options]   Report generation status
//!   guidance clean    [options]   Remove .guidance/ and .guidance.db
//!   guidance structure [options]  Update STRUCTURE.md from guidance JSON
//!   guidance deps     [options]   Generate Makefile .depend file

const std = @import("std");
const types = @import("types.zig");
const structure_mod = @import("structure.zig");
const deps_mod = @import("deps.zig");
const config_mod = @import("config.zig");
const sync_engine_mod = @import("sync_engine.zig");
const query_engine_mod = @import("query_engine.zig");
const llm = @import("common");

pub const version = "0.1.0";

/// Global verbose flag — set by `--verbose` / `--debug` anywhere in argv.
/// Sub-module logFn reads this via the root std_options closure below.
var verbose_mode: bool = false;

/// Custom log implementation — filters debug messages based on verbose flag.
pub const std_options: std.Options = .{
    .logFn = struct {
        fn log(
            comptime level: std.log.Level,
            comptime scope: @Type(.enum_literal),
            comptime format: []const u8,
            args: anytype,
        ) void {
            if (level == .debug and !verbose_mode) return;
            std.log.defaultLog(level, scope, format, args);
        }
    }.log,
};

/// Defines a command type for managing Zig keywords, managing ownership and invariants in the compilation pipeline.
const Command = enum {
    init,
    gen,
    status,
    clean,
    structure,
    deps,
    explain,
    commit,
    check,
    show,
    @"test",
    scrub,
    todo,
    diary,
    telemetry,
    @"cache-stats",
    serve,
    ralph,
    scan,
};

/// Starts the Zig program execution by defining the entry point.
pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const args = try std.process.argsAlloc(allocator);
    defer std.process.argsFree(allocator, args);

    // Check for --verbose flag anywhere in args (global flag)
    for (args) |arg| {
        if (std.mem.eql(u8, arg, "--verbose") or std.mem.eql(u8, arg, "--debug")) {
            verbose_mode = true;
            break;
        }
    }

    if (args.len < 2) {
        try printHelp();
        return;
    }

    if (std.mem.eql(u8, args[1], "-h") or std.mem.eql(u8, args[1], "--help")) {
        try printHelp();
        return;
    }
    if (std.mem.eql(u8, args[1], "-v") or std.mem.eql(u8, args[1], "--version")) {
        std.debug.print("guidance v{s}\n", .{version});
        return;
    }

    const subcmd = std.meta.stringToEnum(Command, args[1]) orelse {
        std.debug.print("Unknown subcommand: {s}\n\n", .{args[1]});
        try printHelp();
        return;
    };

    // Pipeline-failure errors (LintFailed, TestFailed) are expected failures
    // that have already printed their diagnostics.  Propagating them through
    // !void main produces a confusing stack trace; exit(1) is cleaner.
    const run_result = switch (subcmd) {
        .init => sync_engine_mod.cmdInit(allocator, args[2..]),
        .gen => sync_engine_mod.cmdGen(allocator, args[2..]),
        .status => sync_engine_mod.cmdStatus(allocator, args[2..]),
        .clean => sync_engine_mod.cmdClean(allocator, args[2..]),
        .structure => cmdStructure(allocator, args[2..]),
        .deps => cmdDeps(allocator, args[2..]),
        .explain => query_engine_mod.cmdExplain(allocator, args[2..]),
        .commit => sync_engine_mod.cmdCommit(allocator, args[2..]),
        .check => sync_engine_mod.cmdCheck(allocator, args[2..]),
        .show => query_engine_mod.cmdShow(allocator, args[2..]),
        .@"test" => query_engine_mod.cmdTest(allocator, args[2..]),
        .scrub => sync_engine_mod.cmdScrub(allocator, args[2..]),
        .todo => sync_engine_mod.cmdTodo(allocator, args[2..]),
        .diary => sync_engine_mod.cmdDiary(allocator, args[2..]),
        .telemetry => query_engine_mod.cmdTelemetry(allocator, args[2..]),
        .@"cache-stats" => query_engine_mod.cmdCacheStats(allocator, args[2..]),
        .serve => query_engine_mod.cmdServe(allocator, args[2..]),
        .ralph => cmdRalph(allocator, args[2..]),
        .scan => @import("scanner.zig").cmdScan(allocator, args[2..]),
    };
    run_result catch |err| switch (err) {
        error.LintFailed, error.TestFailed => std.process.exit(1),
        else => return err,
    };
}

/// M6: RALPH loop — run a single query through Read→Ask→Learn→Plan stages.
fn cmdRalph(allocator: std.mem.Allocator, args: []const []const u8) !void {
    return query_engine_mod.cmdRalph(allocator, args);
}

fn printHelp() !void {
    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.writeAll(
        \\guidance v0.1.0 — AST-guided SQLite vector search database generator
        \\
        \\Produces .guidiance/src/**/*.json and .guidance.db for NullClaw.
        \\
        \\Usage:
        \\  guidance <command> [options]
        \\  guidance --help | --version
        \\
        \\Commands:
        \\  init       Create default configuration (AGENTS.md integration)
        \\  gen        Generate .guidance/ JSON mirror and .guidance.db
        \\  status     Show generation status (synced, stale, missing)
        \\  clean      Remove .guidance/src and .guidance.db
        \\  structure  Regenerate STRUCTURE.md from guidance JSON
        \\  deps       Generate Makefile .depend file from Zig imports
        \\  explain    Search with LLM-synthesized summary (use --no-llm for raw results)
        \\  check           Run full RALPH loop (test → lint → fmt → guidance → structure)
        \\  commit          Generate AI commit message from staged diff + guidance
        \\  show            Show vector embeddings from .guidance.db (Markdown)
        \\  test            Benchmark explain queries against module-level comments
        \\  sync-comments    Insert/update /// doc comments in Zig source files
        \\  scrub            Blank synthetic LLM-generated comments in guidance JSON files
        \\  todo             Work item lifecycle (new|triage|checklist|status|list|abandon)
        \\  diary            Append a timestamped entry to the current work item DIARY.md
        \\
        \\Init options:
        \\  -g, --guidance-dir DIR   Guidance directory (default: .guidance)
        \\  -o, --db PATH            Database path (default: .guidance.db)
        \\
        \\Gen options:
        \\  --file FILE           Process a single source file (incremental)
        \\  --scan DIR            Process all source files under DIR
        \\  -w, --workspace DIR   Source root directory (default: current directory)
        \\  --guidance-dir DIR    Guidance directory (default: .guidance)
        \\  -o, --db PATH         SQLite database path (default: .guidance.db)
        \\  --no-db               Skip database compilation step
        \\  --regen               LLM-regenerate all comments
        \\  --timeout N           Sleep N seconds after each file (default: 20, set to 0 to disable)
        \\  --dry-run             Show what would change without writing
        \\  --verbose             Print LLM prompts and raw responses
        \\  --api-url URL         LLM API endpoint (default: http://localhost:11434/v1/chat/completions)
        \\  -m, --model NAME      Model name (default: code:latest)
        \\
        \\Explain options:
        \\  <query>              Search query (required)
        \\  -l, --limit N         Max results (default: 10)
        \\  --json                Output JSON (query only)
        \\  -o, --db PATH         Database path (default: .guidance.db)
        \\  -w, --workspace DIR   Workspace root (default: current directory)
        \\  --guidance-dir DIR    Guidance directory (default: .guidance)
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
        \\  --guidance-dir DIR    Guidance JSON directory (default: .guidance)
        \\  --no-ai               Skip AI infill pre-pass
        \\  --api-url URL         LLM endpoint
        \\  -m, --model NAME      Model for AI infill
        \\
        \\Deps options:
        \\  --src DIR             Source directory to scan (default: src)
        \\
        \\Examples:
        \\  guidance init
        \\  guidance gen
        \\  guidance gen --file src/main.zig
        \\  guidance gen --scan src
        \\  guidance gen -o /tmp/project.guidance.db
        \\  guidance query "hash function"
        \\  guidance explain "how does the sync processor work" --limit 5
        \\  guidance show --filter=keywords
        \\  guidance clean
        \\  guidance structure
        \\  guidance deps --src src > zig.depend
        \\  guidance commit
        \\  guidance check
        \\
    );
    try stdout.flush();
}

// =============================================================================
// structure — thin wrapper (delegates to structure_mod)
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
    const _no_ai = no_ai;
    _ = _no_ai;

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const json_dir = try llm.resolvePath(allocator, cwd, json_dir_arg orelse config_mod.DEFAULT_GUIDANCE_DIR);
    defer allocator.free(json_dir);

    var gen = structure_mod.StructureGenerator.init(allocator, cwd, json_dir, false);
    defer gen.deinit();
    try gen.generate();
}

// =============================================================================
// deps — thin wrapper (delegates to deps_mod)
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
// Re-exports for tests.zig (public wrapper surface)
// =============================================================================

pub const parseHunkRangesPub = sync_engine_mod.parseHunkRangesPub;
pub const loadChangedMembersPub = sync_engine_mod.loadChangedMembersPub;
pub const chunkIsIgnoredPub = sync_engine_mod.chunkIsIgnoredPub;
pub const chunkFilePathPub = sync_engine_mod.chunkFilePathPub;
pub const splitDiffByFilePub = sync_engine_mod.splitDiffByFilePub;
pub const reportCapabilityLifecyclePub = sync_engine_mod.reportCapabilityLifecycle;
pub const CapabilityEntryPub = sync_engine_mod.CapabilityEntry;
pub const isExactNameMatchPub = query_engine_mod.isExactNameMatchPub;
pub const loadSkillsFromJsonPub = query_engine_mod.loadSkillsFromJsonPub;
pub const loadUsedByFromJsonPub = query_engine_mod.loadUsedByFromJsonPub;
pub const loadPublicMemberNamesPub = query_engine_mod.loadPublicMemberNamesPub;
pub const loadSkillParaPub = query_engine_mod.loadSkillParaPub;
pub const explainExtractExcerptPub = query_engine_mod.explainExtractExcerptPub;
pub const explainGrepFilePub = query_engine_mod.explainGrepFilePub;
pub const isShortQueryPub = query_engine_mod.isShortQueryPub;

