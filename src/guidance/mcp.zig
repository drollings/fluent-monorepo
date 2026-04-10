//! mcp.zig — guidance MCP server (STDIO transport, JSON-RPC 2.0).
//!
//! Implements the Model Context Protocol server for guidance, allowing AI
//! agents to use guidance tools (explain, gen, check, status) via MCP.
//!
//! Transport: STDIO (stdin → request, stdout → response)
//! Protocol:  JSON-RPC 2.0
//!
//! Tools exposed:
//!   explain — AST-guided code search (maps to cmdExplain)
//!   gen     — regenerate guidance JSON + .guidance.db
//!   check   — run the full RALPH loop (test → lint → fmt → guidance)
//!   status  — report generation status

const std = @import("std");
const common = @import("common");
const query_engine_mod = @import("query_engine.zig");
const sync_engine_mod = @import("sync_engine.zig");

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 request/response types
// ---------------------------------------------------------------------------

/// Maximum size of a single JSON-RPC message (4 MB).
const MAX_MSG_SIZE = 4 * 1024 * 1024;

const Request = struct {
    id: ?std.json.Value = null,
    method: []const u8,
    params: ?std.json.Value = null,
};

/// Writes a JSON-formatted string to a writer, converting input bytes into a JSON-encoded slice.
fn writeJsonString(writer: anytype, s: []const u8) !void {
    try writer.writeByte('"');
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
    try writer.writeByte('"');
}

/// Writes Zig code content to a writer using an ID and JSON value.
fn writeResult(writer: anytype, id: ?std.json.Value, content: []const u8) !void {
    try writer.print(
        \\{{"jsonrpc":"2.0","id":{s},"result":{{"content":[{{"type":"text","text":
    , .{if (id) |v| blk: {
        var buf: [64]u8 = undefined;
        break :blk std.fmt.bufPrint(&buf, "{}", .{v}) catch "null";
    } else "null"});
    try writeJsonString(writer, content);
    try writer.writeAll("}}]}}\n");
}

/// Handles error writing with id, code, and message parameters.
fn writeError(writer: anytype, id: ?std.json.Value, code: i32, msg: []const u8) !void {
    try writer.print(
        \\{{"jsonrpc":"2.0","id":{s},"error":{{"code":{d},"message":
    , .{
        if (id) |v| blk: {
            var buf: [64]u8 = undefined;
            break :blk std.fmt.bufPrint(&buf, "{}", .{v}) catch "null";
        } else "null",
        code,
    });
    try writeJsonString(writer, msg);
    try writer.writeAll("}}\n");
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

/// Initializes memory allocation with provided allocator and JSON ID, returning void.
fn handleInitialize(allocator: std.mem.Allocator, writer: anytype, id: ?std.json.Value) !void {
    _ = allocator;
    const resp =
        \\{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"guidance","version":"0.1.0"}}}
    ;
    _ = id;
    try writer.writeAll(resp);
    try writer.writeByte('\n');
}

/// Processes a JSON object to populate a tools list, returning an owned Zig array.
fn handleToolsList(writer: anytype, id: ?std.json.Value) !void {
    const tools =
        \\{"jsonrpc":"2.0","id":null,"result":{"tools":[
        \\  {"name":"explain","description":"AST-guided code search. Returns structural info about the query.","inputSchema":{"type":"object","required":["query"],"properties":{"query":{"type":"string"},"limit":{"type":"integer","default":10},"no_llm":{"type":"boolean","default":false}}}},
        \\  {"name":"gen","description":"Regenerate guidance JSON and .guidance.db for stale source files.","inputSchema":{"type":"object","properties":{"force":{"type":"boolean","default":false},"file":{"type":"string"}}}},
        \\  {"name":"check","description":"Run the full RALPH loop: test → lint → fmt → guidance gen.","inputSchema":{"type":"object","properties":{}}},
        \\  {"name":"status","description":"Report generation status: synced, stale, missing files.","inputSchema":{"type":"object","properties":{}}}
        \\]}}
    ;
    _ = id;
    try writer.writeAll(tools);
    try writer.writeByte('\n');
}

/// Processes a JSON tool call with allocator, writer, and parameters, returning a processed value.
fn handleToolCall(
    allocator: std.mem.Allocator,
    writer: anytype,
    id: ?std.json.Value,
    name: []const u8,
    params: ?std.json.Value,
) !void {
    // Capture all output to a buffer so we can return it as the tool result.
    var buf: std.ArrayList(u8) = .{};
    defer buf.deinit(allocator);

    if (std.mem.eql(u8, name, "explain")) {
        // Build argv for cmdExplain.
        var argv: std.ArrayList([]const u8) = .{};
        defer argv.deinit(allocator);

        if (params) |p| {
            if (p == .object) {
                if (p.object.get("query")) |q| {
                    if (q == .string) try argv.append(allocator, q.string);
                }
                if (p.object.get("no_llm")) |nl| {
                    if (nl == .bool and nl.bool) try argv.append(allocator, "--no-llm");
                }
                if (p.object.get("limit")) |lim| {
                    if (lim == .integer) {
                        const s = try std.fmt.allocPrint(allocator, "{d}", .{lim.integer});
                        defer allocator.free(s);
                        try argv.append(allocator, "--limit");
                        try argv.append(allocator, s);
                    }
                }
            }
        }

        // Redirect stdout to buf.
        // NOTE: We call cmdExplain with the real stdout; MCP clients see the
        // output inline. Full output capture requires pipe redirection which
        // adds significant complexity. For now, return a confirmation.
        query_engine_mod.cmdExplain(allocator, argv.items) catch |err| {
            try writeError(writer, id, -32000, @errorName(err));
            return;
        };
        try writeResult(writer, id, "explain completed — see stdout");
    } else if (std.mem.eql(u8, name, "gen")) {
        var argv: std.ArrayList([]const u8) = .{};
        defer argv.deinit(allocator);
        if (params) |p| {
            if (p == .object) {
                if (p.object.get("force")) |f| {
                    if (f == .bool and f.bool) try argv.append(allocator, "--force");
                }
                if (p.object.get("file")) |file| {
                    if (file == .string) {
                        try argv.append(allocator, "--file");
                        try argv.append(allocator, file.string);
                    }
                }
            }
        }
        sync_engine_mod.cmdGen(allocator, argv.items) catch |err| {
            try writeError(writer, id, -32000, @errorName(err));
            return;
        };
        try writeResult(writer, id, "gen completed");
    } else if (std.mem.eql(u8, name, "check")) {
        sync_engine_mod.cmdCheck(allocator, &.{}) catch |err| {
            try writeError(writer, id, -32000, @errorName(err));
            return;
        };
        try writeResult(writer, id, "check completed");
    } else if (std.mem.eql(u8, name, "status")) {
        sync_engine_mod.cmdStatus(allocator, &.{}) catch |err| {
            try writeError(writer, id, -32000, @errorName(err));
            return;
        };
        try writeResult(writer, id, "status completed — see stdout");
    } else {
        try writeError(writer, id, -32601, "Method not found");
    }
}

// ---------------------------------------------------------------------------
// Main serve loop
// ---------------------------------------------------------------------------

/// Transforms a Zig source code string into a memory-allocated slice for processing.
pub fn serve(allocator: std.mem.Allocator, args: []const []const u8) !void {
    _ = args;

    var rs: common.ReaderState = .{};
    rs.initStdin();
    const reader = rs.reader();

    var ws: common.WriterState = .{};
    ws.initStdout();
    const writer = ws.writer();

    while (true) {
        const raw_line = reader.takeDelimiterInclusive('\n') catch |err| {
            if (err == error.EndOfStream) break;
            return err;
        };

        const line = std.mem.trim(u8, raw_line, " \t\r\n");
        if (line.len == 0) continue;

        const parsed = std.json.parseFromSlice(std.json.Value, allocator, line, .{}) catch {
            try writeError(writer, null, -32700, "Parse error");
            try writer.flush();
            continue;
        };
        defer parsed.deinit();

        const root = parsed.value;
        if (root != .object) {
            try writeError(writer, null, -32600, "Invalid Request");
            try writer.flush();
            continue;
        }

        const id = root.object.get("id");
        const method_val = root.object.get("method") orelse {
            try writeError(writer, id, -32600, "Missing method");
            try writer.flush();
            continue;
        };
        if (method_val != .string) {
            try writeError(writer, id, -32600, "Method must be string");
            try writer.flush();
            continue;
        }
        const method = method_val.string;
        const params = root.object.get("params");

        if (std.mem.eql(u8, method, "initialize")) {
            try handleInitialize(allocator, writer, id);
        } else if (std.mem.eql(u8, method, "tools/list")) {
            try handleToolsList(writer, id);
        } else if (std.mem.eql(u8, method, "tools/call")) {
            const tool_name = blk: {
                if (params) |p| {
                    if (p == .object) {
                        if (p.object.get("name")) |n| {
                            if (n == .string) break :blk n.string;
                        }
                    }
                }
                try writeError(writer, id, -32602, "Missing tool name");
                try writer.flush();
                continue;
            };
            const tool_params = if (params) |p|
                if (p == .object) p.object.get("arguments") else null
            else
                null;
            try handleToolCall(allocator, writer, id, tool_name, tool_params);
        } else if (std.mem.eql(u8, method, "notifications/initialized")) {
            // Ignore — no response expected for notifications.
        } else {
            try writeError(writer, id, -32601, "Method not found");
        }

        try writer.flush();
    }
}
