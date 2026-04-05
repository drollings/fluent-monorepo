/// dag_executor.zig — M6.1 Parallel DAG Execution
///
/// Execute independent DAG nodes concurrently using Kahn's algorithm
/// with parallel dispatch.
///
/// Thread model: Thread pool for concurrent node execution.
/// Safety: All shared state protected by mutex.
const std = @import("std");

/// Represents a fixed-size buffer in the dag executor, managed by the system, with shared ownership and no thread safety.
pub const DagNode = struct {
    id: i64,
    /// IDs of nodes this node depends on (must complete before this)
    depends: []const i64,
    /// IDs of nodes this node provides (enables after completion)
    provides: []const i64,
    /// Execution status
    status: Status,

    pub const Status = enum {
        pending,
        running,
        completed,
        failed,
    };
};

/// Represents a result structure from a Dag protocol execution, managing ownership and invariants.
pub const DagResult = struct {
    node_id: i64,
    success: bool,
    output: []const u8,
    error_message: ?[]const u8,
};

/// Manages callback registration and invocation; owns DagCallbacks instance; ensures callbacks are properly tracked and invoked.
pub const DagCallbacks = struct {
    ctx: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        /// Execute a single node. Called from worker thread.
        execute: *const fn (ctx: *anyopaque, node_id: i64, allocator: std.mem.Allocator) anyerror!DagResult,
        /// Called when node starts execution.
        onNodeStart: ?*const fn (ctx: *anyopaque, node_id: i64) void = null,
        /// Called when node completes (success or failure).
        onNodeComplete: ?*const fn (ctx: *anyopaque, result: DagResult) void = null,
    };

    pub fn execute(self: DagCallbacks, node_id: i64, allocator: std.mem.Allocator) anyerror!DagResult {
        return self.vtable.execute(self.ctx, node_id, allocator);
    }

    pub fn onNodeStart(self: DagCallbacks, node_id: i64) void {
        if (self.vtable.onNodeStart) |f| f(self.ctx, node_id);
    }

    pub fn onNodeComplete(self: DagCallbacks, result: DagResult) void {
        if (self.vtable.onNodeComplete) |f| f(self.ctx, result);
    }
};

/// Manages DagExecutor logic, owns execution context, ensures consistent state across invocations.
pub const DagExecutor = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    workers: usize,
    callbacks: DagCallbacks,
    /// Shared state
    mu: std.Thread.Mutex = .{},
    /// Nodes by ID
    nodes: std.AutoHashMapUnmanaged(i64, DagNode),
    /// Completed nodes
    completed: std.AutoHashMapUnmanaged(i64, void),
    /// Failed nodes
    failed: std.AutoHashMapUnmanaged(i64, []const u8),
    /// Results from all executions
    results: std.ArrayListUnmanaged(DagResult),
    /// Condition variable for completion
    cv: std.Thread.Condition = .{},
    /// Running task count
    running: usize = 0,
    /// First error encountered
    first_error: ?anyerror = null,

    pub fn init(
        allocator: std.mem.Allocator,
        workers: usize,
        callbacks: DagCallbacks,
    ) Self {
        return .{
            .allocator = allocator,
            .workers = workers,
            .callbacks = callbacks,
            .nodes = .{},
            .completed = .{},
            .failed = .{},
            .results = .{},
        };
    }

    pub fn deinit(self: *Self) void {
        self.nodes.deinit(self.allocator);
        self.completed.deinit(self.allocator);
        var it = self.failed.iterator();
        while (it.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
        }
        self.failed.deinit(self.allocator);
        for (self.results.items) |r| {
            if (r.output.len > 0) self.allocator.free(@constCast(r.output));
            if (r.error_message) |m| self.allocator.free(@constCast(m));
        }
        self.results.deinit(self.allocator);
    }

    /// Add a node to the DAG.
    pub fn addNode(self: *Self, node: DagNode) !void {
        self.mu.lock();
        defer self.mu.unlock();
        try self.nodes.put(self.allocator, node.id, node);
    }

    /// Execute the DAG. Returns all results.
    /// Returns error if any node fails.
    pub fn execute(self: *Self) ![]DagResult {
        var pool: std.Thread.Pool = undefined;
        try pool.init(.{
            .allocator = self.allocator,
            .n_jobs = @min(self.workers, std.Thread.getCpuCount() orelse 4),
        });
        defer pool.deinit();

        // Find all nodes with no dependencies (input set).
        var ready: std.ArrayListUnmanaged(i64) = .{};
        defer ready.deinit(self.allocator);

        {
            self.mu.lock();
            defer self.mu.unlock();
            for (self.nodes.keys(), self.nodes.values()) |id, node| {
                if (node.depends.len == 0) {
                    try ready.append(self.allocator, id);
                }
            }
        }

        // Dispatch ready nodes repeatedly until all done.
        while (true) {
            self.mu.lock();
            const all_done = self.completed.count() + self.failed.count() >= self.nodes.count();
            const has_error = self.first_error != null;
            self.mu.unlock();

            if (has_error) {
                // Cancel all pending tasks.
                pool.deinit();
                return self.first_error.?;
            }
            if (all_done) break;

            // Dispatch all ready nodes.
            {
                self.mu.lock();
                defer self.mu.unlock();

                for (ready.items) |node_id| {
                    try self.dispatchNode(&pool, node_id);
                }
                ready.clearRetainingCapacity();
            }

            // Wait for some completions.
            self.cv.wait(self.mu) catch {};
            self.mu.unlock();

            // Find newly ready nodes.
            self.mu.lock();
            defer self.mu.unlock();

            for (self.nodes.keys(), self.nodes.values()) |id, node| {
                if (self.completed.contains(id)) continue;
                if (self.failed.contains(id)) continue;

                var deps_satisfied = true;
                for (node.depends) |dep_id| {
                    if (!self.completed.contains(dep_id)) {
                        deps_satisfied = false;
                        break;
                    }
                }

                if (deps_satisfied) {
                    try ready.append(self.allocator, id);
                }
            }
        }

        return try self.results.toOwnedSlice(self.allocator);
    }

    fn dispatchNode(self: *Self, pool: *std.Thread.Pool, node_id: i64) !void {
        self.running += 1;
        const ctx = try self.allocator.create(TaskContext);
        ctx.* = .{
            .executor = self,
            .node_id = node_id,
        };
        try pool.spawn(TaskContext, ctx, runTask);
    }

    fn runTask(ctx: *TaskContext) void {
        const self = ctx.executor;
        const node_id = ctx.node_id;
        self.allocator.destroy(ctx);

        // Execute the node.
        self.callbacks.onNodeStart(node_id);

        var arena = std.heap.ArenaAllocator.init(self.allocator);
        defer arena.deinit();

        var result = self.callbacks.execute(node_id, arena.allocator()) catch |err| blk: {
            var err_msg: ?[]const u8 = null;
            if (self.mu.tryLock()) {
                defer self.mu.unlock();
                if (self.first_error == null) {
                    self.first_error = err;
                }
                err_msg = try std.fmt.allocPrint(self.allocator, "execution failed: {}", .{err});
            }
            break :blk DagResult{
                .node_id = node_id,
                .success = false,
                .output = "",
                .error_message = err_msg,
            };
        };

        // Append result.
        {
            self.mu.lock();
            defer self.mu.unlock();

            if (result.success) {
                self.completed.putAssumeCapacity(node_id, {});
            } else {
                const owned_id = try self.allocator.dupe(i64, &[_]i64{node_id});
                try self.failed.putAssumeCapacity(owned_id[0], result.error_message orelse "unknown error");
            }

            // Copy output if needed.
            if (result.output.len > 0) {
                const owned_output = try self.allocator.dupe(u8, result.output);
                result.output = owned_output;
            }

            try self.results.append(self.allocator, result);
            self.callbacks.onNodeComplete(result);
            self.running -= 1;
            self.cv.signal();
        }
    }

    const TaskContext = struct {
        executor: *DagExecutor,
        node_id: i64,
    };
};

// =============================================================================
// Tests — M6.1
// =============================================================================

const testing = std.testing;

test "DagNode: init pending" {
    const node = DagNode{
        .id = 1,
        .depends = &[_]i64{},
        .provides = &[_]i64{},
        .status = .pending,
    };
    try testing.expectEqual(@as(i64, 1), node.id);
    try testing.expectEqual(DagNode.Status.pending, node.status);
}

test "DagExecutor: empty DAG" {
    const TestContext = struct {
        fn execute(ctx: *anyopaque, node_id: i64, allocator: std.mem.Allocator) anyerror!DagResult {
            _ = ctx;
            _ = node_id;
            _ = allocator;
            return DagResult{ .node_id = 0, .success = true, .output = "", .error_message = null };
        }
    };

    var vtable = DagCallbacks.VTable{ .execute = TestContext.execute };
    const callbacks = DagCallbacks{ .ctx = @constCast(&vtable), .vtable = @constCast(&vtable) };

    var executor = DagExecutor.init(testing.allocator, 4, callbacks);
    defer executor.deinit();

    const results = try executor.execute();
    try testing.expectEqual(@as(usize, 0), results.len);
}

test "DagExecutor: single node" {
    const TestContext = struct {
        fn execute(ctx: *anyopaque, node_id: i64, allocator: std.mem.Allocator) anyerror!DagResult {
            _ = ctx;
            _ = allocator;
            return DagResult{ .node_id = node_id, .success = true, .output = "done", .error_message = null };
        }
    };

    var vtable = DagCallbacks.VTable{ .execute = TestContext.execute };
    const callbacks = DagCallbacks{ .ctx = @constCast(&vtable), .vtable = @constCast(&vtable) };

    var executor = DagExecutor.init(testing.allocator, 4, callbacks);
    defer executor.deinit();

    try executor.addNode(.{ .id = 1, .depends = &[_]i64{}, .provides = &[_]i64{}, .status = .pending });

    const results = try executor.execute();
    try testing.expectEqual(@as(usize, 1), results.len);
    try testing.expectEqual(@as(i64, 1), results[0].node_id);
    try testing.expect(results[0].success);
}
