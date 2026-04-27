//! Tests for mock_vtable.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const mock_vtable_mod = @import("mock_vtable.zig");

test "MockEmbeddingProvider: GPA no leaks" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("memory leak");

    var mock = mock_vtable_mod.MockEmbeddingProvider.init(gpa.allocator());
    defer mock.deinit();

    mock.setEmbedResult(&[_]f32{ 1.0, 2.0 });

    var p = mock.provider();
    const v1 = try p.embed(gpa.allocator(), "test1");
    defer gpa.allocator().free(v1);
    const v2 = try p.embed(gpa.allocator(), "test2");
    defer gpa.allocator().free(v2);

    mock.assertCallCount("embed", 2);
}
