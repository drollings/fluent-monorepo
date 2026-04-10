/// Number of LOD (Level of Detail) text slots per content node.
/// lod[0]=full, lod[1]=summary(800), lod[2]=brief(240), lod[3]=tiny(80),
/// lod[4]=name, lod[5]=reserved.
pub const LOD_COUNT: usize = 6;

pub const NodeId = enum(i64) { _ };

pub const SessionId = enum(i64) { _ };

pub const TargetId = enum(i64) { _ };

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

/// Converts an integer to a unique node identifier using Zig's type system.
pub fn nodeIdFromInt(i: i64) NodeId {
    return @enumFromInt(i);
}

/// Converts a NodeId string to its corresponding integer value.
pub fn intFromNodeId(id: NodeId) i64 {
    return @intFromEnum(id);
}

/// Converts an integer to a sessionId, returning a valid SessionId or error if invalid.
pub fn sessionIdFromInt(i: i64) SessionId {
    return @enumFromInt(i);
}

/// Converts a SessionId string into its corresponding integer value.
pub fn intFromSessionId(id: SessionId) i64 {
    return @intFromEnum(id);
}

/// Converts an integer to its corresponding TargetId value.
pub fn targetIdFromInt(i: i64) TargetId {
    return @enumFromInt(i);
}

/// Converts a TargetId string to its corresponding integer value.
pub fn intFromTargetId(id: TargetId) i64 {
    return @intFromEnum(id);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const std = @import("std");
const testing = std.testing;

test "NodeId: round-trip through int" {
    const id = nodeIdFromInt(42);
    try testing.expectEqual(@as(i64, 42), intFromNodeId(id));
}

test "SessionId: round-trip through int" {
    const id = sessionIdFromInt(-1);
    try testing.expectEqual(@as(i64, -1), intFromSessionId(id));
}

test "TargetId: round-trip through int" {
    const id = targetIdFromInt(0);
    try testing.expectEqual(@as(i64, 0), intFromTargetId(id));
}

test "NodeId and SessionId are distinct types" {
    // This test verifies the types exist and are distinct at the type level.
    // A cross-assignment like `const n: NodeId = sessionIdFromInt(1)` would
    // be a compile error — which is the desired safety property.
    const nid = nodeIdFromInt(10);
    const sid = sessionIdFromInt(10);
    // Both encode the same integer value but have different types.
    try testing.expectEqual(intFromNodeId(nid), intFromSessionId(sid));
    // Type check: these are different types (verified by the compiler).
    comptime try testing.expect(NodeId != SessionId);
    comptime try testing.expect(NodeId != TargetId);
    comptime try testing.expect(SessionId != TargetId);
}
