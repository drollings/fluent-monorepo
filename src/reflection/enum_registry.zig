/// enum_registry.zig — EnumRegistry for runtime enum name/value lookups.
const std = @import("std");

/// Manages enum definitions with compile-time registration; owns registry state; ensures type safety and ownership clarity.
pub const EnumRegistry = struct {
    name_to_index: std.StringHashMapUnmanaged(usize),
    index_to_name: std.ArrayListUnmanaged([]const u8),
    index_to_value: std.ArrayListUnmanaged(i64),

    pub fn init(allocator: std.mem.Allocator) EnumRegistry {
        _ = allocator;
        return .{
            .name_to_index = .{},
            .index_to_name = .{},
            .index_to_value = .{},
        };
    }

    pub fn deinit(self: *EnumRegistry, allocator: std.mem.Allocator) void {
        self.name_to_index.deinit(allocator);
        self.index_to_name.deinit(allocator);
        self.index_to_value.deinit(allocator);
    }

    pub fn registerEnum(self: *EnumRegistry, allocator: std.mem.Allocator, comptime T: type) !void {
        const info = @typeInfo(T);
        if (info != .@"enum") return error.NotAnEnum;
        _ = info.@"enum";

        inline for (std.meta.tags(T)) |tag| {
            const name = @tagName(tag);
            const value: i64 = @intCast(@intFromEnum(tag));

            const gop = try self.name_to_index.getOrPut(allocator, name);
            if (!gop.found_existing) {
                const idx = self.index_to_name.items.len;
                try self.index_to_name.append(allocator, name);
                try self.index_to_value.append(allocator, value);
                gop.value_ptr.* = idx;
            }
        }
    }

    pub fn nameToValue(self: *const EnumRegistry, name: []const u8) ?i64 {
        const idx = self.name_to_index.get(name) orelse return null;
        return self.index_to_value.items[idx];
    }

    pub fn valueToName(self: *const EnumRegistry, value: i64) ?[]const u8 {
        for (self.index_to_value.items, 0..) |v, i| {
            if (v == value) return self.index_to_name.items[i];
        }
        return null;
    }
};
