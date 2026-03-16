const std = @import("std");
const args = @import("args.zig");

pub const CommonArgs = args.CommonArgs;
pub const parseCommonArgs = args.parseCommonArgs;

const io = @import("io.zig");
const source = @import("source.zig");

pub const WriterState = io.WriterState;
pub const ReaderState = io.ReaderState;
pub const makePathAbsolute = io.makePathAbsolute;
pub const readFileAlloc = io.readFileAlloc;
pub const readFileAllocErr = io.readFileAllocErr;
pub const resolvePath = io.resolvePath;
pub const DEFAULT_MAX_FILE_SIZE = io.DEFAULT_MAX_FILE_SIZE;

pub const NodeType = source.NodeType;
pub const DEFAULT_MAX_LINES = source.DEFAULT_MAX_LINES;
pub const extractExcerpt = source.extractExcerpt;
pub const extractSimpleExcerpt = source.extractSimpleExcerpt;

pub const LlmError = error{
    InvalidUrl,
    ConnectionFailed,
    TlsError,
    RequestFailed,
    ParseError,
    OutOfMemory,
};

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

/// Strip <think>...</think> or [THINK]...[/THINK] blocks from LLM response.
/// For unclosed tags, strips everything from the open tag to end-of-string.
pub fn stripThinkBlock(text: []const u8) []const u8 {
    // Handle <think> ... </think>
    if (std.mem.indexOf(u8, text, "<think>")) |think_start| {
        const think_end = std.mem.indexOfPos(u8, text, think_start + 7, "</think>");
        if (think_end) |te| {
            const after_think = te + 8;
            if (after_think >= text.len) return "";
            var start = after_think;
            while (start < text.len and (text[start] == ' ' or text[start] == '\n')) {
                start += 1;
            }
            return text[start..];
        } else {
            // Unclosed <think> tag — strip everything from it to end.
            return std.mem.trim(u8, text[0..think_start], " \t\r\n");
        }
    }

    // Handle [THINK] ... [/THINK] (alternative format)
    if (std.mem.indexOf(u8, text, "[THINK]")) |think_start| {
        const think_end = std.mem.indexOfPos(u8, text, think_start + 7, "[/THINK]");
        if (think_end) |te| {
            const after_think = te + 8;
            if (after_think >= text.len) return "";
            var start = after_think;
            while (start < text.len and (text[start] == ' ' or text[start] == '\n')) {
                start += 1;
            }
            return text[start..];
        } else {
            // Unclosed [THINK] tag — strip from it to end.
            return std.mem.trim(u8, text[0..think_start], " \t\r\n");
        }
    }

    return text;
}

/// Strip LLM reasoning preamble from the start of a response.
/// Preambles are phrases like "Let's", "Here's", "I'll", "To answer", etc.
/// Removes the first line if it matches one of these patterns.
pub fn stripPreamble(allocator: std.mem.Allocator, text: []const u8) ![]const u8 {
    const trimmed = std.mem.trim(u8, text, " \t\r\n");
    if (trimmed.len == 0) return allocator.dupe(u8, trimmed);

    // Find first newline to isolate the first line.
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
            // Skip this preamble line; return the rest.
            if (nl_pos >= trimmed.len) return allocator.dupe(u8, "");
            const rest = std.mem.trim(u8, trimmed[nl_pos + 1 ..], " \t\r\n");
            return allocator.dupe(u8, rest);
        }
    }

    return allocator.dupe(u8, trimmed);
}

// ---------------------------------------------------------------------------
// LLM output validation — write-time gate for comment fields
// ---------------------------------------------------------------------------

/// Return true when an LLM response is malformed or unusable as a comment.
/// Call this after stripThinkBlock / stripPreamble.  No allocations.
///
/// Patterns detected:
///   - Empty / whitespace only
///   - Truncated output (dangling preposition/article at end)
///   - Ends with "?" (uncertain / incomplete analysis)
///   - Generic self-referential filler ("this function", "this struct", …)
///   - Single overly-generic word ("helper", "wrapper", …)
///   - LLM preamble phrases present anywhere in the response
pub fn isMalformedResponse(text: []const u8) bool {
    const trimmed = std.mem.trim(u8, text, " \t\r\n");
    if (trimmed.len == 0) return true;

    if (llmHasDanglingEnd(trimmed)) return true;

    const rtrimmed = std.mem.trimRight(u8, trimmed, " \t");
    if (rtrimmed.len > 0 and rtrimmed[rtrimmed.len - 1] == '?') return true;

    if (llmIsGenericSelfRef(trimmed)) return true;
    if (llmIsOverlyGeneric(trimmed)) return true;

    if (llmContainsIgnoreCase(trimmed, "here's a")) return true;
    if (llmContainsIgnoreCase(trimmed, "here is a")) return true;
    if (llmContainsIgnoreCase(trimmed, "i'll ")) return true;
    if (llmContainsIgnoreCase(trimmed, "to summarize")) return true;
    if (llmContainsIgnoreCase(trimmed, "okay,")) return true;
    if (llmContainsIgnoreCase(trimmed, "ok,")) return true;

    // Reasoning-model chain-of-thought that survived stripPreamble (multi-line monologues).
    if (llmContainsIgnoreCase(trimmed, "we need ")) return true;
    if (llmContainsIgnoreCase(trimmed, "let's think")) return true;
    if (llmContainsIgnoreCase(trimmed, "let's craft")) return true;
    if (llmContainsIgnoreCase(trimmed, "let's count")) return true;
    if (llmContainsIgnoreCase(trimmed, "let me think")) return true;
    if (llmContainsIgnoreCase(trimmed, "i need to ")) return true;

    return false;
}

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

fn llmContainsIgnoreCase(haystack: []const u8, needle: []const u8) bool {
    if (needle.len == 0) return true;
    if (needle.len > haystack.len) return false;
    var i: usize = 0;
    while (i + needle.len <= haystack.len) : (i += 1) {
        if (std.ascii.eqlIgnoreCase(haystack[i .. i + needle.len], needle)) return true;
    }
    return false;
}

/// Extract the content of the first <comment>...</comment> tag in an LLM response.
/// Returns a slice into `text` (no allocation).  Returns null when no tag is found.
/// The model may emit any amount of chain-of-thought before or after the tag;
/// only the tag content is returned.
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

pub const LlmClient = struct {
    allocator: std.mem.Allocator,
    config: LlmConfig,
    http_client: std.http.Client,

    pub fn init(allocator: std.mem.Allocator, config: LlmConfig) !LlmClient {
        const http_client = std.http.Client{ .allocator = allocator };
        return .{
            .allocator = allocator,
            .config = config,
            .http_client = http_client,
        };
    }

    pub fn deinit(self: *LlmClient) void {
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
        // Write system message if provided (unconditionally — no /no_think injection;
        // thinking suppression is handled via the `think` param below).
        if (system) |sys| {
            try writer.writeAll("{\"role\":\"system\",\"content\":\"");
            try writeEscapedString(writer, sys);
            try writer.writeAll("\"},");
        }
        try writer.writeAll("{\"role\":\"user\",\"content\":\"");
        try writeEscapedString(writer, prompt);
        try writer.writeAll("\"}]");

        // think / max_tokens logic:
        //   think=true  → thinking explicitly enabled; send think:true and
        //                  max_completion_tokens (covers thinking tokens too).
        //   think=false → thinking-capable model used from a non-thinking slot;
        //                  send think:false to suppress, use max_tokens.
        //   think=null  → standard model; omit think param, use max_tokens.
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

        return self.postJson(self.config.api_url, body.items);
    }

    /// POST `json_body` to `url` and return the extracted response text.
    /// Uses Zig's native HTTP client; no subprocess or temp files.
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
                    // Check content first; strip <think>...</think> blocks (thinking models
                    // embed chain-of-thought there). If stripping leaves text, return it.
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
                    // Thinking models may route output to reasoning_content, reasoning, or
                    // thinking fields when content is empty. Return unconditionally; the
                    // caller (enhancer) will validate via extractCommentTag.
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
            // Prefer `content`; fall back to `thinking` for models that route
            // chain-of-thought output there (e.g. DeepSeek-R1 style Ollama models).
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
    /// Uses Zig's native HTTP client (GET request to the health-check endpoint).
    /// Returns true when the endpoint responds with HTTP 200.
    /// OpenAI-style: GET <base>/v1/models
    pub fn available(self: *LlmClient) bool {
        const check_url = blk: {
            const url = self.config.api_url;
            const scheme_end = std.mem.indexOf(u8, url, "://") orelse 0;
            const host_start = if (scheme_end > 0) scheme_end + 3 else 0;
            const path_start = std.mem.indexOfScalarPos(u8, url, host_start, '/') orelse url.len;
            break :blk std.fmt.allocPrint(self.allocator, "{s}/v1/models", .{url[0..path_start]}) catch return false;
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

test "LlmClient init with chat/completions URL" {
    const allocator = std.testing.allocator;
    const config = LlmConfig{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "code",
        .debug = false,
    };

    var client = try LlmClient.init(allocator, config);
    defer client.deinit();

    try std.testing.expectEqualStrings("http://localhost:11434/v1/chat/completions", client.config.api_url);
}

test "LlmClient init with OpenAI API URL" {
    const allocator = std.testing.allocator;
    const config = LlmConfig{
        .api_url = "https://api.openai.com/v1/chat/completions",
        .model = "gpt-4",
        .debug = false,
    };

    var client = try LlmClient.init(allocator, config);
    defer client.deinit();

    try std.testing.expectEqualStrings("https://api.openai.com/v1/chat/completions", client.config.api_url);
}

test "stripThinkBlock removes think tags" {
    const text1 = "<think>Some thinking</think>\nActual response";
    const result1 = stripThinkBlock(text1);
    try std.testing.expectEqualStrings("Actual response", result1);

    const text2 = "No think tags here";
    const result2 = stripThinkBlock(text2);
    try std.testing.expectEqualStrings(text2, result2);

    const text3 = "<think>Only think</think>";
    const result3 = stripThinkBlock(text3);
    try std.testing.expectEqualStrings("", result3);
}

test "stripThinkBlock handles unclosed think tag" {
    const text = "<think>Reasoning that never ends";
    const result = stripThinkBlock(text);
    // Should strip everything from <think> to end.
    try std.testing.expectEqualStrings("", result);
}

test "stripThinkBlock handles [THINK] tags" {
    const text = "[THINK]reasoning here[/THINK]\nActual answer";
    const result = stripThinkBlock(text);
    try std.testing.expectEqualStrings("Actual answer", result);
}

test "stripPreamble removes leading preamble line" {
    const allocator = std.testing.allocator;

    const text1 = "Let's analyze this function.\nParses JSON from a byte slice.";
    const r1 = try stripPreamble(allocator, text1);
    defer allocator.free(r1);
    try std.testing.expectEqualStrings("Parses JSON from a byte slice.", r1);

    const text2 = "Here's the description:\nBuilds the dep graph.";
    const r2 = try stripPreamble(allocator, text2);
    defer allocator.free(r2);
    try std.testing.expectEqualStrings("Builds the dep graph.", r2);

    const text3 = "Parses JSON tokens efficiently.";
    const r3 = try stripPreamble(allocator, text3);
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
    try std.testing.expect(isMalformedResponse("We Need To write a single-line comment"));
    try std.testing.expect(isMalformedResponse("we need a better approach here"));
    try std.testing.expect(isMalformedResponse("let's think about what this does"));
    try std.testing.expect(isMalformedResponse("Let's craft a comment: something like"));
    try std.testing.expect(isMalformedResponse("let's count characters: Stores and parses"));
    try std.testing.expect(isMalformedResponse("let me think about the ownership model"));
    try std.testing.expect(isMalformedResponse("i need to mention that it owns the allocator"));
}

test "extractCommentTag: returns tag content" {
    const text = "some reasoning\n<comment>Parses JSON from a byte slice.</comment>\nmore text";
    const result = extractCommentTag(text);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("Parses JSON from a byte slice.", result.?);
}

test "extractCommentTag: trims whitespace inside tag" {
    const text = "<comment>  Builds dependency graph.  </comment>";
    const result = extractCommentTag(text);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("Builds dependency graph.", result.?);
}

test "extractCommentTag: returns null when no tag present" {
    try std.testing.expect(extractCommentTag("we need to write a comment for this type") == null);
    try std.testing.expect(extractCommentTag("Parses JSON.") == null);
    try std.testing.expect(extractCommentTag("") == null);
}

test "extractCommentTag: returns null for empty tag" {
    try std.testing.expect(extractCommentTag("<comment>   </comment>") == null);
}

test "extractCommentTag: chain-of-thought before tag is ignored" {
    const text =
        \\We need to write a comment for DepsGenerator.
        \\The comment should be plain English...
        \\Let's craft: something like "Generates dependency graph".
        \\<comment>[skills: zig-current] Walks src/ and resolves @import paths to build a dep graph.</comment>
    ;
    const result = extractCommentTag(text);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("[skills: zig-current] Walks src/ and resolves @import paths to build a dep graph.", result.?);
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
