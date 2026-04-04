//! schema_version.zig — Versioning primitives for the reflection schema.
//!
//! ## Purpose
//!
//! `SchemaVersion` labels each schema with a (major, minor) pair so that
//! stored configurations, database rows, and WASM IPC payloads can be
//! compared against the version that generated them.
//!
//! Compatibility semantics:
//!   - **Same major** → compatible.  Fields added/removed with defaults are
//!     backward-compatible within the same major version.
//!   - **Different major** → breaking change.  Callers must run migration code
//!     before reading or writing schema data.
//!
//! ## Usage in Accessor
//!
//! Each `Accessor` carries a `version_added` field (defaults to v1.0):
//!
//!   const my_accessor = Accessor{
//!       .name = "port",
//!       .version_added = SchemaVersion{ .major = 2 },
//!       // ...
//!   };
//!
//! Before reading a field, callers check `SchemaVersion.compatible`:
//!
//!   if (!accessor.version_added.compatible(stored_version)) return error.SchemaMismatch;
//!
//! Fields with a non-null `version_removed` should be treated as absent for
//! schema versions >= `version_removed`.
//!
//! ## Usage in ConstraintVTable
//!
//! `ConstraintVTable.version` identifies the schema version the constraint was
//! compiled for.  `ConstraintVTable.migrateFn`, if non-null, transforms data
//! from an older schema to the current one:
//!
//!   if (vtable.migrateFn) |migrate| {
//!       try migrate(from_version, to_version, allocator, field_ptr);
//!   }

const std = @import("std");

// ── SchemaVersion ─────────────────────────────────────────────────────────────

/// Defines the schema version structure, manages version metadata, and ensures version invariants are enforced.
pub const SchemaVersion = struct {
    major: u16,
    minor: u16 = 0,

    /// Two schemas are compatible if they share the same major version.
    ///
    /// v1.0 is compatible with v1.3.
    /// v1.3 is NOT compatible with v2.0.
    pub fn compatible(self: SchemaVersion, other: SchemaVersion) bool {
        return self.major == other.major;
    }

    /// Returns true if `self` is strictly newer than `other`.
    pub fn isNewerThan(self: SchemaVersion, other: SchemaVersion) bool {
        if (self.major != other.major) return self.major > other.major;
        return self.minor > other.minor;
    }

    /// Returns true if `self` is equal to `other`.
    pub fn eql(self: SchemaVersion, other: SchemaVersion) bool {
        return self.major == other.major and self.minor == other.minor;
    }

    /// Format as "vMAJOR.MINOR" via `{f}` specifier.
    pub fn format(self: SchemaVersion, writer: anytype) !void {
        try writer.print("v{d}.{d}", .{ self.major, self.minor });
    }
};

/// The current schema version used by all compiled-in Accessor and
/// ConstraintVTable instances unless overridden per-field.
pub const SCHEMA_CURRENT: SchemaVersion = .{ .major = 1, .minor = 0 };

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Validates compatibility between two SchemaVersion instances, returning an error if mismatched.
pub fn checkCompatible(stored: SchemaVersion, current: SchemaVersion) error{SchemaMismatch}!void {
    if (!stored.compatible(current)) return error.SchemaMismatch;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;

test "SchemaVersion: compatible same major" {
    const v1_0 = SchemaVersion{ .major = 1, .minor = 0 };
    const v1_3 = SchemaVersion{ .major = 1, .minor = 3 };
    try testing.expect(v1_0.compatible(v1_3));
    try testing.expect(v1_3.compatible(v1_0));
}

test "SchemaVersion: incompatible different major" {
    const v1 = SchemaVersion{ .major = 1 };
    const v2 = SchemaVersion{ .major = 2 };
    try testing.expect(!v1.compatible(v2));
    try testing.expect(!v2.compatible(v1));
}

test "SchemaVersion: isNewerThan major" {
    const v1 = SchemaVersion{ .major = 1 };
    const v2 = SchemaVersion{ .major = 2 };
    try testing.expect(v2.isNewerThan(v1));
    try testing.expect(!v1.isNewerThan(v2));
    try testing.expect(!v1.isNewerThan(v1));
}

test "SchemaVersion: isNewerThan minor" {
    const v1_0 = SchemaVersion{ .major = 1, .minor = 0 };
    const v1_1 = SchemaVersion{ .major = 1, .minor = 1 };
    try testing.expect(v1_1.isNewerThan(v1_0));
    try testing.expect(!v1_0.isNewerThan(v1_1));
}

test "SchemaVersion: eql" {
    const a = SchemaVersion{ .major = 2, .minor = 3 };
    const b = SchemaVersion{ .major = 2, .minor = 3 };
    const c = SchemaVersion{ .major = 2, .minor = 4 };
    try testing.expect(a.eql(b));
    try testing.expect(!a.eql(c));
}

test "SchemaVersion: format" {
    const v = SchemaVersion{ .major = 3, .minor = 7 };
    var buf: [16]u8 = undefined;
    const s = try std.fmt.bufPrint(&buf, "{f}", .{v});
    try testing.expectEqualStrings("v3.7", s);
}

test "SCHEMA_CURRENT: is v1.0" {
    try testing.expectEqual(@as(u16, 1), SCHEMA_CURRENT.major);
    try testing.expectEqual(@as(u16, 0), SCHEMA_CURRENT.minor);
}

test "checkCompatible: matching major succeeds" {
    try checkCompatible(.{ .major = 1, .minor = 5 }, SCHEMA_CURRENT);
}

test "checkCompatible: different major returns SchemaMismatch" {
    const result = checkCompatible(.{ .major = 2 }, SCHEMA_CURRENT);
    try testing.expectError(error.SchemaMismatch, result);
}

test "SchemaVersion: SCHEMA_CURRENT compatible with itself" {
    try testing.expect(SCHEMA_CURRENT.compatible(SCHEMA_CURRENT));
}


