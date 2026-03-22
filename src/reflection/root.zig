/// reflection — Coral Context field-level reflection, validation, and permission layer.
///
/// Standalone peer module promoted from src/common/reflection.zig (P2.4).
///
/// Sub-module namespaces:
///   reflection.constraint     — ConstraintVTable, Constraint(T), constraintSet/Get
///   reflection.accessor       — Accessor, Editable(T), DynamicEditable, FieldMeta, TypeTag, OwnershipMode
///   reflection.permissions    — Role, RolePermissions, perm_* constants
///   reflection.typed          — TypedAccessorTable(T), TypedEditable, TypedAccessor
///   reflection.binary         — BinaryFieldCodec
///   reflection.enum_registry  — EnumRegistry
///
/// Flat re-exports preserve backward-compatible call sites such as:
///   @import("reflection").Constraint(T)
///   @import("reflection").perm_all
///   @import("reflection").Editable(T)
pub const constraint = @import("constraint.zig");
pub const accessor = @import("accessor.zig");
pub const permissions = @import("permissions.zig");
pub const typed = @import("typed.zig");
pub const binary = @import("binary.zig");
pub const enum_registry = @import("enum_registry.zig");

// ── Flat re-exports: constraint ───────────────────────────────────────────────
pub const ConstraintVTable = constraint.ConstraintVTable;
pub const Constraint = constraint.Constraint;
pub const constraintSet = constraint.constraintSet;
pub const constraintGet = constraint.constraintGet;

// ── Flat re-exports: accessor ─────────────────────────────────────────────────
pub const Accessor = accessor.Accessor;
pub const Editable = accessor.Editable;
pub const DynamicEditable = accessor.DynamicEditable;
pub const FieldMeta = accessor.FieldMeta;
pub const RequiresWhenEntry = accessor.RequiresWhenEntry;
pub const TypeTag = accessor.TypeTag;
pub const OwnershipMode = accessor.OwnershipMode;
pub const describeSchemaFromAccessors = accessor.describeSchemaFromAccessors;

// ── Flat re-exports: permissions ──────────────────────────────────────────────
pub const Role = permissions.Role;
pub const RolePermissions = permissions.RolePermissions;
pub const perm_all = permissions.perm_all;
pub const perm_coder = permissions.perm_coder;
pub const perm_staff = permissions.perm_staff;
pub const perm_public_read = permissions.perm_public_read;

// ── Flat re-exports: typed ────────────────────────────────────────────────────
pub const TypeId = typed.TypeId;
pub const typeIdFromType = typed.typeIdFromType;
pub const TypedAccessor = typed.TypedAccessor;
pub const TypedAccessorTable = typed.TypedAccessorTable;
pub const TypedEditable = typed.TypedEditable;
pub const ValidationError = typed.ValidationError;

// ── Flat re-exports: binary ───────────────────────────────────────────────────
pub const BinaryFieldCodec = binary.BinaryFieldCodec;

// ── Flat re-exports: enum_registry ───────────────────────────────────────────
pub const EnumRegistry = enum_registry.EnumRegistry;

// ── Pull in all tests from sub-modules ───────────────────────────────────────
// Tests from the original reflection.zig (accessor/constraint/permissions/editable)
// are preserved in the inline test blocks below (migrated from src/common/reflection.zig).

const std = @import("std");
const testing = std.testing;

// ── primitive types ──────────────────────────────────────────────────────────

const TestConfig = struct {
    port: u16 = 8080,
    timeout: f32 = 30.0,
    enabled: bool = false,
    editable: Editable(TestConfig) = .{},
};

test "Editable: set and get integer" {
    var config: TestConfig = .{};
    try config.editable.set(testing.allocator, "port", "9000", .player);
    try testing.expectEqual(@as(u16, 9000), config.port);
    const val = try config.editable.get(testing.allocator, "port", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualStrings("9000", val);
}

test "Editable: set and get float" {
    var config: TestConfig = .{};
    try config.editable.set(testing.allocator, "timeout", "45.5", .player);
    try testing.expectEqual(@as(f32, 45.5), config.timeout);
    const val = try config.editable.get(testing.allocator, "timeout", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualSlices(u8, "45.5", val[0..4]);
}

test "Editable: set and get bool" {
    var config: TestConfig = .{};
    try config.editable.set(testing.allocator, "enabled", "true", .player);
    try testing.expect(config.enabled);
    const val = try config.editable.get(testing.allocator, "enabled", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualStrings("true", val);
}

test "Editable: field not found" {
    var config: TestConfig = .{};
    const result = config.editable.set(testing.allocator, "nonexistent", "value", .player);
    try testing.expectError(error.FieldNotFound, result);
}

test "Editable: zero-size mixin" {
    try testing.expectEqual(@as(usize, 0), @sizeOf(Editable(TestConfig)));
}

test "Editable: setFast and getFast" {
    var config: TestConfig = .{};
    config.editable.setFast("port", 1234);
    try testing.expectEqual(@as(u16, 1234), config.port);
    const val = config.editable.getFast("port");
    try testing.expectEqual(@as(u16, 1234), val);
}

// ── string field ─────────────────────────────────────────────────────────────

const TestString = struct {
    name: []const u8 = "default",
    editable: Editable(TestString) = .{},
};

test "Editable: string field set and get" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var obj: TestString = .{};
    try obj.editable.set(allocator, "name", "hello world", .coder);
    defer allocator.free(obj.name);
    try testing.expectEqualStrings("hello world", obj.name);
    const val = try obj.editable.get(allocator, "name", .coder);
    defer allocator.free(val);
    try testing.expectEqualStrings("hello world", val);
}

// ── enum field ───────────────────────────────────────────────────────────────

const Status = enum { pending, active, done };

const TestEnum = struct {
    status: Status = .pending,
    editable: Editable(TestEnum) = .{},
};

test "Editable: enum field set and get" {
    var obj: TestEnum = .{};
    try obj.editable.set(testing.allocator, "status", "active", .player);
    try testing.expectEqual(Status.active, obj.status);
    const val = try obj.editable.get(testing.allocator, "status", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualStrings("active", val);
}

test "Editable: enum invalid value propagates error" {
    var obj: TestEnum = .{};
    const result = obj.editable.set(testing.allocator, "status", "invalid", .player);
    try testing.expectError(error.InvalidEnum, result);
}

// ── optional field ───────────────────────────────────────────────────────────

const TestOptional = struct {
    maybe: ?u32 = null,
    editable: Editable(TestOptional) = .{},
};

test "Editable: optional null" {
    var obj: TestOptional = .{ .maybe = 42 };
    try obj.editable.set(testing.allocator, "maybe", "null", .coder);
    try testing.expect(obj.maybe == null);
}

test "Editable: optional value" {
    var obj: TestOptional = .{};
    try obj.editable.set(testing.allocator, "maybe", "123", .coder);
    try testing.expect(obj.maybe != null);
    try testing.expectEqual(@as(u32, 123), obj.maybe.?);
}

// ── array field ──────────────────────────────────────────────────────────────

const TestArray = struct {
    scores: [3]u8 = .{ 0, 0, 0 },
    editable: Editable(TestArray) = .{},
};

test "Editable: array set and get" {
    var obj: TestArray = .{};
    try obj.editable.set(testing.allocator, "scores", "1, 2, 3", .player);
    try testing.expectEqual(@as(u8, 1), obj.scores[0]);
    try testing.expectEqual(@as(u8, 2), obj.scores[1]);
    try testing.expectEqual(@as(u8, 3), obj.scores[2]);
    const val = try obj.editable.get(testing.allocator, "scores", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualStrings("[1,2,3]", val);
}

// ── vector field ─────────────────────────────────────────────────────────────

const TestVector = struct {
    vec: @Vector(4, f32) = .{ 0, 0, 0, 0 },
    editable: Editable(TestVector) = .{},
};

test "Editable: vector set and get" {
    var obj: TestVector = .{};
    try obj.editable.set(testing.allocator, "vec", "1.0, 2.0, 3.0, 4.0", .player);
    try testing.expectEqual(@as(f32, 1.0), obj.vec[0]);
    try testing.expectEqual(@as(f32, 2.0), obj.vec[1]);
    const val = try obj.editable.get(testing.allocator, "vec", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualStrings("[1,2,3,4]", val);
}

// ── DynamicEditable ──────────────────────────────────────────────────────────

test "DynamicEditable: basic set and get" {
    var buffer align(4) = [_]u8{0} ** 8;

    const u32_vtable = Constraint(u32);
    const f32_vtable = Constraint(f32);

    const accessors = [_]Accessor{
        .{ .name = "id", .offset = 0, .permissions = perm_coder, .constraint = &u32_vtable },
        .{ .name = "value", .offset = 4, .permissions = perm_coder, .constraint = &f32_vtable },
    };

    var dyn = try DynamicEditable.init(testing.allocator, &buffer, &accessors);
    defer dyn.deinit();

    try dyn.set("id", "42", .coder);
    try dyn.set("value", "3.14", .coder);

    const id_val = try dyn.get("id", .coder);
    defer testing.allocator.free(id_val);
    try testing.expectEqualStrings("42", id_val);

    const f_val = try dyn.get("value", .coder);
    defer testing.allocator.free(f_val);
    try testing.expect(std.mem.startsWith(u8, f_val, "3.14"));
}

test "DynamicEditable: role-based permission" {
    var buffer align(4) = [_]u8{0} ** 4;
    const u32_vtable = Constraint(u32);

    const accessors = [_]Accessor{
        .{ .name = "admin_field", .offset = 0, .permissions = .{ .coder_write = true, .player_read = true }, .constraint = &u32_vtable },
    };

    var dyn = try DynamicEditable.init(testing.allocator, &buffer, &accessors);
    defer dyn.deinit();

    try dyn.set("admin_field", "100", .coder);
    const result = dyn.set("admin_field", "200", .player);
    try testing.expectError(error.AccessDenied, result);
}

test "RolePermissions: packed struct size" {
    try testing.expectEqual(@as(usize, 4), @sizeOf(RolePermissions));
}

test "RolePermissions: canRead and canWrite methods" {
    const perms: RolePermissions = .{ .staff_read = true, .staff_write = true };
    try testing.expect(perms.canRead(.staff));
    try testing.expect(perms.canWrite(.staff));
    try testing.expect(!perms.canRead(.player));
    try testing.expect(!perms.canWrite(.coder));
}

test "Editable: access denied on read" {
    var buffer align(4) = [_]u8{0} ** 4;
    const u32_vtable = Constraint(u32);
    const accessors = [_]Accessor{
        .{ .name = "val", .offset = 0, .permissions = .{ .coder_read = true }, .constraint = &u32_vtable },
    };
    var dyn = try DynamicEditable.init(testing.allocator, &buffer, &accessors);
    defer dyn.deinit();
    const result = dyn.get("val", .player);
    try testing.expectError(error.AccessDenied, result);
}

// ── Schema description for AI agents ───────────────────────────────────────

const TestSchema = struct {
    port: u16 = 8080,
    host: []const u8 = "localhost",
    enabled: bool = true,
    level: enum { low, medium, high } = .medium,
    editable: Editable(TestSchema) = .{},

    pub fn describeField(comptime name: []const u8) FieldMeta {
        if (std.mem.eql(u8, name, "port")) {
            return .{
                .description = "TCP port for the server (1-65535)",
                .min = 1,
                .max = 65535,
                .default = "8080",
                .identity = true,
            };
        }
        if (std.mem.eql(u8, name, "host")) {
            return .{
                .description = "Hostname or IP address to bind",
                .examples = &.{ "localhost", "0.0.0.0", "127.0.0.1" },
            };
        }
        if (std.mem.eql(u8, name, "level")) {
            return .{
                .description = "Log level for the server",
                .enum_values = &.{ "low", "medium", "high" },
            };
        }
        return .{};
    }
};

test "Editable: describeSchema generates JSON" {
    const schema = try Editable(TestSchema).describeSchema(testing.allocator);
    defer testing.allocator.free(schema);

    try testing.expect(std.mem.indexOf(u8, schema, "\"port\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "\"host\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "\"enabled\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "\"level\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "\"TCP port for the server") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "$schema") != null);
}

test "Editable: fieldNames returns all fields" {
    const names = try Editable(TestSchema).fieldNames(testing.allocator);
    defer testing.allocator.free(names);
    try testing.expectEqual(@as(usize, 4), names.len);
}

test "Editable: getFieldMeta returns metadata" {
    const meta = Editable(TestSchema).getFieldMeta("port");
    try testing.expect(meta != null);
    try testing.expect(meta.?.description != null);
    try testing.expectEqual(@as(i128, 1), meta.?.min.?);
    try testing.expect(meta.?.identity);
}

test "DynamicEditable: describeSchema generates JSON" {
    var buffer align(8) = [_]u8{0} ** 8;
    const u32_vtable = Constraint(u32);
    const f32_vtable = Constraint(f32);

    const accessors = [_]Accessor{
        .{
            .name = "id",
            .offset = 0,
            .permissions = perm_all,
            .constraint = &u32_vtable,
            .type_tag = .int,
            .meta = .{ .description = "Unique identifier", .identity = true },
        },
        .{
            .name = "value",
            .offset = 4,
            .permissions = perm_all,
            .constraint = &f32_vtable,
            .type_tag = .float,
            .meta = .{ .description = "A floating point value" },
        },
    };

    var dyn = try DynamicEditable.init(testing.allocator, &buffer, &accessors);
    defer dyn.deinit();

    const schema = try dyn.describeSchema(testing.allocator, "TestEntity");
    defer testing.allocator.free(schema);

    try testing.expect(std.mem.indexOf(u8, schema, "\"id\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "\"value\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "Unique identifier") != null);
}

// ── TypedAccessorTable / TypedEditable / BinaryFieldCodec / EnumRegistry tests ──

const TestConfigTyped = struct {
    port: u16 = 8080,
    max_connections: u32 = 100,
    timeout_ms: u64 = 5000,
    enabled: bool = true,
    priority: enum { low, medium, high } = .medium,
    name: []const u8 = "default",
};

test "TypeId from type" {
    try testing.expectEqual(TypeId.int_u16, typeIdFromType(u16));
    try testing.expectEqual(TypeId.int_i32, typeIdFromType(i32));
    try testing.expectEqual(TypeId.float_f32, typeIdFromType(f32));
    try testing.expectEqual(TypeId.bool_type, typeIdFromType(bool));
    try testing.expectEqual(TypeId.string_type, typeIdFromType([]const u8));
    const PriorityEnum = @TypeOf(@as(TestConfigTyped, undefined).priority);
    try testing.expectEqual(TypeId.enum_type, typeIdFromType(PriorityEnum));
}

test "TypedAccessorTable field count" {
    const Table = TypedAccessorTable(TestConfigTyped);
    try testing.expectEqual(@as(usize, 6), Table.field_count);
}

test "TypedAccessorTable field index lookup" {
    const Table = TypedAccessorTable(TestConfigTyped);
    try testing.expectEqual(@as(?usize, 0), Table.getFieldIndex("port"));
    try testing.expectEqual(@as(?usize, null), Table.getFieldIndex("nonexistent"));
}

test "TypedAccessorTable set and get field" {
    var config: TestConfigTyped = .{};
    const Table = TypedAccessorTable(TestConfigTyped);

    Table.setField(&config, "port", @as(u16, 9000));
    try testing.expectEqual(@as(u16, 9000), config.port);

    const port = Table.getField(&config, "port");
    try testing.expectEqual(@as(u16, 9000), port);
}

test "TypedAccessorTable compile-time type checking" {
    var config: TestConfigTyped = .{};
    const Table = TypedAccessorTable(TestConfigTyped);

    Table.setField(&config, "enabled", true);
    try testing.expect(config.enabled);
}

test "BinaryFieldCodec encode/decode integer" {
    var buf: [8]u8 = undefined;
    var stream = std.io.fixedBufferStream(&buf);
    const writer = stream.writer();

    try BinaryFieldCodec.encodeField(u32, 0x12345678, writer);
    try testing.expectEqualSlices(u8, &[_]u8{ 0x78, 0x56, 0x34, 0x12 }, buf[0..4]);

    stream.reset();
    const reader = stream.reader();
    const decoded = try BinaryFieldCodec.decodeField(u32, reader, testing.allocator);
    try testing.expectEqual(@as(u32, 0x12345678), decoded);
}

test "BinaryFieldCodec encode/decode bool" {
    var buf: [1]u8 = undefined;
    var stream = std.io.fixedBufferStream(&buf);
    const writer = stream.writer();

    try BinaryFieldCodec.encodeField(bool, true, writer);
    try testing.expectEqual(@as(u8, 1), buf[0]);

    stream.reset();
    const reader = stream.reader();
    const decoded = try BinaryFieldCodec.decodeField(bool, reader, testing.allocator);
    try testing.expect(decoded);
}

test "BinaryFieldCodec encode/decode enum" {
    const Priority = enum { low, medium, high };
    var buf: [4]u8 = undefined;
    var stream = std.io.fixedBufferStream(&buf);
    const writer = stream.writer();

    try BinaryFieldCodec.encodeField(Priority, .high, writer);

    stream.reset();
    const reader = stream.reader();
    const decoded = try BinaryFieldCodec.decodeField(Priority, reader, testing.allocator);
    try testing.expectEqual(Priority.high, decoded);
}

test "BinaryFieldCodec wire size" {
    try testing.expectEqual(@as(?usize, 2), BinaryFieldCodec.fieldWireSize(u16));
    try testing.expectEqual(@as(?usize, 4), BinaryFieldCodec.fieldWireSize(u32));
    try testing.expectEqual(@as(?usize, 4), BinaryFieldCodec.fieldWireSize(f32));
    try testing.expectEqual(@as(?usize, 1), BinaryFieldCodec.fieldWireSize(bool));
    try testing.expectEqual(@as(?usize, null), BinaryFieldCodec.fieldWireSize([]const u8));
}

test "EnumRegistry register and lookup" {
    const Priority = enum { low, medium, high };
    var registry = EnumRegistry.init(testing.allocator);
    defer registry.deinit(testing.allocator);

    try registry.registerEnum(testing.allocator, Priority);

    try testing.expectEqual(@as(?i64, 0), registry.nameToValue("low"));
    try testing.expectEqual(@as(?i64, 1), registry.nameToValue("medium"));
    try testing.expectEqual(@as(?i64, 2), registry.nameToValue("high"));
    try testing.expectEqual(@as(?i64, null), registry.nameToValue("nonexistent"));

    try testing.expectEqualStrings("low", registry.valueToName(0).?);
    try testing.expectEqualStrings("high", registry.valueToName(2).?);
}

test "TypedAccessor validateRange" {
    const vtable = Constraint(u16);
    const acc = TypedAccessor{
        .name = "port",
        .offset = 0,
        .type_id = .int_u16,
        .size = 2,
        .alignment = 2,
        .permissions = perm_all,
        .string_vtable = &vtable,
        .min_value = 1,
        .max_value = 65535,
    };

    try testing.expect(acc.validateRange(@as(u16, 80)));
    try testing.expect(acc.validateRange(@as(u16, 1)));
    try testing.expect(acc.validateRange(@as(u16, 65535)));
    try testing.expect(!acc.validateRange(@as(u16, 0)));
}

test "TypedEditable set and get typed" {
    var config: TestConfigTyped = .{};

    const port_vtable = Constraint(u16);
    const conn_vtable = Constraint(u32);
    const bool_vtable = Constraint(bool);

    var accessors = [_]TypedAccessor{
        .{
            .name = "port",
            .offset = @offsetOf(TestConfigTyped, "port"),
            .type_id = .int_u16,
            .size = @sizeOf(u16),
            .alignment = @alignOf(u16),
            .permissions = perm_all,
            .string_vtable = &port_vtable,
            .min_value = 1,
            .max_value = 65535,
        },
        .{
            .name = "max_connections",
            .offset = @offsetOf(TestConfigTyped, "max_connections"),
            .type_id = .int_u32,
            .size = @sizeOf(u32),
            .alignment = @alignOf(u32),
            .permissions = perm_all,
            .string_vtable = &conn_vtable,
        },
        .{
            .name = "enabled",
            .offset = @offsetOf(TestConfigTyped, "enabled"),
            .type_id = .bool_type,
            .size = @sizeOf(bool),
            .alignment = @alignOf(bool),
            .permissions = perm_all,
            .string_vtable = &bool_vtable,
        },
    };

    var editable = try TypedEditable.init(testing.allocator, @ptrCast(&config), &accessors);
    defer editable.deinit();

    try editable.setTyped("port", @as(u16, 8080), .coder);
    try testing.expectEqual(@as(u16, 8080), config.port);

    const port = try editable.getTyped("port", u16, .coder);
    try testing.expectEqual(@as(u16, 8080), port);

    try editable.setTyped("enabled", false, .coder);
    try testing.expect(!config.enabled);
}

test "TypedEditable out of range validation" {
    var config: TestConfigTyped = .{};

    const port_vtable = Constraint(u16);

    var accessors = [_]TypedAccessor{
        .{
            .name = "port",
            .offset = @offsetOf(TestConfigTyped, "port"),
            .type_id = .int_u16,
            .size = @sizeOf(u16),
            .alignment = @alignOf(u16),
            .permissions = perm_all,
            .string_vtable = &port_vtable,
            .min_value = 1024,
            .max_value = 49151,
        },
    };

    var editable = try TypedEditable.init(testing.allocator, @ptrCast(&config), &accessors);
    defer editable.deinit();

    try testing.expectError(error.OutOfRange, editable.setTyped("port", @as(u16, 80), .coder));
    try testing.expectError(error.OutOfRange, editable.setTyped("port", @as(u16, 50000), .coder));

    try editable.setTyped("port", @as(u16, 8080), .coder);
    try testing.expectEqual(@as(u16, 8080), config.port);
}

test "TypedEditable string path still works" {
    var config: TestConfigTyped = .{};

    const port_vtable = Constraint(u16);

    var accessors = [_]TypedAccessor{
        .{
            .name = "port",
            .offset = @offsetOf(TestConfigTyped, "port"),
            .type_id = .int_u16,
            .size = @sizeOf(u16),
            .alignment = @alignOf(u16),
            .permissions = perm_all,
            .string_vtable = &port_vtable,
        },
    };

    var editable = try TypedEditable.init(testing.allocator, @ptrCast(&config), &accessors);
    defer editable.deinit();

    try editable.setByString("port", "9000", .coder);
    try testing.expectEqual(@as(u16, 9000), config.port);

    const str_val = try editable.getByString("port", .coder);
    defer testing.allocator.free(str_val);
    try testing.expectEqualStrings("9000", str_val);
}

test "TypedEditable type mismatch error" {
    var config: TestConfigTyped = .{};

    const port_vtable = Constraint(u16);

    var accessors = [_]TypedAccessor{
        .{
            .name = "port",
            .offset = @offsetOf(TestConfigTyped, "port"),
            .type_id = .int_u16,
            .size = @sizeOf(u16),
            .alignment = @alignOf(u16),
            .permissions = perm_all,
            .string_vtable = &port_vtable,
        },
    };

    var editable = try TypedEditable.init(testing.allocator, @ptrCast(&config), &accessors);
    defer editable.deinit();

    const result = editable.setTyped("port", @as(u32, 9000), .coder);
    try testing.expectError(error.TypeMismatch, result);
}
