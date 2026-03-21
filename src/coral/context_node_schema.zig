const std = @import("std");
const reflection = @import("common").reflection;
const coral_db = @import("coral_db");
const ContextNode = coral_db.ContextNode;
const schema = coral_db.schema;

pub const BINARY_SCHEMA_VERSION: u32 = 1;
pub const BINARY_MAGIC: [4]u8 = .{ 'C', 'C', 'N', 'D' };

pub const PayloadType = enum(u32) {
    context_node = 1,
    execution_request = 2,
    execution_result = 3,
    error_response = 4,
    host_function_call = 5,
    host_function_result = 6,
};

pub const BinaryHeader = extern struct {
    magic: [4]u8 align(1),
    version: u32 align(1),
    payload_type: PayloadType align(1),
    payload_size: u32 align(1),
    checksum: u32 align(1),

    pub fn init(payload_type: PayloadType, size: u32) BinaryHeader {
        return .{
            .magic = BINARY_MAGIC,
            .version = BINARY_SCHEMA_VERSION,
            .payload_type = payload_type,
            .payload_size = size,
            .checksum = 0,
        };
    }

    pub fn validate(self: *const BinaryHeader) bool {
        return std.mem.eql(u8, &self.magic, &BINARY_MAGIC) and
            self.version == BINARY_SCHEMA_VERSION;
    }
};

pub const BinaryContextNode = extern struct {
    header: BinaryHeader align(1),
    id: i64 align(1),
    valid_from_ts: i64 align(1),
    valid_to_ts: i64 align(1),
    confidence: i32 align(1),
    provenance_id: i32 align(1),
    lod_offsets: [schema.LOD_COUNT]u32 align(1),
    lod_lengths: [schema.LOD_COUNT]u32 align(1),

    /// Convenience accessor for backward compatibility with wasm.zig tests
    pub fn getId(self: *const BinaryContextNode) i64 {
        return self.id;
    }

    pub fn init(node: *const ContextNode, buffer: []u8) BinaryContextNode {
        var offset: usize = @sizeOf(BinaryContextNode);
        var result: BinaryContextNode = undefined;
        result.header = BinaryHeader.init(.context_node, 0);
        result.id = node.id;
        result.valid_from_ts = @intFromFloat(node.valid_from);
        result.valid_to_ts = if (node.valid_to) |vt| @intFromFloat(vt) else 0;
        result.confidence = node.confidence;
        result.provenance_id = node.provenance_id;
        for (0..schema.LOD_COUNT) |i| {
            result.lod_offsets[i] = @intCast(offset);
            result.lod_lengths[i] = @intCast(node.lod[i].len);
            @memcpy(buffer[offset..][0..node.lod[i].len], node.lod[i]);
            offset += node.lod[i].len;
        }
        result.header.payload_size = @intCast(offset);
        return result;
    }

    pub fn getLod(self: *const BinaryContextNode, buffer: []const u8, level: u3) []const u8 {
        if (level >= schema.LOD_COUNT) return &[_]u8{};
        return buffer[self.lod_offsets[level]..][0..self.lod_lengths[level]];
    }

    pub fn writeToBuffer(node: *const ContextNode, buffer: []u8) usize {
        const bin = init(node, buffer);
        const header_size = @sizeOf(BinaryContextNode);
        @memcpy(buffer[0..header_size], std.mem.asBytes(&bin));
        return bin.header.payload_size;
    }

    pub fn readFromBuffer(
        allocator: std.mem.Allocator,
        buffer: []const u8,
    ) !ContextNode {
        if (buffer.len < @sizeOf(BinaryContextNode)) return error.BufferTooSmall;
        const bin: *const BinaryContextNode = @ptrCast(@alignCast(buffer.ptr));
        if (!std.mem.eql(u8, &bin.header.magic, &.{ 'C', 'C', 'N', 'D' })) return error.InvalidMagic;
        if (bin.header.version != 1) return error.UnsupportedVersion;
        var node = ContextNode{
            .id = bin.id,
            .lod = [_][]const u8{ "", "", "", "", "", "" },
            .lod_owned = 0,
            .embedding = &[_]f32{},
            .valid_from = @floatFromInt(bin.valid_from_ts),
            .valid_to = if (bin.valid_to_ts == 0) null else @floatFromInt(bin.valid_to_ts),
            .confidence = bin.confidence,
            .provenance_id = bin.provenance_id,
        };
        for (0..schema.LOD_COUNT) |i| {
            const lod_slice = bin.getLod(buffer, @intCast(i));
            if (lod_slice.len > 0) {
                node.lod[i] = try allocator.dupe(u8, lod_slice);
                node.lod_owned |= @as(u8, 1) << @intCast(i);
            }
        }
        return node;
    }
};

pub const ContextNodeSchema = struct {
    const Self = @This();
    const ACCESSOR_COUNT = 11;

    accessors: [ACCESSOR_COUNT]reflection.Accessor,

    const id_vtable = reflection.Constraint(i64);
    const valid_from_vtable = reflection.Constraint(f64);
    const confidence_vtable = reflection.Constraint(i32);
    const provenance_id_vtable = reflection.Constraint(i32);

    const lod_vtables = struct {
        const lod0 = reflection.Constraint([]const u8);
        const lod1 = reflection.Constraint([]const u8);
        const lod2 = reflection.Constraint([]const u8);
        const lod3 = reflection.Constraint([]const u8);
        const lod4 = reflection.Constraint([]const u8);
        const lod5 = reflection.Constraint([]const u8);
    };

    const valid_to_vtable = blk: {
        const VT = struct {
            fn set(_: std.mem.Allocator, ptr: *anyopaque, input: []const u8) anyerror!void {
                const typed_ptr: *align(@alignOf(?f64)) ?f64 = @ptrCast(@alignCast(ptr));
                if (std.mem.eql(u8, input, "null") or input.len == 0) {
                    typed_ptr.* = null;
                } else {
                    typed_ptr.* = try std.fmt.parseFloat(f64, input);
                }
            }
            fn get(allocator: std.mem.Allocator, ptr: *const anyopaque) anyerror![]const u8 {
                const typed_ptr: *align(@alignOf(?f64)) const ?f64 = @ptrCast(@alignCast(ptr));
                if (typed_ptr.*) |val| {
                    return std.fmt.allocPrint(allocator, "{d}", .{val});
                } else {
                    return allocator.dupe(u8, "null");
                }
            }
        };
        break :blk reflection.ConstraintVTable{
            .setFn = VT.set,
            .getFn = VT.get,
        };
    };

    pub fn create(allocator: std.mem.Allocator) !*Self {
        const self = try allocator.create(Self);
        errdefer allocator.destroy(self);

        self.accessors[0] = .{
            .name = "id",
            .offset = @offsetOf(ContextNode, "id"),
            .permissions = reflection.perm_coder,
            .constraint = &id_vtable,
            .type_tag = .int,
            .ownership = .value,
            .binary_size = @sizeOf(i64),
        };
        self.accessors[1] = .{
            .name = "lod0",
            .offset = @offsetOf(ContextNode, "lod") + 0 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod0,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
        };
        self.accessors[2] = .{
            .name = "lod1",
            .offset = @offsetOf(ContextNode, "lod") + 1 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod1,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
        };
        self.accessors[3] = .{
            .name = "lod2",
            .offset = @offsetOf(ContextNode, "lod") + 2 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod2,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
        };
        self.accessors[4] = .{
            .name = "lod3",
            .offset = @offsetOf(ContextNode, "lod") + 3 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod3,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
        };
        self.accessors[5] = .{
            .name = "lod4",
            .offset = @offsetOf(ContextNode, "lod") + 4 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod4,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
        };
        self.accessors[6] = .{
            .name = "lod5",
            .offset = @offsetOf(ContextNode, "lod") + 5 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod5,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
        };
        self.accessors[7] = .{
            .name = "valid_from",
            .offset = @offsetOf(ContextNode, "valid_from"),
            .permissions = reflection.perm_public_read,
            .constraint = &valid_from_vtable,
            .type_tag = .float,
            .ownership = .value,
            .binary_size = @sizeOf(f64),
        };
        self.accessors[8] = .{
            .name = "valid_to",
            .offset = @offsetOf(ContextNode, "valid_to"),
            .permissions = reflection.perm_public_read,
            .constraint = &valid_to_vtable,
            .type_tag = .optional,
            .ownership = .value,
            .binary_size = @sizeOf(i64),
        };
        self.accessors[9] = .{
            .name = "confidence",
            .offset = @offsetOf(ContextNode, "confidence"),
            .permissions = reflection.perm_staff,
            .constraint = &confidence_vtable,
            .type_tag = .int,
            .ownership = .value,
            .binary_size = @sizeOf(i32),
        };
        self.accessors[10] = .{
            .name = "provenance_id",
            .offset = @offsetOf(ContextNode, "provenance_id"),
            .permissions = reflection.perm_coder,
            .constraint = &provenance_id_vtable,
            .type_tag = .int,
            .ownership = .value,
            .binary_size = @sizeOf(i32),
        };

        return self;
    }

    pub fn destroy(self: *Self, allocator: std.mem.Allocator) void {
        allocator.destroy(self);
    }

    pub fn viewOf(
        self: *const Self,
        allocator: std.mem.Allocator,
        node: *ContextNode,
    ) !reflection.DynamicEditable {
        const node_bytes: [*]u8 = @ptrCast(node);
        const buf = node_bytes[0..@sizeOf(ContextNode)];
        return reflection.DynamicEditable.init(allocator, buf, &self.accessors);
    }

    pub fn binarySize(node: *const ContextNode) usize {
        var size: usize = @sizeOf(BinaryHeader) + @sizeOf(i64);
        size += @sizeOf(f64);
        size += @sizeOf(i64);
        size += @sizeOf(i32) + @sizeOf(i32);
        for (0..schema.LOD_COUNT) |i| {
            size += @sizeOf(u32) * 2;
            size += node.lod[i].len;
        }
        return size;
    }
};

const testing = std.testing;

test "ContextNodeSchema: create and destroy" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const schema_ptr = try ContextNodeSchema.create(allocator);
    defer schema_ptr.destroy(allocator);
    try testing.expectEqual(@as(usize, 11), schema_ptr.accessors.len);
}

test "ContextNodeSchema: viewOf set and get" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var node = try ContextNode.init(42, "test", "Full content.", allocator);
    defer node.free(allocator);

    const schema_ptr = try ContextNodeSchema.create(allocator);
    defer schema_ptr.destroy(allocator);
    var view = try schema_ptr.viewOf(allocator, &node);
    defer view.deinit();

    const id_val = try view.get("id", .coder);
    defer allocator.free(id_val);
    try testing.expectEqualStrings("42", id_val);

    const lod0_val = try view.get("lod0", .coder);
    defer allocator.free(lod0_val);
    try testing.expectEqualStrings("Full content.", lod0_val);

    try view.set("confidence", "100", .coder);
    try testing.expectEqual(@as(i32, 100), node.confidence);
}

test "ContextNodeSchema: viewOf lod field access" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var node = try ContextNode.init(1, "name", "full text", allocator);
    defer node.free(allocator);

    const schema_ptr = try ContextNodeSchema.create(allocator);
    defer schema_ptr.destroy(allocator);
    var view = try schema_ptr.viewOf(allocator, &node);
    defer view.deinit();

    const got = try view.get("lod0", .coder);
    defer allocator.free(got);
    try testing.expectEqualStrings("full text", got);

    const got4 = try view.get("lod4", .coder);
    defer allocator.free(got4);
    try testing.expectEqualStrings("name", got4);
}

test "ContextNodeSchema: valid_to optional field" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var node = try ContextNode.init(1, "n", "f", allocator);
    defer node.free(allocator);

    const schema_ptr = try ContextNodeSchema.create(allocator);
    defer schema_ptr.destroy(allocator);
    var view = try schema_ptr.viewOf(allocator, &node);
    defer view.deinit();

    node.valid_to = null;
    const null_val = try view.get("valid_to", .coder);
    defer allocator.free(null_val);
    try testing.expectEqualStrings("null", null_val);

    try view.set("valid_to", "123.45", .coder);
    try testing.expect(node.valid_to != null);
    try testing.expectApproxEqAbs(@as(f64, 123.45), node.valid_to.?, 0.001);

    try view.set("valid_to", "null", .coder);
    try testing.expect(node.valid_to == null);
}

test "BinaryContextNode: round-trip serialization" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var original = try ContextNode.init(0xDEAD_BEEF, "test_name", "full text content here", allocator);
    defer original.free(allocator);

    original.setLod(1, "summary");
    original.setLod(2, "brief");
    original.confidence = 95;
    original.provenance_id = 7;

    var buffer: [1024]u8 = undefined;
    const written = BinaryContextNode.writeToBuffer(&original, &buffer);

    var restored = try BinaryContextNode.readFromBuffer(allocator, buffer[0..written]);
    defer restored.free(allocator);

    try testing.expectEqual(original.id, restored.id);
    try testing.expectEqualStrings("full text content here", restored.lod[0]);
    try testing.expectEqualStrings("summary", restored.lod[1]);
    try testing.expectEqualStrings("brief", restored.lod[2]);
    try testing.expectEqualStrings("test_name", restored.lod[4]);
    try testing.expectEqual(@as(i32, 95), restored.confidence);
    try testing.expectEqual(@as(i32, 7), restored.provenance_id);
}

test "ContextNodeSchema: access denied on protected field" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var node = try ContextNode.init(1, "n", "f", allocator);
    defer node.free(allocator);

    const schema_ptr = try ContextNodeSchema.create(allocator);
    defer schema_ptr.destroy(allocator);
    var view = try schema_ptr.viewOf(allocator, &node);
    defer view.deinit();

    const result = view.set("id", "99", .player);
    try testing.expectError(error.AccessDenied, result);
}
