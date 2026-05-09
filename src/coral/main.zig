const std = @import("std");
const clap = @import("clap");
const common = @import("common");
const dag = @import("dag");
const coral_db = @import("coral_db");
const mcp = @import("mcp.zig");
const batch_mod = @import("coral_batch");
const yago_ingest_mod = @import("yago_ingest.zig");
const StringInterner = common.interner.StringInterner;
const TargetRegistry = dag.TargetRegistry;
const BuildContext = dag.BuildContext;
const Repl = dag.repl.Repl;
const json_parser = dag.json_parser;
const llm = @import("llm");
const Library = coral_db.Library;
const QueueReactor = @import("cache.zig").QueueReactor;
const BatchIngestor = batch_mod.BatchIngestor;
const YagoIngestor = yago_ingest_mod.YagoIngestor;
const YagoConfig = yago_ingest_mod.YagoConfig;
const CSRGraph = @import("csr_graph").CSRGraph;
const DegreeCentrality = @import("algorithms/degree_centrality.zig").DegreeCentrality;
const PageRank = @import("algorithms/pagerank.zig").PageRank;
const Louvain = @import("algorithms/louvain.zig").Louvain;

pub const version = "0.1.0";

// Top-level flags shared across all modes.
// The first positional terminates flag parsing so that subcommand-specific
// options (e.g. --damping for compute-pagerank) are left in the iterator
// and handled by each subcommand block.
const main_params = clap.parseParamsComptime(
    \\-h, --help             Display this help and exit.
    \\-v, --version          Show version and exit.
    \\-f, --file <str>       Load targets from JSON file.
    \\-n, --dry-run          Show what would be done without executing.
    \\    --force            Force rebuild all targets.
    \\    --verbose          Enable verbose output.
    \\-l, --list             List available targets.
    \\    --graph            Show dependency graph.
    \\    --repl             Enter interactive REPL mode.
    \\    --llm-query <str>  Query the LLM for AI assistance.
    \\    --api-url <str>    LLM API endpoint (default: http://localhost:11434/v1/chat/completions).
    \\-m, --model <str>      LLM model name (default: fast:latest).
    \\<str>
    \\
);

/// Starts the Zig program execution by defining the entry point.
pub fn main(init: std.process.Init.Minimal) !void {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();
    const io = std.Io.Threaded.global_single_threaded.io();

    var iter = try init.args.iterateAllocator(allocator);
    defer iter.deinit();
    _ = iter.next(); // skip exe name

    var diag = clap.Diagnostic{};
    var res = clap.parseEx(clap.Help, &main_params, clap.parsers.default, &iter, .{
        .diagnostic = &diag,
        .allocator = allocator,
        .terminating_positional = 0,
    }) catch |err| {
        diag.reportToFile(io, .stderr(), err) catch {};
        std.process.exit(1);
    };
    defer res.deinit();

    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    if (res.args.help != 0) {
        try printHelp(stdout);
        try stdout.flush();
        return;
    }

    if (res.args.version != 0) {
        try stdout.print("coral {s}\n", .{version});
        try stdout.flush();
        return;
    }

    // Collect remaining args from the iterator (subcommand options or extra targets).
    var remaining: std.ArrayList([]const u8) = .empty;
    defer {
        for (remaining.items) |a| allocator.free(a);
        remaining.deinit(allocator);
    }
    while (iter.next()) |arg| {
        try remaining.append(allocator, try allocator.dupe(u8, arg));
    }

    const first_positional: ?[]const u8 = res.positionals[0];

    // Dispatch to subcommands based on first positional argument.
    // Unknown first-positional falls through to the primary build mode.

    // --- mcp subcommand ---
    if (first_positional != null and std.mem.eql(u8, first_positional.?, "mcp")) {
        if (remaining.items.len > 0 and
            (std.mem.eql(u8, remaining.items[0], "--help") or
                std.mem.eql(u8, remaining.items[0], "-h")))
        {
            try stdout.writeAll(
                \\Usage: coral mcp
                \\
                \\Start the MCP server using STDIO transport.
                \\Reads ~/.coral/context.db (created if absent).
                \\
                \\Options:
                \\  -h, --help     Show this help
                \\  --dry-run      Use an in-memory database instead of disk
                \\
            );
            try stdout.flush();
            return;
        }
        const use_memory = res.args.@"dry-run" != 0;
        const lib = if (use_memory)
            try Library.init(allocator, .mem, "")
        else blk: {
            const home = init.environ.getPosix("HOME") orelse ".";
            const db_dir = try std.fmt.allocPrint(allocator, "{s}/.coral", .{home});
            defer allocator.free(db_dir);
            std.Io.Dir.createDirAbsolute(io, db_dir, .default_dir) catch |e| {
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

        var mcp_rs: common.ReaderState = .{};
        mcp_rs.initStdin();
        var mcp_ws: common.WriterState = .{};
        mcp_ws.initStdout();
        try server.serve(mcp_rs.reader(), mcp_ws.writer());
        return;
    }

    // --- ingest subcommand ---
    if (first_positional != null and std.mem.eql(u8, first_positional.?, "ingest")) {
        if (remaining.items.len > 0 and
            (std.mem.eql(u8, remaining.items[0], "--help") or
                std.mem.eql(u8, remaining.items[0], "-h")))
        {
            try stdout.writeAll(
                \\Usage: coral ingest --file <path>
                \\       coral ingest <path>
                \\
                \\Ingest a Turtle/N-Triples file into ~/.coral/context.db.
                \\
                \\Options:
                \\  -h, --help          Show this help
                \\  -f, --file <path>   Path to the Turtle/N-Triples source file
                \\
                \\Example:
                \\  coral ingest --file yago.ttl
                \\  coral ingest data/knowledge.ttl
                \\
            );
            try stdout.flush();
            return;
        }
        // Resolve source file from --file flag or first remaining positional.
        const source_path: []const u8 = blk: {
            if (res.args.file) |f| break :blk f;
            if (remaining.items.len > 0 and !std.mem.startsWith(u8, remaining.items[0], "-"))
                break :blk remaining.items[0];
            // Also parse --file from remaining args
            var i: usize = 0;
            while (i < remaining.items.len) : (i += 1) {
                if ((std.mem.eql(u8, remaining.items[i], "--file") or
                    std.mem.eql(u8, remaining.items[i], "-f")) and
                    i + 1 < remaining.items.len)
                {
                    break :blk remaining.items[i + 1];
                }
            }
            std.log.err("coral ingest: specify source with --file <path> or as second argument", .{});
            std.process.exit(1);
        };

        const home = init.environ.getPosix("HOME") orelse ".";
        const db_dir = try std.fmt.allocPrint(allocator, "{s}/.coral", .{home});
        defer allocator.free(db_dir);
        std.Io.Dir.createDirAbsolute(io, db_dir, .default_dir) catch |e| {
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

    // --- yago-ingest subcommand ---
    if (first_positional != null and std.mem.eql(u8, first_positional.?, "yago-ingest")) {
        if (remaining.items.len > 0 and
            (std.mem.eql(u8, remaining.items[0], "--help") or
                std.mem.eql(u8, remaining.items[0], "-h")))
        {
            try stdout.writeAll(
                \\Usage: coral yago-ingest --file <path>
                \\       coral yago-ingest <path>
                \\
                \\Ingest a YAGO 4.5 Turtle directory or file into ~/.coral/yago.db.
                \\Applies type whitelist filtering for a sparse, useful graph.
                \\
                \\Options:
                \\  -h, --help          Show this help
                \\  -f, --file <path>   Path to YAGO Turtle file or directory
                \\  --dry-run           Scan without writing to database
                \\  --verbose           Log progress every 10K triples
                \\
                \\Example:
                \\  coral yago-ingest --file /data/yago4.5/
                \\
            );
            try stdout.flush();
            return;
        }
        const source_path: []const u8 = blk: {
            if (res.args.file) |f| break :blk f;
            if (remaining.items.len > 0 and !std.mem.startsWith(u8, remaining.items[0], "-"))
                break :blk remaining.items[0];
            var i: usize = 0;
            while (i < remaining.items.len) : (i += 1) {
                if ((std.mem.eql(u8, remaining.items[i], "--file") or
                    std.mem.eql(u8, remaining.items[i], "-f")) and
                    i + 1 < remaining.items.len)
                {
                    break :blk remaining.items[i + 1];
                }
            }
            std.log.err("coral yago-ingest: specify source with --file <path> or as second argument", .{});
            std.process.exit(1);
        };

        const dry_run = res.args.@"dry-run" != 0;
        const verbose = res.args.verbose != 0;

        const home = init.environ.getPosix("HOME") orelse ".";
        const db_dir = try std.fmt.allocPrint(allocator, "{s}/.coral", .{home});
        defer allocator.free(db_dir);
        std.Io.Dir.createDirAbsolute(io, db_dir, .default_dir) catch |e| {
            if (e != error.PathAlreadyExists) return e;
        };
        const db_path = try std.fmt.allocPrint(allocator, "{s}/yago.db", .{db_dir});
        defer allocator.free(db_path);

        const lib = try Library.init(allocator, .sqlite, db_path);
        defer lib.deinit();
        try lib.initSchema();

        const ProgressCb = yago_ingest_mod.ProgressCallback;
        const progressCallback: ?ProgressCb = if (verbose) struct {
            fn callback(triples: usize, nodes: usize, edges: usize) void {
                std.log.info("YAGO progress: {d} triples, {d} nodes, {d} edges", .{ triples, nodes, edges });
            }
        }.callback else null;

        const config = YagoConfig{
            .batch_size = 10_000,
            .whitelist_only = true,
            .build_hierarchy = false,
            .skip_errors = true,
            .dry_run = dry_run,
            .on_progress = progressCallback,
        };
        var ingestor = YagoIngestor.init(allocator, config);

        if (dry_run) {
            try stdout.print("YAGO dry-run (no database writes): {s}\n", .{source_path});
        } else {
            try stdout.print("YAGO ingestion starting: {s}\n", .{source_path});
        }
        try stdout.flush();

        const start_time = std.Io.Timestamp.now(io, .real).nanoseconds;
        const stats = try ingestor.ingestFile(source_path, lib, null);
        const elapsed_ns = std.Io.Timestamp.now(io, .real).nanoseconds - start_time;
        const elapsed_s = @as(f64, @floatFromInt(elapsed_ns)) / 1_000_000_000.0;

        if (dry_run) {
            try stdout.print("YAGO dry-run complete ({d:.2}s):\n", .{elapsed_s});
            try stdout.print("  triples scanned    : {d}\n", .{stats.triples_processed});
            try stdout.print("  triples filtered   : {d}\n", .{stats.triples_filtered});
            try stdout.print("  errors skipped     : {d}\n", .{stats.errors_skipped});
        } else {
            const rate = if (elapsed_s > 0) @as(f64, @floatFromInt(stats.triples_processed)) / elapsed_s else 0;
            try stdout.print("YAGO ingestion complete ({d:.2}s, {d:.0} triples/sec):\n", .{ elapsed_s, rate });
            try stdout.print("  triples processed : {d}\n", .{stats.triples_processed});
            try stdout.print("  nodes created    : {d}\n", .{stats.nodes_created});
            try stdout.print("  edges created     : {d}\n", .{stats.edges_created});
            try stdout.print("  errors skipped   : {d}\n", .{stats.errors_skipped});
            try stdout.print("  batches flushed  : {d}\n", .{stats.batches_flushed});
        }
        try stdout.flush();
        return;
    }

    // --- compute-degree subcommand ---
    if (first_positional != null and std.mem.eql(u8, first_positional.?, "compute-degree")) {
        if (remaining.items.len > 0 and
            (std.mem.eql(u8, remaining.items[0], "--help") or
                std.mem.eql(u8, remaining.items[0], "-h")))
        {
            try stdout.writeAll(
                \\Usage: coral compute-degree
                \\
                \\Compute degree centrality for all nodes and persist to database.
                \\
                \\Options:
                \\  -h, --help   Show this help
                \\
            );
            try stdout.flush();
            return;
        }
        const home = init.environ.getPosix("HOME") orelse ".";
        const db_dir = try std.fmt.allocPrint(allocator, "{s}/.coral", .{home});
        defer allocator.free(db_dir);
        const db_path = try std.fmt.allocPrint(allocator, "{s}/context.db", .{db_dir});
        defer allocator.free(db_path);

        const lib = try Library.init(allocator, .sqlite, db_path);
        defer lib.deinit();

        var arena = std.heap.ArenaAllocator.init(allocator);
        defer arena.deinit();

        const start = std.Io.Timestamp.now(io, .real).nanoseconds;
        try DegreeCentrality.compute(arena.allocator(), lib);
        const elapsed_ms = @as(f64, @floatFromInt(std.Io.Timestamp.now(io, .real).nanoseconds - start)) / 1_000_000.0;

        try stdout.print("Degree centrality computed in {d:.2}ms\n", .{elapsed_ms});
        try stdout.flush();
        return;
    }

    // --- compute-pagerank subcommand ---
    if (first_positional != null and std.mem.eql(u8, first_positional.?, "compute-pagerank")) {
        if (remaining.items.len > 0 and
            (std.mem.eql(u8, remaining.items[0], "--help") or
                std.mem.eql(u8, remaining.items[0], "-h")))
        {
            try stdout.writeAll(
                \\Usage: coral compute-pagerank [options]
                \\
                \\Compute PageRank scores for all nodes and persist to database.
                \\
                \\Options:
                \\  -h, --help           Show this help
                \\  --damping <f32>      Damping factor (default: 0.85)
                \\  --tolerance <f32>    Convergence tolerance (default: 0.0001)
                \\  --max-iter <u32>     Maximum iterations (default: 20)
                \\
                \\Example:
                \\  coral compute-pagerank --damping 0.85 --max-iter 50
                \\
            );
            try stdout.flush();
            return;
        }
        var damping: f32 = 0.85;
        var tolerance: f32 = 0.0001;
        var max_iter: u32 = 20;
        var i: usize = 0;
        while (i < remaining.items.len) : (i += 1) {
            if (std.mem.eql(u8, remaining.items[i], "--damping")) {
                i += 1;
                if (i < remaining.items.len)
                    damping = std.fmt.parseFloat(f32, remaining.items[i]) catch 0.85;
            } else if (std.mem.eql(u8, remaining.items[i], "--tolerance")) {
                i += 1;
                if (i < remaining.items.len)
                    tolerance = std.fmt.parseFloat(f32, remaining.items[i]) catch 0.0001;
            } else if (std.mem.eql(u8, remaining.items[i], "--max-iter")) {
                i += 1;
                if (i < remaining.items.len)
                    max_iter = std.fmt.parseInt(u32, remaining.items[i], 10) catch 20;
            }
        }

        const home = init.environ.getPosix("HOME") orelse ".";
        const db_dir = try std.fmt.allocPrint(allocator, "{s}/.coral", .{home});
        defer allocator.free(db_dir);
        const db_path = try std.fmt.allocPrint(allocator, "{s}/context.db", .{db_dir});
        defer allocator.free(db_path);

        const lib = try Library.init(allocator, .sqlite, db_path);
        defer lib.deinit();

        var arena = std.heap.ArenaAllocator.init(allocator);
        defer arena.deinit();

        const graph = try CSRGraph.build(arena.allocator(), lib, null, false);
        if (graph.node_count == 0) {
            try stdout.print("No nodes in database. Run ingest first.\n", .{});
            try stdout.flush();
            return;
        }

        const pr = PageRank.init(.{
            .damping = damping,
            .tolerance = tolerance,
            .max_iterations = max_iter,
        });
        const start = std.Io.Timestamp.now(io, .real).nanoseconds;
        const scores = try pr.run(arena.allocator(), &graph);
        const elapsed_ms = @as(f64, @floatFromInt(std.Io.Timestamp.now(io, .real).nanoseconds - start)) / 1_000_000.0;

        const all_ids = try lib.allNodeIds(arena.allocator());
        for (all_ids, 0..) |node_id, idx| {
            if (idx < scores.len)
                try lib.updateNodePageRank(node_id, scores[idx]);
        }

        try stdout.print("PageRank computed for {d} nodes in {d:.2}ms\n", .{ graph.node_count, elapsed_ms });
        try stdout.flush();
        return;
    }

    // --- compute-communities subcommand ---
    if (first_positional != null and std.mem.eql(u8, first_positional.?, "compute-communities")) {
        if (remaining.items.len > 0 and
            (std.mem.eql(u8, remaining.items[0], "--help") or
                std.mem.eql(u8, remaining.items[0], "-h")))
        {
            try stdout.writeAll(
                \\Usage: coral compute-communities [options]
                \\
                \\Detect communities via Louvain and persist community IDs to database.
                \\
                \\Options:
                \\  -h, --help           Show this help
                \\  --resolution <f32>   Resolution parameter (default: 1.0)
                \\  --max-iter <u32>     Maximum iterations (default: 10)
                \\
                \\Example:
                \\  coral compute-communities --resolution 0.5
                \\
            );
            try stdout.flush();
            return;
        }
        var resolution: f32 = 1.0;
        var max_iter: u32 = 10;
        var i: usize = 0;
        while (i < remaining.items.len) : (i += 1) {
            if (std.mem.eql(u8, remaining.items[i], "--resolution")) {
                i += 1;
                if (i < remaining.items.len)
                    resolution = std.fmt.parseFloat(f32, remaining.items[i]) catch 1.0;
            } else if (std.mem.eql(u8, remaining.items[i], "--max-iter")) {
                i += 1;
                if (i < remaining.items.len)
                    max_iter = std.fmt.parseInt(u32, remaining.items[i], 10) catch 10;
            }
        }

        const home = init.environ.getPosix("HOME") orelse ".";
        const db_dir = try std.fmt.allocPrint(allocator, "{s}/.coral", .{home});
        defer allocator.free(db_dir);
        const db_path = try std.fmt.allocPrint(allocator, "{s}/context.db", .{db_dir});
        defer allocator.free(db_path);

        const lib = try Library.init(allocator, .sqlite, db_path);
        defer lib.deinit();

        var arena = std.heap.ArenaAllocator.init(allocator);
        defer arena.deinit();

        const graph = try CSRGraph.build(arena.allocator(), lib, null, true);
        if (graph.node_count == 0) {
            try stdout.print("No nodes in database. Run ingest first.\n", .{});
            try stdout.flush();
            return;
        }

        const louvain = Louvain.init(.{
            .resolution = resolution,
            .max_iterations = max_iter,
        });
        const start = std.Io.Timestamp.now(io, .real).nanoseconds;
        const communities = try louvain.run(arena.allocator(), &graph);
        const elapsed_ms = @as(f64, @floatFromInt(std.Io.Timestamp.now(io, .real).nanoseconds - start)) / 1_000_000.0;

        const all_ids = try lib.allNodeIds(arena.allocator());
        for (all_ids, 0..) |node_id, idx| {
            if (idx < communities.len)
                try lib.updateNodeCommunity(node_id, @intCast(communities[idx]), 0);
        }

        try stdout.print("Communities detected for {d} nodes in {d:.2}ms\n", .{ graph.node_count, elapsed_ms });
        try stdout.flush();
        return;
    }

    // --- LLM query mode ---
    if (res.args.@"llm-query") |query| {
        const api_url = res.args.@"api-url" orelse "http://localhost:11434/v1/chat/completions";
        const model_name = res.args.model orelse "fast:latest";
        const verbose = res.args.verbose != 0;
        const config = llm.LlmConfig{
            .api_url = api_url,
            .model = model_name,
            .debug = verbose,
        };
        var client = try llm.LlmClient.init(allocator, config);
        defer client.deinit();

        if (verbose) {
            std.log.info("Querying LLM at {s} with model {s}", .{ api_url, model_name });
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

    // --- Primary build / REPL mode ---
    var interner = StringInterner.init(allocator);
    defer interner.deinit();

    var registry = TargetRegistry.init(allocator, &interner);
    defer registry.deinit();

    const config_file = res.args.file;
    const verbose = res.args.verbose != 0;
    const enter_repl = res.args.repl != 0;
    const dry_run = res.args.@"dry-run" != 0;
    const force = res.args.force != 0;
    const show_list = res.args.list != 0;
    const show_graph = res.args.graph != 0;

    if (config_file) |path| {
        json_parser.parseFile(allocator, path, &registry, &interner) catch |err| {
            std.log.err("Failed to load config file '{s}': {}", .{ path, err });
            return err;
        };
    } else if (!enter_repl) {
        const default_paths = [_][]const u8{ "coral.json", "build.json", "targets.json" };
        var found = false;

        for (default_paths) |path| {
            std.Io.Dir.cwd().access(io, path, .{}) catch continue;
            json_parser.parseFile(allocator, path, &registry, &interner) catch continue;
            if (verbose) {
                std.log.info("Loaded config from '{s}'", .{path});
            }
            found = true;
            break;
        }

        if (!found and show_list) {
            std.log.info("No config file found. Creating example coral.json...", .{});
            json_parser.writeExample("coral.json") catch |err| {
                std.log.err("Failed to create example config: {}", .{err});
            };
        }
    }

    var ctx = BuildContext.init(allocator, &registry, &interner);
    ctx.dry_run = dry_run;
    ctx.force = force;
    ctx.verbose = verbose;

    if (enter_repl) {
        var repl = Repl.init(allocator, &ctx);
        defer repl.deinit();
        try repl.run();
        return;
    }

    if (show_list) {
        try ctx.listTargets(stdout);
        try stdout.flush();
        return;
    }

    // Collect targets: first_positional (if not a subcommand) + remaining args.
    var targets: std.ArrayList([]const u8) = .empty;
    defer targets.deinit(allocator);
    if (first_positional) |fp| try targets.append(allocator, fp);
    for (remaining.items) |arg| try targets.append(allocator, arg);

    if (show_graph) {
        try ctx.showGraph(targets.items, stdout);
        try stdout.flush();
        return;
    }

    var result = ctx.build(targets.items) catch |err| {
        std.log.err("Build failed: {}", .{err});
        std.process.exit(1);
    };
    defer result.deinit(allocator);

    if (result.success) {
        if (verbose or result.targets_built > 0) {
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

/// Top-level help: subcommands and options overview, no per-subcommand flags.
fn printHelp(writer: anytype) !void {
    try writer.writeAll(
        \\coral v0.1.0 — Yet Another Make, Zig edition
        \\
        \\Usage: coral [options] [targets...]
        \\       coral <subcommand> [subcommand-options]
        \\
        \\Subcommands:
        \\  mcp                  Start MCP server (STDIO transport)
        \\  ingest               Ingest a Turtle/N-Triples file into ~/.coral/context.db
        \\  yago-ingest          Ingest a YAGO 4.5 Turtle file into ~/.coral/yago.db
        \\  compute-degree       Compute degree centrality and persist to database
        \\  compute-pagerank     Compute PageRank scores and persist to database
        \\  compute-communities  Detect communities via Louvain and persist to database
        \\
        \\Options:
        \\  -h, --help            Show this help
        \\  -v, --version         Show version
        \\  -f, --file <path>     Load targets from JSON file
        \\  -n, --dry-run         Show what would be done without executing
        \\      --force           Force rebuild all targets
        \\      --verbose         Enable verbose output
        \\  -l, --list            List available targets
        \\      --graph           Show dependency graph
        \\      --repl            Enter interactive REPL mode
        \\
        \\LLM Options:
        \\  --llm-query <text>   Query LLM for AI assistance
        \\  --api-url <url>      LLM API endpoint (default: http://localhost:11434/v1/chat/completions)
        \\  -m, --model <name>   LLM model name (default: fast:latest)
        \\
        \\Examples:
        \\  coral                           Build default target from coral.json
        \\  coral build clean               Build 'build' then 'clean' targets
        \\  coral --dry-run all             Show build plan without executing
        \\  coral mcp                       Start MCP server on STDIO
        \\  coral ingest --file yago.ttl    Ingest YAGO 4.5 Turtle file
        \\  coral compute-pagerank          Compute PageRank for all nodes
        \\
        \\Run 'coral <subcommand> --help' for subcommand-specific options.
        \\
    );
}
