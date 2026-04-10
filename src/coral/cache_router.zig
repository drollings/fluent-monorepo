//! cache_router.zig — ParallelRouter for batched concurrent query routing.
const std = @import("std");
const cache_reactor = @import("cache_reactor.zig");
const RoutingResult = cache_reactor.RoutingResult;
const QueueReactor = cache_reactor.QueueReactor;
const ContextNode = @import("coral_db").ContextNode;

pub const ParallelRouter = struct {
    const Self = @This();

    reactor: *QueueReactor,

    pub fn init(reactor: *QueueReactor) Self {
        return .{ .reactor = reactor };
    }

    pub fn routeBatch(
        self: *Self,
        allocator: std.mem.Allocator,
        queries: []const []const u8,
    ) ![]RoutingResult {
        if (queries.len == 0) return &[_]RoutingResult{};

        const results = try allocator.alloc(RoutingResult, queries.len);
        errdefer allocator.free(results);

        if (self.reactor.thread_pool) |pool| {
            const TaskCtx = struct {
                reactor: *QueueReactor,
                query: []const u8,
                result: *RoutingResult,
                err: ?anyerror = null,

                fn run(ctx: *@This()) void {
                    ctx.result.* = ctx.reactor.route(ctx.query) catch |err| blk: {
                        ctx.err = err;
                        break :blk RoutingResult{
                            .nodes = &[_]ContextNode{},
                            .tool_result = &[_]u8{},
                            .llm_response = &[_]u8{},
                            .tier_used = .l5_llm,
                            .latency_ms = 0,
                        };
                    };
                }
            };

            const ctxs = try allocator.alloc(TaskCtx, queries.len);
            defer allocator.free(ctxs);

            for (queries, 0..) |q, i| {
                ctxs[i] = .{
                    .reactor = self.reactor,
                    .query = q,
                    .result = &results[i],
                };
            }

            var wg: std.Thread.WaitGroup = .{};
            for (ctxs) |*ctx| {
                pool.spawnWg(&wg, TaskCtx.run, .{ctx});
            }
            pool.waitAndWork(&wg);
        } else {
            for (queries, 0..) |q, i| {
                results[i] = try self.reactor.route(q);
            }
        }

        return results;
    }
};
