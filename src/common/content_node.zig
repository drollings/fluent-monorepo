/// content_node.zig — ContentNode: LOD text pyramid backed by SharedString
///
/// A ContentNode holds a multi-level-of-detail text representation:
///   lod[0] = full source text (ref-counted via SharedString)
///   lod[1] = summary  (~800 chars)
///   lod[2] = brief    (~240 chars)
///   lod[3] = tiny     (~80 chars)
///   lod[4] = name / label
///   lod[5] = reserved
///
/// Ownership rules:
///   - lod[0] lifetime is managed by the SharedString.Ref in `source`.
///     Bit 0 of lod_owned must always be clear.
///   - lod[1..5] slots are allocator-owned when the corresponding bit in
///     lod_owned is set.
///
/// ContentNode is a common primitive reused by ContextNode (coral) and any
/// other subsystem that needs a SharedString-backed LOD text pyramid.
const std = @import("std");
const SharedString = @import("shared_string.zig").SharedString;
const LOD_COUNT = @import("types.zig").LOD_COUNT;

/// Text content at multiple levels of detail, backed by a ref-counted SharedString for lod[0].
pub const ContentNode = struct {
    /// Ref-counted backing store for lod[0].  Non-null when lod[0] is non-empty.
    /// Bit 0 of lod_owned must always be clear.
    source: ?SharedString.Ref = null,
    lod: [LOD_COUNT][]const u8,
    /// Bitmask: bit i set → lod[i] is allocator-owned.  Bit 0 always clear.
    lod_owned: u8 = 0,

    /// Create a ContentNode with lod[0] = full_text (SharedString).
    /// All other LOD slots are empty.
    pub fn init(allocator: std.mem.Allocator, full_text: []const u8) !ContentNode {
        const src = try SharedString.Ref.init(allocator, full_text);
        return ContentNode{
            .source = src,
            .lod = [_][]const u8{ src.slice(), "", "", "", "", "" },
            .lod_owned = 0,
        };
    }

    pub fn getLod(self: *const ContentNode, level: u3) []const u8 {
        if (level >= LOD_COUNT) return "";
        return self.lod[level];
    }

    /// Set LOD level 1–5.  Level 0 is read-only here; use setSource() instead.
    pub fn setLod(self: *ContentNode, level: u3, value: []const u8) void {
        if (level == 0 or level >= LOD_COUNT) return;
        self.lod[level] = value;
    }

    /// Replace the shared source text (lod[0]).  Releases the old SharedString.
    pub fn setSource(self: *ContentNode, allocator: std.mem.Allocator, text: []const u8) !void {
        const new_src = try SharedString.Ref.init(allocator, text);
        if (self.source) |old| old.release(allocator);
        self.source = new_src;
        self.lod[0] = new_src.slice();
    }

    /// Deep-copy this node.  lod[0] is shared via retain() (no byte copy);
    /// lod[1..5] slots marked in lod_owned are duped into `allocator`.
    pub fn clone(self: *const ContentNode, allocator: std.mem.Allocator) !ContentNode {
        var copy = self.*;
        copy.lod_owned = 0;

        // lod[0]: retain the SharedString ref or dupe the raw slice.
        if (self.source) |src| {
            copy.source = src.retain();
            copy.lod[0] = copy.source.?.slice();
        } else if (self.lod_owned & 1 != 0) {
            copy.lod[0] = try allocator.dupe(u8, self.lod[0]);
            copy.lod_owned |= 1;
        }

        // lod[1..5]: dupe allocator-owned slots.
        for (1..LOD_COUNT) |i| {
            if (self.lod_owned & (@as(u8, 1) << @intCast(i)) != 0) {
                copy.lod[i] = try allocator.dupe(u8, self.lod[i]);
                copy.lod_owned |= @as(u8, 1) << @intCast(i);
            }
        }

        return copy;
    }

    /// Release all owned resources.  Safe to call multiple times.
    pub fn free(self: *ContentNode, allocator: std.mem.Allocator) void {
        // lod[0]: release via SharedString ref.
        if (self.source) |src| {
            src.release(allocator);
            self.source = null;
            self.lod[0] = "";
        }
        // lod[1..5]: free allocator-owned slots (bit 0 must always be clear).
        for (&self.lod, 0..) |*slot, i| {
            if (self.lod_owned & (@as(u8, 1) << @intCast(i)) != 0) {
                allocator.free(slot.*);
                slot.* = "";
                self.lod_owned &= ~(@as(u8, 1) << @intCast(i));
            }
        }
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "ContentNode init and free" {
    const allocator = testing.allocator;
    var cn = try ContentNode.init(allocator, "hello world");
    defer cn.free(allocator);
    try testing.expectEqualStrings("hello world", cn.lod[0]);
    try testing.expectEqualStrings("hello world", cn.getLod(0));
    try testing.expectEqualStrings("", cn.lod[1]);
}

test "ContentNode setLod and getLod" {
    const allocator = testing.allocator;
    var cn = try ContentNode.init(allocator, "full text");
    defer cn.free(allocator);
    cn.setLod(1, "summary");
    try testing.expectEqualStrings("summary", cn.getLod(1));
    // level 0 is read-only via setLod
    cn.setLod(0, "ignored");
    try testing.expectEqualStrings("full text", cn.getLod(0));
}

test "ContentNode setSource" {
    const allocator = testing.allocator;
    var cn = try ContentNode.init(allocator, "original");
    defer cn.free(allocator);
    try cn.setSource(allocator, "replacement");
    try testing.expectEqualStrings("replacement", cn.lod[0]);
}

test "ContentNode clone" {
    const allocator = testing.allocator;
    var cn = try ContentNode.init(allocator, "source text");
    defer cn.free(allocator);
    const name_copy = try allocator.dupe(u8, "my-name");
    cn.lod[4] = name_copy;
    cn.lod_owned |= 1 << 4;

    var copy = try cn.clone(allocator);
    defer copy.free(allocator);

    try testing.expectEqualStrings("source text", copy.lod[0]);
    try testing.expectEqualStrings("my-name", copy.lod[4]);
    // Modifying original lod[4] does not affect copy
    cn.lod[4] = "changed";
    try testing.expectEqualStrings("my-name", copy.lod[4]);
}
