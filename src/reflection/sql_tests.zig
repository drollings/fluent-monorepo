//! Tests for sql.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const sql_mod = @import("sql.zig");

test "TableSchema SQL generation" {
    const testing = std.testing;
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const int_constraint = @import("constraint.zig").Constraint(i64);
    const str_constraint = @import("constraint.zig").Constraint([]const u8);

    const perm_all = @import("permissions.zig").perm_all;
    var columns = [_]sql_mod.SqlColumn{
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

    var schema = try sql_mod.TableSchema.init(allocator, "test_table", &columns);
    defer schema.deinit();

    const insert_sql = try schema.generateInsertSql(allocator);
    defer allocator.free(insert_sql);
    try testing.expectEqualStrings("INSERT INTO test_table (id, name) VALUES (?, ?)", insert_sql);

    const select_sql = try schema.generateSelectSql(allocator, "id = ?");
    defer allocator.free(select_sql);
    try testing.expectEqualStrings("SELECT id, name FROM test_table WHERE id = ?", select_sql);
}
