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
///   Ref.init(allocator, str)    → new allocation, strong = 1, weak = 1 (ghost).
///   ref.retain()                → bumps strong count, returns second handle.
///   ref.release(allocator)      → decrements strong count; zeros bytes on
///                                 hitting 0; frees allocation when weak
///                                 count reaches 0.
///   ref.downgrade()             → returns a Weak handle (bumps weak count).
///   weak.upgrade(allocator)     → strong Ref if still alive, null otherwise.
///   ref.mutate(allocator, new)  → in-place if exclusive and fits in cap;
///                                 CoW otherwise.
///
/// ## Weak references
///
///   Two counters cooperate:
///     strong_count — owning references.
///     weak_count   — user Weak handles, plus one ghost ref held collectively
///                    by all strong Refs while strong_count > 0.  Matching the
///                    Rust/zigrc Arc convention.
///
///   When strong_count reaches zero the string bytes are security-zeroed
///   (content is dead; no Ref can access them any more).  The allocation
///   itself is only freed when weak_count reaches zero, which cannot happen
///   while strong_count > 0 because the ghost ref is still counted.
///
///   Weak handles hold a raw pointer to the SharedString header.  They
///   cannot access the string bytes directly — only via a successful
///   upgrade().
///
/// ## Copy-on-write
///
///   `mutate()` is safe to call on any Ref:
///   - Exclusive owner (strong_count == 1, no outstanding Weak) and new
///     content fits in `cap`:
///     overwrite bytes in-place, update len, zero the unused tail.
///   - Shared, Weak refs outstanding, or new content exceeds cap:
///     allocate a new header+bytes, then release the old ref.
///
///   Exclusivity is implied by strong_count == 1 observed by the owning
///   thread: no other thread can legitimately obtain a strong Ref to this
///   allocation without already holding one (which would make the count ≥ 2).
///   Outstanding Weak handles force the CoW path because a concurrent
///   Weak.upgrade() could otherwise observe mid-mutation bytes.  A data race
///   on the Ref *struct* itself is Undefined Behavior and cannot be papered
///   over by the payload; we therefore do not attempt to synchronise
///   concurrent mutate/retain on the *same* Ref value.
///
/// ## Thread safety
///
///   Count ops use the standard Arc fence protocol (std.atomic §RefCount):
///   monotonic on increment, release on decrement, acquire fence on final
///   drop.  Bytes are immutable between a retain() and the next mutate() on
///   the *exclusive* path.  Different Refs to the same allocation never take
///   the in-place path (count ≥ 2), so the bytes they observe are stable for
///   the lifetime of their handle.
///
/// ## Security
///
///   When strong_count reaches zero the string bytes (full `cap` plus the
///   terminator slot) are zeroed, preventing residual data in heap free lists
///   once the allocation is ultimately freed.  mutate() also zeroes the
///   unused tail after an in-place shrink, so the invariant "bytes in
///   [len..cap] are zero" holds at all observable points.
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
    strong_count: std.atomic.Value(u32),
    weak_count: std.atomic.Value(u32),
    len: u32,
    /// Allocated byte capacity for the string region (>= len always).
    /// Tracks the original allocation size so the backing memory can be
    /// freed correctly and so mutate() can reuse capacity after an in-place
    /// shrink.
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
        std.debug.assert(@sizeOf(SharedString) == 4 * @sizeOf(u32));
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
        self.strong_count = std.atomic.Value(u32).init(1);
        // Start weak_count at 1 — the ghost ref collectively owned by all
        // strong Refs.  Released when the last strong Ref is dropped.
        self.weak_count = std.atomic.Value(u32).init(1);
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

    fn deallocate(self: *SharedString, allocator: std.mem.Allocator) void {
        const payload_len: usize = @as(usize, self.cap) + 1;
        const total = @sizeOf(SharedString) + payload_len;
        const raw: [*]align(@alignOf(SharedString)) u8 = @ptrCast(self);
        allocator.free(raw[0..total]);
    }

    /// Security zero: wipe the string bytes (including the terminator slot)
    /// so nothing sensitive lingers for Weak handles or in heap free lists
    /// after the last strong Ref is dropped.
    fn zeroBytes(self: *SharedString) void {
        const payload_len: usize = @as(usize, self.cap) + 1;
        @memset(self.bytesPtr()[0..payload_len], 0);
    }

    // -----------------------------------------------------------------------
    // Internal: ref-count management
    // -----------------------------------------------------------------------

    fn acquireStrong(self: *SharedString) void {
        // Monotonic: standard Arc clone ordering.  The acquire fence on the
        // final drop synchronises with all prior releases.
        _ = self.strong_count.fetchAdd(1, .monotonic);
    }

    fn acquireWeak(self: *SharedString) void {
        _ = self.weak_count.fetchAdd(1, .monotonic);
    }

    /// Decrement strong count with acquire/release fence protocol
    /// (std.atomic §RefCount).  When the last strong Ref is dropped the
    /// bytes are zeroed and the ghost weak ref is released — the allocation
    /// is freed only once the last Weak is gone.
    fn releaseStrong(self: *SharedString, allocator: std.mem.Allocator) void {
        if (self.strong_count.fetchSub(1, .release) != 1) return;
        // Acquire fence: synchronise with every prior release so we see
        // all writes from all previous decrements before acting on zero.
        _ = self.strong_count.load(.acquire);
        // Security zero: content is dead, but the header (and therefore
        // the Weak handles that still reference it) can live on.
        self.zeroBytes();
        // Drop the ghost weak ref now that no strong Ref exists.
        self.releaseWeak(allocator);
    }

    fn releaseWeak(self: *SharedString, allocator: std.mem.Allocator) void {
        if (self.weak_count.fetchSub(1, .release) != 1) return;
        _ = self.weak_count.load(.acquire);
        self.deallocate(allocator);
    }

    /// Attempt to bump strong_count from N>0 to N+1.  Used by Weak.upgrade().
    /// Returns true on success.
    fn tryAcquireStrong(self: *SharedString) bool {
        var prev = self.strong_count.load(.monotonic);
        while (true) {
            if (prev == 0) return false;
            if (self.strong_count.cmpxchgWeak(prev, prev + 1, .acquire, .monotonic)) |observed| {
                prev = observed;
                std.atomic.spinLoopHint();
                continue;
            }
            return true;
        }
    }

    fn strongCountRaw(self: *const SharedString) usize {
        return @as(*const std.atomic.Value(u32), &self.strong_count).load(.acquire);
    }

    /// User-visible weak count: raw weak_count minus the ghost ref held
    /// collectively by all strong Refs while strong_count > 0.
    fn weakCountUser(self: *const SharedString) usize {
        const raw = @as(*const std.atomic.Value(u32), &self.weak_count).load(.acquire);
        if (@as(*const std.atomic.Value(u32), &self.strong_count).load(.acquire) > 0) {
            return raw - 1;
        }
        return raw;
    }

    // -----------------------------------------------------------------------
    // Public: byte access
    // -----------------------------------------------------------------------

    /// The immutable string contents.  Valid as long as any strong Ref is alive.
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
    /// Value-copyable only via `retain()`; call `release(allocator)` exactly once.
    pub const Ref = struct {
        ptr: *SharedString,

        /// Allocate a new SharedString from `str` and return the first Ref
        /// (strong_count = 1, weak_count = 1 ghost).
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

        /// Increment the strong count and return a second handle to the same
        /// allocation.  Both handles must eventually be passed to `release`.
        pub fn retain(self: Ref) Ref {
            self.ptr.acquireStrong();
            return .{ .ptr = self.ptr };
        }

        /// Decrement the strong count.  The string bytes are security-zeroed
        /// when the count reaches zero; the allocation itself is freed only
        /// once the last Weak handle is also released.  Do not use this Ref
        /// after calling release.
        pub fn release(self: Ref, allocator: std.mem.Allocator) void {
            self.ptr.releaseStrong(allocator);
        }

        /// Decrement the strong count and, if we were the last strong owner,
        /// return an owned, heap-duped copy of the string bytes.  Returns
        /// `null` if other strong refs remain.
        ///
        /// The caller owns the returned slice and must free it with
        /// `allocator`.  This signature differs from zigrc's
        /// `Rc.releaseUnwrap()` (which returns `?T` by value) because
        /// SharedString's payload is variable-length, so extraction
        /// necessarily allocates.  The semantic contract is identical: the
        /// Ref is consumed whether a slice is returned or null.
        ///
        /// On allocator failure the refcount is left untouched and the Ref
        /// remains valid; the caller must either retry or call `release`.
        pub fn releaseUnwrap(self: Ref, allocator: std.mem.Allocator) Error!?[]u8 {
            // Allocate the dupe first so allocation failure leaves the
            // refcount untouched and the Ref still valid.
            const dup = try allocator.dupe(u8, self.ptr.slice());
            if (self.ptr.strong_count.fetchSub(1, .release) != 1) {
                allocator.free(dup);
                return null;
            }
            _ = self.ptr.strong_count.load(.acquire);
            self.ptr.zeroBytes();
            self.ptr.releaseWeak(allocator);
            return dup;
        }

        /// Return an owned, heap-duped copy of the string bytes only if this
        /// Ref is the exclusive strong owner (strong_count == 1).  Succeeds
        /// even if Weak handles are outstanding.
        ///
        /// On success the Ref is consumed and must not be used again.  On
        /// `null` (shared) the Ref is untouched.  On allocator failure the
        /// refcount is untouched and the Ref remains valid.
        ///
        /// Like `releaseUnwrap`, this returns `Error!?[]u8` rather than `?T`
        /// because SharedString's payload is variable-length.
        pub fn tryUnwrap(self: Ref, allocator: std.mem.Allocator) Error!?[]u8 {
            if (self.ptr.strong_count.load(.acquire) != 1) return null;
            // Dupe first so allocation failure leaves refcount untouched.
            const dup = try allocator.dupe(u8, self.ptr.slice());
            // Atomically transition strong 1 → 0.  Fails if a concurrent
            // Weak.upgrade() bumped strong to 2 between the load and here.
            if (self.ptr.strong_count.cmpxchgStrong(1, 0, .acquire, .monotonic) != null) {
                allocator.free(dup);
                return null;
            }
            self.ptr.zeroBytes();
            self.ptr.releaseWeak(allocator);
            return dup;
        }

        /// Produce a Weak handle to this allocation.  Increments the weak
        /// count.  The Weak handle cannot access the string bytes directly;
        /// call `upgrade()` to attempt to obtain a strong Ref.
        pub fn downgrade(self: Ref) Weak {
            self.ptr.acquireWeak();
            return .{ .ptr = self.ptr };
        }

        /// Current strong reference count.
        pub fn strongCount(self: Ref) usize {
            return self.ptr.strongCountRaw();
        }

        /// Current user-visible weak reference count (excludes the ghost
        /// ref held collectively by strong owners).
        pub fn weakCount(self: Ref) usize {
            return self.ptr.weakCountUser();
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
        ///   Case 1 — exclusive strong owner (strong_count == 1), no
        ///            outstanding Weak handles, and new content fits in
        ///            the existing `cap`:
        ///     Overwrite bytes in-place, update len, zero the unused tail.
        ///     `self.ptr` is unchanged.
        ///
        ///   Case 2 — shared owner, outstanding Weak refs, OR new content
        ///            exceeds cap:
        ///     Allocate new header+bytes.  Release the old ref (which frees
        ///     the old allocation if we were the sole owner and no Weaks
        ///     remain, otherwise leaves it alive for the other holders).
        ///     Update `self.ptr`.
        ///
        /// After mutate() returns, `self` is the sole owner of a Ref whose
        /// slice() returns new_content.
        pub fn mutate(self: *Ref, allocator: std.mem.Allocator, new_content: []const u8) Error!void {
            if (new_content.len > MAX_LEN) return error.StringTooLong;
            // Case 1: exclusive, no Weak upgrades possible, fits in capacity.
            //
            // strong_count == 1 observed by this thread implies no other
            // thread holds a strong Ref.  weak_count == 1 (just the ghost)
            // implies no user Weak exists, so no concurrent Weak.upgrade()
            // can race with our byte write.  A race on this *same* Ref
            // value from another thread would be a data race on the Ref
            // struct itself (UB), outside what this type can defend against.
            if (new_content.len <= self.ptr.cap and
                self.ptr.strong_count.load(.acquire) == 1 and
                self.ptr.weak_count.load(.acquire) == 1)
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
            old_ptr.releaseStrong(allocator);
        }
    };

    // -----------------------------------------------------------------------
    // Weak — non-owning handle that cannot access the bytes directly
    // -----------------------------------------------------------------------

    /// A non-owning handle to a SharedString.  Holds a raw pointer to the
    /// header and keeps the allocation alive, but does not keep the content
    /// alive — once the last strong Ref is released the bytes are zeroed.
    /// Use `upgrade()` to attempt to obtain a strong Ref.
    pub const Weak = struct {
        ptr: *SharedString,

        /// Create a Weak from a strong Ref.  Increments the weak count.
        pub fn init(parent: Ref) Weak {
            parent.ptr.acquireWeak();
            return .{ .ptr = parent.ptr };
        }

        /// Increment the weak count and return a second Weak handle.
        pub fn retain(self: Weak) Weak {
            self.ptr.acquireWeak();
            return .{ .ptr = self.ptr };
        }

        /// Decrement the weak count.  The allocation is freed when the
        /// count reaches zero, which can only happen after the last strong
        /// Ref has been released (and therefore also released the ghost
        /// weak ref).
        pub fn release(self: Weak, allocator: std.mem.Allocator) void {
            self.ptr.releaseWeak(allocator);
        }

        /// Attempt to obtain a strong Ref.  Returns `null` if the last
        /// strong Ref has already been released.
        ///
        /// The `allocator` parameter is accepted for signature parity with
        /// zigrc's `Weak.upgrade(alloc)` — no allocation is actually
        /// performed, so the parameter is unused.
        pub fn upgrade(self: Weak, allocator: std.mem.Allocator) ?Ref {
            _ = allocator;
            if (self.ptr.tryAcquireStrong()) return Ref{ .ptr = self.ptr };
            return null;
        }

        /// Current strong reference count.  Returns 0 if the content is dead.
        pub fn strongCount(self: Weak) usize {
            return self.ptr.strongCountRaw();
        }

        /// Current user-visible weak reference count (excludes the ghost
        /// ref held collectively by strong owners).
        pub fn weakCount(self: Weak) usize {
            return self.ptr.weakCountUser();
        }
    };

    // -----------------------------------------------------------------------
    // ManagedRef / ManagedWeak — allocator stored in the handle
    // -----------------------------------------------------------------------

    /// A `Ref` that stores its allocator, mirroring zigrc's managed variants.
    /// All lifecycle methods drop the explicit allocator parameter.
    pub const ManagedRef = struct {
        inner: Ref,
        alloc: std.mem.Allocator,

        pub fn init(allocator: std.mem.Allocator, str: []const u8) Error!ManagedRef {
            return .{ .inner = try Ref.init(allocator, str), .alloc = allocator };
        }

        pub fn initCapacity(
            allocator: std.mem.Allocator,
            str: []const u8,
            min_capacity: usize,
        ) Error!ManagedRef {
            return .{
                .inner = try Ref.initCapacity(allocator, str, min_capacity),
                .alloc = allocator,
            };
        }

        pub fn retain(self: ManagedRef) ManagedRef {
            return .{ .inner = self.inner.retain(), .alloc = self.alloc };
        }

        pub fn release(self: ManagedRef) void {
            self.inner.release(self.alloc);
        }

        pub fn releaseUnwrap(self: ManagedRef) Error!?[]u8 {
            return self.inner.releaseUnwrap(self.alloc);
        }

        pub fn tryUnwrap(self: ManagedRef) Error!?[]u8 {
            return self.inner.tryUnwrap(self.alloc);
        }

        pub fn downgrade(self: ManagedRef) ManagedWeak {
            return .{ .inner = self.inner.downgrade(), .alloc = self.alloc };
        }

        pub fn strongCount(self: ManagedRef) usize {
            return self.inner.strongCount();
        }

        pub fn weakCount(self: ManagedRef) usize {
            return self.inner.weakCount();
        }

        pub fn mutate(self: *ManagedRef, new_content: []const u8) Error!void {
            return self.inner.mutate(self.alloc, new_content);
        }

        pub fn slice(self: ManagedRef) []const u8 {
            return self.inner.slice();
        }

        pub fn sliceZ(self: ManagedRef) [:0]const u8 {
            return self.inner.sliceZ();
        }

        pub fn eql(self: ManagedRef, other: ManagedRef) bool {
            return self.inner.eql(other.inner);
        }

        pub fn eqlSlice(self: ManagedRef, other: []const u8) bool {
            return self.inner.eqlSlice(other);
        }

        pub fn order(self: ManagedRef, other: ManagedRef) std.math.Order {
            return self.inner.order(other.inner);
        }

        pub fn hash(self: ManagedRef) u64 {
            return self.inner.hash();
        }

        pub fn format(self: ManagedRef, writer: *std.Io.Writer) std.Io.Writer.Error!void {
            return self.inner.format(writer);
        }

        pub fn len(self: ManagedRef) usize {
            return self.inner.len();
        }

        pub const HashContext = struct {
            pub fn hash(_: HashContext, key: ManagedRef) u64 {
                return key.hash();
            }
            pub fn eql(_: HashContext, a: ManagedRef, b: ManagedRef) bool {
                return a.eql(b);
            }
        };
    };

    /// A `Weak` that stores its allocator, mirroring zigrc's managed variants.
    pub const ManagedWeak = struct {
        inner: Weak,
        alloc: std.mem.Allocator,

        pub fn init(parent: ManagedRef) ManagedWeak {
            return .{ .inner = Weak.init(parent.inner), .alloc = parent.alloc };
        }

        pub fn retain(self: ManagedWeak) ManagedWeak {
            return .{ .inner = self.inner.retain(), .alloc = self.alloc };
        }

        pub fn release(self: ManagedWeak) void {
            self.inner.release(self.alloc);
        }

        pub fn upgrade(self: ManagedWeak) ?ManagedRef {
            if (self.inner.upgrade(self.alloc)) |ref| {
                return ManagedRef{ .inner = ref, .alloc = self.alloc };
            }
            return null;
        }

        pub fn strongCount(self: ManagedWeak) usize {
            return self.inner.strongCount();
        }

        pub fn weakCount(self: ManagedWeak) usize {
            return self.inner.weakCount();
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
    defer ref.release(allocator);

    try testing.expectEqualStrings("hello", ref.slice());
    try testing.expectEqual(@as(usize, 5), ref.len());
}

test "SharedString: empty string" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const ref = try SharedString.Ref.init(allocator, "");
    defer ref.release(allocator);

    try testing.expectEqualStrings("", ref.slice());
    try testing.expectEqual(@as(usize, 0), ref.len());
}

test "SharedString: retain shares allocation, both release safely" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "world");
    const b = a.retain();

    // Same underlying allocation.
    try testing.expectEqual(a.ptr, b.ptr);
    try testing.expectEqualStrings("world", a.slice());
    try testing.expectEqualStrings("world", b.slice());

    a.release(allocator);
    // b still alive; bytes still valid.
    try testing.expectEqualStrings("world", b.slice());
    b.release(allocator);
    // allocation freed here — DebugAllocator confirms no leak.
}

test "SharedString: strong count reaches zero exactly once" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "refcount");
    const b = a.retain();
    const c = b.retain();

    try testing.expectEqual(@as(usize, 3), a.strongCount());
    c.release(allocator);
    try testing.expectEqual(@as(usize, 2), a.strongCount());
    b.release(allocator);
    try testing.expectEqual(@as(usize, 1), a.strongCount());
    a.release(allocator); // frees allocation
}

test "SharedString: slice pointer stability across retains" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "stable");
    const b = a.retain();

    // Both Refs point to the same bytes.
    try testing.expectEqual(a.slice().ptr, b.slice().ptr);

    b.release(allocator);
    a.release(allocator);
}

test "SharedString: fused allocation — header and bytes are contiguous" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const ref = try SharedString.Ref.init(allocator, "fused");
    defer ref.release(allocator);

    const header_end = @intFromPtr(ref.ptr) + @sizeOf(SharedString);
    const bytes_start = @intFromPtr(ref.slice().ptr);
    try testing.expectEqual(header_end, bytes_start);
}

test "SharedString: mutate in-place when exclusive and fits" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    var ref = try SharedString.Ref.init(allocator, "hello");
    defer ref.release(allocator);

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
    const b = a.retain();
    defer b.release(allocator);

    const original_ptr = a.ptr;
    try a.mutate(allocator, "private");
    defer a.release(allocator);

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
    defer ref.release(allocator);

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
    defer ref.release(allocator);

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
    defer ref.release(allocator);

    try ref.mutate(allocator, "");
    try testing.expectEqualStrings("", ref.slice());
    try testing.expectEqual(@as(usize, 0), ref.len());
}

test "SharedString: sliceZ is NUL-terminated" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const ref = try SharedString.Ref.init(allocator, "hello");
    defer ref.release(allocator);

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
    defer ref.release(allocator);

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
    defer ref.release(allocator);
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
    defer ref.release(allocator);
    try testing.expectEqual(@as(u32, 13), ref.ptr.cap);
    try testing.expectEqualStrings("longer string", ref.slice());
}

test "SharedString: eql, eqlSlice, order" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "apple");
    defer a.release(allocator);
    const b = try SharedString.Ref.init(allocator, "apple");
    defer b.release(allocator);
    const c = try SharedString.Ref.init(allocator, "banana");
    defer c.release(allocator);
    const a_alias = a.retain();
    defer a_alias.release(allocator);

    // Content equality across distinct allocations.
    try testing.expect(a.eql(b));
    try testing.expect(!a.eql(c));
    // Same-allocation fast path.
    try testing.expect(a.eql(a_alias));
    // Raw-slice comparison.
    try testing.expect(a.eqlSlice("apple"));
    try testing.expect(!a.eqlSlice("APPLE"));
    // Ordering.
    try testing.expectEqual(std.math.Order.lt, a.order(c));
    try testing.expectEqual(std.math.Order.gt, c.order(a));
    try testing.expectEqual(std.math.Order.eq, a.order(b));
    try testing.expectEqual(std.math.Order.eq, a.order(a_alias));
}

test "SharedString: hash is content-based" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "key");
    defer a.release(allocator);
    const b = try SharedString.Ref.init(allocator, "key");
    defer b.release(allocator);
    const c = try SharedString.Ref.init(allocator, "different");
    defer c.release(allocator);

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
    defer ref.release(allocator);

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
        while (it.next()) |k| k.release(allocator);
        map.deinit();
    }

    const k1 = try SharedString.Ref.init(allocator, "alpha");
    try map.put(k1, 1);
    const k2 = try SharedString.Ref.init(allocator, "beta");
    try map.put(k2, 2);

    // Lookup with a distinct allocation having the same content.
    const probe = try SharedString.Ref.init(allocator, "alpha");
    defer probe.release(allocator);
    try testing.expectEqual(@as(?u32, 1), map.get(probe));

    const probe2 = try SharedString.Ref.init(allocator, "gamma");
    defer probe2.release(allocator);
    try testing.expectEqual(@as(?u32, null), map.get(probe2));
}

test "SharedString: mutate grow-after-shrink reuses capacity" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    // Start with cap = len = 11.
    var ref = try SharedString.Ref.init(allocator, "eleven char");
    defer ref.release(allocator);
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

// ---------------------------------------------------------------------------
// Weak / unwrap / managed tests
// ---------------------------------------------------------------------------

test "SharedString: weak ref basic lifecycle" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const strong = try SharedString.Ref.init(allocator, "weakly-held");
    const weak = strong.downgrade();

    // A fresh Weak means 1 user weak + 1 ghost = raw 2; user sees 1.
    try testing.expectEqual(@as(usize, 1), strong.strongCount());
    try testing.expectEqual(@as(usize, 1), strong.weakCount());

    // Drop the only strong ref — content should be dead, allocation alive.
    strong.release(allocator);
    try testing.expectEqual(@as(usize, 0), weak.strongCount());
    // Ghost was released; now just the user weak.
    try testing.expectEqual(@as(usize, 1), weak.weakCount());

    // Upgrade after strong death must return null.
    try testing.expect(weak.upgrade(allocator) == null);

    // Releasing the last Weak frees the allocation.
    weak.release(allocator);
}

test "SharedString: upgrade while strong alive returns valid Ref" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const strong = try SharedString.Ref.init(allocator, "upgradeable");
    defer strong.release(allocator);
    const weak = strong.downgrade();
    defer weak.release(allocator);

    const upgraded = weak.upgrade(allocator) orelse return error.TestUpgradeFailed;
    defer upgraded.release(allocator);
    try testing.expectEqual(strong.ptr, upgraded.ptr);
    try testing.expectEqualStrings("upgradeable", upgraded.slice());
    try testing.expectEqual(@as(usize, 2), strong.strongCount());
    try testing.expectEqual(@as(usize, 1), strong.weakCount());
}

test "SharedString: weakCount excludes the ghost ref" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const strong = try SharedString.Ref.init(allocator, "ghost");
    defer strong.release(allocator);

    try testing.expectEqual(@as(usize, 0), strong.weakCount());

    const w1 = strong.downgrade();
    try testing.expectEqual(@as(usize, 1), strong.weakCount());
    const w2 = strong.downgrade();
    try testing.expectEqual(@as(usize, 2), strong.weakCount());
    const w3 = w2.retain();
    try testing.expectEqual(@as(usize, 3), strong.weakCount());

    w3.release(allocator);
    w2.release(allocator);
    w1.release(allocator);
    try testing.expectEqual(@as(usize, 0), strong.weakCount());
}

test "SharedString: allocation lives until last weak is released" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const strong = try SharedString.Ref.init(allocator, "outlive");
    const w1 = strong.downgrade();
    const w2 = strong.downgrade();

    strong.release(allocator);
    // Allocation still live (w1 & w2 outstanding). Strong count is 0.
    try testing.expectEqual(@as(usize, 0), w1.strongCount());
    try testing.expect(w1.upgrade(allocator) == null);

    w1.release(allocator);
    // Still alive (w2 holds it).
    try testing.expectEqual(@as(usize, 0), w2.strongCount());

    w2.release(allocator); // frees allocation — DebugAllocator confirms no leak.
}

test "SharedString: bytes are zeroed when strong_count hits zero" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const strong = try SharedString.Ref.init(allocator, "secretish");
    const weak = strong.downgrade();

    // Before: bytes readable.
    try testing.expectEqualStrings("secretish", strong.slice());

    const cap = strong.ptr.cap;
    strong.release(allocator);

    // Access via Weak's raw header pointer (allocation still live).
    // Same-file test can call private bytesPtr().
    const bp = weak.ptr.bytesPtr();
    var i: usize = 0;
    while (i <= cap) : (i += 1) {
        try testing.expectEqual(@as(u8, 0), bp[i]);
    }
    weak.release(allocator);
}

test "SharedString: tryUnwrap returns null when shared" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "shared-try");
    defer a.release(allocator);
    const b = a.retain();
    defer b.release(allocator);

    const result = try a.tryUnwrap(allocator);
    try testing.expect(result == null);
    // Both refs still valid.
    try testing.expectEqualStrings("shared-try", a.slice());
    try testing.expectEqualStrings("shared-try", b.slice());
    try testing.expectEqual(@as(usize, 2), a.strongCount());
}

test "SharedString: tryUnwrap returns owned bytes when exclusive" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "exclusive");
    const result = try a.tryUnwrap(allocator);
    try testing.expect(result != null);
    const owned = result.?;
    defer allocator.free(owned);
    try testing.expectEqualStrings("exclusive", owned);
    // `a` is consumed; do not use again.
}

test "SharedString: tryUnwrap succeeds with outstanding weak" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "weakly-watched");
    const weak = a.downgrade();
    defer weak.release(allocator);

    const result = try a.tryUnwrap(allocator);
    try testing.expect(result != null);
    const owned = result.?;
    defer allocator.free(owned);
    try testing.expectEqualStrings("weakly-watched", owned);
    // Weak upgrade must now fail — content is dead.
    try testing.expect(weak.upgrade(allocator) == null);
}

test "SharedString: releaseUnwrap returns null when shared" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "shared-release");
    const b = a.retain();
    defer b.release(allocator);

    const result = try a.releaseUnwrap(allocator);
    try testing.expect(result == null);
    // `a`'s strong ref was consumed; `b` still holds one.
    try testing.expectEqual(@as(usize, 1), b.strongCount());
    try testing.expectEqualStrings("shared-release", b.slice());
}

test "SharedString: releaseUnwrap returns owned bytes when last strong" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const a = try SharedString.Ref.init(allocator, "last-owner");
    const result = try a.releaseUnwrap(allocator);
    try testing.expect(result != null);
    const owned = result.?;
    defer allocator.free(owned);
    try testing.expectEqualStrings("last-owner", owned);
    // `a` is consumed; do not use again.
}

test "SharedString: ManagedRef full lifecycle without explicit allocator" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    var m = try SharedString.ManagedRef.init(allocator, "managed");
    defer m.release();

    try testing.expectEqualStrings("managed", m.slice());
    try testing.expectEqual(@as(usize, 7), m.len());
    try testing.expectEqual(@as(usize, 1), m.strongCount());
    try testing.expectEqual(@as(usize, 0), m.weakCount());

    const m2 = m.retain();
    defer m2.release();
    try testing.expectEqual(@as(usize, 2), m.strongCount());

    const mw = m.downgrade();
    defer mw.release();
    try testing.expectEqual(@as(usize, 1), m.weakCount());

    const upgraded = mw.upgrade() orelse return error.UpgradeFailed;
    defer upgraded.release();
    try testing.expectEqualStrings("managed", upgraded.slice());

    // mutate without allocator.
    try m.mutate("mutated");
    try testing.expectEqualStrings("mutated", m.slice());
}

test "SharedString: ManagedRef initCapacity and mutate" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    var m = try SharedString.ManagedRef.initCapacity(allocator, "hi", 32);
    defer m.release();
    const original_ptr = m.inner.ptr;
    try m.mutate("in-place thirty-two byte string!");
    try testing.expectEqual(original_ptr, m.inner.ptr);
    try testing.expectEqualStrings("in-place thirty-two byte string!", m.slice());
}

test "SharedString: ManagedRef tryUnwrap / releaseUnwrap" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    {
        const m = try SharedString.ManagedRef.init(allocator, "try-me");
        const result = try m.tryUnwrap();
        try testing.expect(result != null);
        defer allocator.free(result.?);
        try testing.expectEqualStrings("try-me", result.?);
    }

    {
        const m = try SharedString.ManagedRef.init(allocator, "rel-me");
        const result = try m.releaseUnwrap();
        try testing.expect(result != null);
        defer allocator.free(result.?);
        try testing.expectEqualStrings("rel-me", result.?);
    }

    {
        const m = try SharedString.ManagedRef.init(allocator, "shared-try");
        defer m.release();
        const other = m.retain();
        defer other.release();
        try testing.expect(try m.tryUnwrap() == null);
    }
}

test "SharedString: ManagedWeak full lifecycle" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const m = try SharedString.ManagedRef.init(allocator, "managed-weak");
    const mw = m.downgrade();

    try testing.expectEqual(@as(usize, 1), mw.strongCount());
    try testing.expectEqual(@as(usize, 1), mw.weakCount());

    const upgraded = mw.upgrade() orelse return error.UpgradeFailed;
    upgraded.release();

    m.release();
    try testing.expectEqual(@as(usize, 0), mw.strongCount());
    try testing.expect(mw.upgrade() == null);
    mw.release();
}

test "SharedString: Weak.upgrade accepts allocator parameter" {
    // Compile-time signature parity check with zigrc.Weak.upgrade(alloc).
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer testing.expectEqual(.ok, gpa.deinit()) catch {};
    const allocator = gpa.allocator();

    const strong = try SharedString.Ref.init(allocator, "sig");
    defer strong.release(allocator);
    const weak = strong.downgrade();
    defer weak.release(allocator);

    // Passing the allocator to upgrade must type-check and return a Ref.
    const upgraded = weak.upgrade(allocator) orelse return error.UpgradeFailed;
    upgraded.release(allocator);
}
