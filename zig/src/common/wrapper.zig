//! wrapper.zig — Conditional and composable comptime wrappers (M9).
//!
//! ## Design
//!
//! Zig's type system does not allow creating a wrapper function that perfectly
//! preserves an arbitrary function's parameter types (generic functions cannot be
//! cast to concrete function types).  This module therefore provides two styles:
//!
//! ### 1. Call helpers (recommended for most cases)
//!
//! Instead of wrapping a function once and getting a new function of the same type,
//! call helpers wrap a *call site*:
//!
//!   // Instead of:        const result = try func(arg1, arg2);
//!   // Write:             const result = try retryCall(3, func, .{arg1, arg2});
//!
//! Call helpers compose naturally:
//!
//!   const result = try loggedCall("op", func, .{arg1, arg2});
//!   // (loggedCall is in src/common/logging.zig)
//!
//! ### 2. Conditional identity wrappers (wrapIf)
//!
//! `wrapIf` selects between two comptime-known functions of the *same* type.
//! Useful for eliding debug-only functions from release builds:
//!
//!   const handler = wrapIf(std.debug.runtime_safety, debugVersion, releaseVersion);
//!
//! ### Wrapper composition order
//!
//! When multiple call helpers are composed, apply in this order so each layer
//! sees the full effect of those below it:
//!
//!   1. Rate limiting  — reject early if overloaded
//!   2. Auth           — reject early if unauthorized
//!   3. Tracing        — start span / context
//!   4. Timing         — measure full duration  (see callLogged in logging.zig)
//!   5. Retry          — retry on transient failure  (see retryCall below)
//!   6. Validation     — validate input before processing
//!   7. Core handler
//!
//! Correct:   rateLimit → auth → trace → time → retry → validate → handler
//! Incorrect: time → auth → rateLimit  (timing measures auth+rate-limit overhead)

const std = @import("std");
const builtin = @import("builtin");

// ── wrapIf ────────────────────────────────────────────────────────────────────

/// Applies a conditional transformation, returning the true value wrapped safely.
pub inline fn wrapIf(
    comptime condition: bool,
    comptime if_true: anytype,
    comptime if_false: @TypeOf(if_true),
) @TypeOf(if_true) {
    return if (condition) if_true else if_false;
}

// ── retryCall ─────────────────────────────────────────────────────────────────

/// Retries a Zig function up to max_attempts times, returning its type-safe result.
pub fn retryCall(
    comptime max_attempts: usize,
    func: anytype,
    args: anytype,
) @typeInfo(@TypeOf(func)).@"fn".return_type.? {
    comptime std.debug.assert(max_attempts >= 1);
    var last_err: anyerror = error.Unexpected;
    var attempt: usize = 0;
    while (attempt < max_attempts) : (attempt += 1) {
        return @call(.auto, func, args) catch |err| {
            last_err = err;
            continue;
        };
    }
    return last_err;
}

// ── WrapperKind / Pipeline ────────────────────────────────────────────────────

/// Identifies a wrapper kind for use with `Pipeline`.
pub const WrapperKind = enum {
    /// No-op (identity).
    none,
    /// Retry up to 3 times on error.
    retry,
};

/// Comptime call pipeline.  Applies wrappers around a function call.
///
/// Example:
///
///   const result = try Pipeline.call(&.{.retry}, fetchData, .{url, alloc});
pub const Pipeline = struct {
    /// Apply each wrapper kind in `kinds` around a call to `func(args)`.
    /// Wrappers are applied from the first entry (outermost) to the last.
    pub fn call(
        comptime kinds: []const WrapperKind,
        func: anytype,
        args: anytype,
    ) @typeInfo(@TypeOf(func)).@"fn".return_type.? {
        return applyKinds(kinds, 0, func, args);
    }

    fn applyKinds(
        comptime kinds: []const WrapperKind,
        comptime idx: usize,
        func: anytype,
        args: anytype,
    ) @typeInfo(@TypeOf(func)).@"fn".return_type.? {
        if (idx >= kinds.len) {
            return @call(.auto, func, args);
        }
        return switch (kinds[idx]) {
            .none => applyKinds(kinds, idx + 1, func, args),
            .retry => retryCall(3, struct {
                fn inner(f: @TypeOf(func), a: @TypeOf(args)) @typeInfo(@TypeOf(func)).@"fn".return_type.? {
                    return applyKinds(kinds, idx + 1, f, a);
                }
            }.inner, .{ func, args }),
        };
    }
};

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;

test "wrapIf: condition true returns if_true" {
    const add1 = struct {
        fn f(x: i32) i32 {
            return x + 1;
        }
    }.f;
    const add2 = struct {
        fn f(x: i32) i32 {
            return x + 2;
        }
    }.f;

    const chosen = wrapIf(true, add1, add2);
    try testing.expectEqual(@as(i32, 6), chosen(5)); // 5+1
}

test "wrapIf: condition false returns if_false" {
    const add1 = struct {
        fn f(x: i32) i32 {
            return x + 1;
        }
    }.f;
    const add2 = struct {
        fn f(x: i32) i32 {
            return x + 2;
        }
    }.f;

    const chosen = wrapIf(false, add1, add2);
    try testing.expectEqual(@as(i32, 7), chosen(5)); // 5+2
}

test "retryCall: succeeds on first attempt" {
    var calls: usize = 0;
    const succeed = struct {
        fn f(n: *usize) anyerror!i32 {
            n.* += 1;
            return 42;
        }
    }.f;

    const result = try retryCall(3, succeed, .{&calls});
    try testing.expectEqual(@as(i32, 42), result);
    try testing.expectEqual(@as(usize, 1), calls);
}

test "retryCall: retries up to max_attempts on error" {
    var calls: usize = 0;
    const alwaysFail = struct {
        fn f(n: *usize) anyerror!i32 {
            n.* += 1;
            return error.Oops;
        }
    }.f;

    const result = retryCall(3, alwaysFail, .{&calls});
    try testing.expectError(error.Oops, result);
    try testing.expectEqual(@as(usize, 3), calls);
}

test "retryCall: succeeds after one failure" {
    var calls: usize = 0;
    const failOnce = struct {
        fn f(n: *usize) anyerror!i32 {
            n.* += 1;
            if (n.* < 2) return error.Transient;
            return 7;
        }
    }.f;

    const result = try retryCall(3, failOnce, .{&calls});
    try testing.expectEqual(@as(i32, 7), result);
    try testing.expectEqual(@as(usize, 2), calls);
}

test "Pipeline: .none is identity" {
    const base = struct {
        fn f(x: i32) i32 {
            return x * 3;
        }
    }.f;

    const result = Pipeline.call(&.{.none}, base, .{5});
    try testing.expectEqual(@as(i32, 15), result);
}

test "Pipeline: .retry wraps with retry logic" {
    var calls: usize = 0;
    const failOnce = struct {
        fn f(n: *usize) anyerror!i32 {
            n.* += 1;
            if (n.* < 2) return error.Transient;
            return 99;
        }
    }.f;

    const result = try Pipeline.call(&.{.retry}, failOnce, .{&calls});
    try testing.expectEqual(@as(i32, 99), result);
    try testing.expectEqual(@as(usize, 2), calls);
}
