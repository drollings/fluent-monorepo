/// binary.zig — BinaryFieldCodec for wire-format encoding/decoding of struct fields.
const std = @import("std");

/// Encodes binary data structures; manages ownership and invariants; not thread-safe.
pub const BinaryFieldCodec = struct {
    pub fn encodeField(comptime T: type, value: T, writer: anytype) !void {
        switch (@typeInfo(T)) {
            .int => |i| {
                switch (i.bits) {
                    8 => try writer.writeByte(@intCast(value)),
                    16 => try writer.writeInt(u16, @intCast(value), .little),
                    32 => try writer.writeInt(u32, @intCast(value), .little),
                    64 => try writer.writeInt(u64, @intCast(value), .little),
                    else => return error.UnsupportedIntSize,
                }
            },
            .float => try writer.writeAll(std.mem.asBytes(&value)),
            .bool => try writer.writeByte(if (value) 1 else 0),
            .@"enum" => {
                const int_val: u32 = @intCast(@intFromEnum(value));
                try writer.writeInt(u32, int_val, .little);
            },
            .pointer => |p| {
                if (p.size == .slice and p.child == u8) {
                    const slice: []const u8 = value;
                    try writer.writeInt(u32, @intCast(slice.len), .little);
                    try writer.writeAll(slice);
                }
            },
            else => return error.UnsupportedType,
        }
    }

    pub fn decodeField(comptime T: type, reader: anytype, allocator: std.mem.Allocator) !T {
        switch (@typeInfo(T)) {
            .int => |i| {
                return switch (i.bits) {
                    8 => @intCast(try reader.readByte()),
                    16 => @intCast(try reader.readInt(u16, .little)),
                    32 => @intCast(try reader.readInt(u32, .little)),
                    64 => @intCast(try reader.readInt(u64, .little)),
                    else => error.UnsupportedIntSize,
                };
            },
            .float => {
                var buf: [@sizeOf(T)]u8 = undefined;
                try reader.readNoEof(&buf);
                return @bitCast(buf);
            },
            .bool => return (try reader.readByte()) != 0,
            .@"enum" => {
                const int_val = try reader.readInt(u32, .little);
                return @enumFromInt(int_val);
            },
            .pointer => |p| {
                if (p.size == .slice and p.child == u8) {
                    const len = try reader.readInt(u32, .little);
                    const slice = try allocator.alloc(u8, len);
                    try reader.readNoEof(slice);
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

