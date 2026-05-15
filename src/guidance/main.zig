//! guidance — AST-guided SQLite vector search database generator.
//!
//! Produces:
//!   .guidance/src/**/*.json  — Per-file structured metadata mirror
//!   .guidance.db              — SQLite cosine-similarity database consumed by NullClaw's explain tool
//!
//! Usage:
//!   guidance <command> [options]
//!   guidance --help | --version
//!
//! ## Memory Ownership
//!
//!   - main() creates a DebugAllocator and passes the allocator to all subcommands.
//!     All subcommand allocations are scoped to the command lifetime.
//!   - No module-level state; all data flows through function parameters.
//!   - stdout writes use buffered std.Io.Writer with explicit flush before return.

const std = @import("std");
const clap = @import("clap");
const types = @import("types.zig");
const structure_mod = @import("structure.zig");
const config_mod = @import("config.zig");
const sync_engine_mod = @import("sync_engine.zig");
const query_engine_mod = @import("query_engine.zig");
const health_mod = @import("health/health.zig");
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
    clean,
    explain,
    todo,
    commit,
    serve,
    telemetry,
    benchmark,
    health,
};

const main_params = clap.parseParamsComptime(
    \\-h, --help     Display this help and exit.
    \\-v, --version  Show version and exit.
    \\<str>
    \\
);

/// Starts the Zig program execution by defining the entry point.
pub fn main(init: std.process.Init) !void {
    // Inject the runtime-provided Io into the common io layer so that
    // std.process.run / spawn work correctly.
    common.io.setGlobalIo(init.io);

    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var iter = try init.minimal.args.iterateAllocator(allocator);
    defer iter.deinit();
    _ = iter.next(); // skip exe name

    var diag = clap.Diagnostic{};
    var res = clap.parseEx(clap.Help, &main_params, clap.parsers.default, &iter, .{
        .diagnostic = &diag,
        .allocator = allocator,
        .terminating_positional = 0,
    }) catch |err| {
        diag.reportToFile(init.io, .stderr(), err) catch {};
        std.process.exit(1);
    };
    defer res.deinit();

    if (res.args.help != 0) {
        try printHelp();
        return;
    }
    if (res.args.version != 0) {
        var ws: common.WriterState = .{};
        ws.initStdout();
        const stdout = ws.writer();
        try stdout.print("guidance v{s}\n", .{version});
        try stdout.flush();
        return;
    }

    const subcmd_str = res.positionals[0] orelse {
        try printHelp();
        return;
    };
    const command = std.meta.stringToEnum(Command, subcmd_str) orelse {
        var ws: common.WriterState = .{};
        ws.initStdout();
        const stderr = ws.writer();
        try stderr.print("Unknown subcommand: {s}\n\n", .{subcmd_str});
        try stderr.flush();
        try printHelp();
        return;
    };

    // Collect remaining args from the iterator, scanning for global flags.
    var remaining: std.ArrayList([]const u8) = .empty;
    defer {
        for (remaining.items) |a| allocator.free(a);
        remaining.deinit(allocator);
    }
    while (iter.next()) |arg| {
        if (std.mem.eql(u8, arg, "--debug")) debug_mode = true;
        if (std.mem.eql(u8, arg, "--verbose")) verbose_mode = true;
        try remaining.append(allocator, try allocator.dupe(u8, arg));
    }

    // Check for --help in subcommand args before dispatching.
    for (remaining.items) |arg| {
        if (std.mem.eql(u8, arg, "--help") or std.mem.eql(u8, arg, "-h")) {
            try printSubcommandHelp(command);
            return;
        }
    }

    const subcmd_args = remaining.items;

    // Pipeline-failure errors (LintFailed, TestFailed) are expected failures
    // that have already printed their diagnostics. Exit(1) is cleaner than a stack trace.
    const run_result = switch (command) {
        .init => sync_engine_mod.cmdInit(allocator, subcmd_args),
        .gen => sync_engine_mod.cmdGen(allocator, subcmd_args),
        .clean => sync_engine_mod.cmdClean(allocator, subcmd_args),
        .explain => query_engine_mod.cmdExplain(allocator, subcmd_args),
        .todo => sync_engine_mod.cmdTodo(allocator, subcmd_args),
        .commit => sync_engine_mod.cmdCommit(allocator, subcmd_args),
        .benchmark => query_engine_mod.cmdBenchmark(allocator, subcmd_args),
        .telemetry => query_engine_mod.cmdTelemetry(allocator, subcmd_args),
        .serve => query_engine_mod.cmdServe(allocator, subcmd_args),
        .health => health_mod.cmdHealth(allocator, subcmd_args),
    };
    run_result catch |err| switch (err) {
        error.LintFailed, error.TestFailed => std.process.exit(1),
        else => return err,
    };
}

/// Top-level help: overview of commands and examples only — no per-subcommand flags.
fn printHelp() !void {
    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();
    try stdout.writeAll(
        \\guidance v0.1.0 — AST-guided SQLite vector search database generator
        \\
        \\Produces .guidance/src/**/*.json and .guidance.db for NullClaw.
        \\
        \\Usage:
        \\  guidance <command> [options]
        \\  guidance --help | --version
        \\
        \\Commands:
        \\  init       Create default configuration (AGENTS.md integration)
        \\  gen        Generate .guidance/ JSON mirror and .guidance.db
        \\  clean      Remove .guidance/src and .guidance.db
        \\  explain    Search codebase with optional LLM-synthesized summary
        \\  todo       Work item lifecycle (new|triage|checklist|status|list|abandon|run)
        \\  commit     Generate AI commit message from staged diff + guidance
        \\  benchmark  Benchmark explain queries against module-level comments
        \\  health     Detect unused modules, redundant code, and dead code candidates
        \\
        \\Examples:
        \\  guidance gen
        \\  guidance gen --file src/main.zig
        \\  guidance explain "how does the sync processor work"
        \\  guidance explain "filterStages" --no-llm
        \\  guidance health
        \\  guidance commit
        \\
        \\Run 'guidance <command> --help' for command-specific options.
        \\
    );
    try stdout.flush();
}

/// Per-subcommand help printed when 'guidance <cmd> --help' is invoked.
fn printSubcommandHelp(command: Command) !void {
    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();
    switch (command) {
        .init => try stdout.writeAll(
            \\Usage: guidance init [options]
            \\
            \\Create default .guidance/guidance-config.json and update AGENTS.md.
            \\
            \\Options:
            \\  -h, --help               Show this help
            \\  -g, --guidance-dir DIR   Guidance directory (default: .guidance)
            \\  -o, --db PATH            Database path (default: .guidance.db)
            \\
        ),
        .gen => try stdout.writeAll(
            \\Usage: guidance gen [options]
            \\
            \\Generate .guidance/src/**/*.json metadata mirrors and sync .guidance.db.
            \\Only processes files stale since last run (use --force to regenerate all).
            \\
            \\Options:
            \\  -h, --help              Show this help
            \\  --file FILE             Process a single source file (incremental)
            \\  --scan DIR              Process all source files under DIR
            \\  -w, --workspace DIR     Source root directory (default: current directory)
            \\  --guidance-dir DIR      Guidance directory (default: .guidance)
            \\  -o, --db PATH           SQLite database path (default: .guidance.db)
            \\  --no-db                 Skip database compilation step
            \\  --all-languages         Process Zig and Python source files
            \\  --regen                 LLM-regenerate all comments (implies --force)
            \\  --force                 Bypass staleness checks, reprocess all files
            \\  --timeout N             Sleep N seconds after each LLM call (default: 20)
            \\  --dry-run               Show what would change without writing
            \\  --debug                 Show LLM metadata (api calls, responses)
            \\  --verbose               Alias for --debug
            \\  --show-prompts          Show full LLM prompt text in output
            \\  --api-url URL           LLM API endpoint
            \\  -m, --model NAME        Model name (default: code:latest)
            \\
            \\Examples:
            \\  guidance gen
            \\  guidance gen --file src/guidance/main.zig
            \\  guidance gen --scan src --no-db
            \\  guidance gen --force --all-languages
            \\
        ),
        .clean => try stdout.writeAll(
            \\Usage: guidance clean [options]
            \\
            \\Remove .guidance/src/ JSON mirrors and .guidance.db.
            \\
            \\Options:
            \\  -h, --help               Show this help
            \\  --guidance-dir DIR       Guidance directory (default: .guidance)
            \\  -o, --db PATH            Database path (default: .guidance.db)
            \\
        ),
        .explain => try stdout.writeAll(
            \\Usage: guidance explain <query> [options]
            \\
            \\Search the codebase with keyword + vector hybrid search, optionally
            \\synthesizing a summary via local LLM.
            \\
            \\Options:
            \\  -h, --help                     Show this help
            \\  -l, --limit N                  Max results (default: 10)
            \\  -o, --db PATH                  Database path (default: .guidance.db)
            \\  -w, --workspace DIR            Workspace root (default: current directory)
            \\  --guidance-dir DIR             Guidance directory (default: .guidance)
            \\  --no-llm                       Skip LLM synthesis (fast, raw results)
            \\  --no-cache                     Bypass session query cache
            \\  --staged=false                 Use legacy output format
            \\  --filter=auto|force|skip       LLM relevance filter mode (default: auto)
            \\  --output=json|compact|debug    Output format (default: markdown)
            \\  --api-url URL                  LLM endpoint
            \\  -m, --model NAME               Model for synthesis
            \\
            \\Examples:
            \\  guidance explain "how does the sync processor work"
            \\  guidance explain "filterStages" --no-llm
            \\  guidance explain "cmdGen" --limit 5
            \\  guidance explain "query pipeline" --output=json
            \\
        ),
        .commit => try stdout.writeAll(
            \\Usage: guidance commit [options]
            \\
            \\Generate an AI commit message from staged diff + guidance context.
            \\
            \\Options:
            \\  -h, --help               Show this help
            \\  --guidance-dir DIR       Guidance directory (default: .guidance)
            \\  -o, --db PATH            Database path (default: .guidance.db)
            \\  --api-url URL            LLM endpoint
            \\  -m, --model NAME         Model for synthesis
            \\
        ),
        .benchmark => try stdout.writeAll(
            \\Usage: guidance benchmark [options]
            \\
            \\Benchmark explain queries against module-level comments.
            \\
            \\Options:
            \\  -h, --help               Show this help
            \\  -o, --db PATH            Database path (default: .guidance.db)
            \\  --guidance-dir DIR       Guidance directory (default: .guidance)
            \\
        ),
        .todo => try stdout.writeAll(
            \\Usage: guidance todo <action> [args]
            \\
            \\Work item lifecycle management stored in .guidance/todo/.
            \\
            \\Actions:
            \\  new <description>   Create a new work item
            \\  triage              AI-triage the current work item
            \\  checklist           Generate AI checklist for current item
            \\  status              Show current work item status
            \\  list                List all work items
            \\  abandon             Abandon the current work item
            \\  run                 Execute subagent FSM to resolve checklist items
            \\
            \\Options:
            \\  -h, --help               Show this help
            \\  --guidance-dir DIR       Guidance directory (default: .guidance)
            \\  --api-url URL            LLM endpoint
            \\  -m, --model NAME         Model for AI actions
            \\  --max-iterations N       Max subagent iterations (default: 20)
            \\  --allow-edit             Permit subagent to edit files
            \\
            \\Examples:
            \\  guidance todo new "implement zig-clap integration"
            \\  guidance todo status
            \\  guidance todo list
            \\  guidance todo run
            \\  guidance todo run --max-iterations 50 --allow-edit
            \\
        ),
        .telemetry => try stdout.writeAll(
            \\Usage: guidance telemetry [options]
            \\
            \\Show query telemetry from .guidance.db.
            \\
            \\Options:
            \\  -h, --help               Show this help
            \\  -o, --db PATH            Database path (default: .guidance.db)
            \\  --reset                  Clear cached LLM synthesis entries
            \\
        ),
        .serve => try stdout.writeAll(
            \\Usage: guidance serve [options]
            \\
            \\Start the guidance HTTP/MCP server.
            \\
            \\Options:
            \\  -h, --help               Show this help
            \\  --port N                 Port to listen on (default: 8080)
            \\  -o, --db PATH            Database path (default: .guidance.db)
            \\
        ),
        .health => try stdout.writeAll(
            \\Usage: guidance health [options]
            \\
            \\Detect unused modules, redundant code, and dead code candidates.
            \\Runs multiple analysis phases; use --phases to select specific ones.
            \\
            \\Options:
            \\  -h, --help                    Show this help
            \\  -w, --workspace DIR           Source root (default: current directory)
            \\  --min-age=N                   Min days since last modification (default: 30)
            \\  --format=ai|human|json        Output format (default: ai)
            \\  --simhash-threshold=N         Max Hamming distance for redundancy (default: 3)
            \\  --extract-calls               Enable Phase 2b call graph extraction (slow)
            \\  --phases=N[,N...]             Phases to run: 0,1,1.5,2 (default: all)
            \\  --fix                         Auto-move detached test files to proper locations
            \\  --db PATH                     Database path (default: .guidance.db)
            \\
            \\Examples:
            \\  guidance health
            \\  guidance health --min-age=90 --format=json
            \\  guidance health --simhash-threshold=2
            \\  guidance health --extract-calls
            \\  guidance health --phases=0,1 --format=human
            \\
        ),
    }
    try stdout.flush();
}

// =============================================================================
// Re-exports for tests.zig (public wrapper surface)
// =============================================================================

pub const guidanceDbIsUpToDatePub = sync_engine_mod.guidanceDbIsUpToDatePub;
pub const parseHunkRangesPub = sync_engine_mod.parseHunkRangesPub;
pub const loadChangedMembersPub = sync_engine_mod.loadChangedMembersPub;
pub const generateCommitMessagePub = sync_engine_mod.generateCommitMessagePub;
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
