/// binary.zig — BinaryFieldCodec for wire-format encoding/decoding of struct fields.
const std = @import("std");

pub const BinaryFieldCodec = struct {
    pub fn encodeField(comptime T: type, value: T, writer: *std.Io.Writer) !void {
        switch (@typeInfo(T)) {
            .int => |i| {
                switch (i.bits) {
                    8 => try writer.writeByte(@intCast(value)),
                    16 => {
                        var b: [2]u8 = undefined;
                        std.mem.writeInt(u16, &b, @intCast(value), .little);
                        try writer.writeAll(&b);
                    },
                    32 => {
                        var b: [4]u8 = undefined;
                        std.mem.writeInt(u32, &b, @intCast(value), .little);
                        try writer.writeAll(&b);
                    },
                    64 => {
                        var b: [8]u8 = undefined;
                        std.mem.writeInt(u64, &b, @intCast(value), .little);
                        try writer.writeAll(&b);
                    },
                    else => return error.UnsupportedIntSize,
                }
            },
            .float => try writer.writeAll(std.mem.asBytes(&value)),
            .bool => try writer.writeByte(if (value) 1 else 0),
            .@"enum" => {
                const int_val: u32 = @intCast(@intFromEnum(value));
                var b: [4]u8 = undefined;
                std.mem.writeInt(u32, &b, int_val, .little);
                try writer.writeAll(&b);
            },
            .pointer => |p| {
                if (p.size == .slice and p.child == u8) {
                    const slice: []const u8 = value;
                    var b: [4]u8 = undefined;
                    std.mem.writeInt(u32, &b, @intCast(slice.len), .little);
                    try writer.writeAll(&b);
                    try writer.writeAll(slice);
                }
            },
            else => return error.UnsupportedType,
        }
    }

    pub fn decodeField(comptime T: type, reader: *std.Io.Reader, allocator: std.mem.Allocator) !T {
        switch (@typeInfo(T)) {
            .int => |i| {
                return switch (i.bits) {
                    8 => blk: {
                        var b: [1]u8 = undefined;
                        try reader.readSliceAll(&b);
                        break :blk @intCast(b[0]);
                    },
                    16 => blk: {
                        var b: [2]u8 = undefined;
                        try reader.readSliceAll(&b);
                        break :blk @intCast(std.mem.readInt(u16, &b, .little));
                    },
                    32 => blk: {
                        var b: [4]u8 = undefined;
                        try reader.readSliceAll(&b);
                        break :blk @intCast(std.mem.readInt(u32, &b, .little));
                    },
                    64 => blk: {
                        var b: [8]u8 = undefined;
                        try reader.readSliceAll(&b);
                        break :blk @intCast(std.mem.readInt(u64, &b, .little));
                    },
                    else => error.UnsupportedIntSize,
                };
            },
            .float => {
                var buf: [@sizeOf(T)]u8 = undefined;
                try reader.readSliceAll(&buf);
                return @bitCast(buf);
            },
            .bool => {
                var b: [1]u8 = undefined;
                try reader.readSliceAll(&b);
                return b[0] != 0;
            },
            .@"enum" => {
                var b: [4]u8 = undefined;
                try reader.readSliceAll(&b);
                const int_val = std.mem.readInt(u32, &b, .little);
                return @enumFromInt(int_val);
            },
            .pointer => |p| {
                if (p.size == .slice and p.child == u8) {
                    var lb: [4]u8 = undefined;
                    try reader.readSliceAll(&lb);
                    const len = std.mem.readInt(u32, &lb, .little);
                    const slice = try allocator.alloc(u8, len);
                    try reader.readSliceAll(slice);
                    return slice;
                }
                return error.UnsupportedPointerType;
            },
            else => return error.UnsupportedType,
        }
    }

    pub fn fieldWireSize(comptime T: type) ?usize {
        return switch (@typeInfo(T)) {
            .int => |i| switch (i.bits) {
                8 => 1,
                16 => 2,
                32 => 4,
                64 => 8,
                else => null,
            },
            .float => |f| f.bits / 8,
            .bool => 1,
            .@"enum" => 4,
            else => null,
        };
    }
};
