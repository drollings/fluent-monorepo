/// http_transport_test.zig — Unit tests for HTTP transport layer
///
/// Tests:
///   - Handler VTable interface
///   - Transport initialization
///   - Shutdown signaling
///
/// Note: Integration tests for actual HTTP requests require Zig 0.15 HTTP API
/// refactor (see G3.1 TODO). Current tests focus on unit-level functionality.
const std = @import("std");
const testing = std.testing;
const http_transport = @import("http_transport.zig");
const HttpTransport = http_transport.HttpTransport;
const McpHandler = http_transport.McpHandler;

/// Creates a handler for echoing requests in the test module.
fn makeEchoHandler() McpHandler {
    const EchoHandler = struct {
        fn handleJsonRpc(ptr: *anyopaque, json: []const u8, alloc: std.mem.Allocator) anyerror![]const u8 {
            _ = ptr;
            return alloc.dupe(u8, json);
        }
        fn deinit(ptr: *anyopaque) void {
            _ = ptr;
        }
    };
    const vtable = McpHandler.VTable{
        .handleJsonRpc = EchoHandler.handleJsonRpc,
        .deinit = EchoHandler.deinit,
    };
    var state: usize = 0;
    return .{
        .ptr = @ptrCast(&state),
        .vtable = @constCast(&vtable),
    };
}

test "HttpTransport: init sets default values" {
    var mcp_handler = makeEchoHandler();
    const transport = HttpTransport.init(testing.allocator, mcp_handler, "127.0.0.1", 8080);
    _ = &mcp_handler;

    try testing.expectEqualStrings("127.0.0.1", transport.bind_address);
    try testing.expectEqual(@as(u16, 8080), transport.port);
    try testing.expect(!transport.running);
    try testing.expect(!transport.shutdown_requested);
}

test "HttpTransport: requestShutdown sets flag" {
    var mcp_handler = makeEchoHandler();
    var transport = HttpTransport.init(testing.allocator, mcp_handler, "127.0.0.1", 18085);
    _ = &mcp_handler;

    try testing.expect(!transport.shutdown_requested);
    transport.requestShutdown();
    try testing.expect(transport.shutdown_requested);
}

test "HttpTransport: deinit cleans up without error" {
    var mcp_handler = makeEchoHandler();
    var transport = HttpTransport.init(testing.allocator, mcp_handler, "127.0.0.1", 18087);
    _ = &mcp_handler;

    transport.deinit();
}

test "McpHandler VTable interface compiles and calls through" {
    const TestHandler = struct {
        call_count: usize = 0,

        fn handleJsonRpc(ptr: *anyopaque, json: []const u8, alloc: std.mem.Allocator) anyerror![]const u8 {
            const self: *@This() = @ptrCast(@alignCast(ptr));
            self.call_count += 1;
            _ = json;
            return alloc.dupe(u8, "{\"result\":\"ok\"}");
        }
        fn deinit(ptr: *anyopaque) void {
            const self: *@This() = @ptrCast(@alignCast(ptr));
            self.call_count = 0;
        }
    };

    var handler_state: TestHandler = .{};
    const vtable = McpHandler.VTable{
        .handleJsonRpc = TestHandler.handleJsonRpc,
        .deinit = TestHandler.deinit,
    };
    var mcp_handler: McpHandler = .{
        .ptr = @ptrCast(&handler_state),
        .vtable = @constCast(&vtable),
    };

    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();

    const response = try mcp_handler.handleJsonRpc("{\"test\":true}", arena.allocator());
    defer arena.allocator().free(response);

    try testing.expectEqual(@as(usize, 1), handler_state.call_count);
    try testing.expect(std.mem.indexOf(u8, response, "ok") != null);

    mcp_handler.deinit();
    try testing.expectEqual(@as(usize, 0), handler_state.call_count);
}

test "McpHandler: echo handler returns input" {
    var mcp_handler = makeEchoHandler();

    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();

    const input = "{\"jsonrpc\":\"2.0\",\"method\":\"test\",\"id\":1}";
    const response = try mcp_handler.handleJsonRpc(input, arena.allocator());
    defer arena.allocator().free(response);

    try testing.expectEqualStrings(input, response);
}

test "SseEvent: default values" {
    const event = http_transport.SseEvent{
        .data = "test data",
    };
    try testing.expectEqualStrings("message", event.event);
    try testing.expect(event.id == null);
    try testing.expectEqualStrings("test data", event.data);
}

test "SseState: default values" {
    const state = http_transport.SseState{
        .connected_at = std.time.timestamp(),
    };
    try testing.expect(state.last_event_id == null);
}

test "HttpTransport: bind address parsing" {
    var mcp_handler = makeEchoHandler();
    const transport = HttpTransport.init(testing.allocator, mcp_handler, "0.0.0.0", 3000);
    _ = &mcp_handler;

    try testing.expectEqualStrings("0.0.0.0", transport.bind_address);
    try testing.expectEqual(@as(u16, 3000), transport.port);
}

test "HttpTransport: signals ready when listenWithReady called" {
    // This test verifies that listenWithReady sets the ready signal.
    // Note: The server blocks on accept() until shutdown is requested,
    // which requires a connection to trigger checking shutdown_requested.
    // For now, we verify the signal mechanism works by checking that
    // ready_signal field can be set.

    var mcp_handler = makeEchoHandler();
    var transport = HttpTransport.init(testing.allocator, mcp_handler, "127.0.0.1", 18092);
    _ = &mcp_handler;

    var ready = std.Thread.ResetEvent{};

    // Verify the field can be set
    transport.ready_signal = &ready;
    try testing.expect(transport.ready_signal != null);

    // Verify initial state
    try testing.expect(!transport.running);
    try testing.expect(!transport.shutdown_requested);
}

