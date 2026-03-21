const std = @import("std");
const StringInterner = @import("../common/interner.zig").StringInterner;
const TargetRegistry = @import("../common/registry.zig").TargetRegistry;
const BuildContext = @import("../common/context.zig").BuildContext;
const Repl = @import("../common/repl.zig").Repl;
const json_parser = @import("../common/json_parser.zig");
const llm = @import("common");

pub const version = "0.1.0";

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const raw_args = try std.process.argsAlloc(allocator);
    defer std.process.argsFree(allocator, raw_args);

    var positional: std.ArrayListUnmanaged([]const u8) = .{};
    defer positional.deinit(allocator);

    const args = llm.parseCommonArgs(raw_args[1..], &positional, allocator) catch |err| {
        switch (err) {
            error.MissingValue => {
                std.log.err("Flag requires a value argument", .{});
                std.process.exit(1);
            },
            else => return err,
        }
    };

    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    if (args.show_help) {
        try printHelp(stdout);
        try stdout.flush();
        return;
    }

    if (args.show_version) {
        try stdout.print("coral {s}\n", .{version});
        try stdout.flush();
        return;
    }

    if (args.llm_query) |query| {
        const config = llm.LlmConfig{
            .api_url = args.api_url,
            .model = args.model,
            .debug = args.verbose,
        };
        var client = try llm.LlmClient.init(allocator, config);
        defer client.deinit();

        if (args.verbose) {
            std.log.info("Querying LLM at {s} with model {s}", .{ args.api_url, args.model });
        }

        const system_prompt = "You are a helpful assistant for the Coral build system. Provide concise, practical advice.";
        const response = try client.complete(query, 2000, 0.7, system_prompt);

        if (response) |resp| {
            defer allocator.free(resp);
            try stdout.print("{s}\n", .{resp});
            try stdout.flush();
        } else {
            std.log.err("No response from LLM", .{});
            std.process.exit(1);
        }
        return;
    }

    var interner = StringInterner.init(allocator);
    defer interner.deinit();

    var registry = TargetRegistry.init(allocator, &interner);
    defer registry.deinit();

    if (args.config_file) |path| {
        json_parser.parseFile(allocator, path, &registry, &interner) catch |err| {
            std.log.err("Failed to load config file '{s}': {}", .{ path, err });
            return err;
        };
    } else if (!args.enter_repl) {
        const default_paths = [_][]const u8{ "coral.json", "build.json", "targets.json" };
        var found = false;

        for (default_paths) |path| {
            std.fs.cwd().access(path, .{}) catch continue;
            json_parser.parseFile(allocator, path, &registry, &interner) catch continue;
            if (args.verbose) {
                std.log.info("Loaded config from '{s}'", .{path});
            }
            found = true;
            break;
        }

        if (!found and args.show_list) {
            std.log.info("No config file found. Creating example coral.json...", .{});
            json_parser.writeExample(allocator, "coral.json") catch |err| {
                std.log.err("Failed to create example config: {}", .{err});
            };
        }
    }

    var ctx = BuildContext.init(allocator, &registry, &interner);
    ctx.dry_run = args.dry_run;
    ctx.force = args.force;
    ctx.verbose = args.verbose;

    if (args.enter_repl) {
        var repl = Repl.init(allocator, &ctx);
        defer repl.deinit();
        try repl.run();
        return;
    }

    if (args.show_list) {
        try ctx.listTargets(stdout);
        try stdout.flush();
        return;
    }

    if (args.show_graph) {
        try ctx.showGraph(args.positional, stdout);
        try stdout.flush();
        return;
    }

    var result = ctx.build(args.positional) catch |err| {
        std.log.err("Build failed: {}", .{err});
        std.process.exit(1);
    };
    defer result.deinit(allocator);

    if (result.success) {
        if (args.verbose or result.targets_built > 0) {
            std.log.info("Build completed: {d} targets in {d}ms", .{
                result.targets_built,
                result.duration_ns / 1_000_000,
            });
        }
    } else {
        std.log.err("Build failed: {d}/{d} targets failed", .{
            result.targets_failed,
            result.targets_built + result.targets_failed,
        });
        std.process.exit(1);
    }
}

fn printHelp(writer: anytype) !void {
    try writer.writeAll(
        \\coral - Yet Another Make, Zig edition
        \\
        \\Usage: coral [options] [targets...]
        \\
        \\Options:
        \\  -h, --help          Show this help message
        \\  -v, --version       Show version
        \\  -f, --file <path>   Load targets from JSON file
        \\  -n, --dry-run       Show what would be done without executing
        \\  --force             Force rebuild all targets
        \\  --verbose           Enable verbose output
        \\  -l, --list          List available targets
        \\  --graph             Show dependency graph
        \\  --repl              Enter interactive REPL mode
        \\
        \\LLM Options:
        \\  --llm-query <text>  Query LLM for AI assistance
        \\  --api-url <url>     LLM API endpoint (default: http://localhost:11434/v1/chat/completions)
        \\  -m, --model <name>  LLM model name (default: fast:latest)
        \\
        \\Examples:
        \\  coral                           Build default target from coral.json
        \\  coral build clean               Build 'build' then 'clean' targets
        \\  coral -f custom.json mytarget   Use custom config file
        \\  coral --dry-run all             Show build plan without executing
        \\  coral --repl                    Enter interactive mode
        \\  coral --llm-query "how do I add a new target?"
        \\
    );
}

test "simple imports" {
    _ = @import("../common/interner.zig");
    _ = @import("../common/target.zig");
    _ = @import("../common/registry.zig");
    _ = @import("../common/resolver.zig");
    _ = @import("../common/json_parser.zig");
    _ = @import("../common/context.zig");
    _ = @import("../common/repl.zig");
    // CozoDB schema (CozoScript DDL)
    _ = @import("schema.zig");
    // Unified CozoDB backend
    _ = @import("cozo.zig");
    _ = @import("db.zig");
    _ = @import("context_node_schema.zig");
    // Milestone 4: WebAssembly Sandboxing (Extism)
    _ = @import("wasm.zig");
    // Comment quality filter for ast-guidance infill pipeline
    _ = @import("scrub.zig");

    // Phase 1: RDF/Turtle Parser
    _ = @import("rdf/lexer.zig");
    _ = @import("rdf/parser.zig");
    _ = @import("rdf/normalize.zig");
    _ = @import("rdf/nquads.zig");

    // Phase 2: Ontology Mapping Layer
    _ = @import("ontology/yago.zig");
    _ = @import("ontology/mapper.zig");
    _ = @import("ontology/inference.zig");
    _ = @import("ontology/migration.zig");

    // Phase 3: Ingestion Pipeline
    _ = @import("ingest/targets.zig");
    _ = @import("ingest/batch.zig");
    _ = @import("ingest/cli.zig");
    _ = @import("ingest/verify.zig");

    // Phase 4: ANN / HNSW Stubs
    _ = @import("ann/index.zig");
    _ = @import("ann/hnsw.zig");
    _ = @import("ann/integration.zig");

    // Phase 5: NLP Mapping Interfaces
    _ = @import("nlp/resolver.zig");
    _ = @import("nlp/tool_discovery.zig");
    _ = @import("nlp/planner.zig");

    // Phase 9: 5-Tier Cache Hierarchy
    _ = @import("cache.zig");
}
