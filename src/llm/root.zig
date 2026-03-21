//! llm — General-purpose LLM inference client.
//!
//! Provides:
//!   LlmError    — error set for all LLM operations
//!   LlmConfig   — endpoint, model, timeout, think-mode settings
//!   LlmClient   — HTTP chat-completion client (OpenAI + Ollama)
//!
//! Response post-processing (stripThinkBlock, isMalformedResponse, etc.) lives
//! in src/common/llm.zig alongside the string utilities they depend on.

const std = @import("std");

// ── Error set ────────────────────────────────────────────────────────────────

pub const LlmError = error{
    InvalidUrl,
    ConnectionFailed,
    TlsError,
    RequestFailed,
    ParseError,
    OutOfMemory,
};

// ── Config ───────────────────────────────────────────────────────────────────

pub const LlmConfig = struct {
    api_url: []const u8,
    model: []const u8,
    /// Controls the Ollama `think` parameter:
    ///   null  — don't send the `think` param at all (standard models).
    ///   true  — send `"think":true`; thinking is explicitly enabled.
    ///           Used when the caller explicitly selects the "thinking" model slot.
    ///   false — send `"think":false`; suppress thinking on a thinking-capable
    ///           model that was selected via the "default" or "fast" slot because
    ///           that slot happens to point to the same model as "thinking".
    think: ?bool = null,
    timeout_ms: u32 = 10000,
    debug: bool = false,

    /// Returns true when thinking is explicitly enabled for this config.
    pub fn isThinkingModel(self: LlmConfig) bool {
        return self.think == true;
    }

    /// Extract the model name from a model reference like "provider:model:name".
    /// Returns the part after the first colon, or the whole string if no colon.
    pub fn extractModelName(model_ref: []const u8) []const u8 {
        if (std.mem.indexOfScalar(u8, model_ref, ':')) |colon_pos| {
            return model_ref[colon_pos + 1 ..];
        }
        return model_ref;
    }

    /// Check if an endpoint is an Ollama-native endpoint (uses /api/chat).
    pub fn isOllamaEndpoint(api_url: []const u8) bool {
        return std.mem.indexOf(u8, api_url, "/api/chat") != null;
    }
};

// ── Internal helpers ─────────────────────────────────────────────────────────

fn writeEscapedString(writer: anytype, s: []const u8) !void {
    for (s) |c| {
        switch (c) {
            '"' => try writer.writeAll("\\\""),
            '\\' => try writer.writeAll("\\\\"),
            '\n' => try writer.writeAll("\\n"),
            '\r' => try writer.writeAll("\\r"),
            '\t' => try writer.writeAll("\\t"),
            else => try writer.writeByte(c),
        }
    }
}

// ── HTTP client ───────────────────────────────────────────────────────────────

pub const LlmClient = struct {
    allocator: std.mem.Allocator,
    config: LlmConfig,
    /// True when the endpoint is OpenAI-style (api.openai.com or similar).
    is_openai_format: bool,
    /// The normalised chat completions URL (owned; freed in deinit).
    /// For Ollama: corrects /v1/completions → /v1/chat/completions if needed.
    chat_url: []const u8,
    http_client: std.http.Client,

    pub fn init(allocator: std.mem.Allocator, config: LlmConfig) !LlmClient {
        const is_openai = std.mem.indexOf(u8, config.api_url, "api.openai.com") != null;

        // Normalise legacy /v1/completions → /v1/chat/completions for Ollama endpoints.
        var chat_url: []const u8 = undefined;
        if (is_openai) {
            chat_url = try allocator.dupe(u8, config.api_url);
        } else {
            const is_v1_completions = std.mem.indexOf(u8, config.api_url, "/v1/completions") != null;
            if (is_v1_completions) {
                chat_url = try std.mem.replaceOwned(u8, allocator, config.api_url, "/v1/completions", "/v1/chat/completions");
            } else {
                chat_url = try allocator.dupe(u8, config.api_url);
            }
        }

        const http_client = std.http.Client{ .allocator = allocator };
        return .{
            .allocator = allocator,
            .config = config,
            .is_openai_format = is_openai,
            .chat_url = chat_url,
            .http_client = http_client,
        };
    }

    pub fn deinit(self: *LlmClient) void {
        self.allocator.free(self.chat_url);
        self.http_client.deinit();
    }

    pub fn complete(self: *LlmClient, prompt: []const u8, max_tokens: usize, temperature: f32, system: ?[]const u8) LlmError!?[]const u8 {
        var body: std.ArrayList(u8) = .{};
        defer body.deinit(self.allocator);
        const writer = body.writer(self.allocator);

        // Extract model name from provider:model format (e.g., "local:code:latest" -> "code:latest")
        const model_name = LlmConfig.extractModelName(self.config.model);
        try writer.writeAll("{\"model\":\"");
        try writer.writeAll(model_name);
        try writer.writeAll("\",\"messages\":[");
        if (system) |sys| {
            try writer.writeAll("{\"role\":\"system\",\"content\":\"");
            try writeEscapedString(writer, sys);
            try writer.writeAll("\"},");
        }
        try writer.writeAll("{\"role\":\"user\",\"content\":\"");
        try writeEscapedString(writer, prompt);
        try writer.writeAll("\"}]");

        var temp_buf: [32]u8 = undefined;
        const temp_str = std.fmt.bufPrint(&temp_buf, "{d:.2}", .{@as(f64, temperature)}) catch return LlmError.ParseError;
        if (self.config.think) |think_val| {
            if (think_val) {
                try writer.writeAll(",\"think\":true");
                try writer.writeAll(",\"max_completion_tokens\":");
                try writer.print("{}", .{max_tokens});
            } else {
                try writer.writeAll(",\"think\":false");
                try writer.writeAll(",\"max_tokens\":");
                try writer.print("{}", .{max_tokens});
            }
        } else {
            try writer.writeAll(",\"max_tokens\":");
            try writer.print("{}", .{max_tokens});
        }
        try writer.writeAll(",\"temperature\":");
        try writer.writeAll(temp_str);
        try writer.writeAll(",\"stream\":false}");

        const url = if (self.is_openai_format) self.config.api_url else self.chat_url;
        return self.postJson(url, body.items);
    }

    /// POST `json_body` to `url` and return the extracted response text.
    fn postJson(self: *LlmClient, url: []const u8, json_body: []const u8) LlmError!?[]const u8 {
        if (self.config.debug) std.debug.print("DEBUG: POST {s} ({d} bytes)\n", .{ url, json_body.len });

        var aw: std.Io.Writer.Allocating = .init(self.allocator);
        defer aw.deinit();

        const result = self.http_client.fetch(.{
            .method = .POST,
            .location = .{ .url = url },
            .extra_headers = &[_]std.http.Header{
                .{ .name = "Content-Type", .value = "application/json" },
            },
            .payload = json_body,
            .response_writer = &aw.writer,
            .keep_alive = false,
        }) catch |err| {
            if (self.config.debug) std.debug.print("DEBUG: HTTP POST failed: {}\n", .{err});
            return LlmError.RequestFailed;
        };

        const response_bytes = aw.writer.buffer[0..aw.writer.end];

        if (self.config.debug) {
            std.debug.print("DEBUG: HTTP status {d}, response ({d} bytes): {s}\n", .{
                @intFromEnum(result.status),
                response_bytes.len,
                response_bytes[0..@min(300, response_bytes.len)],
            });
        }

        if (result.status != .ok) return null;
        return self.extractResponseText(response_bytes);
    }

    fn extractResponseText(self: *LlmClient, resp: []const u8) ?[]const u8 {
        var parsed = std.json.parseFromSlice(std.json.Value, self.allocator, resp, .{}) catch return null;
        defer parsed.deinit();

        const root = parsed.value;

        if (root.object.get("choices")) |choices| {
            if (choices.array.items.len > 0) {
                const choice = choices.array.items[0];
                if (choice.object.get("text")) |text| {
                    return self.allocator.dupe(u8, text.string) catch null;
                }
                if (choice.object.get("message")) |msg| {
                    if (msg.object.get("content")) |content| {
                        const s = switch (content) {
                            .string => |sv| sv,
                            else => "",
                        };
                        if (s.len > 0) {
                            // Strip think blocks inline — avoids str_mod dependency.
                            const stripped = stripThinkBlockInline(s);
                            if (stripped.len > 0) return self.allocator.dupe(u8, stripped) catch null;
                        }
                    }
                    for ([_][]const u8{ "reasoning_content", "reasoning", "thinking" }) |field| {
                        if (msg.object.get(field)) |val| {
                            const s = switch (val) {
                                .string => |sv| sv,
                                else => "",
                            };
                            if (s.len > 0) return self.allocator.dupe(u8, s) catch null;
                        }
                    }
                }
            }
        }

        if (root.object.get("message")) |msg| {
            const content_str: ?[]const u8 = blk: {
                if (msg.object.get("content")) |c| {
                    const s = switch (c) {
                        .string => |sv| sv,
                        else => break :blk null,
                    };
                    if (s.len > 0) break :blk s;
                }
                if (msg.object.get("thinking")) |t| {
                    const s = switch (t) {
                        .string => |sv| sv,
                        else => break :blk null,
                    };
                    if (s.len > 0) break :blk s;
                }
                break :blk null;
            };
            if (content_str) |s| return self.allocator.dupe(u8, s) catch null;
        }

        if (root.object.get("response")) |resp_val| {
            return self.allocator.dupe(u8, resp_val.string) catch null;
        }

        return null;
    }

    /// Check whether the LLM endpoint is reachable.
    ///   OpenAI  → GET <base>/v1/models
    ///   Ollama  → GET <scheme>://<host:port>/api/tags
    pub fn available(self: *LlmClient) bool {
        const check_url = if (self.is_openai_format) blk: {
            if (std.mem.indexOf(u8, self.config.api_url, "/v1/")) |pos| {
                break :blk std.fmt.allocPrint(self.allocator, "{s}/v1/models", .{self.config.api_url[0..pos]}) catch return false;
            }
            break :blk self.allocator.dupe(u8, self.config.api_url) catch return false;
        } else blk: {
            const url = self.config.api_url;
            const scheme_end = std.mem.indexOf(u8, url, "://") orelse 0;
            const host_start = if (scheme_end > 0) scheme_end + 3 else 0;
            const path_start = std.mem.indexOfScalarPos(u8, url, host_start, '/') orelse url.len;
            break :blk std.fmt.allocPrint(self.allocator, "{s}/api/tags", .{url[0..path_start]}) catch return false;
        };
        defer self.allocator.free(check_url);

        if (self.config.debug) std.debug.print("DEBUG: availability check GET {s}\n", .{check_url});

        const result = self.http_client.fetch(.{
            .method = .GET,
            .location = .{ .url = check_url },
        }) catch |err| {
            if (self.config.debug) std.debug.print("DEBUG: availability check failed: {}\n", .{err});
            return false;
        };

        if (self.config.debug) std.debug.print("DEBUG: availability HTTP status {d}\n", .{@intFromEnum(result.status)});
        return result.status == .ok;
    }
};

/// Minimal think-block stripper used internally by extractResponseText.
/// The full public stripThinkBlock lives in src/common/llm.zig.
fn stripThinkBlockInline(text: []const u8) []const u8 {
    if (std.mem.indexOf(u8, text, "<think>")) |think_start| {
        const think_end = std.mem.indexOfPos(u8, text, think_start + 7, "</think>");
        if (think_end) |te| {
            const after = te + 8;
            if (after >= text.len) return "";
            var start = after;
            while (start < text.len and (text[start] == ' ' or text[start] == '\n')) start += 1;
            return text[start..];
        }
        return std.mem.trim(u8, text[0..think_start], " \t\r\n");
    }
    if (std.mem.indexOf(u8, text, "[THINK]")) |think_start| {
        const think_end = std.mem.indexOfPos(u8, text, think_start + 7, "[/THINK]");
        if (think_end) |te| {
            const after = te + 8;
            if (after >= text.len) return "";
            var start = after;
            while (start < text.len and (text[start] == ' ' or text[start] == '\n')) start += 1;
            return text[start..];
        }
        return std.mem.trim(u8, text[0..think_start], " \t\r\n");
    }
    return text;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

test "LlmClient init with v1/chat/completions URL uses it directly" {
    const allocator = std.testing.allocator;
    const config = LlmConfig{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "code",
        .debug = false,
    };
    var client = try LlmClient.init(allocator, config);
    defer client.deinit();
    try std.testing.expect(!client.is_openai_format);
    try std.testing.expectEqualStrings("http://localhost:11434/v1/chat/completions", client.chat_url);
}

test "LlmClient init with v1/completions URL normalises to v1/chat/completions" {
    const allocator = std.testing.allocator;
    const config = LlmConfig{
        .api_url = "http://localhost:11434/v1/completions",
        .model = "code",
        .debug = false,
    };
    var client = try LlmClient.init(allocator, config);
    defer client.deinit();
    try std.testing.expect(!client.is_openai_format);
    try std.testing.expectEqualStrings("http://localhost:11434/v1/chat/completions", client.chat_url);
}

test "LlmClient init with OpenAI API URL sets is_openai_format" {
    const allocator = std.testing.allocator;
    const config = LlmConfig{
        .api_url = "https://api.openai.com/v1/chat/completions",
        .model = "gpt-4",
        .debug = false,
    };
    var client = try LlmClient.init(allocator, config);
    defer client.deinit();
    try std.testing.expect(client.is_openai_format);
    try std.testing.expectEqualStrings("https://api.openai.com/v1/chat/completions", client.chat_url);
}

test "writeEscapedString escapes properly" {
    var buf: [256]u8 = undefined;
    var fbs = std.io.fixedBufferStream(&buf);
    const writer = fbs.writer();
    try writeEscapedString(writer, "Hello \"world\"\n");
    try std.testing.expectEqualStrings("Hello \\\"world\\\"\\n", fbs.getWritten());
}

test "writeEscapedString does not escape braces" {
    var buf: [256]u8 = undefined;
    var fbs = std.io.fixedBufferStream(&buf);
    const writer = fbs.writer();
    try writeEscapedString(writer, "{key: value}");
    try std.testing.expectEqualStrings("{key: value}", fbs.getWritten());
}
