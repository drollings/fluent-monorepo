/// typed.zig — TypedAccessorTable(T) and TypedEditable.
const std = @import("std");
const constraint_mod = @import("constraint.zig");
const permissions_mod = @import("permissions.zig");

pub const ConstraintVTable = constraint_mod.ConstraintVTable;
pub const Constraint = constraint_mod.Constraint;
pub const Role = permissions_mod.Role;
pub const RolePermissions = permissions_mod.RolePermissions;
pub const perm_all = permissions_mod.perm_all;

/// Defines a type identifier with enum capabilities; managed centrally; immutable once defined.
pub const TypeId = enum(u8) {
    int_u8,
    int_u16,
    int_u32,
    int_u64,
    int_i8,
    int_i16,
    int_i32,
    int_i64,
    float_f32,
    float_f64,
    bool_type,
    string_type,
    enum_type,
    optional_type,
    array_type,
    vector_type,
    bitset_type,
    struct_type,
    unknown,
};

/// Converts a Zig type to its corresponding TypeId identifier.
pub fn typeIdFromType(comptime T: type) TypeId {
    return switch (@typeInfo(T)) {
        .int => |i| switch (i.bits) {
            8 => if (i.signedness == .signed) .int_i8 else .int_u8,
            16 => if (i.signedness == .signed) .int_i16 else .int_u16,
            32 => if (i.signedness == .signed) .int_i32 else .int_u32,
            64 => if (i.signedness == .signed) .int_i64 else .int_u64,
            else => .unknown,
        },
        .float => |f| switch (f.bits) {
            32 => .float_f32,
            64 => .float_f64,
            else => .unknown,
        },
        .bool => .bool_type,
        .pointer => |p| if (p.size == .slice and p.child == u8) .string_type else .unknown,
        .@"enum" => .enum_type,
        .optional => .optional_type,
        .array => .array_type,
        .vector => .vector_type,
        else => .unknown,
    };
}

/// Defines a typed accessor structure for type-safe field access; manages ownership and invariants.
pub const TypedAccessor = struct {
    name: []const u8,
    offset: usize,
    type_id: TypeId,
    size: usize,
    alignment: usize,
    permissions: RolePermissions,
    string_vtable: *const ConstraintVTable,
    enum_values: ?[]const []const u8 = null,
    min_value: ?i128 = null,
    max_value: ?i128 = null,

    pub fn setTyped(self: *const TypedAccessor, base: *anyopaque, value: anytype) void {
        const T = @TypeOf(value);
        const ptr: *T = @ptrCast(@alignCast(@as([*]u8, @ptrCast(base)) + self.offset));
        ptr.* = value;
    }

    pub fn getTyped(self: *const TypedAccessor, base: *const anyopaque, comptime T: type) T {
        const ptr: *const T = @ptrCast(@alignCast(@as([*]const u8, @ptrCast(base)) + self.offset));
        return ptr.*;
    }

    pub fn setString(self: *const TypedAccessor, allocator: std.mem.Allocator, base: *anyopaque, input: []const u8) !void {
        try self.string_vtable.setFn(allocator, @ptrCast(@as([*]u8, @ptrCast(base)) + self.offset), input);
    }

    pub fn getString(self: *const TypedAccessor, allocator: std.mem.Allocator, base: *const anyopaque) ![]const u8 {
        return self.string_vtable.getFn(allocator, @ptrCast(@as([*]const u8, @ptrCast(base)) + self.offset));
    }

    pub fn validateRange(self: *const TypedAccessor, value: anytype) bool {
        const T = @TypeOf(value);
        const info = @typeInfo(T);
        if (info != .int) return true;

        if (self.min_value) |min| {
            const min_t: T = @intCast(min);
            if (value < min_t) return false;
        }
        if (self.max_value) |max| {
            const max_t: T = @intCast(max);
            if (value > max_t) return false;
        }
        return true;
    }
};

/// Converts a Zig type reference to a typed accessor table, returning the underlying type.
pub fn TypedAccessorTable(comptime Host: type) type {
    return struct {
        const Self = @This();

        pub const FieldInfo = struct {
            name: []const u8,
            type_id: TypeId,
            offset: usize,
            size: usize,
            alignment: usize,
        };

        pub const field_count = blk: {
            var count: usize = 0;
            for (std.meta.fields(Host)) |f| {
                if (!std.mem.eql(u8, f.name, "editable")) count += 1;
            }
            break :blk count;
        };

        pub const field_infos: [field_count]FieldInfo = blk: {
            var infos: [field_count]FieldInfo = undefined;
            var i: usize = 0;
            for (std.meta.fields(Host)) |f| {
                if (std.mem.eql(u8, f.name, "editable")) continue;
                infos[i] = .{
                    .name = f.name,
                    .type_id = typeIdFromType(f.type),
                    .offset = @offsetOf(Host, f.name),
                    .size = @sizeOf(f.type),
                    .alignment = @alignOf(f.type),
                };
                i += 1;
            }
            break :blk infos;
        };

        pub const field_map: std.StaticStringMap(usize) = blk: {
            const KV = struct { []const u8, usize };
            var kvs: [field_count]KV = undefined;
            for (field_infos, 0..) |fi, i| {
                kvs[i] = .{ fi.name, i };
            }
            break :blk std.StaticStringMap(usize).initComptime(kvs);
        };

        pub fn getFieldIndex(name: []const u8) ?usize {
            return field_map.get(name);
        }

        pub fn getFieldType(name: []const u8) ?TypeId {
            const idx = getFieldIndex(name) orelse return null;
            return field_infos[idx].type_id;
        }

        pub fn setField(host: *Host, comptime name: []const u8, value: anytype) void {
            comptime {
                const idx = getFieldIndex(name) orelse @compileError("Field not found: " ++ name);
                const expected_type_id = field_infos[idx].type_id;
                const actual_type_id = typeIdFromType(@TypeOf(value));
                if (expected_type_id != actual_type_id) {
                    @compileError("Type mismatch for field '" ++ name ++ "': expected " ++ @tagName(expected_type_id) ++ ", got " ++ @tagName(actual_type_id));
                }
            }
            @field(host, name) = value;
        }

        pub fn getField(host: *const Host, comptime name: []const u8) blk: {
            _ = getFieldIndex(name) orelse @compileError("Field not found: " ++ name);
            for (std.meta.fields(Host)) |f| {
                if (std.mem.eql(u8, f.name, name)) break :blk f.type;
            }
            @compileError("Field not found: " ++ name);
        } {
            return @field(host, name);
        }
    };
}

/// Represents a typed editable structure with strict ownership and invariants; managed via reflection.
pub const TypedEditable = struct {
    host_ptr: *anyopaque,
    accessors: []const TypedAccessor,
    name_map: std.StringHashMapUnmanaged(usize),
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator, host: *anyopaque, accessors: []const TypedAccessor) !TypedEditable {
        var name_map: std.StringHashMapUnmanaged(usize) = .{};
        errdefer name_map.deinit(allocator);
        for (accessors, 0..) |acc, i| {
            try name_map.put(allocator, acc.name, i);
        }
        return .{
            .host_ptr = host,
            .accessors = accessors,
            .name_map = name_map,
            .allocator = allocator,
        };
    }

    pub fn deinit(self: *TypedEditable) void {
        self.name_map.deinit(self.allocator);
    }

    pub fn setByString(self: *TypedEditable, key: []const u8, value: []const u8, role: Role) !void {
        const idx = self.name_map.get(key) orelse return error.FieldNotFound;
        const accessor = self.accessors[idx];
        if (!accessor.permissions.canWrite(role)) return error.AccessDenied;
        try accessor.setString(self.allocator, self.host_ptr, value);
    }

    pub fn getByString(self: *const TypedEditable, key: []const u8, role: Role) ![]const u8 {
        const idx = self.name_map.get(key) orelse return error.FieldNotFound;
        const accessor = self.accessors[idx];
        if (!accessor.permissions.canRead(role)) return error.AccessDenied;
        return accessor.getString(self.allocator, self.host_ptr);
    }

    pub fn setTyped(self: *TypedEditable, key: []const u8, value: anytype, role: Role) !void {
        const idx = self.name_map.get(key) orelse return error.FieldNotFound;
        const accessor = self.accessors[idx];
        if (!accessor.permissions.canWrite(role)) return error.AccessDenied;

        const T = @TypeOf(value);
        if (typeIdFromType(T) != accessor.type_id) return error.TypeMismatch;

        if (!accessor.validateRange(value)) return error.OutOfRange;

        accessor.setTyped(self.host_ptr, value);
    }

    pub fn getTyped(self: *const TypedEditable, key: []const u8, comptime T: type, role: Role) !T {
        const idx = self.name_map.get(key) orelse return error.FieldNotFound;
        const accessor = self.accessors[idx];
        if (!accessor.permissions.canRead(role)) return error.AccessDenied;

        if (typeIdFromType(T) != accessor.type_id) return error.TypeMismatch;

        return accessor.getTyped(self.host_ptr, T);
    }

    pub fn getAccessor(self: *const TypedEditable, key: []const u8) ?*const TypedAccessor {
        const idx = self.name_map.get(key) orelse return null;
        return &self.accessors[idx];
    }
};

pub const ValidationError = error{
    FieldNotFound,
    AccessDenied,
    TypeMismatch,
    OutOfRange,
    InvalidValue,
    NotAnEnum,
    UnsupportedType,
    UnsupportedIntSize,
    UnsupportedPointerType,
    BufferTooSmall,
};





