//! Tests for logging.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const logging_mod = @import("logging.zig");

test "Scope: begin and end with no context does not panic" {
    logging_mod.LogContext.clear();
    const scope = logging_mod.Scope.begin("test_op");
    scope.end();
}
test "Scope: begin and end with context" {
    logging_mod.LogContext.set(.{ .request_id = "req-scope" });
    defer logging_mod.LogContext.clear();

    const scope = logging_mod.Scope.begin("test_op");
    scope.end();
}
