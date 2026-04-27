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
//!
//! ## Memory Ownership
//!
//!   - main() creates a DebugAllocator and passes the allocator to all subcommands.
//!     All subcommand allocations are scoped to the command lifetime.
//!   - No module-level state; all data flows through function parameters.
//!   - stdout writes use buffered std.Io.Writer with explicit flush before return.

const std = @import("std");
const types = @import("types.zig");
const structure_mod = @import("structure.zig");
const config_mod = @import("config.zig");
const sync_engine_mod = @import("sync_engine.zig");
const query_engine_mod = @import("query_engine.zig");
const codehealth_mod = @import("codehealth/main.zig");
const core_intent_mod = @import("core/intent.zig");
const core_ranking_mod = @import("core/ranking.zig");
const core_excerpt_mod = @import("core/excerpt.zig");
const core_skill_loader_mod = @import("core/skill_loader.zig");
const core_metadata_mod = @import("core/metadata.zig");
const common = @import("common");

pub const version = "0.1.0";

var verbose_mode: bool = false;
var debug_mode: bool = false;

pub const std_options: std.Options = .{
    .logFn = struct {
        fn log(
            comptime level: std.log.Level,
            comptime scope: @EnumLiteral(),
            comptime format: []const u8,
            args: anytype,
        ) void {
            if (level == .debug and !debug_mode) return;
            std.log.defaultLog(level, scope, format, args);
        }
    }.log,
};

const Command = enum {
    init,
    gen,
    status,
    clean,
    structure,
    explain,
    commit,
    check,
    show,
    @"test",
    todo,
    diary,
    telemetry,
    @"cache-stats",
    serve,
    ralph,
    scan,
    codehealth,
};

/// Starts the Zig program execution by defining the entry point.
pub fn main(init: std.process.Init.Minimal) !void {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    // Collect args into owned slice (replaces the removed std.process.argsAlloc)
    var args_list: std.ArrayList([]const u8) = .empty;
    defer {
        for (args_list.items) |a| allocator.free(a);
        args_list.deinit(allocator);
    }
    {
        var iter = try init.args.iterateAllocator(allocator);
        defer iter.deinit();
        while (iter.next()) |arg| try args_list.append(allocator, try allocator.dupe(u8, arg));
    }
    const args = args_list.items;

    for (args) |arg| {
        if (std.mem.eql(u8, arg, "--verbose")) {
            verbose_mode = true;
        } else if (std.mem.eql(u8, arg, "--debug")) {
            debug_mode = true;
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
        .explain => query_engine_mod.cmdExplain(allocator, args[2..]),
        .commit => sync_engine_mod.cmdCommit(allocator, args[2..]),
        .check => sync_engine_mod.cmdCheck(allocator, args[2..]),
        .show => query_engine_mod.cmdShow(allocator, args[2..]),
        .@"test" => query_engine_mod.cmdTest(allocator, args[2..]),
        .todo => sync_engine_mod.cmdTodo(allocator, args[2..]),
        .diary => sync_engine_mod.cmdDiary(allocator, args[2..]),
        .telemetry => query_engine_mod.cmdTelemetry(allocator, args[2..]),
        .@"cache-stats" => query_engine_mod.cmdCacheStats(allocator, args[2..]),
        .serve => query_engine_mod.cmdServe(allocator, args[2..]),
        .ralph => cmdRalph(allocator, args[2..]),
        .scan => @import("scanner.zig").cmdScan(allocator, args[2..]),
        .codehealth => codehealth_mod.cmdCodehealth(allocator, args[2..]),
    };
    run_result catch |err| switch (err) {
        error.LintFailed, error.TestFailed => std.process.exit(1),
        else => return err,
    };
}

/// Processes a Zig command string, validating arguments and preparing execution.
fn cmdRalph(allocator: std.mem.Allocator, args: []const []const u8) !void {
    return query_engine_mod.cmdRalph(allocator, args);
}

/// Displays usage instructions for the Zig library's help system.
fn printHelp() !void {
    var ws: common.WriterState = .{};
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
        \\  explain    Search with LLM-synthesized summary (use --no-llm for raw results)
        \\  check           Run full RALPH loop (test → lint → fmt → guidance → structure)
        \\  commit          Generate AI commit message from staged diff + guidance
        \\  show            Show vector embeddings from .guidance.db (Markdown)
        \\  test            Benchmark explain queries against module-level comments
        \\  todo             Work item lifecycle (new|triage|checklist|status|list|abandon)
        \\  diary            Append a timestamped entry to the current work item DIARY.md
        \\  codehealth       Detect unused modules, redundant code, and dead code candidates
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
        \\  --verbose             Show LLM metadata (api calls, responses)
        \\  --show-prompts        Show LLM prompts in output (separate from --verbose)
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
        \\  --no-llm               Skip AI infill pre-pass
        \\  --api-url URL         LLM endpoint
        \\  -m, --model NAME      Model for AI infill
        \\
        \\Deps options:
        \\  --src DIR             Source directory to scan (default: src)
        \\
        \\Check options:
        \\  --dry-run             Show what would change without writing (skips tests, structure)
        \\  --skip-tests          Skip test suite phase
        \\  --skip-lint           Skip lint phase
        \\  --skip-fmt            Skip format phase
        \\  --no-structure        Skip STRUCTURE.md generation
        \\  --force               Re-process all files, ignoring freshness markers
        \\  --verbose             Show LLM metadata (api calls, responses)
        \\  --timeout N           Sleep N seconds after each file (default: 2, set to 0 to disable)
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
        \\  guidance commit
        \\  guidance check
        \\  guidance codehealth
        \\  guidance codehealth --min-age=90 --format=json
        \\  guidance codehealth --simhash-threshold=2
        \\  guidance codehealth --extract-calls
        \\
        \\Codehealth options:
        \\  --min-age=N            Minimum days since modification (default: 30)
        \\  --format=ai            AI-optimized markdown (default)
        \\  --format=human         Human-readable summary
        \\  --format=json          JSON for scripting
        \\  --simhash-threshold=N  Max Hamming distance for redundancy (default: 3)
        \\  --extract-calls        Enable Phase 2b call graph extraction (expensive)
        \\  --db PATH              Database path (default: .guidance.db)
        \\
    );
    try stdout.flush();
}

// =============================================================================
// structure — thin wrapper (delegates to structure_mod)
// =============================================================================

/// Transforms a C string into a Zig-safe structure using an allocator.
fn cmdStructure(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var json_dir_arg: ?[]const u8 = null;
    var no_llm: bool = false;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--json-dir") or std.mem.eql(u8, arg, "--guidance")) {
            i += 1;
            if (i >= args.len) return;
            json_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "--no-llm")) {
            no_llm = true;
        }
    }
    const _no_llm = no_llm;
    _ = _no_llm;

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const json_dir = try common.resolvePath(allocator, cwd, json_dir_arg orelse config_mod.DEFAULT_GUIDANCE_DIR);
    defer allocator.free(json_dir);

    var gen = structure_mod.StructureGenerator.init(allocator, cwd, json_dir, false);
    defer gen.deinit();
    try gen.generate();
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
pub const isExactNameMatchPub = core_ranking_mod.isExactNameMatch;
pub const loadSkillsFromJsonPub = core_metadata_mod.loadSkillsFromJson;
pub const loadUsedByFromJsonPub = core_metadata_mod.loadUsedByFromJson;
pub const loadPublicMemberNamesPub = core_metadata_mod.loadPublicMemberNames;
pub const loadSkillParaPub = core_skill_loader_mod.loadSkillExcerpt;
pub const explainExtractExcerptPub = core_excerpt_mod.extractFromSource_legacy;
pub const explainGrepFilePub = core_excerpt_mod.grepFile;
pub const isShortQueryPub = core_intent_mod.isShortQuery;
