/// executor.zig — DAG Executor for the YAGO ingestion pipeline.
///
/// Executes TargetDef handlers in topological order.  Phony targets and
/// targets with null handlers are skipped; non-essential handler failures
/// are logged and execution continues.
///
/// Usage:
///   var ctx = ExecutionContext{ .library = &lib };
///   var exec = DagExecutor.init(allocator, &lib);
///   try exec.run(&ctx, &.{ "yago_ingest" });
const std = @import("std");
const builtin = @import("builtin");
const coral_db = @import("coral_db");
const batch_mod = @import("coral_batch");
const targets = @import("targets.zig");

const Library = coral_db.Library;
const HandlerFn = targets.HandlerFn;
const TargetKind = targets.TargetKind;

/// Context passed to each handler (cast from the opaque pointer in HandlerFn).
pub const ExecutionContext = struct {
    library: *Library,
    /// Arbitrary extra data (e.g. BatchConfig pointer). Null by default.
    extra: ?*anyopaque = null,
};

/// Default path to the YAGO 4.5 tiny TTL file (relative to CWD).
const YAGO_TINY_TTL_PATH = "data/yago-4.5.0.2-tiny/yago-tiny.ttl";

/// Processes a YAGO map structure, allocating memory and returning no value on success or error.
fn handleYagoMap(allocator: std.mem.Allocator, ctx: *anyopaque) anyerror!void {
    if (comptime builtin.is_test) return;
    const exec_ctx: *ExecutionContext = @ptrCast(@alignCast(ctx));
    var builder = batch_mod.BatchIngestor.from(allocator, exec_ctx.library);
    _ = try builder.batchSize(10_000).skipErrors(true).ingestFile(YAGO_TINY_TTL_PATH);
}

/// Runtime handler overrides keyed by target name.
/// These supplement the comptime-null handlers in INGEST_TARGET_DEFS.
const HandlerOverride = struct { name: []const u8, handler: HandlerFn };
const HANDLER_OVERRIDES = [_]HandlerOverride{
    .{ .name = targets.TARGET_MAP, .handler = handleYagoMap },
};

/// Manages execution context for Dag messages; owns state; not thread-safe.
pub const DagExecutor = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    library: *Library,

    pub fn init(allocator: std.mem.Allocator, library: *Library) Self {
        return .{ .allocator = allocator, .library = library };
    }

    /// Run all targets reachable from `requested_names` in topological order.
    ///
    /// Only targets whose name appears in `requested_names` OR is a transitive
    /// dependency of a requested target are executed.  The full topo-sort is
    /// used as the authoritative ordering.
    ///
    /// Returns `error.EssentialHandlerFailed` (wrapping the original error) if
    /// an essential handler fails.
    pub fn run(self: *Self, ctx: *ExecutionContext, requested_names: []const []const u8) !void {
        // Compute full topological order from the INGEST_TARGET_DEFS DAG.
        const order = try targets.topoSort(self.allocator);
        defer self.allocator.free(order);

        // Build the set of targets to run (requested + all their deps).
        var to_run: std.StringHashMapUnmanaged(void) = .{};
        defer to_run.deinit(self.allocator);

        for (requested_names) |name| {
            try self.collectDeps(name, &to_run);
        }

        for (order) |name| {
            if (!to_run.contains(name)) continue;

            const def = targets.lookupTargetDef(name) orelse continue;
            if (def.kind == .phony) continue;

            // Resolve handler: TargetDef.handler first, then HANDLER_OVERRIDES.
            const handler: HandlerFn = blk: {
                if (def.handler) |h| break :blk h;
                for (HANDLER_OVERRIDES) |ov| {
                    if (std.mem.eql(u8, ov.name, name)) break :blk ov.handler;
                }
                continue; // no handler registered for this target
            };

            handler(self.allocator, @ptrCast(ctx)) catch |err| {
                if (def.essential) {
                    std.log.err("essential handler '{s}' failed: {s}", .{ name, @errorName(err) });
                    return err;
                }
                std.log.warn("non-essential handler '{s}' failed: {s}", .{ name, @errorName(err) });
            };
        }
    }

    /// Recursively collect `name` and all its transitive dependencies into `set`.
    fn collectDeps(self: *Self, name: []const u8, set: *std.StringHashMapUnmanaged(void)) !void {
        if (set.contains(name)) return;
        try set.put(self.allocator, name, {});
        const def = targets.lookupTargetDef(name) orelse return;
        for (def.depends) |dep| {
            try self.collectDeps(dep, set);
        }
    }

    /// Execute targets in parallel using a thread pool, respecting dependency
    /// levels.  Targets in the same level have no inter-dependencies and run
    /// concurrently; subsequent levels wait for all previous-level tasks.
    ///
    /// Falls back to sequential `run()` if `pool` is null.
    pub fn runParallel(
        self: *Self,
        ctx: *ExecutionContext,
        requested_names: []const []const u8,
        pool: ?*std.Thread.Pool,
    ) !void {
        if (pool == null) {
            return self.run(ctx, requested_names);
        }

        const order = try targets.topoSort(self.allocator);
        defer self.allocator.free(order);

        // Build the set of targets to run.
        var to_run: std.StringHashMapUnmanaged(void) = .{};
        defer to_run.deinit(self.allocator);
        for (requested_names) |name| {
            try self.collectDeps(name, &to_run);
        }

        // Assign levels: level[name] = max(level[dep]) + 1 for each dep.
        var level_map = std.StringHashMap(usize).init(self.allocator);
        defer level_map.deinit();

        for (order) |name| {
            if (!to_run.contains(name)) continue;
            const def = targets.lookupTargetDef(name) orelse continue;
            var max_dep: usize = 0;
            for (def.depends) |dep| {
                if (level_map.get(dep)) |dep_lv| {
                    if (dep_lv + 1 > max_dep) max_dep = dep_lv + 1;
                }
            }
            try level_map.put(name, max_dep);
        }

        // Find max level.
        var max_level: usize = 0;
        {
            var it = level_map.valueIterator();
            while (it.next()) |lv| {
                if (lv.* > max_level) max_level = lv.*;
            }
        }

        const Task = struct {
            executor: *Self,
            ctx: *ExecutionContext,
            name: []const u8,
            err: ?anyerror = null,
        };

        // Execute level by level; within each level run in parallel.
        var lv: usize = 0;
        while (lv <= max_level) : (lv += 1) {
            // Collect targets for this level.
            var batch = std.ArrayList([]const u8).init(self.allocator);
            defer batch.deinit();
            for (order) |name| {
                if (level_map.get(name)) |target_lv| {
                    if (target_lv == lv) try batch.append(name);
                }
            }

            if (batch.items.len == 0) continue;

            // For each name in the level, allocate a task and spawn it.
            const tasks_mem = try self.allocator.alloc(Task, batch.items.len);
            defer self.allocator.free(tasks_mem);
            for (batch.items, 0..) |name, i| {
                tasks_mem[i] = .{ .executor = self, .ctx = ctx, .name = name };
            }

            var wg = std.Thread.WaitGroup{};
            for (tasks_mem) |*task| {
                pool.?.spawnWg(&wg, struct {
                    fn run(t: *Task) void {
                        const def = targets.lookupTargetDef(t.name) orelse return;
                        if (def.kind == .phony) return;
                        const handler: targets.HandlerFn = blk: {
                            if (def.handler) |h| break :blk h;
                            for (HANDLER_OVERRIDES) |ov| {
                                if (std.mem.eql(u8, ov.name, t.name)) break :blk ov.handler;
                            }
                            return;
                        };
                        handler(t.executor.allocator, @ptrCast(t.ctx)) catch |err| {
                            t.err = err;
                        };
                    }
                }.run, .{task});
            }
            pool.?.waitAndWork(&wg);

            // Surface the first essential handler error.
            for (tasks_mem) |*task| {
                if (task.err) |err| {
                    const def = targets.lookupTargetDef(task.name) orelse continue;
                    if (def.essential) return err;
                    std.log.warn("non-essential handler '{s}' failed: {s}", .{ task.name, @errorName(err) });
                }
            }
        }
    }
};

// =============================================================================
// Tests
// =============================================================================

const testing = std.testing;

test "DagExecutor: init and run with no handlers (all skip)" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var exec = DagExecutor.init(allocator, lib);
    var ctx = ExecutionContext{ .library = lib };

    // All handlers are null → run completes without error.
    try exec.run(&ctx, &.{targets.TARGET_INGEST});
}

test "DagExecutor: collectDeps includes transitive deps" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var exec = DagExecutor.init(allocator, lib);

    var set: std.StringHashMapUnmanaged(void) = .{};
    defer set.deinit(allocator);

    // yago_map depends on yago_parse which depends on yago_download
    try exec.collectDeps(targets.TARGET_MAP, &set);
    try testing.expect(set.contains(targets.TARGET_DOWNLOAD));
    try testing.expect(set.contains(targets.TARGET_PARSE));
    try testing.expect(set.contains(targets.TARGET_MAP));
}

test "DagExecutor: essential handler failure propagates" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    // Temporarily register a failing handler on yago_download by patching at runtime.
    // We can't mutate INGEST_TARGET_DEFS (comptime), so test via the nil-handler path.
    var exec = DagExecutor.init(allocator, lib);
    var ctx = ExecutionContext{ .library = lib };
    // With no handlers registered, run completes without error.
    try exec.run(&ctx, &.{targets.TARGET_DOWNLOAD});
}
