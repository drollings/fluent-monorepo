const std = @import("std");
const simhash = @import("simhash");
const tokenizer = @import("common").tokenizer;

pub const Centroid = struct {
    id: []const u8,
    domain: []const u8,
    simhash_value: u64,
    member_count: u32,
    intent_domain: []const u8,
};

pub const CentroidIndex = struct {
    centroids: std.ArrayList(Centroid),
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator) CentroidIndex {
        return .{
            .centroids = .empty,
            .allocator = allocator,
        };
    }

    pub fn deinit(self: *CentroidIndex) void {
        for (self.centroids.items) |c| {
            self.allocator.free(c.id);
            self.allocator.free(c.domain);
            self.allocator.free(c.intent_domain);
        }
        self.centroids.deinit(self.allocator);
    }

    pub fn addCentroid(self: *CentroidIndex, id: []const u8, domain: []const u8, simhash_value: u64, member_count: u32, intent_domain: []const u8) !void {
        try self.centroids.append(self.allocator, .{
            .id = try self.allocator.dupe(u8, id),
            .domain = try self.allocator.dupe(u8, domain),
            .simhash_value = simhash_value,
            .member_count = member_count,
            .intent_domain = try self.allocator.dupe(u8, intent_domain),
        });
    }

    pub fn computeSimHashFromTokens(tokens: []const []const u8) u64 {
        return simhash.TokenSimHash.compute(tokens, 2);
    }

    pub fn computeSimHashFromText(text: []const u8) u64 {
        var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
        defer arena.deinit();
        const aa = arena.allocator();

        var tokens: std.ArrayList([]const u8) = .empty;
        defer tokens.deinit(aa);

        var tok: tokenizer.WordTokenizer = .{ .buf = text };
        while (tok.next()) |word| {
            if (word.len >= 2) {
                const duped = aa.dupe(u8, word) catch continue;
                try tokens.append(aa, duped);
            }
        }

        const token_slices = tokens.items;
        if (token_slices.len == 0) return 0;
        return computeSimHashFromTokens(token_slices);
    }

    pub const ClassificationResult = struct {
        domain: []const u8,
        confidence: f64,
        centroid_id: []const u8,
    };

    pub fn classifyDomain(self: *CentroidIndex, query: []const u8) ?ClassificationResult {
        if (self.centroids.items.len == 0) return null;

        const query_hash = computeSimHashFromText(query);
        if (query_hash == 0) return null;

        var best_centroid: ?*const Centroid = null;
        var best_distance: u7 = 64;

        for (self.centroids.items) |*c| {
            const distance = simhash.hammingDistance(query_hash, c.simhash_value);
            if (distance < best_distance) {
                best_distance = distance;
                best_centroid = c;
            }
        }

        const centroid = best_centroid orelse return null;

        const confidence: f64 = blk: {
            if (best_distance <= 3) break :blk 0.85 + @as(f64, @floatFromInt(3 - best_distance)) * 0.03;
            if (best_distance <= 7) break :blk 0.70 + @as(f64, @floatFromInt(7 - best_distance)) * 0.02;
            if (best_distance <= 10) break :blk 0.50 + @as(f64, @floatFromInt(10 - best_distance)) * 0.02;
            break :blk @max(0.1, 0.50 - @as(f64, @floatFromInt(best_distance - 10)) * 0.05);
        };

        return .{
            .domain = centroid.domain,
            .confidence = confidence,
            .centroid_id = centroid.id,
        };
    }

    pub fn hammingDistance(a: u64, b: u64) u7 {
        return simhash.hammingDistance(a, b);
    }
};

const testing = std.testing;

test "CentroidIndex classifyDomain" {
    var idx = CentroidIndex.init(testing.allocator);
    defer idx.deinit();

    try idx.addCentroid("coral-database", "coral", simhash.TokenSimHash.compute(&[_][]const u8{ "database", "vector", "search", "embedding" }, 2), 10, "identifier");
    try idx.addCentroid("guidance-query", "guidance", simhash.TokenSimHash.compute(&[_][]const u8{ "query", "explain", "search", "pipeline" }, 2), 8, "how-to");

    const result = idx.classifyDomain("database search");
    try testing.expect(result != null);
    try testing.expect(result.?.confidence > 0.5);
}

test "CentroidIndex computeSimHashFromText" {
    const hash1 = CentroidIndex.computeSimHashFromText("filterStages function pipeline");
    const hash2 = CentroidIndex.computeSimHashFromText("filterStages function pipeline");
    try testing.expect(hash1 == hash2);

    const hash3 = CentroidIndex.computeSimHashFromText("completely different content about cats");
    const distance = simhash.hammingDistance(hash1, hash3);
    try testing.expect(distance > 0);
}

test "CentroidIndex empty returns null" {
    var idx = CentroidIndex.init(testing.allocator);
    defer idx.deinit();
    try testing.expect(idx.classifyDomain("anything") == null);
}