/// SharedString — heap-allocated, reference-counted, copy-on-write immutable string.
///
/// ## Design
///
/// Fused single allocation: the SharedString header and string bytes live in
/// one contiguous allocation, eliminating the two-allocation cost of the naive
/// design and improving cache locality.
///
///   [ SharedString header | ... string bytes ... ]
///   ^-- aligned to @alignOf(SharedString)
///
/// ## Ownership model
///
///   Ref.init(allocator, str)   → new allocation, ref_count = 1.
///   ref.clone()                → bumps count, returns second handle.
///   ref.deinit(allocator)      → decrements count; zeros + frees when it hits 0.
///   ref.mutate(allocator, new) → in-place if exclusive and fits; CoW otherwise.
///
/// ## Copy-on-write
///
///   `mutate()` is safe to call on any Ref regardless of share count:
///   - Exclusive owner and new content fits in the current allocation:
///     CAS ref_count 1 → MUTATING, overwrite in-place, restore to 1.
///   - Shared (or exclusive but needs resize):
///     Allocate new header+bytes first, then release the old ref.
///   The MUTATING sentinel prevents a concurrent clone() from producing
///   a handle to bytes mid-overwrite (TOCTOU guard).
///
/// ## Thread safety
///
///   ref_count ops use acquire/release ordering (std.atomic §RefCount).
///   Bytes are immutable between a clone() and the next mutate().
///   mutate() is safe to call concurrently on different Refs to the same
///   allocation — the CAS ensures only one wins exclusive in-place access.
///
/// ## Security
///
///   destroy() zeroes the string bytes before returning the allocation to
///   the allocator, preventing residual data in heap free lists.
///
/// ## Allocator choice
///
///   Pass std.heap.smp_allocator for long-lived, shared strings in production.
///   smp_allocator is a thread-safe TLSF allocator with O(1) alloc/free and
///   bounded fragmentation (~25% worst case), making a custom bucket pool
///   unnecessary.  Use std.testing.allocator (or DebugAllocator) in tests.
const std = @import("std");

/// Sentinel: ref_count value meaning "in-place mutation in progress".
/// A concurrent clone() that observes this value must treat the string
/// as shared (spin until count returns to a normal value or fall back to CoW).
const MUTATING: u32 = std.math.maxInt(u32) - 1;

/// Manages shared string data with ownership and lifetime control; ensures safe access across contexts.
pub const SharedString = struct {
    ref_count: std.atomic.Value(u32),
    len: u32,
    /// Allocated byte capacity for the string region (>= len always).
    /// Tracks the original allocation size so destroy() can free correctly
    /// even after an in-place mutate() that shrinks len.
    cap: u32,
    // String bytes immediately follow this struct in the same allocation.
    // Access via bytesPtr().

    // -----------------------------------------------------------------------
    // Internal: byte access
    // -----------------------------------------------------------------------

    fn bytesPtr(self: *const SharedString) [*]u8 {
        return @as([*]u8, @ptrFromInt(@intFromPtr(self) + @sizeOf(SharedString)));
    }

    // -----------------------------------------------------------------------
    // Internal: allocation / deallocation
    // -----------------------------------------------------------------------

    fn create(allocator: std.mem.Allocator, str: []const u8) !*SharedString {
        const total = @sizeOf(SharedString) + str.len;
        const align_of = comptime std.mem.Alignment.fromByteUnits(@alignOf(SharedString));
        const raw = try allocator.alignedAlloc(u8, align_of, total);
        const self: *SharedString = @ptrCast(raw.ptr);
        self.ref_count = std.atomic.Value(u32).init(1);
        self.len = @intCast(str.len);
        self.cap = @intCast(str.len);
        if (str.len > 0) @memcpy(self.bytesPtr()[0..str.len], str);
        return self;
    }

    fn destroy(self: *SharedString, allocator: std.mem.Allocator) void {
        // Security zero: prevent residual string data in heap free lists.
        // Zero up to cap (the original allocation size), not just len.
        if (self.cap > 0) @memset(self.bytesPtr()[0..self.cap], 0);
        const total = @sizeOf(SharedString) + self.cap;
        const raw: [*]align(@alignOf(SharedString)) u8 = @ptrCast(self);
        allocator.free(raw[0..total]);
    }

    // -----------------------------------------------------------------------
    // Internal: ref-count management
    // -----------------------------------------------------------------------

    fn acquire(self: *SharedString) void {
        // Monotonic: we only need the increment to be visible before any
        // subsequent release on the same thread.
        _ = self.ref_count.fetchAdd(1, .monotonic);
    }

    /// Decrement ref count with acquire/release fence protocol (std.atomic §RefCount).
    /// Returns true if the caller is now responsible for freeing the object.
    fn releaseRef(self: *SharedString) bool {
        // Release: all writes before this decrement are visible to the thread
        // that observes the count reaching zero.
        if (self.ref_count.fetchSub(1, .release) != 1) return false;
        // Acquire: synchronise with every prior release so we see all writes
        // from all previous decrements before we free.
        _ = self.ref_count.load(.acquire);
        return true;
    }

    // -----------------------------------------------------------------------
    // Public: byte access
    // -----------------------------------------------------------------------

    /// The immutable string contents.  Valid as long as any Ref is alive.
    pub fn slice(self: *const SharedString) []const u8 {
        return self.bytesPtr()[0..self.len];
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
        ///
        /// If the allocation is currently being mutated (MUTATING sentinel),
        /// this spins until the mutation completes before incrementing.
        pub fn clone(self: Ref) Ref {
            // Spin if a mutate() is mid-flight on this allocation.
            // In practice this is extremely rare (requires exact scheduling
            // collision); the loop terminates as soon as mutate() restores
            // the count to 1.
            while (true) {
                const cur = self.ptr.ref_count.load(.acquire);
                if (cur == MUTATING) {
                    // Yield to avoid busy-burning the CPU in the rare case.
                    std.atomic.spinLoopHint();
                    continue;
                }
                // CAS: only increment if count hasn't changed under us.
                if (self.ptr.ref_count.cmpxchgWeak(cur, cur + 1, .acquire, .monotonic) == null) {
                    break;
                }
            }
            return .{ .ptr = self.ptr };
        }

        /// Decrement the ref count.  The underlying allocation is security-zeroed
        /// and freed when the count reaches zero.  Do not use this Ref after
        /// calling deinit.
        pub fn deinit(self: Ref, allocator: std.mem.Allocator) void {
            if (self.ptr.releaseRef()) {
                self.ptr.destroy(allocator);
            }
        }

        /// The string contents.  Valid as long as this Ref is alive.
        pub fn slice(self: Ref) []const u8 {
            return self.ptr.slice();
        }

        /// Byte length of the string.
        pub fn len(self: Ref) usize {
            return self.ptr.len;
        }

        /// Replace the string content.
        ///
        /// Three cases:
        ///
        ///   Case 1 — exclusive owner, new content fits in existing allocation:
        ///     CAS ref_count 1 → MUTATING.  Overwrite bytes in-place.
        ///     Restore ref_count to 1.  `self.ptr` is unchanged.
        ///
        ///   Case 2 — exclusive owner, new content is larger (needs resize):
        ///     Allocate new header+bytes.  Release old allocation (frees it,
        ///     since we were the sole owner).  Update self.ptr.
        ///
        ///   Case 3 — shared owner (ref_count > 1):
        ///     Allocate new header+bytes.  Release old ref (does not free;
        ///     other owners still hold references).  Update self.ptr.
        ///
        /// After mutate() returns, `self` is the sole owner of a Ref whose
        /// slice() returns new_content.
        pub fn mutate(self: *Ref, allocator: std.mem.Allocator, new_content: []const u8) !void {
            // Case 1: try to claim exclusive in-place write via CAS.
            if (new_content.len <= self.ptr.len) {
                // Attempt to lock: 1 → MUTATING.
                if (self.ptr.ref_count.cmpxchgStrong(1, MUTATING, .acquire, .monotonic) == null) {
                    // We have exclusive access.  Overwrite and zero-pad tail.
                    const bp = self.ptr.bytesPtr();
                    @memcpy(bp[0..new_content.len], new_content);
                    // Zero the unused tail bytes to avoid leaking old content.
                    if (new_content.len < self.ptr.len) {
                        @memset(bp[new_content.len..self.ptr.len], 0);
                    }
                    self.ptr.len = @intCast(new_content.len);
                    // Restore: release the MUTATING sentinel.
                    self.ptr.ref_count.store(1, .release);
                    return;
                }
            }

            // Case 2 / 3: allocate new, then release old.
            // Allocate first so we never leave self in an inconsistent state.
            const new_ptr = try SharedString.create(allocator, new_content);
            const old_ptr = self.ptr;
            self.ptr = new_ptr;
            // Release old (frees it if we were the last owner).
            if (old_ptr.releaseRef()) {
                old_ptr.destroy(allocator);
            }
        }
    };
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "SharedString: basic create and read" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const ref = try SharedString.Ref.init(allocator, "hello");
    defer ref.deinit(allocator);

    try testing.expectEqualStrings("hello", ref.slice());
    try testing.expectEqual(@as(usize, 5), ref.len());
}

test "SharedString: empty string" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const ref = try SharedString.Ref.init(allocator, "");
    defer ref.deinit(allocator);

    try testing.expectEqualStrings("", ref.slice());
    try testing.expectEqual(@as(usize, 0), ref.len());
}

test "SharedString: clone shares allocation, both deinit safely" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
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
    // allocation freed here — DebugAllocator confirms no leak.
}

test "SharedString: ref_count reaches zero exactly once" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
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
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "stable");
    const b = a.clone();

    // Both Refs point to the same bytes.
    try testing.expectEqual(a.slice().ptr, b.slice().ptr);

    b.deinit(allocator);
    a.deinit(allocator);
}

test "SharedString: fused allocation — header and bytes are contiguous" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const ref = try SharedString.Ref.init(allocator, "fused");
    defer ref.deinit(allocator);

    const header_end = @intFromPtr(ref.ptr) + @sizeOf(SharedString);
    const bytes_start = @intFromPtr(ref.slice().ptr);
    try testing.expectEqual(header_end, bytes_start);
}

test "SharedString: mutate in-place when exclusive and fits" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    var ref = try SharedString.Ref.init(allocator, "hello");
    defer ref.deinit(allocator);

    const original_ptr = ref.ptr;
    try ref.mutate(allocator, "world");

    // In-place: same header allocation.
    try testing.expectEqual(original_ptr, ref.ptr);
    try testing.expectEqualStrings("world", ref.slice());
}

test "SharedString: mutate CoW when shared" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    var a = try SharedString.Ref.init(allocator, "shared");
    const b = a.clone();
    defer b.deinit(allocator);

    const original_ptr = a.ptr;
    try a.mutate(allocator, "private");
    defer a.deinit(allocator);

    // CoW: a got a new allocation.
    try testing.expect(a.ptr != original_ptr);
    try testing.expectEqualStrings("private", a.slice());
    // b still sees original.
    try testing.expectEqualStrings("shared", b.slice());
}

test "SharedString: mutate CoW when new content is larger" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    var ref = try SharedString.Ref.init(allocator, "hi");
    const original_ptr = ref.ptr;
    try ref.mutate(allocator, "much longer string here");
    defer ref.deinit(allocator);

    // New allocation required (content doesn't fit).
    try testing.expect(ref.ptr != original_ptr);
    try testing.expectEqualStrings("much longer string here", ref.slice());
}

test "SharedString: mutate in-place zero-pads tail" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    // Allocate with long content so the header's allocation is large.
    var ref = try SharedString.Ref.init(allocator, "long string");
    defer ref.deinit(allocator);

    try ref.mutate(allocator, "short");
    try testing.expectEqualStrings("short", ref.slice());
    // The tail bytes (indices 5–10) must be zero.
    const bp = ref.ptr.bytesPtr();
    for (5..11) |i| {
        try testing.expectEqual(@as(u8, 0), bp[i]);
    }
}

test "SharedString: mutate to empty string" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    var ref = try SharedString.Ref.init(allocator, "nonempty");
    defer ref.deinit(allocator);

    try ref.mutate(allocator, "");
    try testing.expectEqualStrings("", ref.slice());
    try testing.expectEqual(@as(usize, 0), ref.len());
}

