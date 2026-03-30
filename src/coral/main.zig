const std = @import("std");
const common = @import("common");
const coral_db = @import("coral_db");
const mcp = @import("mcp.zig");
const batch_mod = @import("coral_batch");
const StringInterner = common.interner.StringInterner;
const TargetRegistry = common.registry.TargetRegistry;
const BuildContext = common.context.BuildContext;
const Repl = common.repl.Repl;
const json_parser = common.json_parser;
const llm = common;
const Library = coral_db.Library;
const QueueReactor = @import("cache.zig").QueueReactor;
const BatchIngestor = batch_mod.BatchIngestor;

pub const version = "0.1.0";

/// Starts the Zig program execution by defining the entry point.
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

    // MCP subcommand: `coral mcp` or `coral --mcp`
    const want_mcp = blk: {
        if (positional.items.len > 0 and std.mem.eql(u8, positional.items[0], "mcp")) break :blk true;
        break :blk false;
    };
    if (want_mcp) {
        const use_memory = args.dry_run; // --dry-run doubles as --memory for MCP
        const lib = if (use_memory)
            try Library.init(allocator, .mem, "")
        else blk: {
            // Default db path: ~/.coral/context.db
            const home = std.posix.getenv("HOME") orelse ".";
            const db_dir = try std.fmt.allocPrint(allocator, "{s}/.coral", .{home});
            defer allocator.free(db_dir);
            std.fs.makeDirAbsolute(db_dir) catch |e| {
                if (e != error.PathAlreadyExists) return e;
            };
            const db_path = try std.fmt.allocPrint(allocator, "{s}/context.db", .{db_dir});
            defer allocator.free(db_path);
            break :blk try Library.init(allocator, .sqlite, db_path);
        };
        defer lib.deinit();
        try lib.initSchema();

        var reactor = QueueReactor.init(allocator, lib, 20);
        defer reactor.deinit();

        var server = mcp.McpServer{
            .allocator = allocator,
            .reactor = &reactor,
        };

        var mcp_rs: llm.ReaderState = .{};
        mcp_rs.initStdin();
        var mcp_ws: llm.WriterState = .{};
        mcp_ws.initStdout();
        try server.serve(mcp_rs.reader(), mcp_ws.writer());
        return;
    }

    // Ingest subcommand: `coral ingest --source <ttl-file>`
    const want_ingest = positional.items.len > 0 and std.mem.eql(u8, positional.items[0], "ingest");
    if (want_ingest) {
        // Resolve source file from --file flag or second positional arg.
        const source_path: []const u8 = blk: {
            if (args.config_file) |f| break :blk f;
            if (positional.items.len > 1) break :blk positional.items[1];
            std.log.err("coral ingest: specify source with --file <path> or as second argument", .{});
            std.process.exit(1);
        };

        // Default db: ~/.coral/context.db
        const home = std.posix.getenv("HOME") orelse ".";
        const db_dir = try std.fmt.allocPrint(allocator, "{s}/.coral", .{home});
        defer allocator.free(db_dir);
        std.fs.makeDirAbsolute(db_dir) catch |e| {
            if (e != error.PathAlreadyExists) return e;
        };
        const db_path = try std.fmt.allocPrint(allocator, "{s}/context.db", .{db_dir});
        defer allocator.free(db_path);

        const lib = try Library.init(allocator, .sqlite, db_path);
        defer lib.deinit();
        try lib.initSchema();

        var builder = BatchIngestor.from(allocator, lib);
        const stats = try builder.batchSize(10_000).skipErrors(true).ingestFile(source_path);

        try stdout.print("Ingestion complete:\n", .{});
        try stdout.print("  triples processed : {d}\n", .{stats.triples_processed});
        try stdout.print("  nodes created     : {d}\n", .{stats.nodes_created});
        try stdout.print("  edges created     : {d}\n", .{stats.edges_created});
        try stdout.print("  errors skipped    : {d}\n", .{stats.errors_skipped});
        try stdout.print("  batches flushed   : {d}\n", .{stats.batches_flushed});
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
        \\Subcommands:
        \\  coral mcp              Start MCP server (STDIO transport)
        \\  coral ingest           Ingest a Turtle/N-Triples file into ~/.coral/context.db
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
        \\  coral mcp                       Start MCP server on STDIO
        \\  coral ingest --file yago.ttl    Ingest YAGO 4.5 Turtle file (sparse, type-whitelisted)
        \\
    );
}

test "coral: core module imports compile" {
    // db.zig and schema.zig are exposed via the `coral_db` named module.
    // They cannot be imported relatively when coral_db is a build dep (module conflict).
    _ = coral_db; // exercises db.zig + schema.zig via named module
    _ = @import("scrub.zig"); // scrub.zig is not part of coral_db, safe to import relatively
    _ = @import("yago_ingest.zig"); // yago_ingest.zig pulls in coral_batch + ontology
    _ = @import("token_budget.zig"); // M7.1 TokenEstimator
    _ = @import("metrics.zig"); // M8.1 LatencyHistogram
}
