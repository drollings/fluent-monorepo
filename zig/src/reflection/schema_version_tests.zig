//! Tests for schema_version.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const schema_version_mod = @import("schema_version.zig");

test "checkCompatible: matching major succeeds" {
    try schema_version_mod.checkCompatible(.{ .major = 1, .minor = 5 }, schema_version_mod.SCHEMA_CURRENT);
}
