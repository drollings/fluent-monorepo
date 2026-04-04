/// execution_request.zig — M1.1 ExecutionRequestBuilder and ExecutionResultReader
///
/// Fluent builder for BinaryExecutionRequest payloads following the
/// TargetBuilder pattern from FLUENT_WEAVER.md.
///
/// Buffer layout produced by build():
///   [BinaryHeader: 16 bytes]
///   [BinaryExecutionRequest: fixed size]
///   [input_bytes: variable]
///   [provides_words: variable, 8 bytes each (LE u64)]
///
/// Pattern: Arena-Backed Builder — arena owns all intermediates;
/// build() transfers ownership of the final slice to the caller.
const std = @import("std");
const context_node_schema = @import("coral_schema");

const BinaryHeader = context_node_schema.BinaryHeader;
const BinaryExecutionRequest = context_node_schema.BinaryExecutionRequest;
const BinaryExecutionResult = context_node_schema.BinaryExecutionResult;

const registry_mod = @import("common").registry;
const BuilderError = registry_mod.BuilderError;
const BuilderPhase = registry_mod.BuilderPhase;

// ---------------------------------------------------------------------------
// ExecutionRequestBuilder
// ---------------------------------------------------------------------------

/// Manages execution request construction, owns build logic, ensures correct ownership and invariants.
pub const ExecutionRequestBuilder = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    /// Owns all intermediate allocations; freed in build() on all paths.
    arena: std.heap.ArenaAllocator,
    _target_id: i64 = 0,
    _input: ?[]const u8 = null,
    _flags: u32 = 0,
    _provides_bitset: ?std.bit_set.DynamicBitSetUnmanaged = null,
    /// Rich structured error captured from any setter; surfaced by build().
    err: ?*BuilderError = null,

    pub fn init(allocator: std.mem.Allocator) Self {
        return .{
            .allocator = allocator,
            .arena = std.heap.ArenaAllocator.init(allocator),
        };
    }

    fn hasError(self: *Self) bool {
        return self.err != null;
    }

    fn setError(self: *Self, phase: BuilderPhase, field: []const u8, constraint: []const u8, cause: anyerror) void {
        if (self.err != null) return;
        self.err = BuilderError.init(
            self.arena.allocator(),
            phase,
            field,
            null,
            constraint,
            cause,
        ) catch return;
    }

    /// Set the target ID (required; must be non-zero).
    pub fn targetId(self: *Self, id: i64) *Self {
        if (self.hasError()) return self;
        if (id == 0) {
            self.setError(.initialization, "target_id", "must_be_nonzero", error.InvalidTargetId);
            return self;
        }
        self._target_id = id;
        return self;
    }

    /// Set the input bytes to pass to the WASM tool.
    pub fn input(self: *Self, bytes: []const u8) *Self {
        if (self.hasError()) return self;
        if (bytes.len > std.math.maxInt(u32)) {
            self.setError(.initialization, "input", "input_too_large", error.InputTooLarge);
            return self;
        }
        self._input = self.arena.allocator().dupe(u8, bytes) catch |e| {
            self.setError(.initialization, "input", "allocation_failed", e);
            return self;
        };
        return self;
    }

    /// OR-in a flag (VERBOSE, DRY_RUN, FORCE).
    pub fn flag(self: *Self, f: u32) *Self {
        if (self.hasError()) return self;
        self._flags |= f;
        return self;
    }

    /// Attach an optional provides bitset (stored by reference; caller retains ownership).
    pub fn providesBitset(self: *Self, bs: std.bit_set.DynamicBitSetUnmanaged) *Self {
        if (self.hasError()) return self;
        self._provides_bitset = bs;
        return self;
    }

    /// Terminal: assemble and return the complete binary payload.
    ///
    /// Returns an allocator-owned `[]u8`; the internal arena is freed.
    /// On error the arena is still freed.
    pub fn build(self: *Self) ![]u8 {
        defer self.arena.deinit();

        if (self.err) |e| return e.cause;
        if (self._target_id == 0) return error.MissingTargetId;

        const input_bytes: []const u8 = self._input orelse &[_]u8{};
        const provides_word_count: u32 = if (self._provides_bitset) |bs|
            @intCast(bs.masks.len)
        else
            0;

        const req_offset = @sizeOf(BinaryHeader);
        const input_offset: u32 = @intCast(@sizeOf(BinaryExecutionRequest));
        const provides_offset: u32 = input_offset + @as(u32, @intCast(input_bytes.len));
        const total_payload: u32 = provides_offset + provides_word_count * 8;
        const total_size = req_offset + total_payload;

        var buf = try std.ArrayList(u8).initCapacity(self.allocator, total_size);
        errdefer buf.deinit(self.allocator);

        // Write outer header (wraps the entire payload including the inner request header)
        const outer_header = BinaryHeader.init(.execution_request, total_payload);
        try buf.appendSlice(self.allocator, std.mem.asBytes(&outer_header));

        // Write BinaryExecutionRequest struct (has its own embedded header)
        const req = BinaryExecutionRequest{
            .header = BinaryHeader.init(.execution_request, total_payload),
            .target_id = self._target_id,
            .input_offset = input_offset,
            .input_len = @intCast(input_bytes.len),
            .flags = self._flags,
        };
        try buf.appendSlice(self.allocator, std.mem.asBytes(&req));

        // Write input bytes
        try buf.appendSlice(self.allocator, input_bytes);

        // Write provides words (LE u64 each)
        if (self._provides_bitset) |bs| {
            for (bs.masks) |mask| {
                var word_bytes: [8]u8 = undefined;
                std.mem.writeInt(u64, &word_bytes, @as(u64, mask), .little);
                try buf.appendSlice(self.allocator, &word_bytes);
            }
        }

        return buf.toOwnedSlice(self.allocator);
    }
};

// ---------------------------------------------------------------------------
// WasmExecution — result from ExecutionResultReader
// ---------------------------------------------------------------------------

/// Represents execution context for Wasm; manages state and calls; owned by the module; ensures safe interaction.
pub const WasmExecution = struct {
    /// Allocator-owned output bytes; free with allocator.free(output).
    output: []const u8,
    /// Optional provides bitset; deinit with deinit(allocator) if non-null.
    provides_bitset: ?std.bit_set.DynamicBitSetUnmanaged,
    success: bool,
    error_code: u32,
    allocator: std.mem.Allocator,

    pub fn deinit(self: *WasmExecution) void {
        self.allocator.free(self.output);
        if (self.provides_bitset) |*bs| bs.deinit(self.allocator);
    }
};

// ---------------------------------------------------------------------------
// ExecutionResultReader
// ---------------------------------------------------------------------------

/// Parse an Extism output buffer into a WasmExecution.
///
/// Zero-copy where possible: output is duped to caller's allocator,
/// provides bitset reconstructed from the trailing word array.
pub const ExecutionResultReader = struct {
    /// Parse `payload` as a [BinaryHeader + BinaryExecutionResult + trailing data] buffer.
    /// Caller must call result.deinit() to free output and bitset.
    pub fn read(allocator: std.mem.Allocator, payload: []const u8) !WasmExecution {
        const header_size = @sizeOf(BinaryHeader);
        const result_size = @sizeOf(BinaryExecutionResult);

        if (payload.len < header_size) return error.PayloadTooShort;
        const header: *const BinaryHeader = @ptrCast(@alignCast(payload.ptr));
        if (!header.validate()) return error.InvalidMagic;

        if (payload.len < header_size + result_size) return error.PayloadTooShort;
        const result_ptr: *const BinaryExecutionResult = @ptrCast(
            @alignCast(payload[header_size..].ptr),
        );

        // Compute output byte range relative to start of result struct.
        const out_start = header_size + result_size + result_ptr.output_offset;
        const out_end = out_start + result_ptr.output_len;
        if (out_end > payload.len) return error.OutputOutOfBounds;
        const output = try allocator.dupe(u8, payload[out_start..out_end]);
        errdefer allocator.free(output);

        // Reconstruct provides bitset if present.
        var provides_bitset: ?std.bit_set.DynamicBitSetUnmanaged = null;
        if (result_ptr.provides_words_count > 0) {
            provides_bitset = try result_ptr.getProvidesBitSet(allocator, payload[header_size..]);
        }

        return WasmExecution{
            .output = output,
            .provides_bitset = provides_bitset,
            .success = result_ptr.success != 0,
            .error_code = result_ptr.error_code,
            .allocator = allocator,
        };
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "ExecutionRequestBuilder: round-trip target_id and input" {
    var builder = ExecutionRequestBuilder.init(testing.allocator);
    const payload = try builder
        .targetId(42)
        .input("hello world")
        .build();
    defer testing.allocator.free(payload);

    // Outer header
    try testing.expect(payload.len >= @sizeOf(BinaryHeader) + @sizeOf(BinaryExecutionRequest));
    const header: *const BinaryHeader = @ptrCast(@alignCast(payload.ptr));
    try testing.expect(header.validate());

    // Inner request struct
    const req: *const BinaryExecutionRequest = @ptrCast(@alignCast(payload[@sizeOf(BinaryHeader)..].ptr));
    try testing.expectEqual(@as(i64, 42), req.target_id);
    try testing.expectEqual(@as(u32, 11), req.input_len);
    try testing.expectEqual(@as(u32, 0), req.flags);
}

test "ExecutionRequestBuilder: flag accumulation" {
    var builder = ExecutionRequestBuilder.init(testing.allocator);
    const payload = try builder
        .targetId(1)
        .flag(BinaryExecutionRequest.Flag.VERBOSE)
        .flag(BinaryExecutionRequest.Flag.DRY_RUN)
        .build();
    defer testing.allocator.free(payload);

    const req: *const BinaryExecutionRequest = @ptrCast(@alignCast(payload[@sizeOf(BinaryHeader)..].ptr));
    try testing.expectEqual(
        BinaryExecutionRequest.Flag.VERBOSE | BinaryExecutionRequest.Flag.DRY_RUN,
        req.flags,
    );
}

test "ExecutionRequestBuilder: error on zero target_id" {
    var builder = ExecutionRequestBuilder.init(testing.allocator);
    try testing.expectError(error.InvalidTargetId, builder.targetId(0).build());
}

test "ExecutionRequestBuilder: error on missing target_id" {
    var builder = ExecutionRequestBuilder.init(testing.allocator);
    try testing.expectError(error.MissingTargetId, builder.input("test").build());
}

test "ExecutionRequestBuilder: empty input builds valid payload" {
    var builder = ExecutionRequestBuilder.init(testing.allocator);
    const payload = try builder.targetId(99).build();
    defer testing.allocator.free(payload);
    const req: *const BinaryExecutionRequest = @ptrCast(@alignCast(payload[@sizeOf(BinaryHeader)..].ptr));
    try testing.expectEqual(@as(i64, 99), req.target_id);
    try testing.expectEqual(@as(u32, 0), req.input_len);
}
