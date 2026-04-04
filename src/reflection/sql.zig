/// sql.zig — Schema-driven SQLite binding and hydration.
///
/// Replaces manual sqlite3_bind_* calls with reflection-driven serialization
/// using the existing Accessor/ConstraintVTable infrastructure.
///
/// Architecture:
///   §1 SqlBinder — incremental parameter binding wrapper
///   §2 SqlColumn — column metadata derived from Accessor
///   §3 bindFromSchema — bind struct fields to prepared statement parameters
///   §4 hydrateFromSchema — populate struct from SELECT result columns
///   §5 TableSchema — INSERT/SELECT SQL generation from column list
///
/// Usage (bind):
///   var binder = SqlBinder.init(stmt);
///   try bindFromSchema(&binder, accessors, @ptrCast(&node), .coder);
///   try binder.exec();
///
/// Usage (hydrate):
///   var hydrator = SqlHydrator.init(allocator, stmt);
///   try hydrateFromSchema(&hydrator, accessors, @ptrCast(&node), .coder);
///
/// This follows the FLUENT_WEAVER pattern:
///   - VTable dispatch via ConstraintVTable (zero overhead in release)
///   - Arena-backed string allocation for hydration
///   - Role-based permission checks on each field
const std = @import("std");
const accessor_mod = @import("accessor.zig");
const ConstraintVTable = accessor_mod.ConstraintVTable;
const Accessor = accessor_mod.Accessor;
const TypeTag = accessor_mod.TypeTag;
const Role = accessor_mod.Role;
const RolePermissions = accessor_mod.RolePermissions;
const FieldMeta = accessor_mod.FieldMeta;

pub const c = @cImport({
    @cInclude("sqlite3.h");
});

pub const SQLITE_STATIC: c.sqlite3_destructor_type = null;
pub const SQLITE_TRANSIENT: c.sqlite3_destructor_type = @ptrCast(@as(?*anyopaque, -1));

// ============================================================
// §1 SqlBinder — incremental parameter binding
// ============================================================

/// Manages SQL binding structures, owns runtime context, ensures consistent state across invocations.
pub const SqlBinder = struct {
    stmt: *c.sqlite3_stmt,
    next_param: c_int = 1,
    err: ?anyerror = null,

    pub fn init(stmt: *c.sqlite3_stmt) SqlBinder {
        return .{ .stmt = stmt };
    }

    pub fn bindInt64(self: *SqlBinder, value: i64) void {
        if (self.err != null) return;
        const rc = c.sqlite3_bind_int64(self.stmt, self.next_param, value);
        if (rc != c.SQLITE_OK) self.err = error.BindFailed;
        self.next_param += 1;
    }

    pub fn bindDouble(self: *SqlBinder, value: f64) void {
        if (self.err != null) return;
        const rc = c.sqlite3_bind_double(self.stmt, self.next_param, value);
        if (rc != c.SQLITE_OK) self.err = error.BindFailed;
        self.next_param += 1;
    }

    pub fn bindText(self: *SqlBinder, value: []const u8) void {
        if (self.err != null) return;
        const rc = c.sqlite3_bind_text(self.stmt, self.next_param, value.ptr, @intCast(value.len), SQLITE_STATIC);
        if (rc != c.SQLITE_OK) self.err = error.BindFailed;
        self.next_param += 1;
    }

    pub fn bindTextOwned(self: *SqlBinder, value: []const u8) void {
        if (self.err != null) return;
        const rc = c.sqlite3_bind_text(self.stmt, self.next_param, value.ptr, @intCast(value.len), SQLITE_TRANSIENT);
        if (rc != c.SQLITE_OK) self.err = error.BindFailed;
        self.next_param += 1;
    }

    pub fn bindBlob(self: *SqlBinder, value: []const u8) void {
        if (self.err != null) return;
        const rc = c.sqlite3_bind_blob(self.stmt, self.next_param, value.ptr, @intCast(value.len), SQLITE_STATIC);
        if (rc != c.SQLITE_OK) self.err = error.BindFailed;
        self.next_param += 1;
    }

    pub fn bindBlobOwned(self: *SqlBinder, value: []const u8) void {
        if (self.err != null) return;
        const rc = c.sqlite3_bind_blob(self.stmt, self.next_param, value.ptr, @intCast(value.len), SQLITE_TRANSIENT);
        if (rc != c.SQLITE_OK) self.err = error.BindFailed;
        self.next_param += 1;
    }

    pub fn bindNull(self: *SqlBinder) void {
        if (self.err != null) return;
        const rc = c.sqlite3_bind_null(self.stmt, self.next_param);
        if (rc != c.SQLITE_OK) self.err = error.BindFailed;
        self.next_param += 1;
    }

    pub fn bindBool(self: *SqlBinder, value: bool) void {
        self.bindInt64(if (value) 1 else 0);
    }

    pub fn reset(self: *SqlBinder) void {
        _ = c.sqlite3_reset(self.stmt);
        self.next_param = 1;
        self.err = null;
    }

    pub fn step(self: *SqlBinder) anyerror!bool {
        if (self.err) |e| return e;
        const rc = c.sqlite3_step(self.stmt);
        return switch (rc) {
            c.SQLITE_ROW => true,
            c.SQLITE_DONE => false,
            c.SQLITE_CONSTRAINT => error.ConstraintViolation,
            c.SQLITE_BUSY => error.Busy,
            c.SQLITE_LOCKED => error.Locked,
            c.SQLITE_NOMEM => error.OutOfMemory,
            else => error.SqlError,
        };
    }

    pub fn exec(self: *SqlBinder) anyerror!void {
        if (self.err) |e| return e;
        const rc = c.sqlite3_step(self.stmt);
        if (rc != c.SQLITE_DONE) {
            return switch (rc) {
                c.SQLITE_ROW => error.UnexpectedRow,
                c.SQLITE_CONSTRAINT => error.ConstraintViolation,
                c.SQLITE_BUSY => error.Busy,
                c.SQLITE_LOCKED => error.Locked,
                c.SQLITE_NOMEM => error.OutOfMemory,
                else => error.SqlError,
            };
        }
    }

    pub fn finalize(self: *SqlBinder) void {
        _ = c.sqlite3_finalize(self.stmt);
    }
};

// ============================================================
// §2 SqlHydrator — column extraction wrapper
// ============================================================

/// Manages SQL query execution context, owns runtime bindings; ensures consistent state across calls.
pub const SqlHydrator = struct {
    allocator: std.mem.Allocator,
    arena: std.heap.ArenaAllocator,
    stmt: *c.sqlite3_stmt,
    next_col: c_int = 0,
    err: ?anyerror = null,

    pub fn init(allocator: std.mem.Allocator, stmt: *c.sqlite3_stmt) SqlHydrator {
        return .{
            .allocator = allocator,
            .arena = std.heap.ArenaAllocator.init(allocator),
            .stmt = stmt,
        };
    }

    pub fn deinit(self: *SqlHydrator) void {
        self.arena.deinit();
    }

    pub fn reset(self: *SqlHydrator) void {
        self.arena.reset();
        self.next_col = 0;
        self.err = null;
    }

    pub fn columnInt64(self: *SqlHydrator) i64 {
        if (self.err != null) return 0;
        const val = c.sqlite3_column_int64(self.stmt, self.next_col);
        self.next_col += 1;
        return val;
    }

    pub fn columnDouble(self: *SqlHydrator) f64 {
        if (self.err != null) return 0.0;
        const val = c.sqlite3_column_double(self.stmt, self.next_col);
        self.next_col += 1;
        return val;
    }

    pub fn columnText(self: *SqlHydrator) ?[]const u8 {
        if (self.err != null) return null;
        if (c.sqlite3_column_type(self.stmt, self.next_col) == c.SQLITE_NULL) {
            self.next_col += 1;
            return null;
        }
        const ptr = c.sqlite3_column_text(self.stmt, self.next_col);
        const len = c.sqlite3_column_bytes(self.stmt, self.next_col);
        self.next_col += 1;
        if (ptr == null or len == 0) return "";
        const slice = ptr.?[0..@intCast(len)];
        return self.arena.allocator().dupe(u8, slice) catch {
            self.err = error.OutOfMemory;
            return null;
        };
    }

    pub fn columnTextOwned(self: *SqlHydrator) ?[]const u8 {
        if (self.err != null) return null;
        if (c.sqlite3_column_type(self.stmt, self.next_col) == c.SQLITE_NULL) {
            self.next_col += 1;
            return null;
        }
        const ptr = c.sqlite3_column_text(self.stmt, self.next_col);
        const len = c.sqlite3_column_bytes(self.stmt, self.next_col);
        self.next_col += 1;
        if (ptr == null or len == 0) return "";
        const slice = ptr.?[0..@intCast(len)];
        return self.allocator.dupe(u8, slice) catch {
            self.err = error.OutOfMemory;
            return null;
        };
    }

    pub fn columnBlob(self: *SqlHydrator) ?[]const u8 {
        if (self.err != null) return null;
        if (c.sqlite3_column_type(self.stmt, self.next_col) == c.SQLITE_NULL) {
            self.next_col += 1;
            return null;
        }
        const ptr = c.sqlite3_column_blob(self.stmt, self.next_col);
        const len = c.sqlite3_column_bytes(self.stmt, self.next_col);
        self.next_col += 1;
        if (ptr == null or len == 0) return null;
        return ptr.?[0..@intCast(len)];
    }

    pub fn columnBlobOwned(self: *SqlHydrator) ?[]const u8 {
        if (self.err != null) return null;
        if (c.sqlite3_column_type(self.stmt, self.next_col) == c.SQLITE_NULL) {
            self.next_col += 1;
            return null;
        }
        const ptr = c.sqlite3_column_blob(self.stmt, self.next_col);
        const len = c.sqlite3_column_bytes(self.stmt, self.next_col);
        self.next_col += 1;
        if (ptr == null or len == 0) return null;
        return self.allocator.dupe(u8, ptr.?[0..@intCast(len)]) catch {
            self.err = error.OutOfMemory;
            return null;
        };
    }

    pub fn columnBool(self: *SqlHydrator) bool {
        return self.columnInt64() != 0;
    }

    pub fn columnType(self: *SqlHydrator) c_int {
        const t = c.sqlite3_column_type(self.stmt, self.next_col);
        self.next_col += 1;
        return t;
    }

    pub fn peekType(self: *const SqlHydrator) c_int {
        return c.sqlite3_column_type(self.stmt, self.next_col);
    }

    pub fn peekCol(self: *const SqlHydrator) c_int {
        return self.next_col;
    }
};

// ============================================================
// §3 SqlColumn — column metadata
// ============================================================

/// Represents a SQL column structure with compile-time metadata; owned by the module; ensures type safety and invariants.
pub const SqlColumn = struct {
    name: []const u8,
    sql_type: SqlType,
    accessor: *const Accessor,
    /// Column order in the table (0-based)
    order: usize,

    pub const SqlType = enum {
        integer,
        real,
        text,
        blob,

        pub fn fromTypeTag(tag: TypeTag) SqlType {
            return switch (tag) {
                .int => .integer,
                .float => .real,
                .bool => .integer,
                .@"enum" => .integer,
                .string_owned => .text,
                .string_borrowed => .text,
                .string_rc => .text,
                .bitset => .blob,
                .array => .text,
                .vector => .blob,
                .optional => .text,
                .collection => .blob,
                .unknown => .text,
            };
        }

        pub fn toString(self: SqlType) []const u8 {
            return switch (self) {
                .integer => "INTEGER",
                .real => "REAL",
                .text => "TEXT",
                .blob => "BLOB",
            };
        }
    };
};

// ============================================================
// §4 bindFromSchema — schema-driven parameter binding
// ============================================================

/// Transforms a schema reference into a Zig data structure, handling binders and accessors.
pub fn bindFromSchema(
    binder: *SqlBinder,
    allocator: std.mem.Allocator,
    accessors: []const Accessor,
    data: *anyopaque,
    role: Role,
) anyerror!void {
    for (accessors) |acc| {
        if (!acc.permissions.canRead(role)) {
            binder.bindNull();
            continue;
        }
        const field_ptr: *anyopaque = @as([*]u8, @ptrCast(data))[acc.offset..].ptr;
        const str_value = if (acc.constraint.getCtxFn) |getCtxFn|
            try getCtxFn(acc.constraint, allocator, field_ptr)
        else
            try acc.constraint.getFn(allocator, field_ptr);
        defer allocator.free(str_value);
        const sql_type = SqlColumn.SqlType.fromTypeTag(acc.type_tag);
        switch (sql_type) {
            .integer => {
                const int_val = std.fmt.parseInt(i64, str_value, 10) catch {
                    binder.bindTextOwned(str_value);
                    continue;
                };
                binder.bindInt64(int_val);
            },
            .real => {
                const float_val = std.fmt.parseFloat(f64, str_value) catch {
                    binder.bindTextOwned(str_value);
                    continue;
                };
                binder.bindDouble(float_val);
            },
            .text => {
                binder.bindTextOwned(str_value);
            },
            .blob => {
                binder.bindBlobOwned(str_value);
            },
        }
    }
    if (binder.err) |e| return e;
}

/// Assigns a value to a specified field using reflection, returning no value on success or an error on failure.
pub fn bindField(
    binder: *SqlBinder,
    allocator: std.mem.Allocator,
    accessor: *const Accessor,
    data: *anyopaque,
    role: Role,
) anyerror!void {
    if (!accessor.permissions.canRead(role)) {
        binder.bindNull();
        return;
    }
    const field_ptr: *anyopaque = @as([*]u8, @ptrCast(data))[accessor.offset..].ptr;
    const str_value = if (accessor.constraint.getCtxFn) |getCtxFn|
        try getCtxFn(accessor.constraint, allocator, field_ptr)
    else
        try accessor.constraint.getFn(allocator, field_ptr);
    defer allocator.free(str_value);
    const sql_type = SqlColumn.SqlType.fromTypeTag(accessor.type_tag);
    switch (sql_type) {
        .integer => binder.bindInt64(try std.fmt.parseInt(i64, str_value, 10)),
        .real => binder.bindDouble(try std.fmt.parseFloat(f64, str_value)),
        .text => binder.bindTextOwned(str_value),
        .blob => binder.bindBlobOwned(str_value),
    }
}

// ============================================================
// §5 hydrateFromSchema — schema-driven row hydration
// ============================================================

/// Transforms a schema into hydration data, ensuring accessors are properly initialized.
pub fn hydrateFromSchema(
    hydrator: *SqlHydrator,
    accessors: []const Accessor,
    data: *anyopaque,
    role: Role,
) anyerror!void {
    for (accessors) |acc| {
        if (!acc.permissions.canWrite(role)) {
            _ = hydrator.columnType();
            continue;
        }
        const field_ptr: *anyopaque = @as([*]u8, @ptrCast(data))[acc.offset..].ptr;
        const sql_type = SqlColumn.SqlType.fromTypeTag(acc.type_tag);
        switch (sql_type) {
            .integer => {
                const val = hydrator.columnInt64();
                if (hydrator.err) |e| return e;
                const str_val = try std.fmt.allocPrint(hydrator.arena.allocator(), "{}", .{val});
                if (acc.constraint.setCtxFn) |setCtxFn|
                    try setCtxFn(acc.constraint, hydrator.allocator, field_ptr, str_val)
                else
                    try acc.constraint.setFn(hydrator.allocator, field_ptr, str_val);
            },
            .real => {
                const val = hydrator.columnDouble();
                if (hydrator.err) |e| return e;
                const str_val = try std.fmt.allocPrint(hydrator.arena.allocator(), "{d}", .{val});
                if (acc.constraint.setCtxFn) |setCtxFn|
                    try setCtxFn(acc.constraint, hydrator.allocator, field_ptr, str_val)
                else
                    try acc.constraint.setFn(hydrator.allocator, field_ptr, str_val);
            },
            .text => {
                const val = hydrator.columnTextOwned();
                if (hydrator.err) |e| return e;
                if (val) |v| {
                    if (acc.constraint.setCtxFn) |setCtxFn|
                        try setCtxFn(acc.constraint, hydrator.allocator, field_ptr, v)
                    else
                        try acc.constraint.setFn(hydrator.allocator, field_ptr, v);
                    hydrator.allocator.free(v);
                } else {
                    if (acc.type_tag == .optional) {
                        if (acc.constraint.setCtxFn) |setCtxFn|
                            try setCtxFn(acc.constraint, hydrator.allocator, field_ptr, "null")
                        else
                            try acc.constraint.setFn(hydrator.allocator, field_ptr, "null");
                    }
                }
            },
            .blob => {
                const val = hydrator.columnBlobOwned();
                if (hydrator.err) |e| return e;
                if (val) |v| {
                    if (acc.constraint.setCtxFn) |setCtxFn|
                        try setCtxFn(acc.constraint, hydrator.allocator, field_ptr, v)
                    else
                        try acc.constraint.setFn(hydrator.allocator, field_ptr, v);
                    hydrator.allocator.free(v);
                }
            },
        }
    }
    if (hydrator.err) |e| return e;
}

/// Hydrates a field using a hydrator, accessor, and role, returning an array of errors or null.
pub fn hydrateField(
    hydrator: *SqlHydrator,
    accessor: *const Accessor,
    data: *anyopaque,
    role: Role,
) anyerror!?[]const u8 {
    if (!accessor.permissions.canWrite(role)) {
        _ = hydrator.columnType();
        return null;
    }
    const sql_type = SqlColumn.SqlType.fromTypeTag(accessor.type_tag);
    const str_val: []const u8 = switch (sql_type) {
        .integer => blk: {
            const val = hydrator.columnInt64();
            if (hydrator.err) |e| return e;
            break :blk try std.fmt.allocPrint(hydrator.arena.allocator(), "{}", .{val});
        },
        .real => blk: {
            const val = hydrator.columnDouble();
            if (hydrator.err) |e| return e;
            break :blk try std.fmt.allocPrint(hydrator.arena.allocator(), "{d}", .{val});
        },
        .text => blk: {
            const val = hydrator.columnText();
            if (hydrator.err) |e| return e;
            break :blk val orelse "";
        },
        .blob => blk: {
            const val = hydrator.columnBlob();
            if (hydrator.err) |e| return e;
            break :blk val orelse "";
        },
    };
    const field_ptr: *anyopaque = @as([*]u8, @ptrCast(data))[accessor.offset..].ptr;
    if (accessor.constraint.setCtxFn) |setCtxFn|
        try setCtxFn(accessor.constraint, hydrator.allocator, field_ptr, str_val)
    else
        try accessor.constraint.setFn(hydrator.allocator, field_ptr, str_val);
    return str_val;
}

// ============================================================
// §6 TableSchema — SQL generation from column list
// ============================================================

/// Defines a schema table structure with fixed-size buffers; managed via ownership model; immutable by design.
pub const TableSchema = struct {
    name: []const u8,
    columns: []const SqlColumn,
    /// Arena-allocated SQL strings
    arena: std.heap.ArenaAllocator,

    pub fn init(allocator: std.mem.Allocator, name: []const u8, columns: []const SqlColumn) !TableSchema {
        var arena = std.heap.ArenaAllocator.init(allocator);
        errdefer arena.deinit();
        const name_copy = try arena.allocator().dupe(u8, name);
        return .{
            .name = name_copy,
            .columns = columns,
            .arena = arena,
        };
    }

    pub fn deinit(self: *TableSchema) void {
        self.arena.deinit();
    }

    pub fn generateInsertSql(self: *TableSchema, allocator: std.mem.Allocator) ![]const u8 {
        var buf = std.ArrayList(u8).init(allocator);
        errdefer buf.deinit();
        const writer = buf.writer();
        try writer.print("INSERT INTO {s} (", .{self.name});
        for (self.columns, 0..) |col, i| {
            if (i > 0) try writer.writeAll(", ");
            try writer.writeAll(col.name);
        }
        try writer.writeAll(") VALUES (");
        for (self.columns, 0..) |_, i| {
            if (i > 0) try writer.writeAll(", ");
            try writer.writeAll("?");
        }
        try writer.writeAll(")");
        return buf.toOwnedSlice();
    }

    pub fn generateSelectSql(self: *TableSchema, allocator: std.mem.Allocator, where_clause: ?[]const u8) ![]const u8 {
        var buf = std.ArrayList(u8).init(allocator);
        errdefer buf.deinit();
        const writer = buf.writer();
        try writer.print("SELECT ", .{});
        for (self.columns, 0..) |col, i| {
            if (i > 0) try writer.writeAll(", ");
            try writer.writeAll(col.name);
        }
        try writer.print(" FROM {s}", .{self.name});
        if (where_clause) |wc| {
            try writer.print(" WHERE {s}", .{wc});
        }
        return buf.toOwnedSlice();
    }

    pub fn generateUpdateSql(self: *TableSchema, allocator: std.mem.Allocator, where_clause: []const u8) ![]const u8 {
        var buf = std.ArrayList(u8).init(allocator);
        errdefer buf.deinit();
        const writer = buf.writer();
        try writer.print("UPDATE {s} SET ", .{self.name});
        for (self.columns, 0..) |col, i| {
            if (i > 0) try writer.writeAll(", ");
            try writer.print("{s} = ?", .{col.name});
        }
        try writer.print(" WHERE {s}", .{where_clause});
        return buf.toOwnedSlice();
    }

    pub fn generateCreateTableSql(self: *TableSchema, allocator: std.mem.Allocator, primary_key: ?[]const u8) ![]const u8 {
        var buf = std.ArrayList(u8).init(allocator);
        errdefer buf.deinit();
        const writer = buf.writer();
        try writer.print("CREATE TABLE IF NOT EXISTS {s} (", .{self.name});
        for (self.columns, 0..) |col, i| {
            if (i > 0) try writer.writeAll(", ");
            try writer.print("{s} {s}", .{ col.name, col.sql_type.toString() });
        }
        if (primary_key) |pk| {
            try writer.print(", PRIMARY KEY ({s})", .{pk});
        }
        try writer.writeAll(")");
        return buf.toOwnedSlice();
    }

    /// Create a TableSchema from an Accessor array.
    /// The accessor must have sql_type set (defaults to TEXT for unknown).
    pub fn fromAccessors(
        allocator: std.mem.Allocator,
        table_name: []const u8,
        accessors: []const Accessor,
        column_names: ?[]const []const u8,
    ) !TableSchema {
        var arena = std.heap.ArenaAllocator.init(allocator);
        errdefer arena.deinit();
        var columns = std.ArrayList(SqlColumn).init(allocator);
        errdefer columns.deinit();
        for (accessors, 0..) |*acc, i| {
            const col_name = if (column_names) |names| names[i] else acc.name;
            const name_copy = try arena.allocator().dupe(u8, col_name);
            try columns.append(.{
                .name = name_copy,
                .sql_type = SqlColumn.SqlType.fromTypeTag(acc.type_tag),
                .accessor = acc,
                .order = i,
            });
        }
        const name_copy = try arena.allocator().dupe(u8, table_name);
        return .{
            .name = name_copy,
            .columns = columns.toOwnedSlice(),
            .arena = arena,
        };
    }
};

// ============================================================
// §7 Tests (non-sqlite dependent)
// ============================================================

test "TableSchema SQL generation" {
    const testing = std.testing;
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const int_constraint = @import("constraint.zig").Constraint(i64);
    const str_constraint = @import("constraint.zig").Constraint([]const u8);

    const perm_all = @import("permissions.zig").perm_all;
    var columns = [_]SqlColumn{
        .{
            .name = "id",
            .sql_type = .integer,
            .accessor = &.{
                .name = "id",
                .offset = 0,
                .permissions = perm_all,
                .constraint = &int_constraint,
                .type_tag = .int,
                .sql_type = .integer,
            },
            .order = 0,
        },
        .{
            .name = "name",
            .sql_type = .text,
            .accessor = &.{
                .name = "name",
                .offset = 8,
                .permissions = perm_all,
                .constraint = &str_constraint,
                .type_tag = .string_owned,
                .sql_type = .text,
            },
            .order = 1,
        },
    };

    var schema = try TableSchema.init(allocator, "test_table", &columns);
    defer schema.deinit();

    const insert_sql = try schema.generateInsertSql(allocator);
    defer allocator.free(insert_sql);
    try testing.expectEqualStrings("INSERT INTO test_table (id, name) VALUES (?, ?)", insert_sql);

    const select_sql = try schema.generateSelectSql(allocator, "id = ?");
    defer allocator.free(select_sql);
    try testing.expectEqualStrings("SELECT id, name FROM test_table WHERE id = ?", select_sql);
}








