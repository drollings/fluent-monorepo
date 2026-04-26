const std = @import("std");

pub const SharedString = struct {
    data: []const u8,
    rc: *RefCount,

    pub const RefCount = struct {
        count: usize,
        allocator: std.mem.Allocator,
    };

    pub const Ref = SharedString;

    pub fn init(allocator: std.mem.Allocator, data: []const u8) !SharedString {
        const rc = try allocator.create(RefCount);
        rc.* = .{ .count = 1, .allocator = allocator };
        errdefer allocator.destroy(rc);
        const copy = try allocator.dupe(u8, data);
        return .{ .data = copy, .rc = rc };
    }

    pub fn asSlice(self: *const SharedString) []const u8 {
        return self.data;
    }

    pub fn retain(self: *const SharedString) SharedString {
        self.rc.count += 1;
        return .{ .data = self.data, .rc = self.rc };
    }

    pub fn release(self: *const SharedString, allocator: std.mem.Allocator) void {
        _ = allocator;
        self.rc.count -= 1;
        if (self.rc.count == 0) {
            self.rc.allocator.free(self.data);
            self.rc.allocator.destroy(self.rc);
        }
    }

    pub fn slice(self: *const SharedString) []const u8 {
        return self.data;
    }
};
