const std = @import("std");
const reflection = @import("common").reflection;
const SharedString = @import("common").SharedString;
const coral_db = @import("coral_db");
const ContextNode = coral_db.ContextNode;
const schema = coral_db.schema;

pub const BINARY_SCHEMA_VERSION: u32 = 1;
pub const BINARY_MAGIC: [4]u8 = .{ 'C', 'C', 'N', 'D' };

/// Defines a payload type with enum-based payload definitions, managed by owner; ensures fixed-size buffers and clear ownership model.
pub const PayloadType = enum(u32) {
    context_node = 1,
    execution_request = 2,
    execution_result = 3,
    error_response = 4,
    host_function_call = 5,
    host_function_result = 6,
};

/// Manages binary header structures with fixed-size buffers; owned by the context; ensures invariant buffer integrity.
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

/// Manages binary context nodes with ownership and invariants; ensures safe access patterns.
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
        // lod[0]: wrap in a SharedString so the round-tripped node follows
        // the same ownership model as nodes created via ContextNode.init().
        const lod0_slice = bin.getLod(buffer, 0);
        const src = try SharedString.Ref.init(allocator, lod0_slice);
        node.source = src;
        node.lod[0] = src.slice();
        // lod[1..5]: allocator-owned copies (bit 0 stays clear).
        for (1..schema.LOD_COUNT) |i| {
            const lod_slice = bin.getLod(buffer, @intCast(i));
            if (lod_slice.len > 0) {
                node.lod[i] = try allocator.dupe(u8, lod_slice);
                node.lod_owned |= @as(u8, 1) << @intCast(i);
            }
        }
        return node;
    }
};

// ---------------------------------------------------------------------------
// Binary IPC — Execution Request / Result
// ---------------------------------------------------------------------------
// Defined here (in the canonical binary schema file) so both wasm.zig and
// execution_request.zig can import these types via coral_schema without
// creating a circular dependency.

/// Represents a binary execution request with ownership and invariants; managed by the system; not thread-safe.
pub const BinaryExecutionRequest = extern struct {
    header: BinaryHeader align(1),
    target_id: i64 align(1),
    input_offset: u32 align(1),
    input_len: u32 align(1),
    flags: u32 align(1),

    pub const Flag = struct {
        pub const VERBOSE: u32 = 1 << 0;
        pub const DRY_RUN: u32 = 1 << 1;
        pub const FORCE: u32 = 1 << 2;
    };
};

/// Represents execution outcome data, managed by owner; key invariant is correct result struct; not thread-safe.
pub const BinaryExecutionResult = extern struct {
    header: BinaryHeader align(1),
    success: u32 align(1), // 0 = failure, 1 = success
    error_code: u32 align(1),
    output_offset: u32 align(1),
    output_len: u32 align(1),
    provides_words_offset: u32 align(1),
    provides_words_count: u32 align(1),

    /// Reconstruct the provides bitset from the trailing word array.
    /// Caller owns the returned bitset and must deinit it.
    pub fn getProvidesBitSet(
        self: *const BinaryExecutionResult,
        allocator: std.mem.Allocator,
        payload: []const u8,
    ) !std.bit_set.DynamicBitSetUnmanaged {
        const wc = self.provides_words_count;
        const bit_length = @as(usize, wc) * @bitSizeOf(usize);
        var bs = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, bit_length);
        errdefer bs.deinit(allocator);
        const off = self.provides_words_offset;
        for (0..wc) |i| {
            const w = std.mem.readInt(u64, payload[off + i * 8 ..][0..8], .little);
            bs.masks[i] = @intCast(w);
        }
        return bs;
    }
};

/// Defines a schema for context nodes, managing ownership and invariants like fixed-size buffers and initialization/deinit cycles.
pub const ContextNodeSchema = struct {
    /// SQL bind indices — column order for INSERT OR REPLACE in context_nodes.
    /// Matches the column order in DDL_CONTEXT_NODES.
    pub const sql_bind_order = [_][]const u8{
        "id",        "lod0",       "lod1",     "lod2",       "lod3",          "lod4", "lod5",
        "embedding", "valid_from", "valid_to", "confidence", "provenance_id",
    };
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
            .sql_type = .integer,
        };
        self.accessors[1] = .{
            .name = "lod0",
            .offset = @offsetOf(ContextNode, "lod") + 0 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod0,
            // lod[0] is owned by the SharedString ref (node.source), not by the
            // allocator directly.  Mark borrowed so the reflection layer does not
            // attempt to free it on write.  Use node.setSource() for mutation.
            .type_tag = .string_owned,
            .ownership = .borrowed,
            .binary_size = 0,
            .sql_type = .text,
        };
        self.accessors[2] = .{
            .name = "lod1",
            .offset = @offsetOf(ContextNode, "lod") + 1 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod1,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
            .sql_type = .text,
        };
        self.accessors[3] = .{
            .name = "lod2",
            .offset = @offsetOf(ContextNode, "lod") + 2 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod2,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
            .sql_type = .text,
        };
        self.accessors[4] = .{
            .name = "lod3",
            .offset = @offsetOf(ContextNode, "lod") + 3 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod3,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
            .sql_type = .text,
        };
        self.accessors[5] = .{
            .name = "lod4",
            .offset = @offsetOf(ContextNode, "lod") + 4 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod4,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
            .sql_type = .text,
        };
        self.accessors[6] = .{
            .name = "lod5",
            .offset = @offsetOf(ContextNode, "lod") + 5 * @sizeOf([]const u8),
            .permissions = reflection.perm_all,
            .constraint = &lod_vtables.lod5,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
            .sql_type = .text,
        };
        self.accessors[7] = .{
            .name = "valid_from",
            .offset = @offsetOf(ContextNode, "valid_from"),
            .permissions = reflection.perm_public_read,
            .constraint = &valid_from_vtable,
            .type_tag = .float,
            .ownership = .value,
            .binary_size = @sizeOf(f64),
            .sql_type = .real,
        };
        self.accessors[8] = .{
            .name = "valid_to",
            .offset = @offsetOf(ContextNode, "valid_to"),
            .permissions = reflection.perm_public_read,
            .constraint = &valid_to_vtable,
            .type_tag = .optional,
            .ownership = .value,
            .binary_size = @sizeOf(i64),
            .sql_type = .real,
        };
        self.accessors[9] = .{
            .name = "confidence",
            .offset = @offsetOf(ContextNode, "confidence"),
            .permissions = reflection.perm_staff,
            .constraint = &confidence_vtable,
            .type_tag = .int,
            .ownership = .value,
            .binary_size = @sizeOf(i32),
            .sql_type = .integer,
        };
        self.accessors[10] = .{
            .name = "provenance_id",
            .offset = @offsetOf(ContextNode, "provenance_id"),
            .permissions = reflection.perm_coder,
            .constraint = &provenance_id_vtable,
            .type_tag = .int,
            .ownership = .value,
            .binary_size = @sizeOf(i32),
            .sql_type = .integer,
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

test "ContextNodeSchema: sql_bind_order has correct length" {
    try testing.expectEqual(@as(usize, 12), ContextNodeSchema.sql_bind_order.len);
}

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

test "ContextNodeSchema: sql_type is correctly set on all accessors" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const schema_ptr = try ContextNodeSchema.create(allocator);
    defer schema_ptr.destroy(allocator);

    // Verify sql_type matches SQL column types:
    // id, confidence, provenance_id -> INTEGER
    // lod0-5 -> TEXT
    // valid_from, valid_to -> REAL
    try testing.expectEqual(reflection.SqlType.integer, schema_ptr.accessors[0].sql_type); // id
    try testing.expectEqual(reflection.SqlType.text, schema_ptr.accessors[1].sql_type); // lod0
    try testing.expectEqual(reflection.SqlType.text, schema_ptr.accessors[2].sql_type); // lod1
    try testing.expectEqual(reflection.SqlType.text, schema_ptr.accessors[3].sql_type); // lod2
    try testing.expectEqual(reflection.SqlType.text, schema_ptr.accessors[4].sql_type); // lod3
    try testing.expectEqual(reflection.SqlType.text, schema_ptr.accessors[5].sql_type); // lod4
    try testing.expectEqual(reflection.SqlType.text, schema_ptr.accessors[6].sql_type); // lod5
    try testing.expectEqual(reflection.SqlType.real, schema_ptr.accessors[7].sql_type); // valid_from
    try testing.expectEqual(reflection.SqlType.real, schema_ptr.accessors[8].sql_type); // valid_to
    try testing.expectEqual(reflection.SqlType.integer, schema_ptr.accessors[9].sql_type); // confidence
    try testing.expectEqual(reflection.SqlType.integer, schema_ptr.accessors[10].sql_type); // provenance_id
}
