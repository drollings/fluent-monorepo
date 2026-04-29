const std = @import("std");
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

/// Starts the Zig program execution by defining the entry point.
pub fn main(init: std.process.Init.Minimal) !void {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();
    const io = std.Io.Threaded.global_single_threaded.io();

    // Collect args into owned slice (replaces the removed std.process.argsAlloc)
    var raw_args_list: std.ArrayList([]const u8) = .empty;
    defer {
        for (raw_args_list.items) |a| allocator.free(a);
        raw_args_list.deinit(allocator);
    }
    {
        var iter = try init.args.iterateAllocator(allocator);
        defer iter.deinit();
        while (iter.next()) |arg| try raw_args_list.append(allocator, try allocator.dupe(u8, arg));
    }
    const raw_args = raw_args_list.items;

    var positional: std.ArrayListUnmanaged([]const u8) = .empty;
    defer positional.deinit(allocator);

    const args = common.parseCommonArgs(raw_args[1..], &positional, allocator) catch |err| {
        switch (err) {
            error.MissingValue => {
                std.log.err("Flag requires a value argument", .{});
                std.process.exit(1);
            },
            else => return err,
        }
    };

    var ws: common.WriterState = .{};
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

    // yago-ingest subcommand: `coral yago-ingest <ttl-dir-or-file>`
    const want_yago_ingest = positional.items.len > 0 and std.mem.eql(u8, positional.items[0], "yago-ingest");
    if (want_yago_ingest) {
        const source_path: []const u8 = blk: {
            if (args.config_file) |f| break :blk f;
            if (positional.items.len > 1) break :blk positional.items[1];
            std.log.err("coral yago-ingest: specify source with --file <path> or as second argument", .{});
            std.process.exit(1);
        };

        const dry_run = args.dry_run;

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

        // Progress callback for verbose mode - logs every batch (10K triples)
        const ProgressCb = yago_ingest_mod.ProgressCallback;
        const progressCallback: ?ProgressCb = if (args.verbose) struct {
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

    // compute-degree subcommand: `coral compute-degree`
    const want_compute_degree = positional.items.len > 0 and std.mem.eql(u8, positional.items[0], "compute-degree");
    if (want_compute_degree) {
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

    // compute-pagerank subcommand: `coral compute-pagerank [--damping D] [--tolerance T] [--max-iter N]`
    const want_compute_pagerank = positional.items.len > 0 and std.mem.eql(u8, positional.items[0], "compute-pagerank");
    if (want_compute_pagerank) {
        // Parse optional flags
        var damping: f32 = 0.85;
        var tolerance: f32 = 0.0001;
        var max_iter: u32 = 20;
        var i: usize = 1;
        while (i < positional.items.len) : (i += 1) {
            if (std.mem.eql(u8, positional.items[i], "--damping")) {
                i += 1;
                if (i < positional.items.len) {
                    damping = std.fmt.parseFloat(f32, positional.items[i]) catch 0.85;
                }
            } else if (std.mem.eql(u8, positional.items[i], "--tolerance")) {
                i += 1;
                if (i < positional.items.len) {
                    tolerance = std.fmt.parseFloat(f32, positional.items[i]) catch 0.0001;
                }
            } else if (std.mem.eql(u8, positional.items[i], "--max-iter")) {
                i += 1;
                if (i < positional.items.len) {
                    max_iter = std.fmt.parseInt(u32, positional.items[i], 10) catch 20;
                }
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

        // Build CSR graph
        const graph = try CSRGraph.build(arena.allocator(), lib, null, false);
        if (graph.node_count == 0) {
            try stdout.print("No nodes in database. Run ingest first.\n", .{});
            try stdout.flush();
            return;
        }

        // Run PageRank
        const pr = PageRank.init(.{
            .damping = damping,
            .tolerance = tolerance,
            .max_iterations = max_iter,
        });
        const start = std.Io.Timestamp.now(io, .real).nanoseconds;
        const scores = try pr.run(arena.allocator(), &graph);
        const elapsed_ms = @as(f64, @floatFromInt(std.Io.Timestamp.now(io, .real).nanoseconds - start)) / 1_000_000.0;

        // Persist scores
        const all_ids = try lib.allNodeIds(arena.allocator());
        for (all_ids, 0..) |node_id, idx| {
            if (idx < scores.len) {
                try lib.updateNodePageRank(node_id, scores[idx]);
            }
        }

        try stdout.print("PageRank computed for {d} nodes in {d:.2}ms\n", .{ graph.node_count, elapsed_ms });
        try stdout.flush();
        return;
    }

    // compute-communities subcommand: `coral compute-communities [--resolution R] [--max-iter N]`
    const want_compute_communities = positional.items.len > 0 and std.mem.eql(u8, positional.items[0], "compute-communities");
    if (want_compute_communities) {
        var resolution: f32 = 1.0;
        var max_iter: u32 = 10;
        var i: usize = 1;
        while (i < positional.items.len) : (i += 1) {
            if (std.mem.eql(u8, positional.items[i], "--resolution")) {
                i += 1;
                if (i < positional.items.len) {
                    resolution = std.fmt.parseFloat(f32, positional.items[i]) catch 1.0;
                }
            } else if (std.mem.eql(u8, positional.items[i], "--max-iter")) {
                i += 1;
                if (i < positional.items.len) {
                    max_iter = std.fmt.parseInt(u32, positional.items[i], 10) catch 10;
                }
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

        // Build CSR graph
        const graph = try CSRGraph.build(arena.allocator(), lib, null, true);
        if (graph.node_count == 0) {
            try stdout.print("No nodes in database. Run ingest first.\n", .{});
            try stdout.flush();
            return;
        }

        // Run Louvain
        const louvain = Louvain.init(.{
            .resolution = resolution,
            .max_iterations = max_iter,
        });
        const start = std.Io.Timestamp.now(io, .real).nanoseconds;
        const communities = try louvain.run(arena.allocator(), &graph);
        const elapsed_ms = @as(f64, @floatFromInt(std.Io.Timestamp.now(io, .real).nanoseconds - start)) / 1_000_000.0;

        // Persist communities
        const all_ids = try lib.allNodeIds(arena.allocator());
        for (all_ids, 0..) |node_id, idx| {
            if (idx < communities.len) {
                try lib.updateNodeCommunity(node_id, @intCast(communities[idx]), 0);
            }
        }

        try stdout.print("Communities detected for {d} nodes in {d:.2}ms\n", .{ graph.node_count, elapsed_ms });
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
            std.Io.Dir.cwd().access(io, path, .{}) catch continue;
            json_parser.parseFile(allocator, path, &registry, &interner) catch continue;
            if (args.verbose) {
                std.log.info("Loaded config from '{s}'", .{path});
            }
            found = true;
            break;
        }

        if (!found and args.show_list) {
            std.log.info("No config file found. Creating example coral.json...", .{});
            json_parser.writeExample("coral.json") catch |err| {
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

/// Prints help information using the provided writer object.
fn printHelp(writer: anytype) !void {
    try writer.writeAll(
        \\coral - Yet Another Make, Zig edition
        \\
        \\Usage: coral [options] [targets...]
        \\
        \\Subcommands:
        \\  coral mcp              Start MCP server (STDIO transport)
        \\  coral ingest           Ingest a Turtle/N-Triples file into ~/.coral/context.db
        \\  coral compute-degree   Compute degree centrality and persist to database
        \\  coral compute-pagerank [--damping D] [--tolerance T] [--max-iter N]
        \\                         Compute PageRank scores and persist to database
        \\  coral compute-communities [--resolution R] [--max-iter N]
        \\                         Detect communities via Louvain and persist to database
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
        \\  coral compute-pagerank          Compute PageRank for all nodes
        \\  coral compute-communities      Detect communities via Louvain
        \\
    );
}
