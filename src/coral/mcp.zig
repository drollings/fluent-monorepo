/// mcp.zig — Coral MCP (Model Context Protocol) server.
///
/// Implements JSON-RPC 2.0 over STDIO transport for use with Claude Code,
/// NullClaw, and Cursor.  JSON is parsed only at the perimeter; all internal
/// routing uses QueueReactor directly.
///
/// Arena #5 of 5: each request gets its own arena.  Only the serialized
/// response escapes to the caller's allocator.
const std = @import("std");
const coral_db = @import("coral_db");
const cache = @import("cache.zig");
const common = @import("common");
const reflection = common.reflection;

const Library = coral_db.Library;
const ContextNode = coral_db.ContextNode;
const ContextPacker = coral_db.ContextPacker;
const QueueReactor = cache.QueueReactor;

// ---------------------------------------------------------------------------
// MCP tool parameter structs (M2.6)
// Enables Editable(T).describeSchema() to generate JSON Schema at runtime,
// replacing the hardcoded input_schema strings.
// ---------------------------------------------------------------------------

/// Manages query parameters for Coral queries, owns state, ensures consistent invariants across sessions.
const CoralQueryParams = struct {
    query: []const u8,
    pub const editable: reflection.Editable(@This()) = .{};
    pub fn describeField(comptime name: []const u8) reflection.FieldMeta {
        if (comptime std.mem.eql(u8, name, "query")) {
            return .{
                .description = "The natural language query to route through the tiered cache",
                .examples = &.{ "Who is Ada Lovelace?", "error: module not found" },
                .identity = true,
            };
        }
        return .{};
    }
};

/// Manages node insertion parameters with fixed-size buffers; encapsulates ownership and invariants.
const InsertNodeParams = struct {
    name: []const u8,
    description: []const u8,
    pub const editable: reflection.Editable(@This()) = .{};
    pub fn describeField(comptime name: []const u8) reflection.FieldMeta {
        if (comptime std.mem.eql(u8, name, "name")) {
            return .{
                .description = "Human-readable name for the context node (lod4)",
                .identity = true,
                .examples = &.{ "Ada Lovelace", "build_zig", "HTTP gateway" },
            };
        }
        if (comptime std.mem.eql(u8, name, "description")) {
            return .{
                .description = "Full text content for the context node (lod0)",
                .examples = &.{"A pioneering computer scientist known for her work on the Analytical Engine."},
            };
        }
        return .{};
    }
};

/// Defines configuration parameters for Zig compilation; manages ownership and invariants during build.
const ExplainParams = struct {
    name: []const u8,
    max_tokens: i64 = 4096,
    pub const editable: reflection.Editable(@This()) = .{};
    pub fn describeField(comptime name: []const u8) reflection.FieldMeta {
        if (comptime std.mem.eql(u8, name, "name")) {
            return .{
                .description = "Name of the context node to explain (matches lod4)",
                .identity = true,
                .relation = "context_nodes",
            };
        }
        if (comptime std.mem.eql(u8, name, "max_tokens")) {
            return .{
                .description = "Maximum token budget for LOD-packed context output",
                .default = "4096",
                .min = 256,
                .max = 32768,
                .units = "tokens",
            };
        }
        return .{};
    }
};

/// Check that all required (identity=true) fields are present in the args map.
fn validateRequiredFields(
    comptime T: type,
    args: std.json.ObjectMap,
) !void {
    const accessors = reflection.Editable(T).accessors;
    inline for (accessors) |acc| {
        if (acc.meta.identity) {
            if (args.get(acc.name) == null) return error.MissingRequiredField;
        }
    }
}

// ---------------------------------------------------------------------------
// Tool definitions (P4.2)
// ---------------------------------------------------------------------------

/// MCP tool descriptor: name, human-readable description, and JSON Schema for the input object.
pub const ToolDef = struct {
    name: []const u8,
    description: []const u8,
    input_schema: []const u8,
};

pub const TOOLS = [_]ToolDef{
    .{
        .name = "coral_query",
        .description = "Route a query through the Coral tier cache (L1 memory → L4 KNN → L5 LLM)",
        .input_schema =
        \\{"type":"object","properties":{"query":{"type":"string","description":"The query to route"}},"required":["query"]}
        ,
    },
    .{
        .name = "coral_insert_node",
        .description = "Add a ContextNode to the Coral database",
        .input_schema =
        \\{"type":"object","properties":{"name":{"type":"string"},"description":{"type":"string"}},"required":["name","description"]}
        ,
    },
    .{
        .name = "coral_explain",
        .description = "Return BFS neighbors of a named node as LOD-packed context",
        .input_schema =
        \\{"type":"object","properties":{"name":{"type":"string"},"max_depth":{"type":"integer","default":3}},"required":["name"]}
        ,
    },
};

// ---------------------------------------------------------------------------
// JSON-RPC primitives
// ---------------------------------------------------------------------------

/// Incoming JSON-RPC 2.0 request frame; `id` is null for notifications.
const JsonRpcRequest = struct {
    jsonrpc: []const u8 = "2.0",
    id: ?std.json.Value = null,
    method: []const u8,
    params: ?std.json.Value = null,
};

// ---------------------------------------------------------------------------
// McpServer
// ---------------------------------------------------------------------------

/// Manages server state with fixed buffers; encapsulates initialization logic; not thread-safe.
pub const McpServer = struct {
    allocator: std.mem.Allocator,
    reactor: *QueueReactor,
    server_info: ServerInfo = .{},

    pub const ServerInfo = struct {
        name: []const u8 = "coral",
        version: []const u8 = "0.1.0",
    };

    /// Serve requests from reader, writing responses to writer.
    /// Reads Content-Length framed JSON-RPC messages until EOF.
    pub fn serve(self: *McpServer, reader: *std.Io.Reader, writer: *std.Io.Writer) !void {
        var serve_arena = std.heap.ArenaAllocator.init(self.allocator);
        defer serve_arena.deinit();
        const a = serve_arena.allocator();

        while (true) {
            // Parse Content-Length header lines until blank line.
            // takeDelimiterInclusive returns the line including '\n', buffered.
            var content_length: ?usize = null;
            while (true) {
                const line_with_nl = reader.takeDelimiterInclusive('\n') catch |err| {
                    if (err == error.EndOfStream) return;
                    return err;
                };
                // Trim \r\n
                const line = std.mem.trimRight(u8, line_with_nl, "\r\n");
                if (line.len == 0) break; // blank line separates headers from body

                const prefix = "Content-Length:";
                if (std.ascii.startsWithIgnoreCase(line, prefix)) {
                    const val_str = std.mem.trim(u8, line[prefix.len..], " \t");
                    content_length = try std.fmt.parseInt(usize, val_str, 10);
                }
            }

            const len = content_length orelse continue;

            // P3: Reject requests larger than 10 MB (see common/limits.zig MAX_MCP_REQUEST_SIZE).
            const MAX_MCP_REQUEST_SIZE: usize = 10 * 1024 * 1024;
            if (len > MAX_MCP_REQUEST_SIZE) {
                // Drain remaining headers/body to keep the connection clean, then signal error.
                std.log.warn("MCP request body too large: {d} bytes (max {d})", .{ len, MAX_MCP_REQUEST_SIZE });
                try writer.print("Content-Length: 0\r\n\r\n", .{});
                try writer.flush();
                _ = serve_arena.reset(.retain_capacity);
                continue;
            }

            // Read body into arena-allocated buffer
            const body = try reader.readAlloc(a, len);

            const response = try self.handleRequest(body);
            defer self.allocator.free(response);

            try writer.print("Content-Length: {d}\r\n\r\n{s}", .{ response.len, response });
            try writer.flush();

            _ = serve_arena.reset(.retain_capacity);
        }
    }

    /// Handle a single JSON-RPC request.
    /// Returns an allocator-owned JSON response string (caller must free).
    /// arena #5: all parse/routing scratch lives in req_arena; only the
    /// serialized response escapes to self.allocator.
    pub fn handleRequest(self: *McpServer, raw_json: []const u8) ![]const u8 {
        var req_arena = std.heap.ArenaAllocator.init(self.allocator);
        defer req_arena.deinit();
        const a = req_arena.allocator();

        const req = parseJsonRpc(a, raw_json) catch |err| {
            return self.errorResponse(null, -32700, "Parse error", err);
        };

        const result = self.dispatch(a, req) catch |err| {
            return self.errorResponse(req.id, -32603, "Internal error", err);
        };
        return result;
    }

    // -----------------------------------------------------------------------
    // JSON-RPC dispatch
    // -----------------------------------------------------------------------

    fn dispatch(self: *McpServer, a: std.mem.Allocator, req: JsonRpcRequest) ![]const u8 {
        if (std.mem.eql(u8, req.method, "initialize")) {
            return self.handleInitialize(a, req);
        } else if (std.mem.eql(u8, req.method, "tools/list")) {
            return self.handleToolsList(a, req);
        } else if (std.mem.eql(u8, req.method, "tools/call")) {
            return self.handleToolsCall(a, req);
        } else {
            // Method not found
            return self.errorResponse(req.id, -32601, "Method not found", error.UnknownMethod);
        }
    }

    fn handleInitialize(self: *McpServer, a: std.mem.Allocator, req: JsonRpcRequest) ![]const u8 {
        _ = a;
        var buf: std.ArrayList(u8) = .{};
        defer buf.deinit(self.allocator);
        const w = buf.writer(self.allocator);

        const id_str = try idToJson(self.allocator, req.id);
        defer self.allocator.free(id_str);

        try w.print(
            \\{{"jsonrpc":"2.0","id":{s},"result":{{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"{s}","version":"{s}"}}}}}}
        , .{ id_str, self.server_info.name, self.server_info.version });
        return buf.toOwnedSlice(self.allocator);
    }

    fn handleToolsList(self: *McpServer, a: std.mem.Allocator, req: JsonRpcRequest) ![]const u8 {
        var buf: std.ArrayList(u8) = .{};
        defer buf.deinit(self.allocator);
        const w = buf.writer(self.allocator);

        const id_str = try idToJson(self.allocator, req.id);
        defer self.allocator.free(id_str);

        // Generate schemas from Editable(T) — one allocation per schema, freed at end.
        const query_schema = try reflection.Editable(CoralQueryParams).describeSchema(a);
        const insert_schema = try reflection.Editable(InsertNodeParams).describeSchema(a);
        const explain_schema = try reflection.Editable(ExplainParams).describeSchema(a);

        try w.print(
            \\{{"jsonrpc":"2.0","id":{s},"result":{{"tools":[
        , .{id_str});

        const tool_defs = [_]struct { name: []const u8, description: []const u8, schema: []const u8 }{
            .{ .name = "coral_query", .description = TOOLS[0].description, .schema = query_schema },
            .{ .name = "coral_insert_node", .description = TOOLS[1].description, .schema = insert_schema },
            .{ .name = "coral_explain", .description = TOOLS[2].description, .schema = explain_schema },
        };

        for (tool_defs, 0..) |td, i| {
            if (i > 0) try w.writeByte(',');
            try w.print(
                \\{{"name":"{s}","description":"{s}","inputSchema":
            , .{ td.name, td.description });
            try w.writeAll(td.schema);
            try w.writeByte('}');
        }

        try w.writeAll("]}}}");
        return buf.toOwnedSlice(self.allocator);
    }

    fn handleToolsCall(self: *McpServer, a: std.mem.Allocator, req: JsonRpcRequest) ![]const u8 {
        const params = req.params orelse return self.errorResponse(req.id, -32602, "Invalid params", error.MissingParams);
        const params_obj = switch (params) {
            .object => |o| o,
            else => return self.errorResponse(req.id, -32602, "Invalid params", error.InvalidParams),
        };

        const name_val = params_obj.get("name") orelse return self.errorResponse(req.id, -32602, "Missing tool name", error.MissingToolName);
        const tool_name = switch (name_val) {
            .string => |s| s,
            else => return self.errorResponse(req.id, -32602, "Invalid tool name", error.InvalidToolName),
        };

        const args_val = params_obj.get("arguments") orelse std.json.Value{ .object = std.json.ObjectMap.init(a) };
        const args_obj = switch (args_val) {
            .object => |o| o,
            else => std.json.ObjectMap.init(a),
        };

        if (std.mem.eql(u8, tool_name, "coral_query")) {
            return self.toolCoralQuery(a, req, args_obj);
        } else if (std.mem.eql(u8, tool_name, "coral_insert_node")) {
            return self.toolCoralInsertNode(a, req, args_obj);
        } else if (std.mem.eql(u8, tool_name, "coral_explain")) {
            return self.toolCoralExplain(a, req, args_obj);
        } else {
            return self.errorResponse(req.id, -32601, "Unknown tool", error.UnknownTool);
        }
    }

    // -----------------------------------------------------------------------
    // Tool handlers
    // -----------------------------------------------------------------------

    fn toolCoralQuery(self: *McpServer, a: std.mem.Allocator, req: JsonRpcRequest, args: std.json.ObjectMap) ![]const u8 {
        _ = a;
        validateRequiredFields(CoralQueryParams, args) catch |err| {
            return self.errorResponse(req.id, -32602, "Missing required field", err);
        };
        const query_val = args.get("query") orelse return self.errorResponse(req.id, -32602, "Missing query", error.MissingQuery);
        const query = switch (query_val) {
            .string => |s| s,
            else => return self.errorResponse(req.id, -32602, "Invalid query type", error.InvalidQuery),
        };

        const routing_result = self.reactor.route(query) catch |err| {
            return self.errorResponse(req.id, -32603, "Route failed", err);
        };

        // Serialize the routing result
        var buf: std.ArrayList(u8) = .{};
        defer buf.deinit(self.allocator);
        const w = buf.writer(self.allocator);

        const id_str = try idToJson(self.allocator, req.id);
        defer self.allocator.free(id_str);

        const tier_name = @tagName(routing_result.tier_used);
        try w.print(
            \\{{"jsonrpc":"2.0","id":{s},"result":{{"content":[{{"type":"text","text":{{"tier":"{s}","latency_ms":{d},"node_count":{d}}}}}]}}}}
        , .{ id_str, tier_name, routing_result.latency_ms, routing_result.nodes.len });

        return buf.toOwnedSlice(self.allocator);
    }

    fn toolCoralInsertNode(self: *McpServer, a: std.mem.Allocator, req: JsonRpcRequest, args: std.json.ObjectMap) ![]const u8 {
        _ = a;
        validateRequiredFields(InsertNodeParams, args) catch |err| {
            return self.errorResponse(req.id, -32602, "Missing required field", err);
        };
        const name_val = args.get("name") orelse return self.errorResponse(req.id, -32602, "Missing name", error.MissingName);
        const name = switch (name_val) {
            .string => |s| s,
            else => return self.errorResponse(req.id, -32602, "Invalid name type", error.InvalidName),
        };

        const desc_val = args.get("description") orelse return self.errorResponse(req.id, -32602, "Missing description", error.MissingDescription);
        const description = switch (desc_val) {
            .string => |s| s,
            else => return self.errorResponse(req.id, -32602, "Invalid description type", error.InvalidDescription),
        };

        // Use a timestamp-based id for simplicity
        const node_id: i64 = std.time.timestamp();
        var node = ContextNode.init(node_id, name, description, self.allocator) catch |err| {
            return self.errorResponse(req.id, -32603, "Node init failed", err);
        };
        defer node.free(self.allocator);

        self.reactor.library.insertNode(node) catch |err| {
            return self.errorResponse(req.id, -32603, "Insert failed", err);
        };

        var buf: std.ArrayList(u8) = .{};
        defer buf.deinit(self.allocator);
        const w = buf.writer(self.allocator);

        const id_str = try idToJson(self.allocator, req.id);
        defer self.allocator.free(id_str);

        try w.print(
            \\{{"jsonrpc":"2.0","id":{s},"result":{{"content":[{{"type":"text","text":"inserted node {d}"}}]}}}}
        , .{ id_str, node_id });

        return buf.toOwnedSlice(self.allocator);
    }

    fn toolCoralExplain(self: *McpServer, a: std.mem.Allocator, req: JsonRpcRequest, args: std.json.ObjectMap) ![]const u8 {
        validateRequiredFields(ExplainParams, args) catch |err| {
            return self.errorResponse(req.id, -32602, "Missing required field", err);
        };
        const name_val = args.get("name") orelse return self.errorResponse(req.id, -32602, "Missing name", error.MissingName);
        const name = switch (name_val) {
            .string => |s| s,
            else => return self.errorResponse(req.id, -32602, "Invalid name type", error.InvalidName),
        };

        const token_budget: usize = blk: {
            if (args.get("max_tokens")) |tv| {
                break :blk switch (tv) {
                    .integer => |n| @intCast(@max(256, n)),
                    else => 4096,
                };
            }
            break :blk 4096;
        };

        const maybe_id = self.reactor.library.findNodeByName(name) catch |err| {
            return self.errorResponse(req.id, -32603, "findNodeByName failed", err);
        };

        const id_str = try idToJson(self.allocator, req.id);
        defer self.allocator.free(id_str);

        if (maybe_id == null) {
            var buf: std.ArrayList(u8) = .{};
            defer buf.deinit(self.allocator);
            const w = buf.writer(self.allocator);
            try w.print(
                \\{{"jsonrpc":"2.0","id":{s},"result":{{"content":[{{"type":"text","text":"node not found"}}]}}}}
            , .{id_str});
            return buf.toOwnedSlice(self.allocator);
        }

        // Use ContextPacker for LOD-aware token-budget packing.
        var packer = ContextPacker.init(a, self.reactor.library, token_budget);
        const packed_text = packer.pack(maybe_id.?) catch |err| {
            return self.errorResponse(req.id, -32603, "ContextPacker failed", err);
        };

        var buf: std.ArrayList(u8) = .{};
        defer buf.deinit(self.allocator);
        const w = buf.writer(self.allocator);

        try w.print(
            \\{{"jsonrpc":"2.0","id":{s},"result":{{"content":[{{"type":"text","text":"
        , .{id_str});
        try writeJsonEscaped(w, packed_text);
        try w.writeAll("\"}}]}}}");
        return buf.toOwnedSlice(self.allocator);
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn errorResponse(self: *McpServer, id: ?std.json.Value, code: i32, message: []const u8, err: anyerror) []const u8 {
        std.log.debug("MCP error: {}", .{err});
        const id_str = idToJson(self.allocator, id) catch return self.allocator.dupe(u8, "{}") catch "{}";
        defer self.allocator.free(id_str);

        const result = std.fmt.allocPrint(
            self.allocator,
            \\{{"jsonrpc":"2.0","id":{s},"error":{{"code":{d},"message":"{s}"}}}}
        ,
            .{ id_str, code, message },
        ) catch return self.allocator.dupe(u8, "{}") catch "{}";
        return result;
    }

    fn idToJson(allocator: std.mem.Allocator, id: ?std.json.Value) ![]const u8 {
        if (id == null) return try allocator.dupe(u8, "null");
        return switch (id.?) {
            .null => try allocator.dupe(u8, "null"),
            .integer => |n| try std.fmt.allocPrint(allocator, "{d}", .{n}),
            .float => |f| try std.fmt.allocPrint(allocator, "{d}", .{f}),
            .string => |s| try std.fmt.allocPrint(allocator, "\"{s}\"", .{s}),
            else => try allocator.dupe(u8, "null"),
        };
    }
};

fn parseJsonRpc(allocator: std.mem.Allocator, raw: []const u8) !JsonRpcRequest {
    const parsed = try std.json.parseFromSlice(std.json.Value, allocator, raw, .{});
    // Note: parsed is arena-allocated via allocator, no defer needed
    const root = parsed.value;

    const obj = switch (root) {
        .object => |o| o,
        else => return error.InvalidRequest,
    };

    const method_val = obj.get("method") orelse return error.MissingMethod;
    const method = switch (method_val) {
        .string => |s| s,
        else => return error.InvalidMethod,
    };

    return JsonRpcRequest{
        .jsonrpc = "2.0",
        .id = obj.get("id"),
        .method = method,
        .params = obj.get("params"),
    };
}

/// Write `s` to `writer` with JSON string escaping (no surrounding quotes).
fn writeJsonEscaped(writer: anytype, s: []const u8) !void {
    for (s) |ch| {
        switch (ch) {
            '"' => try writer.writeAll("\\\""),
            '\\' => try writer.writeAll("\\\\"),
            '\n' => try writer.writeAll("\\n"),
            '\r' => try writer.writeAll("\\r"),
            '\t' => try writer.writeAll("\\t"),
            0x00...0x08, 0x0B...0x0C, 0x0E...0x1F => try writer.print("\\u{x:0>4}", .{ch}),
            else => try writer.writeByte(ch),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "McpServer: handleRequest initialize" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    var server = McpServer{
        .allocator = allocator,
        .reactor = &reactor,
    };

    const req = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}";
    const resp = try server.handleRequest(req);
    defer allocator.free(resp);

    try testing.expect(std.mem.indexOf(u8, resp, "protocolVersion") != null);
    try testing.expect(std.mem.indexOf(u8, resp, "2024-11-05") != null);
}

test "McpServer: handleRequest tools/list" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    var server = McpServer{
        .allocator = allocator,
        .reactor = &reactor,
    };

    const req = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}";
    const resp = try server.handleRequest(req);
    defer allocator.free(resp);

    try testing.expect(std.mem.indexOf(u8, resp, "coral_query") != null);
    try testing.expect(std.mem.indexOf(u8, resp, "coral_insert_node") != null);
    try testing.expect(std.mem.indexOf(u8, resp, "coral_explain") != null);
}

test "McpServer: tools/call coral_query routes to L5" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    var server = McpServer{
        .allocator = allocator,
        .reactor = &reactor,
    };

    const req =
        \\{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"coral_query","arguments":{"query":"hello world"}}}
    ;
    const resp = try server.handleRequest(req);
    defer allocator.free(resp);

    try testing.expect(std.mem.indexOf(u8, resp, "result") != null);
    try testing.expect(std.mem.indexOf(u8, resp, "tier") != null);
}

test "McpServer: unknown method returns error -32601" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    var server = McpServer{
        .allocator = allocator,
        .reactor = &reactor,
    };

    const req = "{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"no_such_method\"}";
    const resp = try server.handleRequest(req);
    defer allocator.free(resp);

    try testing.expect(std.mem.indexOf(u8, resp, "-32601") != null);
}

test "McpServer: missing required field returns error" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try Library.init(allocator, .mem, "");
    defer lib.deinit();
    try lib.initSchema();

    var reactor = QueueReactor.init(allocator, lib, 10);
    defer reactor.deinit();

    var server = McpServer{
        .allocator = allocator,
        .reactor = &reactor,
    };

    const req = "{\"jsonrpc\":\"2.0\",\"id\":10,\"method\":\"tools/call\",\"params\":{\"name\":\"coral_query\",\"arguments\":{}}}";
    const resp = try server.handleRequest(req);
    defer allocator.free(resp);
    // Should return an error (missing required "query" field)
    try testing.expect(std.mem.indexOf(u8, resp, "error") != null);
}
