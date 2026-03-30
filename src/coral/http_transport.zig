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

/// MCP handler interface — must implement handleJsonRpc.
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

/// SSE event for streaming responses.
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

/// HTTP transport server wrapping an MCP handler.
pub const HttpTransport = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    bind_address: []const u8,
    port: u16,
    mcp_handler: McpHandler,
    running: bool = false,
    shutdown_requested: bool = false,
    /// SSE connections for progress streaming
    sse_connections: std.ArrayListUnmanaged(*SseConnection) = .{},
    /// Heartbeat interval for SSE connections (milliseconds)
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

    /// Request graceful shutdown.
    pub fn requestShutdown(self: *Self) void {
        self.shutdown_requested = true;
    }

    fn handleConnection(self: *Self, conn: net.StreamServer.Connection, read_buf: []u8) !void {
        var arena = std.heap.ArenaAllocator.init(self.allocator);
        defer arena.deinit();

        var server = http.Server.init(conn.stream, read_buf);
        defer server.deinit();

        while (true) {
            var request = server.receiveHead() catch |err| {
                if (err == error.HttpConnectionClosing) return;
                return err;
            };

            try self.handleRequest(&request, arena.allocator());

            // Check for keep-alive or close.
            const connection = request.head.headers.getFirstValue("Connection");
            const keep_alive = if (connection) |c| std.ascii.eqlIgnoreCase(c, "keep-alive") else false;
            if (!keep_alive) break;
        }
    }

    fn handleRequest(self: *Self, request: *http.Server.Request, arena: std.mem.Allocator) !void {
        const method = request.head.method;
        const target = request.head.target;

        // Check for SSE streaming request
        const accept = request.head.headers.getFirstValue("Accept");
        const x_stream = request.head.headers.getFirstValue("X-MCP-Stream");
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
        // Read body.
        const body = try (try request.reader()).readAllAlloc(arena, 1024 * 1024);

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
        // Read body.
        const body = try (try request.reader()).readAllAlloc(arena, 1024 * 1024);

        // Check for resume with Last-Event-ID
        const last_event_id = request.head.headers.getFirstValue("Last-Event-ID");

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
        try buf.writer().writeAll("# HELP coral_connections_active Number of active connections\n");
        try buf.writer().writeAll("# TYPE coral_connections_active gauge\n");
        try buf.writer().print("coral_connections_active {d}\n", .{self.sse_connections.items.len});

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
