/// SharedString — heap-allocated, reference-counted, immutable string.
///
/// Follows the same acquire/release pattern as std.atomic.Value's built-in
/// RefCount example (std/atomic.zig §RefCount).
///
/// Ownership model:
///   `Ref.init(allocator, str)` → new allocation, ref_count = 1.
///   `ref.clone()`              → bumps count, returns second handle.
///   `ref.deinit(allocator)`    → decrements count; frees when it hits 0.
///   Callers must pass the same allocator used for `init` to every `deinit`.
///
/// Thread safety:
///   ref_count ops are atomic.  Bytes are immutable after creation, so
///   concurrent reads need no lock.
///
/// Memory layout: two allocations per unique string — one for the header
/// struct, one for the byte slice.  Simple, correct, no alignment tricks.
const std = @import("std");

/// Manages shared string data with ownership and lifetime control; ensures safe access across contexts.
pub const SharedString = struct {
    ref_count: std.atomic.Value(u32),
    bytes: []const u8, // allocator-owned copy; freed when ref_count → 0

    // -----------------------------------------------------------------------
    // Internal: allocation / deallocation
    // -----------------------------------------------------------------------

    fn create(allocator: std.mem.Allocator, str: []const u8) !*SharedString {
        const self = try allocator.create(SharedString);
        errdefer allocator.destroy(self);
        self.ref_count = std.atomic.Value(u32).init(1);
        self.bytes = try allocator.dupe(u8, str);
        return self;
    }

    fn destroy(self: *SharedString, allocator: std.mem.Allocator) void {
        allocator.free(self.bytes);
        allocator.destroy(self);
    }

    // -----------------------------------------------------------------------
    // Internal: ref-count management (used via Ref)
    // -----------------------------------------------------------------------

    fn acquire(self: *SharedString) void {
        // Monotonic: we only need the counter to be strictly increasing;
        // the updated value is not published to other threads here.
        _ = self.ref_count.fetchAdd(1, .monotonic);
    }

    fn release(self: *SharedString, allocator: std.mem.Allocator) void {
        // Release: all writes visible before this decrement are visible to
        // the thread that observes count → 0.
        if (self.ref_count.fetchSub(1, .release) != 1) return;
        // Acquire: synchronise with every prior release so we see all
        // writes before any of the previous decrements.
        _ = self.ref_count.load(.acquire);
        self.destroy(allocator);
    }

    // -----------------------------------------------------------------------
    // Public: byte access
    // -----------------------------------------------------------------------

    /// The immutable string contents.  Valid as long as any Ref is alive.
    pub fn slice(self: *const SharedString) []const u8 {
        return self.bytes;
    }

    // -----------------------------------------------------------------------
    // Ref — the public owner handle
    // -----------------------------------------------------------------------

    /// A reference-counted handle to a SharedString.
    /// Value-copyable only via `clone()`; call `deinit(allocator)` exactly once.
    pub const Ref = struct {
        ptr: *SharedString,

        /// Allocate a new SharedString from `str` and return the first Ref
        /// (ref_count = 1).
        pub fn init(allocator: std.mem.Allocator, str: []const u8) !Ref {
            return .{ .ptr = try SharedString.create(allocator, str) };
        }

        /// Increment the ref count and return a second handle to the same
        /// allocation.  Both handles must eventually be passed to `deinit`.
        pub fn clone(self: Ref) Ref {
            self.ptr.acquire();
            return .{ .ptr = self.ptr };
        }

        /// Decrement the ref count.  The underlying allocation is freed when
        /// the count reaches zero.  Do not use this Ref after calling deinit.
        pub fn deinit(self: Ref, allocator: std.mem.Allocator) void {
            self.ptr.release(allocator);
        }

        /// The string contents.  Valid as long as this Ref is alive.
        pub fn slice(self: Ref) []const u8 {
            return self.ptr.slice();
        }

        /// Byte length of the string.
        pub fn len(self: Ref) usize {
            return self.ptr.bytes.len;
        }
    };
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "SharedString: basic create and read" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const ref = try SharedString.Ref.init(allocator, "hello");
    defer ref.deinit(allocator);

    try testing.expectEqualStrings("hello", ref.slice());
    try testing.expectEqual(@as(usize, 5), ref.len());
}

test "SharedString: empty string" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const ref = try SharedString.Ref.init(allocator, "");
    defer ref.deinit(allocator);

    try testing.expectEqualStrings("", ref.slice());
    try testing.expectEqual(@as(usize, 0), ref.len());
}

test "SharedString: clone shares allocation, both deinit safely" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "world");
    const b = a.clone();

    // Same underlying allocation.
    try testing.expectEqual(a.ptr, b.ptr);
    try testing.expectEqualStrings("world", a.slice());
    try testing.expectEqualStrings("world", b.slice());

    a.deinit(allocator);
    // b still alive; bytes still valid.
    try testing.expectEqualStrings("world", b.slice());
    b.deinit(allocator);
    // allocation freed here — GPA confirms no leak.
}

test "SharedString: ref_count reaches zero exactly once" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "refcount");
    const b = a.clone();
    const c = b.clone();

    try testing.expectEqual(@as(u32, 3), a.ptr.ref_count.load(.monotonic));
    c.deinit(allocator);
    try testing.expectEqual(@as(u32, 2), a.ptr.ref_count.load(.monotonic));
    b.deinit(allocator);
    try testing.expectEqual(@as(u32, 1), a.ptr.ref_count.load(.monotonic));
    a.deinit(allocator); // frees allocation
}

test "SharedString: slice pointer stability across clones" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "stable");
    const b = a.clone();

    // Both Refs point to the same bytes.
    try testing.expectEqual(a.slice().ptr, b.slice().ptr);

    b.deinit(allocator);
    a.deinit(allocator);
}

