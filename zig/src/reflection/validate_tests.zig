//! Tests for validate.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const validate_mod = @import("validate.zig");

test "validateEnumValues: null passes unconditionally" {
    try validate_mod.validateEnumValues(.{}, "anything");
}
test "validatePattern: null passes" {
    try validate_mod.validatePattern(.{}, "anything");
}
