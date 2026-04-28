//! cache_router.zig — ParallelRouter for batched query routing.
//! Thread pool dispatch is not available in Zig 0.16.0; queries run sequentially.
const std = @import("std");
const cache_reactor = @import("cache_reactor.zig");
const RoutingResult = cache_reactor.RoutingResult;
const QueueReactor = cache_reactor.QueueReactor;

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

        for (queries, 0..) |q, i| {
            results[i] = try self.reactor.route(q);
        }

        return results;
    }
};
