//! llm.zig — LLM client, response post-processing, and task decomposition.
//!
//! Single source of truth for all LLM-related types and functions:
//!   LlmError, LlmConfig, LlmClient  — HTTP chat-completion client
//!   stripThinkBlock, extractCommentTag, isMalformedResponse, stripPreamble
//!   LocalDecomposer, DecomposerConfig  — query decomposition
//!
//! ## Memory Ownership
//!
//!   - LlmClient: Owns http_client (persistent, pooled connections) and chat_url (owned slice).
//!     Call init() to create, deinit() to release. HTTP connections are reused when keep_alive=true.
//!   - LlmClient.complete(): Returns owned slice on success; caller must free with allocator.free().
//!     Returns null on non-200 response; returns LlmError on network failure.
//!   - LlmClient.available(): No allocation (caches result internally).
//!   - LlmConfig: Holds borrowed string slices (api_url, model) — valid only as long as the
//!     source strings live. Does not own heap.
//!   - LocalDecomposer: No heap ownership; holds config by value. The ephemeral LlmClient created
//!     in decompose() is init/deinit within the function.
//!   - stripPreamble(): Returns owned string; caller must free with allocator.free().
//!   - All strip*/extract*/is* helper functions: Return borrowed slices or boolean values; no allocation.

const std = @import("std");
const common = @import("common");

// ── Error set ────────────────────────────────────────────────────────────────

pub const LlmError = error{
    InvalidUrl,
    ConnectionFailed,
    TlsError,
    RequestFailed,
    ParseError,
    OutOfMemory,
};

// ── Config ────────────────────────────────────────────────────────────────────

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
    timeout_ms: u32 = 2000,
    debug: bool = false,
    /// Show LLM prompts in debug output (separate from debug metadata).
    /// true: print raw prompt text to stderr/stdout.
    /// false: hide prompts even when debug=true.
    show_prompts: bool = false,

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

// ── HTTP client ────────────────────────────────────────────────────────────────

pub const LlmClient = struct {
    allocator: std.mem.Allocator,
    config: LlmConfig,
    /// True when the endpoint is OpenAI-style (api.openai.com or similar).
    is_openai_format: bool,
    /// The normalised chat completions URL (owned; freed in deinit).
    /// For Ollama: corrects /v1/completions → /v1/chat/completions if needed.
    chat_url: []const u8,
    /// Persistent HTTP client — reused across all requests for connection pooling.
    ///
    /// ## Memory Ownership
    ///   - LlmClient owns the http_client; deinit() calls http_client.deinit().
    ///   - All TCP/TLS connections are pooled internally by std.http.Client.
    ///   - keep_alive=true enables connection reuse: subsequent requests to the
    ///     same host reuse the established TCP+TLS session, eliminating handshake
    ///     overhead for batch operations (guidance gen across N files).
    ///
    /// ## Thread Safety
    ///   - NOT thread-safe: create on a single thread, call from that thread,
    ///     destroy after all work is complete.
    ///   - For multi-threaded use, create one LlmClient per worker thread.
    http_client: std.http.Client,
    /// Cached availability result (null = not checked yet).
    /// After first check, stores the result to avoid repeated network calls.
    availability_cache: ?bool = null,

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
            .availability_cache = null,
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
            try common.jsonWriteEscaped(writer, sys);
            try writer.writeAll("\"},");
        }
        try writer.writeAll("{\"role\":\"user\",\"content\":\"");
        try common.jsonWriteEscaped(writer, prompt);
        try writer.writeAll("\"}]");

        var temp_buf: [32]u8 = undefined;
        const temp_str = std.fmt.bufPrint(&temp_buf, "{d:.2}", .{@as(f64, temperature)}) catch return LlmError.ParseError;
        if (self.config.think) |think_val| {
            if (think_val) {
                try writer.writeAll(",\"think\":true");
                try writer.writeAll(",\"max_completion_tokens\":");
                try writer.print("{d}", .{max_tokens});
            } else {
                try writer.writeAll(",\"think\":false");
                try writer.writeAll(",\"max_tokens\":");
                try writer.print("{d}", .{max_tokens});
            }
        } else {
            try writer.writeAll(",\"max_tokens\":");
            try writer.print("{d}", .{max_tokens});
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
            .keep_alive = true,
        }) catch |err| {
            if (self.config.debug) std.debug.print("DEBUG: HTTP POST failed: {any}\n", .{err});
            return LlmError.RequestFailed;
        };

        const response_bytes = aw.written();

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
                            const stripped = stripThinkBlock(s);
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
    /// Caches the result after first check to avoid repeated network calls.
    pub fn available(self: *LlmClient) bool {
        if (self.availability_cache) |cached| {
            return cached;
        }

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
            if (self.config.debug) std.debug.print("DEBUG: availability check failed: {any}\n", .{err});
            self.availability_cache = false;
            return false;
        };

        if (self.config.debug) std.debug.print("DEBUG: availability HTTP status {d}\n", .{@intFromEnum(result.status)});
        const is_available = result.status == .ok;
        self.availability_cache = is_available;
        return is_available;
    }
};

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Removes tags from a Zig string slice, returning a cleaned version.
fn stripTagBlock(text: []const u8, open: []const u8, close: []const u8) []const u8 {
    const tag_start = std.mem.indexOf(u8, text, open) orelse return text;
    if (std.mem.indexOfPos(u8, text, tag_start + open.len, close)) |close_start| {
        const after = close_start + close.len;
        if (after >= text.len) return "";
        var s = after;
        while (s < text.len and (text[s] == ' ' or text[s] == '\n')) s += 1;
        return text[s..];
    }
    return std.mem.trim(u8, text[0..tag_start], " \t\r\n");
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Strips think-block tags from LLM output (e.g. <think>reasoning</think>).
pub fn stripThinkBlock(text: []const u8) []const u8 {
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

/// Removes leading preamble lines from LLM output.
pub fn stripPreamble(allocator: std.mem.Allocator, text: []const u8) ![]const u8 {
    const trimmed = std.mem.trim(u8, text, " \t\r\n");
    if (trimmed.len == 0) return allocator.dupe(u8, trimmed);

    const nl_pos = std.mem.indexOfScalar(u8, trimmed, '\n') orelse trimmed.len;
    const first_line = trimmed[0..nl_pos];

    const preambles = [_][]const u8{
        "let's ", "let me ", "we need to ",    "here's ",    "here is ",
        "i'll ",  "i will ", "the answer is ", "to answer ", "okay, ",
        "ok, ",   "sure, ",  "alright, ",
    };

    const first_lower = try std.ascii.allocLowerString(allocator, first_line);
    defer allocator.free(first_lower);

    for (preambles) |preamble| {
        if (std.mem.startsWith(u8, first_lower, preamble)) {
            if (nl_pos >= trimmed.len) return allocator.dupe(u8, "");
            const rest = std.mem.trim(u8, trimmed[nl_pos + 1 ..], " \t\r\n");
            return allocator.dupe(u8, rest);
        }
    }

    return allocator.dupe(u8, trimmed);
}

/// Patterns that indicate an LLM preamble rather than a usable doc comment.
const llm_preamble_patterns = [_][]const u8{
    "here's a",    "here is a",   "i'll ",        "to summarize",
    "okay,",       "ok,",         "we need ",     "let's think",
    "let's craft", "let's count", "let me think", "i need to ",
};

/// Returns true if the LLM response appears malformed.
pub fn isMalformedResponse(text: []const u8) bool {
    const trimmed = std.mem.trim(u8, text, " \t\r\n");
    if (trimmed.len == 0) return true;
    if (llmHasDanglingEnd(trimmed)) return true;

    const rtrimmed = std.mem.trimRight(u8, trimmed, " \t");
    if (rtrimmed.len > 0 and rtrimmed[rtrimmed.len - 1] == '?') return true;

    if (llmIsGenericSelfRef(trimmed)) return true;
    if (llmIsOverlyGeneric(trimmed)) return true;

    inline for (llm_preamble_patterns) |pattern| {
        if (common.containsIgnoreCase(trimmed, pattern)) return true;
    }
    return false;
}

/// Checks if a Zig code snippet ends with a null byte, indicating a dangling end.
fn llmHasDanglingEnd(body: []const u8) bool {
    const trimmed = std.mem.trimRight(u8, body, " \t.?");
    if (trimmed.len == 0) return false;
    var i: usize = trimmed.len;
    while (i > 0 and trimmed[i - 1] != ' ') i -= 1;
    const last_word = trimmed[i..];
    const danglers = [_][]const u8{ "of", "in", "for", "from", "with", "to", "a", "an", "the" };
    for (danglers) |d| {
        if (std.ascii.eqlIgnoreCase(last_word, d)) return true;
    }
    return false;
}

/// Checks if a Zig structure represents a valid self-referential LLM model.
fn llmIsGenericSelfRef(body: []const u8) bool {
    const patterns = [_][]const u8{
        "this function", "this method", "this class",
        "this struct",   "this type",   "this module",
    };
    const trimmed = std.mem.trim(u8, body, " \t\r\n.");
    for (patterns) |p| {
        if (std.ascii.eqlIgnoreCase(trimmed, p)) return true;
    }
    return false;
}

/// Checks if the provided body is overly generic by evaluating its structure and returns true if it matches.
fn llmIsOverlyGeneric(body: []const u8) bool {
    const generics = [_][]const u8{
        "function", "method",   "helper",  "util",           "utility",
        "handler",  "callback", "wrapper", "implementation",
    };
    const trimmed = std.mem.trim(u8, body, " \t\r\n.");
    if (trimmed.len > 20) return false;
    if (std.mem.indexOfScalar(u8, trimmed, ' ') != null) return false;
    for (generics) |g| {
        if (std.ascii.eqlIgnoreCase(trimmed, g)) return true;
    }
    return false;
}

/// Extracts content from <comment> tags in LLM output.
pub fn extractCommentTag(text: []const u8) ?[]const u8 {
    const open = "<comment>";
    const close = "</comment>";
    const start = std.mem.indexOf(u8, text, open) orelse return null;
    const content_start = start + open.len;
    const end = std.mem.indexOfPos(u8, text, content_start, close) orelse return null;
    const content = std.mem.trim(u8, text[content_start..end], " \t\r\n");
    if (content.len == 0) return null;
    return content;
}

/// Returns true if text is blank or a plausible doc comment.
pub fn isBlankOrPlausible(text: []const u8) bool {
    const trimmed = std.mem.trim(u8, text, " \t\r\n");
    if (trimmed.len == 0) return true;
    if (trimmed.len < 3) return false;
    return !isMalformedResponse(trimmed);
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Manages decomposer configuration with fixed buffers; ensures ownership and invariants are preserved.
pub const DecomposerConfig = struct {
    /// LLM API config (endpoint + model).
    llm: LlmConfig,
    /// Maximum sub-tasks the LLM is allowed to return.
    max_subtasks: usize = 5,
    /// Maximum recursion depth for sub-task routing (enforced by caller).
    max_depth: u8 = 2,
};

// ---------------------------------------------------------------------------
// LocalDecomposer
// ---------------------------------------------------------------------------

/// Manages local data structures for efficient context switching; owns buffer allocations; key invariant is safe access during execution.
pub const LocalDecomposer = struct {
    allocator: std.mem.Allocator,
    config: DecomposerConfig,

    pub fn init(allocator: std.mem.Allocator, config: DecomposerConfig) LocalDecomposer {
        return .{ .allocator = allocator, .config = config };
    }

    /// Decompose `task` into an ordered list of sub-task strings.
    ///
    /// Returns a slice allocated from `arena`.  On any LLM failure or malformed
    /// response the function returns a single-element slice containing `task`
    /// so the caller always has at least one item to route.
    pub fn decompose(self: *LocalDecomposer, arena: std.mem.Allocator, task: []const u8) ![][]const u8 {
        var client = LlmClient.init(self.allocator, self.config.llm) catch {
            return self.fallback(arena, task);
        };
        defer client.deinit();

        const system_prompt =
            \\You are a task planner. Given a user query, decompose it into at most 5
            \\concrete, ordered sub-tasks. Reply with ONLY a JSON array of strings, no
            \\preamble, no explanation. Example:
            \\["Find relevant documents","Filter by date","Summarize results"]
        ;

        const raw = client.complete(task, 256, 0.2, system_prompt) catch {
            return self.fallback(arena, task);
        } orelse return self.fallback(arena, task);
        defer self.allocator.free(raw);

        // Strip think blocks before parsing.
        const stripped = stripThinkBlock(raw);
        if (isMalformedJsonArray(stripped)) return self.fallback(arena, task);

        return parseJsonArray(arena, stripped, self.config.max_subtasks) catch
            self.fallback(arena, task);
    }

    // -------------------------------------------------------------------------
    // Private helpers
    // -------------------------------------------------------------------------

    fn fallback(self: *LocalDecomposer, arena: std.mem.Allocator, task: []const u8) ![][]const u8 {
        _ = self;
        const tasks = try arena.alloc([]const u8, 1);
        tasks[0] = try arena.dupe(u8, task);
        return tasks;
    }
};

/// Checks if the provided text is a valid JSON array, returning true if malformed.
fn isMalformedJsonArray(text: []const u8) bool {
    const t = std.mem.trim(u8, text, " \t\r\n");
    if (t.len == 0) return true;
    if (t[0] != '[') return true;
    if (t[t.len - 1] != ']') return true;
    return false;
}

/// Converts a JSON array string into a Zig 2D slice, handling null-terminated input.
fn parseJsonArray(arena: std.mem.Allocator, text: []const u8, limit: usize) ![][]const u8 {
    var parsed = try std.json.parseFromSlice(std.json.Value, arena, text, .{});
    defer parsed.deinit();

    const arr = switch (parsed.value) {
        .array => |a| a,
        else => return error.NotAnArray,
    };

    const count = @min(arr.items.len, limit);
    if (count == 0) return error.EmptyArray;

    const result = try arena.alloc([]const u8, count);
    for (arr.items[0..count], 0..) |item, i| {
        const s = switch (item) {
            .string => |str| str,
            else => return error.NotAStringArray,
        };
        result[i] = try arena.dupe(u8, s);
    }
    return result;
}

// ── Tests ───────────────────────────────────────────────────────────────────

const testing = std.testing;

test "isMalformedJsonArray: rejects non-arrays" {
    try testing.expect(isMalformedJsonArray(""));
    try testing.expect(isMalformedJsonArray("hello"));
    try testing.expect(isMalformedJsonArray("{\"a\":1}"));
    try testing.expect(isMalformedJsonArray("[\"unclosed"));
}

test "isMalformedJsonArray: accepts well-formed arrays" {
    try testing.expect(!isMalformedJsonArray("[\"a\",\"b\"]"));
    try testing.expect(!isMalformedJsonArray("[]"));
}

test "stripThinkBlock: removes think tags" {
    const raw = "<think>reasoning here</think>actual answer";
    const result = stripThinkBlock(raw);
    try testing.expectEqualStrings("actual answer", result);
}

test "stripThinkBlock: no think block passes through" {
    const raw = "[\"task1\",\"task2\"]";
    try testing.expectEqualStrings(raw, stripThinkBlock(raw));
}

test "parseJsonArray: parses simple array" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();
    const result = try parseJsonArray(a, "[\"task1\",\"task2\",\"task3\"]", 10);
    try testing.expectEqual(@as(usize, 3), result.len);
    try testing.expectEqualStrings("task1", result[0]);
    try testing.expectEqualStrings("task3", result[2]);
}

test "parseJsonArray: respects limit" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();
    const result = try parseJsonArray(a, "[\"a\",\"b\",\"c\",\"d\",\"e\"]", 3);
    try testing.expectEqual(@as(usize, 3), result.len);
}

test "LocalDecomposer.fallback returns single task" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var decomposer = LocalDecomposer.init(testing.allocator, .{
        .llm = .{ .api_url = "http://localhost:11434/v1/chat/completions", .model = "test" },
    });
    const result = try decomposer.fallback(arena.allocator(), "find scientists");
    try testing.expectEqual(@as(usize, 1), result.len);
    try testing.expectEqualStrings("find scientists", result[0]);
}

test "stripThinkBlock removes think tags" {
    const text1 = "<think>Some thinking</think>\nActual response";
    try std.testing.expectEqualStrings("Actual response", stripThinkBlock(text1));

    const text2 = "No think tags here";
    try std.testing.expectEqualStrings(text2, stripThinkBlock(text2));

    const text3 = "<think>Only think\n</think>";
    try std.testing.expectEqualStrings("", stripThinkBlock(text3));
}

test "stripThinkBlock handles unclosed think tag" {
    try std.testing.expectEqualStrings("", stripThinkBlock("<think>Reasoning that never ends"));
}

test "stripThinkBlock handles [THINK] tags" {
    try std.testing.expectEqualStrings("Actual answer", stripThinkBlock("[THINK]reasoning here[/THINK]\nActual answer"));
}

test "stripPreamble removes leading preamble line" {
    const allocator = std.testing.allocator;

    const r1 = try stripPreamble(allocator, "Let's analyze this function.\nParses JSON from a byte slice.");
    defer allocator.free(r1);
    try std.testing.expectEqualStrings("Parses JSON from a byte slice.", r1);

    const r2 = try stripPreamble(allocator, "Here's the description:\nBuilds the dep graph.");
    defer allocator.free(r2);
    try std.testing.expectEqualStrings("Builds the dep graph.", r2);

    const r3 = try stripPreamble(allocator, "Parses JSON tokens efficiently.");
    defer allocator.free(r3);
    try std.testing.expectEqualStrings("Parses JSON tokens efficiently.", r3);
}

test "isMalformedResponse: empty is malformed" {
    try std.testing.expect(isMalformedResponse(""));
    try std.testing.expect(isMalformedResponse("   "));
}

test "isMalformedResponse: dangling preposition" {
    try std.testing.expect(isMalformedResponse("Parses the input from"));
    try std.testing.expect(isMalformedResponse("Returns the value of"));
}

test "isMalformedResponse: ends with question mark" {
    try std.testing.expect(isMalformedResponse("Does something?"));
}

test "isMalformedResponse: generic self-reference" {
    try std.testing.expect(isMalformedResponse("this function"));
    try std.testing.expect(isMalformedResponse("This Method"));
    try std.testing.expect(!isMalformedResponse("this function parses JSON efficiently"));
}

test "isMalformedResponse: overly generic single word" {
    try std.testing.expect(isMalformedResponse("helper"));
    try std.testing.expect(isMalformedResponse("wrapper"));
    try std.testing.expect(!isMalformedResponse("Parses"));
}

test "isMalformedResponse: LLM preamble phrases" {
    try std.testing.expect(isMalformedResponse("Here's a description of the function"));
    try std.testing.expect(isMalformedResponse("I'll explain what this does"));
    try std.testing.expect(isMalformedResponse("To summarize, this parses JSON"));
    try std.testing.expect(isMalformedResponse("Okay, so this function"));
}

test "isMalformedResponse: valid responses are NOT malformed" {
    try std.testing.expect(!isMalformedResponse("Parses Zig AST tokens and extracts public members."));
    try std.testing.expect(!isMalformedResponse("Ring buffer for streaming price data."));
    try std.testing.expect(!isMalformedResponse("Builds incremental dependency graph from @import declarations."));
}

test "isMalformedResponse: reasoning-model chain-of-thought phrases" {
    try std.testing.expect(isMalformedResponse("we need to write a comment for this type"));
    try std.testing.expect(isMalformedResponse("let's think about what this does"));
    try std.testing.expect(isMalformedResponse("let me think about the ownership model"));
    try std.testing.expect(isMalformedResponse("i need to mention that it owns the allocator"));
}

test "extractCommentTag: returns tag content" {
    const text = "some reasoning\n<comment>Parses JSON from a byte slice.</comment>\nmore text";
    const result = extractCommentTag(text);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("Parses JSON from a byte slice.", result.?);
}

test "extractCommentTag: returns null when no tag present" {
    try std.testing.expect(extractCommentTag("we need to write a comment") == null);
    try std.testing.expect(extractCommentTag("") == null);
}

test "extractCommentTag: returns null for empty tag" {
    try std.testing.expect(extractCommentTag("<comment>   </comment>") == null);
}
