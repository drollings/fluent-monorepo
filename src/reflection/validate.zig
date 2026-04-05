//! validate.zig — Runtime validation pipeline for FieldMeta constraints (M6).
//!
//! ## Purpose
//!
//! `FieldMeta` carries validation rules (`min`, `max`, `enum_values`, `pattern`,
//! `custom_validate`).  This module enforces those rules at runtime when a field
//! is set via `Editable.set()`.
//!
//! The pipeline runs in this order for each set() call:
//!
//!   1. `enum_values` check — if set, value must match one of the allowed strings.
//!   2. `custom_validate` check — if set, the function must return true.
//!   3. `pattern` check — if set, value must match the simplified glob pattern.
//!
//! Range (min/max) validation is already handled by `Constraint(T).setFn` at the
//! type level (integer/float parsing enforces numeric bounds via the constraint
//! pipeline).  Only the string-level checks above need a separate pass.
//!
//! ## Pattern Syntax (simplified)
//!
//! A subset of common patterns:
//!   - `^prefix`   — value must start with "prefix"
//!   - `suffix$`   — value must end with "suffix"
//!   - `^prefix$`  — value must equal "prefix"
//!   - `*word*`    — value must contain "word" (literal `*` as wildcard)
//!   - No regex character classes or quantifiers.
//!
//! Patterns are case-sensitive.  For full regex support, use `custom_validate`.
//!
//! ## Usage
//!
//! Editable.set() calls validateValue before dispatching to the constraint vtable:
//!
//!   try validateValue(accessor.meta, raw_value);
//!   try accessor.constraint.setFn(allocator, field_ptr, raw_value);

const std = @import("std");
const accessor_mod = @import("accessor.zig");
const FieldMeta = accessor_mod.FieldMeta;

// ── Error set ─────────────────────────────────────────────────────────────────

pub const ValidationError = error{
    /// Value not in the `enum_values` list.
    InvalidEnumValue,
    /// `custom_validate` returned false.
    CustomValidationFailed,
    /// Value does not match the `pattern`.
    PatternMismatch,
};

// ── validateEnumValues ────────────────────────────────────────────────────────

/// Validates enum values against a provided slice of u8, returning a ValidationError if any value is invalid.
pub fn validateEnumValues(meta: FieldMeta, value: []const u8) ValidationError!void {
    const allowed = meta.enum_values orelse return;
    for (allowed) |candidate| {
        if (std.mem.eql(u8, value, candidate)) return;
    }
    return error.InvalidEnumValue;
}

// ── validateCustom ────────────────────────────────────────────────────────────

/// Validates a Zig field against a Zig string, returning a ValidationError if mismatched.
pub fn validateCustom(meta: FieldMeta, value: []const u8) ValidationError!void {
    const f = meta.custom_validate orelse return;
    if (!f(value)) return error.CustomValidationFailed;
}

// ── validatePattern ───────────────────────────────────────────────────────────

/// Validates a Zig pattern against a given value, returning a ValidationError if mismatched.
pub fn validatePattern(meta: FieldMeta, value: []const u8) ValidationError!void {
    const pat = meta.pattern orelse return;
    if (pat.len == 0) return;

    const has_start = pat[0] == '^';
    const has_end = pat[pat.len - 1] == '$';
    var inner = pat;
    if (has_start) inner = inner[1..];
    if (has_end) inner = inner[0 .. inner.len - 1];

    // Fast path: no wildcards.
    if (std.mem.indexOfScalar(u8, inner, '*') == null) {
        if (has_start and has_end) {
            if (!std.mem.eql(u8, value, inner)) return error.PatternMismatch;
        } else if (has_start) {
            if (!std.mem.startsWith(u8, value, inner)) return error.PatternMismatch;
        } else if (has_end) {
            if (!std.mem.endsWith(u8, value, inner)) return error.PatternMismatch;
        } else {
            if (std.mem.indexOf(u8, value, inner) == null) return error.PatternMismatch;
        }
        return;
    }

    // Wildcard path: split `inner` on `*` to get literal segments.
    // The first segment must match at the start (if `has_start`).
    // The last segment must match at the end (if `has_end`).
    // Intermediate segments must appear in order.
    var segments: [16][]const u8 = undefined;
    var seg_count: usize = 0;

    var remaining = inner;
    while (std.mem.indexOfScalar(u8, remaining, '*')) |star_pos| {
        if (seg_count >= 15) break;
        segments[seg_count] = remaining[0..star_pos];
        seg_count += 1;
        remaining = remaining[star_pos + 1 ..];
    }
    segments[seg_count] = remaining;
    seg_count += 1;

    var pos: usize = 0;
    for (segments[0..seg_count], 0..) |seg, si| {
        if (seg.len == 0) continue;
        if (si == 0 and has_start) {
            if (!std.mem.startsWith(u8, value, seg)) return error.PatternMismatch;
            pos = seg.len;
        } else if (si == seg_count - 1 and has_end) {
            if (!std.mem.endsWith(u8, value, seg)) return error.PatternMismatch;
        } else {
            const found = std.mem.indexOf(u8, value[pos..], seg) orelse return error.PatternMismatch;
            pos += found + seg.len;
        }
    }
}

// ── validateValue ─────────────────────────────────────────────────────────────

/// Validates a Zig value slice against a field metadata constraint.
pub fn validateValue(meta: FieldMeta, value: []const u8) ValidationError!void {
    try validateEnumValues(meta, value);
    try validateCustom(meta, value);
    try validatePattern(meta, value);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;

test "validateEnumValues: valid value passes" {
    const meta: FieldMeta = .{ .enum_values = &.{ "low", "medium", "high" } };
    try validateEnumValues(meta, "medium");
}

test "validateEnumValues: invalid value fails" {
    const meta: FieldMeta = .{ .enum_values = &.{ "low", "medium", "high" } };
    try testing.expectError(error.InvalidEnumValue, validateEnumValues(meta, "critical"));
}

test "validateEnumValues: null passes unconditionally" {
    try validateEnumValues(.{}, "anything");
}

test "validateCustom: function returning true passes" {
    const alwaysTrue = struct {
        fn f(_: []const u8) bool {
            return true;
        }
    }.f;
    const meta: FieldMeta = .{ .custom_validate = alwaysTrue };
    try validateCustom(meta, "value");
}

test "validateCustom: function returning false fails" {
    const alwaysFalse = struct {
        fn f(_: []const u8) bool {
            return false;
        }
    }.f;
    const meta: FieldMeta = .{ .custom_validate = alwaysFalse };
    try testing.expectError(error.CustomValidationFailed, validateCustom(meta, "value"));
}

test "validateCustom: length check" {
    const minLen8 = struct {
        fn f(v: []const u8) bool {
            return v.len >= 8;
        }
    }.f;
    const meta: FieldMeta = .{ .custom_validate = minLen8 };
    try validateCustom(meta, "password1");
    try testing.expectError(error.CustomValidationFailed, validateCustom(meta, "short"));
}

test "validatePattern: start anchor" {
    const meta: FieldMeta = .{ .pattern = "^user_" };
    try validatePattern(meta, "user_alice");
    try testing.expectError(error.PatternMismatch, validatePattern(meta, "admin_bob"));
}

test "validatePattern: end anchor" {
    const meta: FieldMeta = .{ .pattern = "_id$" };
    try validatePattern(meta, "context_id");
    try testing.expectError(error.PatternMismatch, validatePattern(meta, "context_name"));
}

test "validatePattern: exact match (start+end)" {
    const meta: FieldMeta = .{ .pattern = "^admin$" };
    try validatePattern(meta, "admin");
    try testing.expectError(error.PatternMismatch, validatePattern(meta, "admin_user"));
}

test "validatePattern: wildcard" {
    const meta: FieldMeta = .{ .pattern = "^user_*_id$" };
    try validatePattern(meta, "user_alice_id");
    try testing.expectError(error.PatternMismatch, validatePattern(meta, "user_alice"));
}

test "validatePattern: no anchor (substring match)" {
    const meta: FieldMeta = .{ .pattern = "foo" };
    try validatePattern(meta, "foobar");
    try validatePattern(meta, "my_foo_thing");
    try testing.expectError(error.PatternMismatch, validatePattern(meta, "bar"));
}

test "validatePattern: null passes" {
    try validatePattern(.{}, "anything");
}

test "validateValue: runs all checks in order" {
    const minLen3 = struct {
        fn f(v: []const u8) bool {
            return v.len >= 3;
        }
    }.f;
    const meta: FieldMeta = .{
        .enum_values = &.{ "low", "medium", "high" },
        .custom_validate = minLen3,
    };
    try validateValue(meta, "low");
    try testing.expectError(error.InvalidEnumValue, validateValue(meta, "critical"));
}

test "validateValue: enum passes then custom fails" {
    const rejectMedium = struct {
        fn f(v: []const u8) bool {
            return !std.mem.eql(u8, v, "medium");
        }
    }.f;
    const meta: FieldMeta = .{
        .enum_values = &.{ "low", "medium", "high" },
        .custom_validate = rejectMedium,
    };
    try validateValue(meta, "low");
    try testing.expectError(error.CustomValidationFailed, validateValue(meta, "medium"));
}
