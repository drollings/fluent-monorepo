/// tool_registry.zig — Tool Registry Pattern (P4.1)
///
/// Self-registering tool registry for Coral Context agent tools.
/// Inspired by Hermes `registry.py` — tools describe themselves (name,
/// description, parameter schema) and are filtered by toolset membership
/// and runtime availability.
///
/// §Design:
///   Tools are registered by value (no heap indirection).  `getDefinitions`
///   filters by an optional toolset allowlist and can skip unavailable tools
///   (tools that return false from `isAvailable`).
///
/// §Thread safety:
///   ToolRegistry is NOT thread-safe.  Register all tools at startup before
///   the registry is shared.
const std = @import("std");
const Allocator = std.mem.Allocator;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Manages tool registry entries with fixed-size buffers; owned by the system; ensures consistent state across operations.
pub const Tool = struct {
    /// Unique name used in LLM tool calls.
    name: []const u8,
    /// Human-readable description injected into the system prompt.
    description: []const u8,
    /// JSON Schema string describing the parameters.
    parameters_json: []const u8,
    /// Optional toolset tag for filtering (e.g. "search", "memory", "admin").
    toolset: ?[]const u8 = null,
    /// Runtime availability check.  If null, tool is always available.
    is_available: ?*const fn () bool = null,
};

/// Manages tool definitions with a fixed-size buffer pool; owned by the tool registry; ensures consistent state across operations.
pub const ToolDefinition = struct {
    name: []const u8,
    description: []const u8,
    parameters_json: []const u8,
};

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// Manages tool registry entries with ownership and invariants; ensures consistent access patterns.
pub const ToolRegistry = struct {
    tools: std.ArrayListUnmanaged(Tool) = .empty,

    const Self = @This();

    pub fn deinit(self: *Self, allocator: Allocator) void {
        self.tools.deinit(allocator);
    }

    /// Register a tool.  Duplicate names are allowed (last write wins is NOT
    /// enforced; callers are responsible for uniqueness).
    pub fn register(self: *Self, allocator: Allocator, tool: Tool) !void {
        try self.tools.append(allocator, tool);
    }

    /// Return definitions for all matching tools.
    ///
    /// `toolset_filter`: if non-null, include only tools whose `toolset`
    ///   matches one of the listed strings.  Pass null to include all toolsets.
    /// `check_availability`: if true, skip tools whose `isAvailable()` returns false.
    ///
    /// Returns an arena-owned slice of ToolDefinition values.
    pub fn getDefinitions(
        self: *const Self,
        arena: Allocator,
        toolset_filter: ?[]const []const u8,
        check_availability: bool,
    ) ![]ToolDefinition {
        var result: std.ArrayListUnmanaged(ToolDefinition) = .empty;
        for (self.tools.items) |tool| {
            // Toolset filter.
            if (toolset_filter) |filter| {
                var matched = false;
                for (filter) |ts| {
                    if (tool.toolset != null and std.mem.eql(u8, tool.toolset.?, ts)) {
                        matched = true;
                        break;
                    }
                }
                if (!matched) continue;
            }
            // Availability check.
            if (check_availability) {
                if (tool.is_available) |avail_fn| {
                    if (!avail_fn()) continue;
                }
            }
            try result.append(arena, .{
                .name = tool.name,
                .description = tool.description,
                .parameters_json = tool.parameters_json,
            });
        }
        return result.toOwnedSlice(arena);
    }

    /// Return the number of registered tools.
    pub fn count(self: *const Self) usize {
        return self.tools.items.len;
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

/// Checks if the system is always available by evaluating a condition; returns true or false.
fn alwaysAvailable() bool {
    return true;
}
/// Checks for a never available condition with no parameters and returns false.
fn neverAvailable() bool {
    return false;
}

test "ToolRegistry: register and count" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var reg = ToolRegistry{};
    defer reg.deinit(arena.allocator());

    try reg.register(arena.allocator(), .{ .name = "search", .description = "Search docs", .parameters_json = "{}" });
    try reg.register(arena.allocator(), .{ .name = "memory", .description = "Update memory", .parameters_json = "{}" });
    try testing.expectEqual(@as(usize, 2), reg.count());
}

test "ToolRegistry: getDefinitions returns all when no filter" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var reg = ToolRegistry{};
    defer reg.deinit(arena.allocator());

    try reg.register(arena.allocator(), .{ .name = "a", .description = "A", .parameters_json = "{}" });
    try reg.register(arena.allocator(), .{ .name = "b", .description = "B", .parameters_json = "{}" });

    const defs = try reg.getDefinitions(arena.allocator(), null, false);
    try testing.expectEqual(@as(usize, 2), defs.len);
}

test "ToolRegistry: toolset filter includes only matching tools" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var reg = ToolRegistry{};
    defer reg.deinit(arena.allocator());

    try reg.register(arena.allocator(), .{ .name = "search", .description = "S", .parameters_json = "{}", .toolset = "search" });
    try reg.register(arena.allocator(), .{ .name = "memory", .description = "M", .parameters_json = "{}", .toolset = "memory" });
    try reg.register(arena.allocator(), .{ .name = "admin", .description = "A", .parameters_json = "{}", .toolset = "admin" });

    const filter = [_][]const u8{"search"};
    const defs = try reg.getDefinitions(arena.allocator(), &filter, false);
    try testing.expectEqual(@as(usize, 1), defs.len);
    try testing.expectEqualStrings("search", defs[0].name);
}

test "ToolRegistry: availability check skips unavailable tools" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var reg = ToolRegistry{};
    defer reg.deinit(arena.allocator());

    try reg.register(arena.allocator(), .{ .name = "ok", .description = "ok", .parameters_json = "{}", .is_available = alwaysAvailable });
    try reg.register(arena.allocator(), .{ .name = "no", .description = "no", .parameters_json = "{}", .is_available = neverAvailable });

    const defs = try reg.getDefinitions(arena.allocator(), null, true);
    try testing.expectEqual(@as(usize, 1), defs.len);
    try testing.expectEqualStrings("ok", defs[0].name);
}

test "ToolRegistry: availability=false skips check" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var reg = ToolRegistry{};
    defer reg.deinit(arena.allocator());

    try reg.register(arena.allocator(), .{ .name = "no", .description = "no", .parameters_json = "{}", .is_available = neverAvailable });

    // check_availability=false → include even unavailable tools
    const defs = try reg.getDefinitions(arena.allocator(), null, false);
    try testing.expectEqual(@as(usize, 1), defs.len);
}





