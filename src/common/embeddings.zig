//! Embedding providers — convert text to vectors for semantic search.
//!
//! Provides:
//!   - EmbeddingProvider  vtable interface
//!   - NoopEmbedding      returns empty vectors (keyword-only fallback)
//!   - OllamaEmbedding    Ollama local /api/embed endpoint
//!   - OpenAiEmbedding    OpenAI-compatible /v1/embeddings endpoint
//!   - createEmbeddingProvider() factory

const std = @import("std");
const builtin = @import("builtin");
const url_mod = @import("url.zig");
const json_mod = @import("json.zig");
const hash_mod = @import("hash.zig");

/// True in Debug and ReleaseSafe builds — enables thread-safety assertions.
const runtime_safety = builtin.mode == .Debug or builtin.mode == .ReleaseSafe;

// ── JSON helper ───────────────────────────────────────────────────

/// Append `text` to `buf`, JSON-escaping characters that require it.
const appendJsonEscaped = json_mod.appendEscaped;

// ── URL validation ────────────────────────────────────────────────

const validateHttpsOrLocalHttp = url_mod.validateHttpsOrLocalHttp;

// ── EmbeddingProvider vtable ──────────────────────────────────────

pub const EmbeddingProvider = struct {
    ptr: *anyopaque,
    vtable: *const VTable,
    /// Thread ID of the creating thread (debug/ReleaseSafe builds only).
    /// Used by embed() to assert single-threaded use unless the impl is thread-safe.
    thread_id: if (runtime_safety) std.Thread.Id else void,
    /// Set to true for implementations that are safe to call from any thread.
    thread_safe: bool = false,

    pub const VTable = struct {
        name: *const fn (ptr: *anyopaque) []const u8,
        dimensions: *const fn (ptr: *anyopaque) u32,
        embed: *const fn (ptr: *anyopaque, allocator: std.mem.Allocator, text: []const u8) anyerror![]f32,
        deinit: *const fn (ptr: *anyopaque) void,
    };

    /// Internal constructor — use implementation-specific `provider()` methods instead.
    pub fn make(ptr: *anyopaque, vtable: *const VTable) EmbeddingProvider {
        return .{
            .ptr = ptr,
            .vtable = vtable,
            .thread_id = if (runtime_safety) std.Thread.getCurrentId() else {},
            .thread_safe = false,
        };
    }

    pub fn getName(self: EmbeddingProvider) []const u8 {
        return self.vtable.name(self.ptr);
    }

    pub fn getDimensions(self: EmbeddingProvider) u32 {
        return self.vtable.dimensions(self.ptr);
    }

    /// Embed a single text into a vector. Caller owns the returned slice.
    ///
    /// In debug/ReleaseSafe builds, asserts that this is called from the creating
    /// thread unless `thread_safe` is set to true on the handle.
    pub fn embed(self: EmbeddingProvider, allocator: std.mem.Allocator, text: []const u8) ![]f32 {
        if (runtime_safety and !self.thread_safe) {
            std.debug.assert(std.Thread.getCurrentId() == self.thread_id);
        }
        return self.vtable.embed(self.ptr, allocator, text);
    }

    pub fn deinit(self: EmbeddingProvider) void {
        self.vtable.deinit(self.ptr);
    }
};

// ── Noop provider (keyword-only fallback) ─────────────────────────

/// Manages static embedding structures without computation; owned by the module; no runtime dependencies.
pub const NoopEmbedding = struct {
    allocator: ?std.mem.Allocator = null,

    const Self = @This();

    fn implName(_: *anyopaque) []const u8 {
        return "none";
    }

    fn implDimensions(_: *anyopaque) u32 {
        return 0;
    }

    fn implEmbed(_: *anyopaque, allocator: std.mem.Allocator, _: []const u8) anyerror![]f32 {
        return allocator.alloc(f32, 0);
    }

    fn implDeinit(ptr: *anyopaque) void {
        const self_: *Self = @ptrCast(@alignCast(ptr));
        if (self_.allocator) |alloc| {
            alloc.destroy(self_);
        }
    }

    const vtable = EmbeddingProvider.VTable{
        .name = &implName,
        .dimensions = &implDimensions,
        .embed = &implEmbed,
        .deinit = &implDeinit,
    };

    pub fn provider(self: *Self) EmbeddingProvider {
        return EmbeddingProvider.make(@ptrCast(self), &vtable);
    }
};

// ── Ollama embedding provider ─────────────────────────────────────

/// Manages Ollama embeddings with fixed-size storage; owned by the service; key invariant is consistent embedding retrieval.
pub const OllamaEmbedding = struct {
    allocator: std.mem.Allocator,
    base_url: []const u8,
    model: []const u8,
    dims: u32,

    const Self = @This();

    pub const default_base_url = "http://localhost:11434";
    pub const default_model = "nomic-embed-text";
    pub const default_dims: u32 = 768;

    pub fn init(
        allocator: std.mem.Allocator,
        model: ?[]const u8,
        base_url: ?[]const u8,
        dims: ?u32,
    ) !*Self {
        try validateHttpsOrLocalHttp(base_url orelse default_base_url);

        const self_ = try allocator.create(Self);
        errdefer allocator.destroy(self_);

        const owned_url = try allocator.dupe(u8, base_url orelse default_base_url);
        errdefer allocator.free(owned_url);
        const owned_model = try allocator.dupe(u8, model orelse default_model);

        self_.* = .{
            .allocator = allocator,
            .base_url = owned_url,
            .model = owned_model,
            .dims = dims orelse default_dims,
        };
        return self_;
    }

    pub fn deinitSelf(self: *Self) void {
        self.allocator.free(self.base_url);
        self.allocator.free(self.model);
        self.allocator.destroy(self);
    }

    fn buildUrl(self: *const Self, allocator: std.mem.Allocator) ![]u8 {
        return std.fmt.allocPrint(allocator, "{s}/api/embed", .{self.base_url});
    }

    fn implName(_: *anyopaque) []const u8 {
        return "ollama";
    }

    fn implDimensions(ptr: *anyopaque) u32 {
        const self_: *Self = @ptrCast(@alignCast(ptr));
        return self_.dims;
    }

    fn implEmbed(ptr: *anyopaque, allocator: std.mem.Allocator, text: []const u8) anyerror![]f32 {
        const self_: *Self = @ptrCast(@alignCast(ptr));

        if (text.len == 0) {
            return allocator.alloc(f32, 0);
        }

        var body_buf: std.ArrayListUnmanaged(u8) = .empty;
        defer body_buf.deinit(allocator);

        try body_buf.appendSlice(allocator, "{\"model\":\"");
        try appendJsonEscaped(&body_buf, allocator, self_.model);
        try body_buf.appendSlice(allocator, "\",\"input\":\"");
        try appendJsonEscaped(&body_buf, allocator, text);
        try body_buf.appendSlice(allocator, "\"}");

        const url = try self_.buildUrl(allocator);
        defer allocator.free(url);

        var client = std.http.Client{ .allocator = allocator };
        defer client.deinit();

        var aw: std.Io.Writer.Allocating = .init(allocator);
        defer aw.deinit();

        const result = client.fetch(.{
            .location = .{ .url = url },
            .method = .POST,
            .payload = body_buf.items,
            .extra_headers = &.{
                .{ .name = "Content-Type", .value = "application/json" },
            },
            .response_writer = &aw.writer,
        }) catch return error.EmbeddingApiError;

        if (result.status != .ok) {
            return error.EmbeddingApiError;
        }

        const resp_body = aw.writer.buffer[0..aw.writer.end];
        if (resp_body.len == 0) return error.EmbeddingApiError;

        return parseOllamaResponse(allocator, resp_body);
    }

    fn implDeinit(ptr: *anyopaque) void {
        const self_: *Self = @ptrCast(@alignCast(ptr));
        self_.deinitSelf();
    }

    const vtable = EmbeddingProvider.VTable{
        .name = &implName,
        .dimensions = &implDimensions,
        .embed = &implEmbed,
        .deinit = &implDeinit,
    };

    pub fn provider(self: *Self) EmbeddingProvider {
        return EmbeddingProvider.make(@ptrCast(self), &vtable);
    }
};

/// Converts a JSON string into a slice of f32 values using the allocator.
pub fn parseOllamaResponse(allocator: std.mem.Allocator, json_bytes: []const u8) ![]f32 {
    const parsed = std.json.parseFromSlice(std.json.Value, allocator, json_bytes, .{}) catch return error.InvalidEmbeddingResponse;
    defer parsed.deinit();

    const root = switch (parsed.value) {
        .object => |obj| obj,
        else => return error.InvalidEmbeddingResponse,
    };
    const embeddings = root.get("embeddings") orelse return error.InvalidEmbeddingResponse;
    const outer_array = switch (embeddings) {
        .array => |a| a,
        else => return error.InvalidEmbeddingResponse,
    };
    if (outer_array.items.len == 0) return error.InvalidEmbeddingResponse;

    const inner = outer_array.items[0];
    const emb_array = switch (inner) {
        .array => |a| a,
        else => return error.InvalidEmbeddingResponse,
    };

    const result = try allocator.alloc(f32, emb_array.items.len);
    errdefer allocator.free(result);
    for (emb_array.items, 0..) |val, i| {
        result[i] = switch (val) {
            .float => |f| @floatCast(f),
            .integer => |n| @floatFromInt(n),
            else => return error.InvalidEmbeddingResponse,
        };
    }
    return result;
}

// ── OpenAI-compatible provider ────────────────────────────────────

/// Manages OpenAI embeddings with a fixed-size structure; owned by the service; key invariants include fixed dimensions and pre-trained weights.
pub const OpenAiEmbedding = struct {
    allocator: std.mem.Allocator,
    base_url: []const u8,
    api_key: []const u8,
    model: []const u8,
    dims: u32,

    const Self = @This();

    pub fn init(
        allocator: std.mem.Allocator,
        base_url: []const u8,
        api_key: []const u8,
        model: []const u8,
        dims: u32,
    ) !*Self {
        try validateHttpsOrLocalHttp(base_url);

        const self_ = try allocator.create(Self);
        errdefer allocator.destroy(self_);

        const owned_url = try allocator.dupe(u8, base_url);
        errdefer allocator.free(owned_url);
        const owned_key = try allocator.dupe(u8, api_key);
        errdefer allocator.free(owned_key);
        const owned_model = try allocator.dupe(u8, model);

        self_.* = .{
            .allocator = allocator,
            .base_url = owned_url,
            .api_key = owned_key,
            .model = owned_model,
            .dims = dims,
        };
        return self_;
    }

    pub fn deinitSelf(self: *Self) void {
        self.allocator.free(self.base_url);
        self.allocator.free(self.api_key);
        self.allocator.free(self.model);
        self.allocator.destroy(self);
    }

    fn embeddingsUrl(self: *const Self, allocator: std.mem.Allocator) ![]u8 {
        if (std.mem.endsWith(u8, self.base_url, "/embeddings")) {
            return allocator.dupe(u8, self.base_url);
        }
        // Check for explicit path component (beyond just "/")
        const after_scheme = blk: {
            if (std.mem.indexOf(u8, self.base_url, "://")) |idx| break :blk self.base_url[idx + 3 ..];
            break :blk self.base_url;
        };
        const path_start = std.mem.indexOfScalar(u8, after_scheme, '/') orelse 0;
        const path = after_scheme[path_start..];
        const trimmed = std.mem.trimRight(u8, path, "/");
        if (trimmed.len > 0 and !std.mem.eql(u8, trimmed, "/")) {
            return std.fmt.allocPrint(allocator, "{s}/embeddings", .{self.base_url});
        }
        return std.fmt.allocPrint(allocator, "{s}/v1/embeddings", .{self.base_url});
    }

    fn implName(_: *anyopaque) []const u8 {
        return "openai";
    }

    fn implDimensions(ptr: *anyopaque) u32 {
        const self_: *Self = @ptrCast(@alignCast(ptr));
        return self_.dims;
    }

    fn implEmbed(ptr: *anyopaque, allocator: std.mem.Allocator, text: []const u8) anyerror![]f32 {
        const self_: *Self = @ptrCast(@alignCast(ptr));

        if (text.len == 0) {
            return allocator.alloc(f32, 0);
        }

        var body_buf: std.ArrayListUnmanaged(u8) = .empty;
        defer body_buf.deinit(allocator);

        try body_buf.appendSlice(allocator, "{\"model\":\"");
        try appendJsonEscaped(&body_buf, allocator, self_.model);
        try body_buf.appendSlice(allocator, "\",\"input\":\"");
        try appendJsonEscaped(&body_buf, allocator, text);
        try body_buf.appendSlice(allocator, "\"}");

        const url = try self_.embeddingsUrl(allocator);
        defer allocator.free(url);

        const auth_header = try std.fmt.allocPrint(allocator, "Bearer {s}", .{self_.api_key});
        defer allocator.free(auth_header);

        var client = std.http.Client{ .allocator = allocator };
        defer client.deinit();

        var aw: std.Io.Writer.Allocating = .init(allocator);
        defer aw.deinit();

        const result = client.fetch(.{
            .location = .{ .url = url },
            .method = .POST,
            .payload = body_buf.items,
            .extra_headers = &.{
                .{ .name = "Authorization", .value = auth_header },
                .{ .name = "Content-Type", .value = "application/json" },
            },
            .response_writer = &aw.writer,
        }) catch return error.EmbeddingApiError;

        if (result.status != .ok) {
            return error.EmbeddingApiError;
        }

        const resp_body = aw.writer.buffer[0..aw.writer.end];
        if (resp_body.len == 0) return error.EmbeddingApiError;

        return parseOpenAiResponse(allocator, resp_body);
    }

    fn implDeinit(ptr: *anyopaque) void {
        const self_: *Self = @ptrCast(@alignCast(ptr));
        self_.deinitSelf();
    }

    const vtable = EmbeddingProvider.VTable{
        .name = &implName,
        .dimensions = &implDimensions,
        .embed = &implEmbed,
        .deinit = &implDeinit,
    };

    pub fn provider(self: *Self) EmbeddingProvider {
        return EmbeddingProvider.make(@ptrCast(self), &vtable);
    }
};

/// Converts a JSON array of AI response bytes into a slice of f32 values.
pub fn parseOpenAiResponse(allocator: std.mem.Allocator, json_bytes: []const u8) ![]f32 {
    const parsed = std.json.parseFromSlice(std.json.Value, allocator, json_bytes, .{}) catch return error.InvalidEmbeddingResponse;
    defer parsed.deinit();

    const root = switch (parsed.value) {
        .object => |obj| obj,
        else => return error.InvalidEmbeddingResponse,
    };
    const data = root.get("data") orelse return error.InvalidEmbeddingResponse;
    const data_array = switch (data) {
        .array => |a| a,
        else => return error.InvalidEmbeddingResponse,
    };
    if (data_array.items.len == 0) return error.InvalidEmbeddingResponse;

    const first = data_array.items[0];
    const embedding = switch (first) {
        .object => |obj| obj.get("embedding") orelse return error.InvalidEmbeddingResponse,
        else => return error.InvalidEmbeddingResponse,
    };
    const emb_array = switch (embedding) {
        .array => |a| a,
        else => return error.InvalidEmbeddingResponse,
    };

    const result = try allocator.alloc(f32, emb_array.items.len);
    errdefer allocator.free(result);
    for (emb_array.items, 0..) |val, i| {
        result[i] = switch (val) {
            .float => |f| @floatCast(f),
            .integer => |n| @floatFromInt(n),
            else => return error.InvalidEmbeddingResponse,
        };
    }
    return result;
}

// ── Content hash for embedding cache ─────────────────────────────

/// SHA-256-based 16-hex-char content+model hash.  Delegates to hash.zig.
pub const contentHashWithModel = hash_mod.contentHashWithModel;

// ── Factory ───────────────────────────────────────────────────────

/// Creates an embedding provider using an allocator, parameters, and model dimensions.
pub fn createEmbeddingProvider(
    allocator: std.mem.Allocator,
    provider_name: []const u8,
    api_key: ?[]const u8,
    model: []const u8,
    dims: u32,
) !EmbeddingProvider {
    if (std.mem.eql(u8, provider_name, "none") or provider_name.len == 0) {
        const noop_inst = try allocator.create(NoopEmbedding);
        noop_inst.* = .{ .allocator = allocator };
        return noop_inst.provider();
    }

    if (std.mem.eql(u8, provider_name, "ollama") or std.mem.eql(u8, provider_name, "local")) {
        const m = if (model.len > 0) model else null;
        var impl_ = try OllamaEmbedding.init(allocator, m, null, if (dims > 0) dims else null);
        return impl_.provider();
    }

    if (std.mem.startsWith(u8, provider_name, "ollama:")) {
        const m = provider_name["ollama:".len..];
        var impl_ = try OllamaEmbedding.init(allocator, if (m.len > 0) m else null, null, if (dims > 0) dims else null);
        return impl_.provider();
    }

    if (std.mem.eql(u8, provider_name, "openai")) {
        var impl_ = try OpenAiEmbedding.init(
            allocator,
            "https://api.openai.com",
            api_key orelse "",
            if (model.len > 0) model else "text-embedding-3-small",
            if (dims > 0) dims else 1536,
        );
        return impl_.provider();
    }

    if (std.mem.startsWith(u8, provider_name, "custom:")) {
        const base_url = provider_name["custom:".len..];
        var impl_ = try OpenAiEmbedding.init(
            allocator,
            base_url,
            api_key orelse "",
            if (model.len > 0) model else "text-embedding-3-small",
            if (dims > 0) dims else 1536,
        );
        return impl_.provider();
    }

    // Default: noop
    const noop_inst = try allocator.create(NoopEmbedding);
    noop_inst.* = .{ .allocator = allocator };
    return noop_inst.provider();
}

// ── Tests ─────────────────────────────────────────────────────────

// ── M2: Thread safety model ───────────────────────────────────────────────────

test "EmbeddingProvider: thread_id set on creation thread" {
    var noop: NoopEmbedding = .{};
    const p = noop.provider();
    if (runtime_safety) {
        try std.testing.expectEqual(std.Thread.getCurrentId(), p.thread_id);
    }
}
