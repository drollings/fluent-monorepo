//! Tests for wrapper.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const builtin = @import("builtin");
const wrapper_mod = @import("wrapper.zig");

test "wrapIf: build-mode selection compiles correctly" {
    const debugOnly = struct {
        fn f(x: i32) i32 {
            return x + 100;
        }
    }.f;
    const normal = struct {
        fn f(x: i32) i32 {
            return x;
        }
    }.f;

    // In test builds (Debug mode), this selects debugOnly.
    const handler = wrapper_mod.wrapIf(builtin.mode == .Debug, debugOnly, normal);
    // Just verify it compiles and returns a value (exact value depends on build mode).
    _ = handler(1);
}
