//! mock_vtable.zig — Mock implementations of VTable interfaces for testing.
//!
//! Provides `MockEmbeddingProvider` which:
//!   - Records all calls with their arguments
//!   - Returns configurable vectors or errors
//!   - Asserts call count and arguments
//!   - Requires no external services (Ollama, OpenAI, etc.)
//!
//! Usage:
//!
//!   var mock = MockEmbeddingProvider.init(std.testing.allocator);
//!   defer mock.deinit();
//!
//!   mock.setEmbedResult(&[_]f32{ 0.1, 0.2, 0.3 });
//!
//!   var provider = mock.provider();
//!   const vec = try provider.embed(std.testing.allocator, "hello");
//!   defer std.testing.allocator.free(vec);
//!
//!   mock.assertCallCount("embed", 1);

const std = @import("std");

// ── CallRecord ────────────────────────────────────────────────────────────────

/// A single recorded call to the mock.
pub const CallRecord = struct {
    method: []const u8,
    /// The text argument passed to embed(). Owned by the mock's arena.
    text: []const u8,
};

// ── MockEmbeddingProvider ─────────────────────────────────────────────────────

/// Mock implementation of EmbeddingProvider.VTable for unit testing.
///
/// All state is owned by the mock struct.  Call deinit() after the test.
pub const MockEmbeddingProvider = struct {
    allocator: std.mem.Allocator,
    /// All recorded calls in order.
    calls: std.ArrayListUnmanaged(CallRecord),
    /// If set, embed() returns a copy of this slice.
    embed_result: ?[]const f32,
    /// If set, embed() returns this error instead of a vector.
    embed_error: ?anyerror,

    const Self = @This();

    // ── Lifecycle ────────────────────────────────────────────────────────────

    pub fn init(allocator: std.mem.Allocator) Self {
        return .{
            .allocator = allocator,
            .calls = .empty,
            .embed_result = null,
            .embed_error = null,
        };
    }

    pub fn deinit(self: *Self) void {
        for (self.calls.items) |rec| {
            self.allocator.free(rec.text);
        }
        self.calls.deinit(self.allocator);
        if (self.embed_result) |r| self.allocator.free(r);
    }

    // ── Configuration ─────────────────────────────────────────────────────────

    /// Configure embed() to return a copy of `result`.
    /// Clears any previously configured error.
    pub fn setEmbedResult(self: *Self, result: []const f32) void {
        if (self.embed_result) |old| self.allocator.free(old);
        self.embed_result = self.allocator.dupe(f32, result) catch @panic("OOM in setEmbedResult");
        self.embed_error = null;
    }

    /// Configure embed() to return `err` instead of a vector.
    /// Clears any previously configured result.
    pub fn setEmbedError(self: *Self, err: anyerror) void {
        if (self.embed_result) |old| self.allocator.free(old);
        self.embed_result = null;
        self.embed_error = err;
    }

    // ── VTable implementation ─────────────────────────────────────────────────

    fn implName(_: *anyopaque) []const u8 {
        return "mock";
    }

    fn implDimensions(_: *anyopaque) u32 {
        return 3; // test dimension
    }

    fn implEmbed(ptr: *anyopaque, allocator: std.mem.Allocator, text: []const u8) anyerror![]f32 {
        const self: *Self = @ptrCast(@alignCast(ptr));

        // Record the call (copy the text into the mock's allocator).
        const text_copy = self.allocator.dupe(u8, text) catch return error.OutOfMemory;
        self.calls.append(self.allocator, .{
            .method = "embed",
            .text = text_copy,
        }) catch {
            self.allocator.free(text_copy);
            return error.OutOfMemory;
        };

        // Return configured error if set.
        if (self.embed_error) |err| return err;

        // Return configured result if set.
        if (self.embed_result) |result| {
            return allocator.dupe(f32, result);
        }

        // Default: return empty vector.
        return allocator.alloc(f32, 0);
    }

    fn implDeinit(_: *anyopaque) void {
        // MockEmbeddingProvider.deinit() handles cleanup.
    }

    const vtable = struct {
        const vt: EmbeddingVTable = .{
            .name = &implName,
            .dimensions = &implDimensions,
            .embed = &implEmbed,
            .deinit = &implDeinit,
        };
    };

    // ── Handle factory ────────────────────────────────────────────────────────

    /// Return an EmbeddingHandle pointing to this mock.
    /// The handle borrows from the mock — the mock must outlive the handle.
    pub fn provider(self: *Self) EmbeddingHandle {
        return .{
            .ptr = @ptrCast(self),
            .vtable = &vtable.vt,
        };
    }

    // ── Assertions ────────────────────────────────────────────────────────────

    /// Assert that `method` was called exactly `expected` times.
    pub fn assertCallCount(self: *const Self, method: []const u8, expected: usize) void {
        var count: usize = 0;
        for (self.calls.items) |rec| {
            if (std.mem.eql(u8, rec.method, method)) count += 1;
        }
        if (count != expected) {
            std.debug.panic(
                "assertCallCount: expected {s} called {d} times, got {d}",
                .{ method, expected, count },
            );
        }
    }

    /// Assert that `method` was called at least once with `text` as the argument.
    pub fn assertCalledWith(self: *const Self, method: []const u8, text: []const u8) void {
        for (self.calls.items) |rec| {
            if (std.mem.eql(u8, rec.method, method) and std.mem.eql(u8, rec.text, text)) return;
        }
        std.debug.panic(
            "assertCalledWith: {s}({s}) was never called",
            .{ method, text },
        );
    }

    /// Return how many times `method` was called.
    pub fn callCount(self: *const Self, method: []const u8) usize {
        var count: usize = 0;
        for (self.calls.items) |rec| {
            if (std.mem.eql(u8, rec.method, method)) count += 1;
        }
        return count;
    }
};

// ── EmbeddingHandle (mirror of EmbeddingProvider from embeddings.zig) ─────────
//
// Defined here as a minimal 2-pointer handle matching the production struct
// layout.  Tests import MockEmbeddingProvider and use EmbeddingHandle, avoiding
// a dependency on the full common module.
//
// If the production EmbeddingProvider is available (via the common module
// dependency), cast or assign directly:
//
//   const prod_provider: common.EmbeddingProvider = mock.provider(); // layout-compatible

/// Minimal vtable function table matching EmbeddingProvider.VTable.
pub const EmbeddingVTable = struct {
    name: *const fn (ptr: *anyopaque) []const u8,
    dimensions: *const fn (ptr: *anyopaque) u32,
    embed: *const fn (ptr: *anyopaque, allocator: std.mem.Allocator, text: []const u8) anyerror![]f32,
    deinit: *const fn (ptr: *anyopaque) void,
};

/// Minimal 2-pointer VTable handle, layout-compatible with EmbeddingProvider.
pub const EmbeddingHandle = struct {
    ptr: *anyopaque,
    vtable: *const EmbeddingVTable,

    pub fn getName(self: EmbeddingHandle) []const u8 {
        return self.vtable.name(self.ptr);
    }

    pub fn getDimensions(self: EmbeddingHandle) u32 {
        return self.vtable.dimensions(self.ptr);
    }

    pub fn embed(self: EmbeddingHandle, allocator: std.mem.Allocator, text: []const u8) ![]f32 {
        return self.vtable.embed(self.ptr, allocator, text);
    }

    pub fn deinit(self: EmbeddingHandle) void {
        self.vtable.deinit(self.ptr);
    }
};

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;

test "MockEmbeddingProvider: init and deinit" {
    var mock = MockEmbeddingProvider.init(testing.allocator);
    defer mock.deinit();
    try testing.expectEqual(@as(usize, 0), mock.calls.items.len);
}

test "MockEmbeddingProvider: records embed call" {
    var mock = MockEmbeddingProvider.init(testing.allocator);
    defer mock.deinit();

    var p = mock.provider();
    const vec = try p.embed(testing.allocator, "hello");
    defer testing.allocator.free(vec);

    try testing.expectEqual(@as(usize, 1), mock.calls.items.len);
    try testing.expectEqualStrings("embed", mock.calls.items[0].method);
    try testing.expectEqualStrings("hello", mock.calls.items[0].text);
}

test "MockEmbeddingProvider: setEmbedResult returns configured vector" {
    var mock = MockEmbeddingProvider.init(testing.allocator);
    defer mock.deinit();

    mock.setEmbedResult(&[_]f32{ 0.1, 0.2, 0.3 });

    var p = mock.provider();
    const vec = try p.embed(testing.allocator, "text");
    defer testing.allocator.free(vec);

    try testing.expectEqual(@as(usize, 3), vec.len);
    try testing.expect(@abs(vec[0] - 0.1) < 0.001);
    try testing.expect(@abs(vec[1] - 0.2) < 0.001);
    try testing.expect(@abs(vec[2] - 0.3) < 0.001);
}

test "MockEmbeddingProvider: setEmbedError returns configured error" {
    var mock = MockEmbeddingProvider.init(testing.allocator);
    defer mock.deinit();

    mock.setEmbedError(error.NetworkUnreachable);

    var p = mock.provider();
    const result = p.embed(testing.allocator, "text");
    try testing.expectError(error.NetworkUnreachable, result);
}

test "MockEmbeddingProvider: assertCallCount passes" {
    var mock = MockEmbeddingProvider.init(testing.allocator);
    defer mock.deinit();

    var p = mock.provider();
    const v1 = try p.embed(testing.allocator, "a");
    defer testing.allocator.free(v1);
    const v2 = try p.embed(testing.allocator, "b");
    defer testing.allocator.free(v2);

    mock.assertCallCount("embed", 2);
    try testing.expectEqual(@as(usize, 2), mock.callCount("embed"));
}

test "MockEmbeddingProvider: assertCalledWith passes for matching text" {
    var mock = MockEmbeddingProvider.init(testing.allocator);
    defer mock.deinit();

    var p = mock.provider();
    const vec = try p.embed(testing.allocator, "target_text");
    defer testing.allocator.free(vec);

    mock.assertCalledWith("embed", "target_text");
}

test "MockEmbeddingProvider: default returns empty vector" {
    var mock = MockEmbeddingProvider.init(testing.allocator);
    defer mock.deinit();

    var p = mock.provider();
    const vec = try p.embed(testing.allocator, "anything");
    defer testing.allocator.free(vec);

    try testing.expectEqual(@as(usize, 0), vec.len);
}

test "MockEmbeddingProvider: name and dimensions" {
    var mock = MockEmbeddingProvider.init(testing.allocator);
    defer mock.deinit();

    var p = mock.provider();
    try testing.expectEqualStrings("mock", p.getName());
    try testing.expectEqual(@as(u32, 3), p.getDimensions());
}

test "MockEmbeddingProvider: GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("memory leak");

    var mock = MockEmbeddingProvider.init(gpa.allocator());
    defer mock.deinit();

    mock.setEmbedResult(&[_]f32{ 1.0, 2.0 });

    var p = mock.provider();
    const v1 = try p.embed(gpa.allocator(), "test1");
    defer gpa.allocator().free(v1);
    const v2 = try p.embed(gpa.allocator(), "test2");
    defer gpa.allocator().free(v2);

    mock.assertCallCount("embed", 2);
}
