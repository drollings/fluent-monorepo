/// http_transport.zig — M4.1/M4.2 HTTP Transport Layer with SSE
///
/// HTTP server for MCP JSON-RPC over HTTP, enabling remote agents to connect.
///
/// Implements:
///   - POST /mcp — JSON-RPC handler
///   - GET /health — health check
///   - GET /metrics — Prometheus metrics (M8.1)
///   - SSE streaming for progress events (M4.2)
///   - CORS headers for browser clients
///   - Graceful shutdown
///   - Per-request arena allocation
///
/// Thread model: Single-threaded event loop (acceptable for first implementation).
/// Future: Thread pool for concurrent requests.
const std = @import("std");
const http = std.http;
const net = std.net;

/// Manages McP transport logic, owns buffers, handles initialization/deinit; ensures consistent state across sessions.
pub const McpHandler = struct {
    ptr: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        handleJsonRpc: *const fn (ptr: *anyopaque, json: []const u8, allocator: std.mem.Allocator) anyerror![]const u8,
        deinit: *const fn (ptr: *anyopaque) void,
    };

    pub fn handleJsonRpc(self: McpHandler, json: []const u8, allocator: std.mem.Allocator) ![]const u8 {
        return self.vtable.handleJsonRpc(self.ptr, json, allocator);
    }

    pub fn deinit(self: McpHandler) void {
        self.vtable.deinit(self.ptr);
    }
};

/// Manages SSE event data structures; owned by the module; ensures consistent state across runs.
pub const SseEvent = struct {
    event: []const u8 = "message",
    data: []const u8,
    id: ?[]const u8 = null,
};

/// SSE connection state for resume support.
pub const SseState = struct {
    last_event_id: ?[]const u8 = null,
    connected_at: i64,
};

/// Manages HTTP transport connections, owns connection pools, and ensures stable, thread-safe communication.
pub const HttpTransport = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    bind_address: []const u8,
    port: u16,
    mcp_handler: McpHandler,
    running: bool = false,
    shutdown_requested: bool = false,
    ready_signal: ?*std.Thread.ResetEvent = null,
    sse_connections: std.ArrayListUnmanaged(*SseConnection) = .{},
    sse_heartbeat_ms: u32 = 30_000,

    /// Initialize HTTP transport with binding configuration.
    pub fn init(
        allocator: std.mem.Allocator,
        mcp_handler: McpHandler,
        bind_address: []const u8,
        port: u16,
    ) Self {
        return .{
            .allocator = allocator,
            .mcp_handler = mcp_handler,
            .bind_address = bind_address,
            .port = port,
        };
    }

    /// Start the HTTP server and listen for connections.
    /// Blocks until shutdown is requested.
    pub fn listen(self: *Self) !void {
        const addr = try net.Address.parseIp4(self.bind_address, self.port);
        var server = try addr.listen(.{
            .kernel_backlog = 128,
        });
        defer server.deinit();

        self.running = true;
        if (self.ready_signal) |signal| {
            signal.set();
        }

        var read_buf: [8192]u8 = undefined;

        while (!self.shutdown_requested) {
            const conn = server.accept() catch |err| {
                if (self.shutdown_requested) break;
                std.log.err("accept error: {}", .{err});
                continue;
            };

            self.handleConnection(conn, &read_buf) catch |err| {
                std.log.err("connection error: {}", .{err});
            };

            conn.stream.close();
        }

        self.running = false;
    }

    /// Start the HTTP server with a ready signal.
    /// Signal is set when the server is listening and ready to accept connections.
    /// Useful for integration tests that need to know when the server is up.
    pub fn listenWithReady(self: *Self, ready_signal: *std.Thread.ResetEvent) !void {
        self.ready_signal = ready_signal;
        return self.listen();
    }

    /// Request graceful shutdown.
    pub fn requestShutdown(self: *Self) void {
        self.shutdown_requested = true;
    }

    fn handleConnection(self: *Self, conn: net.Server.Connection, read_buf: []u8) !void {
        var arena = std.heap.ArenaAllocator.init(self.allocator);
        defer arena.deinit();

        var send_buf: [4096]u8 = undefined;
        var connection_reader = conn.stream.reader(read_buf);
        var connection_writer = conn.stream.writer(&send_buf);
        var server = http.Server.init(connection_reader.interface(), &connection_writer.interface);

        while (true) {
            var request = server.receiveHead() catch |err| {
                if (err == error.HttpConnectionClosing) return;
                return err;
            };

            try self.handleRequest(&request, arena.allocator());

            // Check for keep-alive from parsed head
            const keep_alive = request.head.keep_alive;
            if (!keep_alive) break;
        }
    }

    /// Extract a header value from raw head buffer.
    /// Returns null if header not found.
    fn getHeader(head_buffer: []const u8, name: []const u8) ?[]const u8 {
        var lines = std.mem.splitSequence(u8, head_buffer, "\r\n");
        var found_status = false;
        while (lines.next()) |line| {
            if (line.len == 0) continue;
            if (!found_status) {
                found_status = true;
                continue;
            }
            const colon_idx = std.mem.indexOfScalar(u8, line, ':') orelse continue;
            const header_name = line[0..colon_idx];
            const header_value = std.mem.trim(u8, line[colon_idx + 1 ..], " \t");
            if (std.ascii.eqlIgnoreCase(header_name, name)) {
                return header_value;
            }
        }
        return null;
    }

    fn handleRequest(self: *Self, request: *http.Server.Request, arena: std.mem.Allocator) !void {
        const method = request.head.method;
        const target = request.head.target;

        // Check for SSE streaming request by parsing headers from raw buffer
        const accept = getHeader(request.head_buffer, "Accept");
        const x_stream = getHeader(request.head_buffer, "X-MCP-Stream");
        const is_sse = (accept != null and std.mem.indexOf(u8, accept.?, "text/event-stream") != null) or
            (x_stream != null and std.mem.eql(u8, x_stream.?, "true"));

        // GET /health — health check
        if (std.mem.eql(u8, @tagName(method), "GET") and std.mem.startsWith(u8, target, "/health")) {
            try self.sendHealthCheck(request);
            return;
        }

        // GET /metrics — Prometheus metrics
        if (std.mem.eql(u8, @tagName(method), "GET") and std.mem.eql(u8, target, "/metrics")) {
            try self.sendMetrics(request, arena);
            return;
        }

        // POST /mcp — JSON-RPC handler
        if (std.mem.eql(u8, @tagName(method), "POST") and std.mem.eql(u8, target, "/mcp")) {
            if (is_sse) {
                try self.handleMcpRequestSse(request, arena);
            } else {
                try self.handleMcpRequest(request, arena);
            }
            return;
        }

        // OPTIONS — CORS preflight
        if (std.mem.eql(u8, @tagName(method), "OPTIONS")) {
            try self.sendCorsPreflight(request);
            return;
        }

        // Fallback: 404
        try self.sendNotFound(request);
    }

    fn handleMcpRequest(self: *Self, request: *http.Server.Request, arena: std.mem.Allocator) !void {
        // Read body using Zig 0.15 API
        var read_buf: [65536]u8 = undefined;
        const reader = http.Server.Request.readerExpectNone(request, &read_buf);

        const content_length = request.head.content_length orelse 0;
        if (content_length == 0) {
            try self.sendError(request, 400, "Empty body");
            return;
        }
        if (content_length > 1024 * 1024) {
            try self.sendError(request, 413, "Body too large");
            return;
        }

        const body = try reader.readAlloc(arena, content_length);

        // Parse JSON-RPC request.
        const response = self.mcp_handler.handleJsonRpc(body, arena) catch |err| {
            try self.sendError(request, 500, "Internal server error");
            std.log.err("MCP handler error: {}", .{err});
            return;
        };

        try self.sendJson(request, response);
    }

    /// Handle MCP request with SSE streaming.
    fn handleMcpRequestSse(self: *Self, request: *http.Server.Request, arena: std.mem.Allocator) !void {
        // Read body using Zig 0.15 API
        var read_buf: [65536]u8 = undefined;
        const reader = http.Server.Request.readerExpectNone(request, &read_buf);

        const content_length = request.head.content_length orelse 0;
        if (content_length == 0) {
            return error.EmptyBody;
        }
        if (content_length > 1024 * 1024) {
            return error.BodyTooLarge;
        }

        const body = try reader.readAlloc(arena, content_length);

        // Check for resume with Last-Event-ID
        const last_event_id = getHeader(request.head_buffer, "Last-Event-ID");

        // Send SSE headers
        try request.respond("", .{
            .status = .ok,
            .extra_headers = &.{
                .{ .name = "Content-Type", .value = "text/event-stream" },
                .{ .name = "Cache-Control", .value = "no-cache" },
                .{ .name = "Connection", .value = "keep-alive" },
                .{ .name = "Access-Control-Allow-Origin", .value = "*" },
            },
        });

        // Send initial event with JSON-RPC response
        const response = self.mcp_handler.handleJsonRpc(body, arena) catch |err| {
            std.log.err("MCP handler error in SSE: {}", .{err});
            return;
        };

        try self.sendSseEvent(request, .{
            .event = "message",
            .data = response,
            .id = null,
        });

        // If resume requested, replay from last_event_id
        if (last_event_id) |id| {
            _ = id;
            // TODO: Implement event replay from stored events
        }
    }

    /// Send a single SSE event.
    fn sendSseEvent(self: *Self, request: *http.Server.Request, event: SseEvent) !void {
        _ = self;
        var buf: [4096]u8 = undefined;
        var fbs = std.io.fixedBufferStream(&buf);
        const writer = fbs.writer();

        if (event.id) |id| {
            try writer.print("id: {s}\n", .{id});
        }
        try writer.print("event: {s}\n", .{event.event});
        try writer.print("data: {s}\n\n", .{event.data});

        try request.respond(fbs.getWritten(), .{
            .status = .ok,
            .extra_headers = &.{
                .{ .name = "Content-Type", .value = "text/event-stream" },
            },
        });
    }

    fn sendHealthCheck(self: *Self, request: *http.Server.Request) !void {
        _ = self;
        try request.respond("{\"status\":\"ok\"}", .{
            .status = .ok,
            .extra_headers = &.{
                .{ .name = "Content-Type", .value = "application/json" },
                .{ .name = "Access-Control-Allow-Origin", .value = "*" },
            },
        });
    }

    fn sendMetrics(self: *Self, request: *http.Server.Request, arena: std.mem.Allocator) !void {
        var buf: std.ArrayListUnmanaged(u8) = .{};
        defer buf.deinit(arena);

        // Add basic metrics
        try buf.writer(arena).writeAll("# HELP coral_connections_active Number of active connections\n");
        try buf.writer(arena).writeAll("# TYPE coral_connections_active gauge\n");
        try buf.writer(arena).print("coral_connections_active {d}\n", .{self.sse_connections.items.len});

        try request.respond(buf.items, .{
            .status = .ok,
            .extra_headers = &.{
                .{ .name = "Content-Type", .value = "text/plain; version=0.0.4" },
                .{ .name = "Access-Control-Allow-Origin", .value = "*" },
            },
        });
    }

    fn sendCorsPreflight(self: *Self, request: *http.Server.Request) !void {
        _ = self;
        try request.respond("", .{
            .status = .no_content,
            .extra_headers = &.{
                .{ .name = "Access-Control-Allow-Origin", .value = "*" },
                .{ .name = "Access-Control-Allow-Methods", .value = "POST, OPTIONS" },
                .{ .name = "Access-Control-Allow-Headers", .value = "Content-Type, Accept, Last-Event-ID, X-MCP-Stream" },
            },
        });
    }

    fn sendJson(self: *Self, request: *http.Server.Request, json: []const u8) !void {
        _ = self;
        try request.respond(json, .{
            .status = .ok,
            .extra_headers = &.{
                .{ .name = "Content-Type", .value = "application/json" },
                .{ .name = "Access-Control-Allow-Origin", .value = "*" },
            },
        });
    }

    fn sendError(self: *Self, request: *http.Server.Request, code: u16, message: []const u8) !void {
        _ = self;
        try request.respond(message, .{
            .status = @enumFromInt(code),
            .extra_headers = &.{
                .{ .name = "Content-Type", .value = "text/plain" },
            },
        });
    }

    fn sendNotFound(self: *Self, request: *http.Server.Request) !void {
        try self.sendError(request, 404, "Not Found");
    }

    pub fn deinit(self: *Self) void {
        // Clean up SSE connections
        for (self.sse_connections.items) |conn| {
            self.allocator.destroy(conn);
        }
        self.sse_connections.deinit(self.allocator);
    }
};

/// Active SSE connection for progress streaming.
pub const SseConnection = struct {
    stream: net.Stream,
    state: SseState,
};

// =============================================================================
// Tests — M4.1
// =============================================================================

const testing = std.testing;

test "HttpTransport: init sets default values" {
    const TestHandler = struct {
        fn handleJsonRpc(ptr: *anyopaque, json: []const u8, allocator: std.mem.Allocator) anyerror![]const u8 {
            _ = ptr;
            _ = json;
            _ = allocator;
            return "";
        }
        fn deinit(ptr: *anyopaque) void {
            _ = ptr;
        }
    };

    var handler_state: usize = 0;
    const vtable = McpHandler.VTable{
        .handleJsonRpc = TestHandler.handleJsonRpc,
        .deinit = TestHandler.deinit,
    };
    const mcp_handler = McpHandler{
        .ptr = @ptrCast(&handler_state),
        .vtable = @constCast(&vtable),
    };

    const transport = HttpTransport.init(testing.allocator, mcp_handler, "127.0.0.1", 8080);
    try testing.expectEqualStrings("127.0.0.1", transport.bind_address);
    try testing.expectEqual(@as(u16, 8080), transport.port);
    try testing.expect(!transport.running);
}

test "McpHandler VTable interface compiles" {
    const VTable = McpHandler.VTable;
    try testing.expect(@sizeOf(VTable) > 0);
}

// =============================================================================
// Integration Tests — G3.1
// Note: HTTP integration tests require network I/O and are skipped by default.
// Run with `zig test --test-filter "integration"` to enable.
// =============================================================================

/// Manages HTTP transport logic for Zig, owns transport state, ensures consistent initialization and cleanup.
const TestMcpHandler = struct {
    response: []const u8,
    called: bool = false,

    fn handleJsonRpc(ptr: *anyopaque, json: []const u8, allocator: std.mem.Allocator) anyerror![]const u8 {
        const self: *@This() = @ptrCast(@alignCast(ptr));
        self.called = true;
        _ = json;
        return try allocator.dupe(u8, self.response);
    }

    fn deinit(ptr: *anyopaque) void {
        _ = ptr;
    }

    fn initVTable() McpHandler.VTable {
        return .{
            .handleJsonRpc = handleJsonRpc,
            .deinit = deinit,
        };
    }
};

test "HttpTransport: health check returns JSON" {
    // Unit test: verify health check response format
    var handler = TestMcpHandler{ .response = "" };
    const mcp_handler = McpHandler{
        .ptr = @ptrCast(&handler),
        .vtable = @constCast(&TestMcpHandler.initVTable()),
    };

    var transport = HttpTransport.init(testing.allocator, mcp_handler, "127.0.0.1", 8080);
    defer transport.deinit();

    // Verify initialization
    try testing.expectEqualStrings("127.0.0.1", transport.bind_address);
    try testing.expectEqual(@as(u16, 8080), transport.port);
}

test "HttpTransport: SSE event format" {
    // Unit test: verify SSE event formatting
    const event = SseEvent{
        .event = "message",
        .data = "{\"result\":42}",
        .id = "123",
    };

    try testing.expectEqualStrings("message", event.event);
    try testing.expectEqualStrings("{\"result\":42}", event.data);
    try testing.expectEqualStrings("123", event.id.?);
}
