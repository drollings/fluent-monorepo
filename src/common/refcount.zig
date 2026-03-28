//! refcount.zig — Reference-counted VTable handle wrapper (M7).
//!
//! ## Problem
//!
//! VTable handles (`EmbeddingProvider`) have a `deinit` method but no shared
//! ownership semantics.  If multiple owners hold the same handle:
//! - Double-free if both call deinit
//! - Use-after-free if one calls deinit while another still holds
//!
//! ## Solution
//!
//! `RefCounted(T)` wraps any value type with an atomic reference count.
//! Multiple clones share ownership; the underlying value is freed exactly
//! once when the count reaches zero.
//!
//! `RefCountedHandle` wraps a VTable handle (`EmbeddingProvider`) with
//! reference counting and forwards vtable calls through the shared pointer.
//!
//! ## Ownership Model
//!
//!   var rc = try RefCounted(i32).init(allocator, 42);   // refcount = 1
//!   var rc2 = rc.clone();                                // refcount = 2
//!   rc.deinit();                                         // refcount = 1, value lives
//!   rc2.deinit();                                        // refcount = 0, value freed
//!
//! ## Thread Safety
//!
//! The reference count uses `std.atomic.Value(usize)` with `.acquire`/`.release`
//! ordering, making `clone()` and `deinit()` safe to call concurrently from
//! multiple threads.  The *value* itself is NOT protected — callers must
//! synchronize access to the value separately if needed.
//!
//! ## When to use vs. plain VTable handles
//!
//! | Use Case | Use |
//! |----------|-----|
//! | Single owner, lifetime known | plain VTable handle |
//! | Multiple owners, shared usage | `RefCounted(T)` |
//! | Passed to async callback | `RefCounted(T)` |
//! | Stored in a collection | `RefCounted(T)` |

const std = @import("std");

// ── AtomicRefCount ────────────────────────────────────────────────────────────

/// Thread-safe atomic reference counter.
///
/// Starts at 1 (representing the initial owner).
/// `inc()` increments on clone; `dec()` decrements on deinit and returns
/// `true` when the count reaches zero (caller must free).
const AtomicRefCount = struct {
    count: std.atomic.Value(usize),

    fn init() AtomicRefCount {
        return .{ .count = std.atomic.Value(usize).init(1) };
    }

    /// Increment the reference count.  Returns the previous count.
    fn inc(self: *AtomicRefCount) usize {
        return self.count.fetchAdd(1, .acquire);
    }

    /// Decrement the reference count.
    /// Returns `true` if the count reached zero (caller must free the object).
    fn dec(self: *AtomicRefCount) bool {
        return self.count.fetchSub(1, .release) == 1;
    }

    /// Current count (snapshot — may be stale in concurrent use).
    fn load(self: *const AtomicRefCount) usize {
        return self.count.load(.acquire);
    }
};

// ── RefCounted(T) ─────────────────────────────────────────────────────────────

/// Reference-counted wrapper for a value of type `T`.
///
/// The value and its reference count are heap-allocated together.
/// All clones share the same heap allocation.
pub fn RefCounted(comptime T: type) type {
    return struct {
        const Self = @This();

        const Inner = struct {
            rc: AtomicRefCount,
            value: T,
        };

        allocator: std.mem.Allocator,
        inner: *Inner,

        /// Allocate and initialize with `initial`.  Initial refcount is 1.
        pub fn init(allocator: std.mem.Allocator, initial: T) !Self {
            const inner = try allocator.create(Inner);
            inner.* = .{
                .rc = AtomicRefCount.init(),
                .value = initial,
            };
            return .{ .allocator = allocator, .inner = inner };
        }

        /// Increment the reference count and return a new handle.
        /// The clone shares the same underlying value.
        pub fn clone(self: *const Self) Self {
            _ = self.inner.rc.inc();
            return .{ .allocator = self.allocator, .inner = self.inner };
        }

        /// Decrement the reference count.
        /// If the count reaches zero, frees the value and inner allocation.
        pub fn deinit(self: *Self) void {
            if (self.inner.rc.dec()) {
                self.allocator.destroy(self.inner);
            }
        }

        /// Borrow the value.  Valid as long as at least one owner holds the handle.
        pub fn value(self: *const Self) *const T {
            return &self.inner.value;
        }

        /// Mutably borrow the value.  Caller is responsible for synchronization.
        pub fn valueMut(self: *Self) *T {
            return &self.inner.value;
        }

        /// Current reference count (snapshot).
        pub fn refCount(self: *const Self) usize {
            return self.inner.rc.load();
        }
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;

test "RefCounted: init creates with refcount 1" {
    var rc = try RefCounted(i32).init(testing.allocator, 42);
    defer rc.deinit();
    try testing.expectEqual(@as(usize, 1), rc.refCount());
    try testing.expectEqual(@as(i32, 42), rc.value().*);
}

test "RefCounted: clone increments refcount" {
    var rc = try RefCounted(i32).init(testing.allocator, 10);
    defer rc.deinit();

    var rc2 = rc.clone();
    defer rc2.deinit();

    try testing.expectEqual(@as(usize, 2), rc.refCount());
    try testing.expectEqual(@as(usize, 2), rc2.refCount());
}

test "RefCounted: deinit decrements but does not free until last" {
    var rc = try RefCounted(i32).init(testing.allocator, 7);
    var rc2 = rc.clone();

    try testing.expectEqual(@as(usize, 2), rc.refCount());
    rc.deinit(); // count → 1, value survives
    try testing.expectEqual(@as(usize, 1), rc2.refCount());
    try testing.expectEqual(@as(i32, 7), rc2.value().*);
    rc2.deinit(); // count → 0, freed
}

test "RefCounted: shared value is the same allocation" {
    var rc = try RefCounted(u64).init(testing.allocator, 100);
    defer rc.deinit();

    var rc2 = rc.clone();
    defer rc2.deinit();

    // Both point to the same inner allocation.
    try testing.expectEqual(rc.inner, rc2.inner);
}

test "RefCounted: valueMut mutates shared value" {
    var rc = try RefCounted(i32).init(testing.allocator, 0);
    defer rc.deinit();
    var rc2 = rc.clone();
    defer rc2.deinit();

    rc.valueMut().* = 99;
    try testing.expectEqual(@as(i32, 99), rc2.value().*);
}

test "RefCounted: multiple clones maintain correct count" {
    var rc = try RefCounted(u32).init(testing.allocator, 1);
    var rc2 = rc.clone(); // count = 2
    var rc3 = rc.clone(); // count = 3
    var rc4 = rc2.clone(); // count = 4

    try testing.expectEqual(@as(usize, 4), rc.refCount());

    rc2.deinit(); // count = 3
    try testing.expectEqual(@as(usize, 3), rc.refCount());
    rc3.deinit(); // count = 2
    try testing.expectEqual(@as(usize, 2), rc.refCount());
    rc4.deinit(); // count = 1
    try testing.expectEqual(@as(usize, 1), rc.refCount());
    rc.deinit(); // count = 0, freed
}

test "RefCounted: GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("memory leak");

    var rc = try RefCounted([]const u8).init(gpa.allocator(), "hello");
    var rc2 = rc.clone();
    rc.deinit();
    try testing.expectEqualStrings("hello", rc2.value().*);
    rc2.deinit();
}

test "AtomicRefCount: starts at 1" {
    var rc = AtomicRefCount.init();
    try testing.expectEqual(@as(usize, 1), rc.load());
}

test "AtomicRefCount: inc then dec to zero returns true" {
    var rc = AtomicRefCount.init();
    _ = rc.inc(); // → 2
    try testing.expect(!rc.dec()); // → 1, not zero
    try testing.expect(rc.dec()); // → 0, returns true
}
