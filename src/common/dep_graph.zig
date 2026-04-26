const std = @import("std");

pub const DependencyGraph = struct {
    forward: std.StringHashMap([]const []const u8),
    reverse: std.StringHashMap(std.StringHashMap(void)),
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator) DependencyGraph {
        return .{
            .forward = std.StringHashMap([]const []const u8).init(allocator),
            .reverse = std.StringHashMap(std.StringHashMap(void)).init(allocator),
            .allocator = allocator,
        };
    }

    pub fn deinit(self: *DependencyGraph) void {
        var fwd_iter = self.forward.iterator();
        while (fwd_iter.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            for (entry.value_ptr.*) |dep| self.allocator.free(dep);
            self.allocator.free(entry.value_ptr.*);
        }
        self.forward.deinit();

        var rev_iter = self.reverse.iterator();
        while (rev_iter.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            entry.value_ptr.deinit();
        }
        self.reverse.deinit();
    }

    pub fn setDeps(self: *DependencyGraph, path: []const u8, deps: []const []const u8) !void {
        const owned_path = try self.allocator.dupe(u8, path);

        if (self.forward.fetchRemove(owned_path)) |removed| {
            self.allocator.free(removed.key);
            self.allocator.free(removed.value);
        }
        for (deps) |dep| {
            _ = self.removeReverse(dep, owned_path);
        }

        const owned_deps = try self.allocator.alloc([]const u8, deps.len);
        for (deps, 0..) |dep, i| {
            owned_deps[i] = try self.allocator.dupe(u8, dep);
        }
        try self.forward.put(owned_path, owned_deps);

        var base_name_buf: [256]u8 = undefined;
        for (deps) |dep| {
            const gop = try self.reverse.getOrPut(dep);
            if (!gop.found_existing) {
                gop.key_ptr.* = try self.allocator.dupe(u8, dep);
                gop.value_ptr.* = std.StringHashMap(void).init(self.allocator);
            }
            try gop.value_ptr.put(owned_path, {});

            const basename = std.fs.path.basename(dep);
            if (basename.len != dep.len and basename.len > 0) {
                const bname = std.fmt.bufPrint(&base_name_buf, "{s}", .{basename}) catch basename;
                const bname_duped = try self.allocator.dupe(u8, bname);
                const bgop = try self.reverse.getOrPut(bname_duped);
                if (!bgop.found_existing) {
                    bgop.key_ptr.* = bname_duped;
                    bgop.value_ptr.* = std.StringHashMap(void).init(self.allocator);
                } else {
                    self.allocator.free(bname_duped);
                }
                try bgop.value_ptr.put(owned_path, {});
            }
        }
    }

    pub fn remove(self: *DependencyGraph, path: []const u8) void {
        var fwd_key_to_free: ?[]const u8 = null;
        var fwd_vals_to_free: ?[]const []const u8 = null;

        if (self.forward.fetchRemove(path)) |removed| {
            const fwd_deps = removed.value;
            for (fwd_deps) |dep| {
                _ = self.removeReverse(dep, removed.key);
            }

            var rev_iter = self.reverse.iterator();
            while (rev_iter.next()) |entry| {
                _ = entry.value_ptr.remove(path);
            }

            for (fwd_deps) |dep| {
                self.allocator.free(dep);
            }
            fwd_key_to_free = removed.key;
            fwd_vals_to_free = fwd_deps;
        }

        var keys_to_remove_buf: [64][]const u8 = undefined;
        var keys_to_remove_len: usize = 0;
        var rev_iter2 = self.reverse.iterator();
        while (rev_iter2.next()) |entry| {
            if (entry.value_ptr.count() == 0) {
                if (keys_to_remove_len < keys_to_remove_buf.len) {
                    keys_to_remove_buf[keys_to_remove_len] = entry.key_ptr.*;
                    keys_to_remove_len += 1;
                }
            }
        }
        for (keys_to_remove_buf[0..keys_to_remove_len]) |k| {
            if (self.reverse.fetchRemove(k)) |removed| {
                var val = removed.value;
                val.deinit();
                self.allocator.free(removed.key);
            }
        }

        if (fwd_key_to_free) |k| self.allocator.free(k);
        if (fwd_vals_to_free) |v| self.allocator.free(v);
    }

    fn removeReverse(self: *DependencyGraph, dep: []const u8, path: []const u8) bool {
        if (self.reverse.getEntry(dep)) |entry| {
            _ = entry.value_ptr.remove(path);
            return true;
        }
        return false;
    }

    pub fn getImportedBy(self: *DependencyGraph, path: []const u8, allocator: std.mem.Allocator) ![]const []const u8 {
        var result: std.ArrayList([]const u8) = .empty;
        errdefer {
            for (result.items) |item| allocator.free(item);
            result.deinit(allocator);
        }

        if (self.reverse.getEntry(path)) |entry| {
            var iter = entry.value_ptr.keyIterator();
            while (iter.next()) |k| {
                const duped = try allocator.dupe(u8, k.*);
                try result.append(allocator, duped);
            }
        }

        const basename = std.fs.path.basename(path);
        if (basename.len != path.len and !std.mem.eql(u8, basename, path)) {
            if (self.reverse.getEntry(basename)) |entry| {
                var iter = entry.value_ptr.keyIterator();
                while (iter.next()) |k| {
                    const duped = try allocator.dupe(u8, k.*);
                    try result.append(allocator, duped);
                }
            }
        }

        return result.toOwnedSlice(allocator);
    }

    pub fn getTransitiveDependents(self: *DependencyGraph, path: []const u8, allocator: std.mem.Allocator, max_depth: u32) ![]const []const u8 {
        var seen = std.StringHashMap(void).init(allocator);
        defer {
            var it = seen.keyIterator();
            while (it.next()) |k| allocator.free(k.*);
            seen.deinit();
        }

        var result: std.ArrayList([]const u8) = .empty;
        errdefer {
            for (result.items) |item| allocator.free(item);
            result.deinit(allocator);
        }

        var queue: std.ArrayList([]const u8) = .empty;
        defer queue.deinit(allocator);
        try queue.append(allocator, path);

        var depth: u32 = 0;
        while (queue.items.len > 0 and depth < max_depth) : (depth += 1) {
            const current = queue.pop();
            const imported_by = try self.getImportedBy(current.?, allocator);
            defer allocator.free(imported_by);
            for (imported_by) |dep| {
                const gop = try seen.getOrPut(dep);
                if (!gop.found_existing) {
                    gop.key_ptr.* = try allocator.dupe(u8, dep);
                    const duped = try allocator.dupe(u8, dep);
                    try result.append(allocator, duped);
                    try queue.append(allocator, dep);
                } else {
                    allocator.free(dep);
                }
            }
        }

        return result.toOwnedSlice(allocator);
    }
};

const testing = std.testing;

test "DependencyGraph setDeps and getImportedBy" {
    var graph = DependencyGraph.init(testing.allocator);
    defer graph.deinit();

    try graph.setDeps("src/main.zig", &[_][]const u8{ "src/utils.zig", "src/types.zig" });
    try graph.setDeps("src/app.zig", &[_][]const u8{ "src/utils.zig" });

    const imported_by = try graph.getImportedBy("src/utils.zig", testing.allocator);
    defer {
        for (imported_by) |item| testing.allocator.free(item);
        testing.allocator.free(imported_by);
    }
    try testing.expect(imported_by.len >= 2);
}

test "DependencyGraph remove" {
    var graph = DependencyGraph.init(testing.allocator);
    defer graph.deinit();

    try graph.setDeps("src/main.zig", &[_][]const u8{ "src/utils.zig" });

    const before = try graph.getImportedBy("src/utils.zig", testing.allocator);
    defer {
        for (before) |item| testing.allocator.free(item);
        testing.allocator.free(before);
    }
    try testing.expect(before.len >= 1);

    graph.remove("src/main.zig");

    const imported_by = try graph.getImportedBy("src/utils.zig", testing.allocator);
    defer {
        for (imported_by) |item| testing.allocator.free(item);
        testing.allocator.free(imported_by);
    }
    try testing.expectEqual(@as(usize, 0), imported_by.len);
}

test "DependencyGraph getTransitiveDependents" {
    var graph = DependencyGraph.init(testing.allocator);
    defer graph.deinit();

    try graph.setDeps("src/main.zig", &[_][]const u8{ "src/utils.zig" });
    try graph.setDeps("src/app.zig", &[_][]const u8{ "src/main.zig" });

    const transitive = try graph.getTransitiveDependents("src/utils.zig", testing.allocator, 5);
    defer {
        for (transitive) |item| testing.allocator.free(item);
        testing.allocator.free(transitive);
    }
    try testing.expect(transitive.len >= 1);
}