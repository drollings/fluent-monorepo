/// SharedString — heap-allocated, reference-counted, copy-on-write immutable string.
///
/// ## Design
///
/// Fused single allocation: the SharedString header and string bytes live in
/// one contiguous allocation, eliminating the two-allocation cost of the naive
/// design and improving cache locality.
///
///   [ SharedString header | ... string bytes (cap) ... ]
///   ^-- aligned to @alignOf(SharedString)
///
/// ## Ownership model
///
///   Ref.init(allocator, str)   → new allocation, ref_count = 1.
///   ref.clone()                → bumps count, returns second handle.
///   ref.deinit(allocator)      → decrements count; zeros + frees when it hits 0.
///   ref.mutate(allocator, new) → in-place if exclusive and fits in cap;
///                                CoW otherwise.
///
/// ## Copy-on-write
///
///   `mutate()` is safe to call on any Ref:
///   - Exclusive owner (ref_count == 1) and new content fits in `cap`:
///     overwrite bytes in-place, update len, zero the unused tail.
///   - Shared, or new content exceeds cap:
///     allocate a new header+bytes, then release the old ref.
///
///   Exclusivity is implied by ref_count == 1 observed by the owning thread:
///   no other thread can legitimately obtain a Ref to this allocation without
///   already holding one (which would make the count ≥ 2).  A data race on
///   the Ref *struct* itself is Undefined Behavior and cannot be papered over
///   by the payload; we therefore do not attempt to synchronise concurrent
///   mutate/clone on the *same* Ref value.
///
/// ## Thread safety
///
///   ref_count ops use the standard Arc fence protocol (std.atomic §RefCount):
///   monotonic on increment, release on decrement, acquire fence on final drop.
///   Bytes are immutable between a clone() and the next mutate() on the
///   *exclusive* path.  Different Refs to the same allocation never take the
///   in-place path (count ≥ 2), so the bytes they observe are stable for the
///   lifetime of their handle.
///
/// ## Security
///
///   destroy() zeroes the string bytes (full `cap`) before returning the
///   allocation to the allocator, preventing residual data in heap free lists.
///   mutate() zeroes the unused tail after an in-place shrink, so the
///   invariant "bytes in [len..cap] are zero" holds at all observable points.
///
/// ## Size limits
///
///   `len` and `cap` are `u32`, capping any single SharedString at 4 GiB.
///   This saves 8 bytes of header overhead on 64-bit systems versus `usize`
///   and is more than sufficient for every expected use case.
///
/// ## Allocator choice
///
///   Pass std.heap.smp_allocator for long-lived, shared strings in production.
///   smp_allocator is a thread-safe TLSF allocator with O(1) alloc/free and
///   bounded fragmentation (~25% worst case), making a custom bucket pool
///   unnecessary.  Use std.testing.allocator (or DebugAllocator) in tests.
const std = @import("std");

/// Returned by init/mutate when the requested content exceeds the u32
/// capacity limit (4 GiB - 1).  See the "Size limits" section in the
/// SharedString doc comment.
pub const Error = std.mem.Allocator.Error || error{StringTooLong};

const MAX_LEN: usize = std.math.maxInt(u32);

/// Manages shared string data with ownership and lifetime control; ensures safe access across contexts.
pub const SharedString = struct {
    ref_count: std.atomic.Value(u32),
    len: u32,
    /// Allocated byte capacity for the string region (>= len always).
    /// Tracks the original allocation size so destroy() can free correctly
    /// and so mutate() can reuse capacity after an in-place shrink.
    cap: u32,
    // String bytes immediately follow this struct in the same allocation.
    // Access via bytesPtr().
    //
    // Invariants:
    //   - bytes in [len..cap] are zero
    //   - the backing allocation is always (cap + 1) bytes long so that
    //     byte[len] is a guaranteed NUL terminator for sliceZ() / C interop
    //   - len <= cap <= MAX_LEN (= u32 max)

    comptime {
        // bytesPtr() assumes the payload begins exactly at @sizeOf(SharedString)
        // with no trailing padding.  If a future field introduces padding, this
        // assertion will fail loudly at compile time.
        std.debug.assert(@sizeOf(SharedString) == 3 * @sizeOf(u32));
        std.debug.assert(@alignOf(SharedString) == @alignOf(u32));
    }

    // -----------------------------------------------------------------------
    // Internal: byte access
    // -----------------------------------------------------------------------

    fn bytesPtr(self: *const SharedString) [*]u8 {
        return @as([*]u8, @ptrFromInt(@intFromPtr(self) + @sizeOf(SharedString)));
    }

    // -----------------------------------------------------------------------
    // Internal: allocation / deallocation
    // -----------------------------------------------------------------------

    fn create(allocator: std.mem.Allocator, str: []const u8, min_cap: usize) Error!*SharedString {
        const want_cap = @max(str.len, min_cap);
        if (want_cap > MAX_LEN) return error.StringTooLong;
        // +1 for the guaranteed NUL terminator at byte[len].
        const total = @sizeOf(SharedString) + want_cap + 1;
        const align_of = comptime std.mem.Alignment.fromByteUnits(@alignOf(SharedString));
        const raw = try allocator.alignedAlloc(u8, align_of, total);
        const self: *SharedString = @ptrCast(raw.ptr);
        self.ref_count = std.atomic.Value(u32).init(1);
        self.len = @intCast(str.len);
        self.cap = @intCast(want_cap);
        const bp = self.bytesPtr();
        if (str.len > 0) @memcpy(bp[0..str.len], str);
        // Zero [len..cap] to establish the invariant, and byte[cap] as the
        // final terminator slot.  (byte[len] is covered by this memset as
        // long as len < cap; if len == cap, byte[cap] covers it.)
        @memset(bp[str.len .. want_cap + 1], 0);
        return self;
    }

    fn destroy(self: *SharedString, allocator: std.mem.Allocator) void {
        // Security zero: prevent residual string data in heap free lists.
        // Zero the full cap + terminator slot (original allocation payload).
        const payload_len: usize = @as(usize, self.cap) + 1;
        @memset(self.bytesPtr()[0..payload_len], 0);
        const total = @sizeOf(SharedString) + payload_len;
        const raw: [*]align(@alignOf(SharedString)) u8 = @ptrCast(self);
        allocator.free(raw[0..total]);
    }

    // -----------------------------------------------------------------------
    // Internal: ref-count management
    // -----------------------------------------------------------------------

    fn acquire(self: *SharedString) void {
        // Monotonic: standard Arc clone ordering.  The acquire fence on the
        // final drop synchronises with all prior releases.
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

    /// The immutable string contents as a NUL-terminated slice, suitable
    /// for passing directly to C APIs.  The byte at `[len]` is guaranteed
    /// to be `0` at all observable times; this is maintained by `create()`
    /// (tail zero) and `mutate()` (re-zero on every write).
    pub fn sliceZ(self: *const SharedString) [:0]const u8 {
        return self.bytesPtr()[0..self.len :0];
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
        pub fn init(allocator: std.mem.Allocator, str: []const u8) Error!Ref {
            return .{ .ptr = try SharedString.create(allocator, str, 0) };
        }

        /// Allocate a new SharedString from `str` with at least `min_capacity`
        /// bytes of capacity reserved.  Use this when you know the string will
        /// grow shortly after creation to avoid an immediate CoW realloc.
        pub fn initCapacity(
            allocator: std.mem.Allocator,
            str: []const u8,
            min_capacity: usize,
        ) Error!Ref {
            return .{ .ptr = try SharedString.create(allocator, str, min_capacity) };
        }

        /// Increment the ref count and return a second handle to the same
        /// allocation.  Both handles must eventually be passed to `deinit`.
        pub fn clone(self: Ref) Ref {
            self.ptr.acquire();
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

        /// NUL-terminated view of the contents for C interop.
        /// Valid as long as this Ref is alive.
        pub fn sliceZ(self: Ref) [:0]const u8 {
            return self.ptr.sliceZ();
        }

        /// Byte-wise equality.  Two Refs that share the same underlying
        /// allocation short-circuit to true.
        pub fn eql(self: Ref, other: Ref) bool {
            if (self.ptr == other.ptr) return true;
            return std.mem.eql(u8, self.slice(), other.slice());
        }

        /// Byte-wise equality against a raw string.
        pub fn eqlSlice(self: Ref, other: []const u8) bool {
            return std.mem.eql(u8, self.slice(), other);
        }

        /// Lexicographic ordering, compatible with std.sort.
        pub fn order(self: Ref, other: Ref) std.math.Order {
            if (self.ptr == other.ptr) return .eq;
            return std.mem.order(u8, self.slice(), other.slice());
        }

        /// 64-bit hash of the contents, suitable for std.HashMap /
        /// std.AutoHashMap-style containers when used via a custom Context.
        pub fn hash(self: Ref) u64 {
            return std.hash.Wyhash.hash(0, self.slice());
        }

        /// std.fmt integration: prints the string contents directly.
        /// Enables `std.debug.print("{f}", .{my_ref})` and friends.
        pub fn format(self: Ref, writer: *std.Io.Writer) std.Io.Writer.Error!void {
            try writer.writeAll(self.slice());
        }

        /// Context type for use with `std.HashMap(Ref, V, Ref.HashContext, ...)`.
        /// Hashes and compares by string content, not pointer identity, so two
        /// separately-allocated Refs with the same bytes map to the same slot.
        pub const HashContext = struct {
            pub fn hash(_: HashContext, key: Ref) u64 {
                return key.hash();
            }
            pub fn eql(_: HashContext, a: Ref, b: Ref) bool {
                return a.eql(b);
            }
        };

        /// Byte length of the string.
        pub fn len(self: Ref) usize {
            return self.ptr.len;
        }

        /// Replace the string content.
        ///
        /// Two cases:
        ///
        ///   Case 1 — exclusive owner (ref_count == 1) and new content fits
        ///            in the existing `cap`:
        ///     Overwrite bytes in-place, update len, zero the unused tail.
        ///     `self.ptr` is unchanged.
        ///
        ///   Case 2 — shared owner OR new content exceeds cap:
        ///     Allocate new header+bytes.  Release the old ref (which frees
        ///     the old allocation if we were the sole owner, otherwise leaves
        ///     it alive for the other holders).  Update `self.ptr`.
        ///
        /// After mutate() returns, `self` is the sole owner of a Ref whose
        /// slice() returns new_content.
        pub fn mutate(self: *Ref, allocator: std.mem.Allocator, new_content: []const u8) Error!void {
            if (new_content.len > MAX_LEN) return error.StringTooLong;
            // Case 1: exclusive and fits in capacity.
            //
            // ref_count == 1 observed by this thread implies no other thread
            // holds a Ref to this allocation, so no concurrent clone() or
            // mutate() on *another* Ref can race with us.  A race on this
            // *same* Ref value from another thread would be a data race on
            // the Ref struct itself (UB), outside what this type can defend
            // against.
            if (new_content.len <= self.ptr.cap and
                self.ptr.ref_count.load(.acquire) == 1)
            {
                const bp = self.ptr.bytesPtr();
                const old_len = self.ptr.len;
                if (new_content.len > 0) @memcpy(bp[0..new_content.len], new_content);
                // Preserve both invariants:
                //   - bytes in [new_len..cap] are zero
                //   - byte[new_len] is the NUL terminator for sliceZ()
                // [old_len..cap] was already zero (and byte[cap] was already
                // zero as the prior terminator slot), so we only need to zero
                // the region that was previously visible: [new_len..old_len].
                // When growing within cap (new_len > old_len) the target
                // terminator position byte[new_len] was already zero by the
                // prior invariant, so no extra write is needed.
                if (new_content.len < old_len) {
                    @memset(bp[new_content.len..old_len], 0);
                }
                self.ptr.len = @intCast(new_content.len);
                return;
            }

            // Case 2: allocate new, then release old.
            // Allocate first so we never leave self in an inconsistent state
            // on allocation failure.
            const new_ptr = try SharedString.create(allocator, new_content, 0);
            const old_ptr = self.ptr;
            self.ptr = new_ptr;
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

test "SharedString: sliceZ is NUL-terminated" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const ref = try SharedString.Ref.init(allocator, "hello");
    defer ref.deinit(allocator);

    const z = ref.sliceZ();
    try testing.expectEqualStrings("hello", z);
    // Sentinel byte at [len] is 0.
    try testing.expectEqual(@as(u8, 0), z.ptr[z.len]);
}

test "SharedString: sliceZ remains valid after in-place mutate" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    var ref = try SharedString.Ref.initCapacity(allocator, "hi", 16);
    defer ref.deinit(allocator);

    try ref.mutate(allocator, "bigger!");
    const z = ref.sliceZ();
    try testing.expectEqualStrings("bigger!", z);
    try testing.expectEqual(@as(u8, 0), z.ptr[z.len]);

    try ref.mutate(allocator, "x");
    const z2 = ref.sliceZ();
    try testing.expectEqualStrings("x", z2);
    try testing.expectEqual(@as(u8, 0), z2.ptr[z2.len]);
}

test "SharedString: initCapacity reserves headroom, avoids CoW on grow" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    var ref = try SharedString.Ref.initCapacity(allocator, "hi", 32);
    defer ref.deinit(allocator);
    const original_ptr = ref.ptr;
    try testing.expectEqual(@as(u32, 32), ref.ptr.cap);
    try testing.expectEqual(@as(u32, 2), ref.ptr.len);

    try ref.mutate(allocator, "this fits in thirty-two bytes!!!");
    try testing.expectEqual(original_ptr, ref.ptr); // in-place, no CoW
    try testing.expectEqualStrings("this fits in thirty-two bytes!!!", ref.slice());
}

test "SharedString: initCapacity with min_capacity < str.len uses str.len" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const ref = try SharedString.Ref.initCapacity(allocator, "longer string", 3);
    defer ref.deinit(allocator);
    try testing.expectEqual(@as(u32, 13), ref.ptr.cap);
    try testing.expectEqualStrings("longer string", ref.slice());
}

test "SharedString: eql, eqlSlice, order" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "apple");
    defer a.deinit(allocator);
    const b = try SharedString.Ref.init(allocator, "apple");
    defer b.deinit(allocator);
    const c = try SharedString.Ref.init(allocator, "banana");
    defer c.deinit(allocator);
    const a_clone = a.clone();
    defer a_clone.deinit(allocator);

    // Content equality across distinct allocations.
    try testing.expect(a.eql(b));
    try testing.expect(!a.eql(c));
    // Same-allocation fast path.
    try testing.expect(a.eql(a_clone));
    // Raw-slice comparison.
    try testing.expect(a.eqlSlice("apple"));
    try testing.expect(!a.eqlSlice("APPLE"));
    // Ordering.
    try testing.expectEqual(std.math.Order.lt, a.order(c));
    try testing.expectEqual(std.math.Order.gt, c.order(a));
    try testing.expectEqual(std.math.Order.eq, a.order(b));
    try testing.expectEqual(std.math.Order.eq, a.order(a_clone));
}

test "SharedString: hash is content-based" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "key");
    defer a.deinit(allocator);
    const b = try SharedString.Ref.init(allocator, "key");
    defer b.deinit(allocator);
    const c = try SharedString.Ref.init(allocator, "different");
    defer c.deinit(allocator);

    try testing.expectEqual(a.hash(), b.hash());
    try testing.expect(a.hash() != c.hash());
    // Matches raw std.hash.Wyhash of the slice.
    try testing.expectEqual(std.hash.Wyhash.hash(0, "key"), a.hash());
}

test "SharedString: format prints contents" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const ref = try SharedString.Ref.init(allocator, "hello world");
    defer ref.deinit(allocator);

    const rendered = try std.fmt.allocPrint(allocator, "<{f}>", .{ref});
    defer allocator.free(rendered);
    try testing.expectEqualStrings("<hello world>", rendered);
}

test "SharedString: HashContext usable with std.HashMap" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    var map = std.HashMap(
        SharedString.Ref,
        u32,
        SharedString.Ref.HashContext,
        std.hash_map.default_max_load_percentage,
    ).init(allocator);
    defer {
        var it = map.keyIterator();
        while (it.next()) |k| k.deinit(allocator);
        map.deinit();
    }

    const k1 = try SharedString.Ref.init(allocator, "alpha");
    try map.put(k1, 1);
    const k2 = try SharedString.Ref.init(allocator, "beta");
    try map.put(k2, 2);

    // Lookup with a distinct allocation having the same content.
    const probe = try SharedString.Ref.init(allocator, "alpha");
    defer probe.deinit(allocator);
    try testing.expectEqual(@as(?u32, 1), map.get(probe));

    const probe2 = try SharedString.Ref.init(allocator, "gamma");
    defer probe2.deinit(allocator);
    try testing.expectEqual(@as(?u32, null), map.get(probe2));
}

test "SharedString: mutate grow-after-shrink reuses capacity" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    // Start with cap = len = 11.
    var ref = try SharedString.Ref.init(allocator, "eleven char");
    defer ref.deinit(allocator);
    const original_ptr = ref.ptr;
    try testing.expectEqual(@as(u32, 11), ref.ptr.cap);

    // Shrink to 5: in-place, cap unchanged.
    try ref.mutate(allocator, "short");
    try testing.expectEqual(original_ptr, ref.ptr);
    try testing.expectEqual(@as(u32, 11), ref.ptr.cap);
    try testing.expectEqual(@as(u32, 5), ref.ptr.len);

    // Grow back to 8: must still be in-place (8 <= cap=11).
    // Previous buggy impl would CoW here because 8 > len=5.
    try ref.mutate(allocator, "eightchr");
    try testing.expectEqual(original_ptr, ref.ptr);
    try testing.expectEqualStrings("eightchr", ref.slice());
    try testing.expectEqual(@as(u32, 11), ref.ptr.cap);

    // Grow to exactly cap: still in-place.
    try ref.mutate(allocator, "01234567890"[0..11]);
    try testing.expectEqual(original_ptr, ref.ptr);
    try testing.expectEqualStrings("01234567890", ref.slice());

    // Exceed cap: CoW.
    try ref.mutate(allocator, "twelve chars");
    try testing.expect(ref.ptr != original_ptr);
    try testing.expectEqualStrings("twelve chars", ref.slice());
}
