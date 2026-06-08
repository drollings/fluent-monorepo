//! capability_eval.zig — Per-file thinking LLM capability evaluation.
//!
//! Implements `CapabilityEvaluator`, which reads a processed source file's
//! `.guidance/src/**/*.json` and calls the thinking LLM to determine whether
//! the file belongs to an existing capability or represents a novel one.
//!
//! ## Memory Ownership
//!
//!   - CapabilityEvaluator.init(): Returns null if index is absent. Call deinit() on success.
//!   - evaluate(): Returns EvalResult owning its string fields. Free with freeEvalResult().
//!   - writeCapabilityMd(): Returns owned path slice; caller must free.

const std = @import("std");
const llm = @import("llm");

// =============================================================================
// Public types
// =============================================================================

/// Result of matching a file to an existing capability.
pub const CapabilityMatch = struct {
    /// Existing capability name from index (owned by caller after freeEvalResult).
    capability_name: []const u8,
    /// Confidence score 0.0–1.0.
    confidence: f32,
};

/// A novel capability detected by the LLM; used to generate CAPABILITY.md.
pub const NovelCapability = struct {
    name: []const u8, // kebab-case (owned)
    title: []const u8, // Title Case (owned)
    description: []const u8, // one-line, max 120 chars (owned)
    anchors: [][]const u8, // public type/function names (owned slice + owned strings)
    body_md: []const u8, // markdown body for CAPABILITY.md (owned)
};

/// Result of a per-file capability evaluation.
pub const EvalResult = union(enum) {
    /// File belongs to an existing capability (confidence ≥ 0.75).
    matched: CapabilityMatch,
    /// File represents a novel capability not yet captured.
    novel: NovelCapability,
    /// File skipped (test file, no public API, or evaluation failed).
    skip,
};

/// An entry from `capability-index.json` used to build the LLM prompt.
const CapabilityEntry = struct {
    name: []const u8,
    description: []const u8,
};

// =============================================================================
// CapabilityEvaluator
// =============================================================================

/// Per-file thinking LLM evaluator.  Holds the loaded capability index and
/// an LlmClient.  Construct once per `guidance gen` run; call evaluate() per stale file.
pub const CapabilityEvaluator = struct {
    allocator: std.mem.Allocator,
    client: llm.LlmClient,
    /// Loaded capability index entries.  Owned; freed in deinit().
    index: []CapabilityEntry,

    /// Initialise an evaluator from the given thinking LLM config and capability-index.json path.
    /// Returns null when the index is absent or empty (→ all evaluate() calls will return .skip).
    pub fn init(
        allocator: std.mem.Allocator,
        thinking_config: llm.LlmConfig,
        index_path: []const u8,
    ) !?CapabilityEvaluator {
        const index = loadCapabilityIndex(allocator, index_path) orelse return null;
        errdefer freeIndex(allocator, index);

        const client = try llm.LlmClient.init(allocator, thinking_config);
        return .{
            .allocator = allocator,
            .client = client,
            .index = index,
        };
    }

    pub fn deinit(self: *CapabilityEvaluator) void {
        freeIndex(self.allocator, self.index);
        self.client.deinit();
    }

    /// Evaluate a `.guidance/src/**/*.json` file.
    /// Returns `.skip` on any failure; never aborts the sync pipeline.
    pub fn evaluate(self: *CapabilityEvaluator, json_path: []const u8) EvalResult {
        return self.evaluateInner(json_path) catch .skip;
    }

    fn evaluateInner(self: *CapabilityEvaluator, json_path: []const u8) !EvalResult {
        // ── Skip heuristics ───────────────────────────────────────────────────
        if (shouldSkipByPath(json_path)) return .skip;

        const io = std.Io.Threaded.global_single_threaded.io();
        const content = std.Io.Dir.cwd().readFileAlloc(
            io,
            json_path,
            self.allocator,
            .limited(512 * 1024),
        ) catch return .skip;
        defer self.allocator.free(content);

        var parsed = std.json.parseFromSlice(std.json.Value, self.allocator, content, .{}) catch return .skip;
        defer parsed.deinit();

        const root = parsed.value;
        if (root != .object) return .skip;

        // Skip when no public members.
        const members_v = root.object.get("members") orelse return .skip;
        if (members_v != .array or members_v.array.items.len == 0) return .skip;

        // Skip small root shim files (module ends in "root" and ≤ 3 members).
        if (root.object.get("meta")) |meta_v| {
            if (meta_v == .object) {
                if (meta_v.object.get("module")) |mod_v| {
                    if (mod_v == .string) {
                        if (std.mem.endsWith(u8, mod_v.string, "root") and
                            members_v.array.items.len <= 3)
                        {
                            return .skip;
                        }
                    }
                }
            }
        }

        // ── Staleness gate: skip when evaluated_at_hash matches first member ──
        if (root.object.get("capability_eval")) |eval_v| {
            if (eval_v == .object) {
                if (eval_v.object.get("evaluated_at_hash")) |hash_v| {
                    if (hash_v == .string and members_v.array.items.len > 0) {
                        const first = members_v.array.items[0];
                        if (first == .object) {
                            if (first.object.get("match_hash")) |mh_v| {
                                if (mh_v == .string and std.mem.eql(u8, mh_v.string, hash_v.string)) {
                                    return .skip;
                                }
                            }
                        }
                    }
                }
            }
        }

        // ── Build prompt and call thinking LLM ────────────────────────────────
        const prompt = try self.buildPrompt(root, members_v);
        defer self.allocator.free(prompt);

        const raw = self.client.completeOrNull(prompt, 2048, 0.1, null) orelse return .skip;
        defer self.allocator.free(raw);

        const stripped = llm.stripThinkBlock(raw);
        const json_text = extractJson(stripped) orelse return .skip;

        // ── Parse LLM response ────────────────────────────────────────────────
        var resp = std.json.parseFromSlice(std.json.Value, self.allocator, json_text, .{}) catch return .skip;
        defer resp.deinit();

        if (resp.value != .object) return .skip;

        // Collect actual member names for anchor validation.
        const member_names = try collectMemberNames(self.allocator, members_v);
        defer {
            for (member_names) |n| self.allocator.free(n);
            self.allocator.free(member_names);
        }

        // ── match path ────────────────────────────────────────────────────────
        if (resp.value.object.get("match")) |match_v| {
            if (match_v == .object) {
                const name_v = match_v.object.get("name") orelse return .skip;
                const conf_v = match_v.object.get("confidence");
                if (name_v == .string) {
                    const conf: f32 = if (conf_v) |cv| switch (cv) {
                        .float => |f| @floatCast(f),
                        .integer => |n| @floatFromInt(n),
                        else => 0.0,
                    } else 0.0;
                    if (conf >= 0.75) {
                        return .{ .matched = .{
                            .capability_name = try self.allocator.dupe(u8, name_v.string),
                            .confidence = conf,
                        } };
                    }
                    return .skip; // below threshold — do not record
                }
            }
        }

        // ── novel path ────────────────────────────────────────────────────────
        if (resp.value.object.get("novel")) |novel_v| {
            if (novel_v == .object) {
                return self.parseNovel(novel_v, member_names);
            }
        }

        return .skip;
    }

    fn parseNovel(
        self: *CapabilityEvaluator,
        novel_v: std.json.Value,
        member_names: []const []const u8,
    ) !EvalResult {
        const name_v = novel_v.object.get("name") orelse return .skip;
        const title_v = novel_v.object.get("title") orelse return .skip;
        const desc_v = novel_v.object.get("description") orelse return .skip;
        const body_v = novel_v.object.get("body") orelse return .skip;

        if (name_v != .string or title_v != .string or desc_v != .string or body_v != .string)
            return .skip;

        // Duplicate name guard: if name normalises to an existing entry → low-confidence match.
        const norm_name = try normalizeName(self.allocator, name_v.string);
        defer self.allocator.free(norm_name);

        for (self.index) |entry| {
            const norm_entry = try normalizeName(self.allocator, entry.name);
            defer self.allocator.free(norm_entry);
            if (std.mem.eql(u8, norm_name, norm_entry)) {
                return .{ .matched = .{
                    .capability_name = try self.allocator.dupe(u8, entry.name),
                    .confidence = 0.70,
                } };
            }
        }

        // Anchor validation: keep only anchors that appear in the actual member list.
        var valid_anchors: std.ArrayList([]const u8) = .empty;
        errdefer {
            for (valid_anchors.items) |a| self.allocator.free(a);
            valid_anchors.deinit(self.allocator);
        }

        if (novel_v.object.get("anchors")) |anch_v| {
            if (anch_v == .array) {
                for (anch_v.array.items) |anch| {
                    if (anch != .string) continue;
                    const is_real = for (member_names) |mn| {
                        if (std.mem.eql(u8, mn, anch.string)) break true;
                    } else false;
                    if (is_real) {
                        try valid_anchors.append(self.allocator, try self.allocator.dupe(u8, anch.string));
                    }
                }
            }
        }

        if (valid_anchors.items.len == 0) return .skip; // no verifiable API surface

        // Body length guard (max 4000 bytes).
        const body_raw = body_v.string;
        const body_len = @min(body_raw.len, 4000);

        const name_owned = try self.allocator.dupe(u8, name_v.string);
        errdefer self.allocator.free(name_owned);
        const title_owned = try self.allocator.dupe(u8, title_v.string);
        errdefer self.allocator.free(title_owned);
        const desc_owned = try self.allocator.dupe(u8, desc_v.string);
        errdefer self.allocator.free(desc_owned);
        const body_owned = try self.allocator.dupe(u8, body_raw[0..body_len]);
        errdefer self.allocator.free(body_owned);

        const anchors_slice = try valid_anchors.toOwnedSlice(self.allocator);

        return .{ .novel = .{
            .name = name_owned,
            .title = title_owned,
            .description = desc_owned,
            .anchors = anchors_slice,
            .body_md = body_owned,
        } };
    }

    fn buildPrompt(
        self: *CapabilityEvaluator,
        root: std.json.Value,
        members_v: std.json.Value,
    ) ![]const u8 {
        var aw: std.Io.Writer.Allocating = .init(self.allocator);
        errdefer aw.deinit();
        const w = &aw.writer;

        // Section 1 — existing capabilities.
        try w.writeAll("EXISTING CAPABILITIES:\n");
        for (self.index) |entry| {
            try w.print("- {s}: {s}\n", .{ entry.name, entry.description });
        }

        // Section 2 — file under evaluation.
        const source: []const u8 = blk: {
            if (root.object.get("meta")) |mv| {
                if (mv == .object) {
                    if (mv.object.get("source")) |sv| {
                        if (sv == .string) break :blk sv.string;
                    }
                }
            }
            break :blk "(unknown)";
        };
        const module: []const u8 = blk: {
            if (root.object.get("meta")) |mv| {
                if (mv == .object) {
                    if (mv.object.get("module")) |mv2| {
                        if (mv2 == .string) break :blk mv2.string;
                    }
                }
            }
            break :blk "(unknown)";
        };
        const comment: []const u8 = blk: {
            if (root.object.get("comment")) |cv| {
                if (cv == .string) break :blk cv.string;
            }
            break :blk "";
        };

        try w.print("\nFILE: {s}\nMODULE: {s}\n", .{ source, module });
        if (comment.len > 0) try w.print("COMMENT: {s}\n", .{comment});
        try w.writeAll("PUBLIC API (from guidance JSON):\n");

        var count: usize = 0;
        for (members_v.array.items) |m| {
            if (count >= 20) break;
            if (m != .object) continue;
            const mname_v = m.object.get("name") orelse continue;
            if (mname_v != .string) continue;
            if (m.object.get("signature")) |sv| {
                if (sv == .string and sv.string.len > 0) {
                    try w.print("  {s}\n", .{sv.string});
                    count += 1;
                    continue;
                }
            }
            try w.print("  {s}\n", .{mname_v.string});
            count += 1;
        }

        try w.writeAll(
            \\
            \\TASK: Determine if this file belongs to one of the EXISTING CAPABILITIES above
            \\ (respond with the capability name and a confidence score 0.0-1.0), OR if it
            \\ represents a novel capability not yet captured.
            \\
            \\If novel, provide:
            \\  name: <kebab-case>
            \\  title: <Title Case>
            \\  description: <one-line, max 120 chars>
            \\  anchors: [<PublicType>, <publicFn>, ...]
            \\  body: <2-4 paragraphs of architecture prose>
            \\
            \\Respond in JSON only:
            \\{
            \\  "match": null | {"name": "...", "confidence": 0.85},
            \\  "novel": null | {"name": "...", "title": "...", "description": "...", "anchors": [...], "body": "..."}
            \\}
        );

        return aw.toOwnedSlice();
    }
};

// =============================================================================
// writeCapabilityMd — M4
// =============================================================================

/// Create `<capabilities_dir>/<novel.name>/CAPABILITY.md`.
/// Never overwrites an existing file.
/// Returns the path to the created (or already existing) file (owned — caller must free).
pub fn writeCapabilityMd(
    allocator: std.mem.Allocator,
    capabilities_dir: []const u8,
    novel: NovelCapability,
) ![]const u8 {
    const io = std.Io.Threaded.global_single_threaded.io();

    const dir_path = try std.fs.path.join(allocator, &.{ capabilities_dir, novel.name });
    defer allocator.free(dir_path);

    std.Io.Dir.cwd().createDirPath(io, dir_path) catch |err| {
        if (err != error.PathAlreadyExists) return err;
    };

    const file_path = try std.fs.path.join(allocator, &.{ dir_path, "CAPABILITY.md" });
    errdefer allocator.free(file_path);

    // Idempotency: do not overwrite an existing file.
    const file_exists = blk: {
        std.Io.Dir.accessAbsolute(io, file_path, .{}) catch |err| {
            if (err == error.FileNotFound) break :blk false;
            break :blk true; // other access error → assume exists
        };
        break :blk true;
    };

    if (file_exists) {
        std.debug.print("[capability-eval] debug: CAPABILITY.md already exists: {s}\n", .{file_path});
        return file_path;
    }

    // Build CAPABILITY.md content.
    var aw: std.Io.Writer.Allocating = .init(allocator);
    defer aw.deinit();
    const w = &aw.writer;

    try w.print("---\nname: {s}\ndescription: {s}\nanchors:\n", .{
        novel.name, novel.description,
    });
    for (novel.anchors) |anchor| {
        try w.print("  - {s}\n", .{anchor});
    }
    try w.print("---\n\n# {s}\n\n{s}\n", .{ novel.title, novel.body_md });
    // AUTO-SOURCES section is NOT written here; cmdDiscoverCapabilitySources appends it.

    const md_content = aw.written();

    const file = try std.Io.Dir.createFileAbsolute(io, file_path, .{});
    defer file.close(io);

    var wbuf: [4096]u8 = undefined;
    var fw = file.writer(io, &wbuf);
    try fw.interface.writeAll(md_content);
    try fw.interface.flush();

    std.debug.print("[guidance] created new capability: {s}\n", .{file_path});
    return file_path;
}

// =============================================================================
// freeEvalResult
// =============================================================================

/// Release all heap memory owned by an EvalResult.
pub fn freeEvalResult(allocator: std.mem.Allocator, result: EvalResult) void {
    switch (result) {
        .matched => |m| allocator.free(m.capability_name),
        .novel => |n| {
            allocator.free(n.name);
            allocator.free(n.title);
            allocator.free(n.description);
            for (n.anchors) |a| allocator.free(a);
            allocator.free(n.anchors);
            allocator.free(n.body_md);
        },
        .skip => {},
    }
}

// =============================================================================
// Internal helpers
// =============================================================================

/// // =============================================================================
fn shouldSkipByPath(json_path: []const u8) bool {
    const base = std.fs.path.basename(json_path);
    if (std.mem.indexOf(u8, base, "_tests.zig") != null) return true;
    if (std.mem.startsWith(u8, base, "test_")) return true;
    return false;
}

fn loadCapabilityIndex(allocator: std.mem.Allocator, index_path: []const u8) ?[]CapabilityEntry {
    const io = std.Io.Threaded.global_single_threaded.io();
    const content = std.Io.Dir.cwd().readFileAlloc(
        io,
        index_path,
        allocator,
        .limited(4 * 1024 * 1024),
    ) catch return null;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{}) catch return null;
    defer parsed.deinit();

    if (parsed.value != .array) return null;

    var entries: std.ArrayList(CapabilityEntry) = .empty;
    errdefer {
        for (entries.items) |e| {
            allocator.free(e.name);
            allocator.free(e.description);
        }
        entries.deinit(allocator);
    }

    for (parsed.value.array.items) |item| {
        if (item != .object) continue;
        const name_v = item.object.get("name") orelse continue;
        if (name_v != .string) continue;

        const desc: []const u8 = blk: {
            if (item.object.get("description")) |dv| {
                if (dv == .string) break :blk dv.string;
            }
            break :blk "";
        };

        const name_owned = allocator.dupe(u8, name_v.string) catch continue;
        const desc_owned = allocator.dupe(u8, desc) catch {
            allocator.free(name_owned);
            continue;
        };
        entries.append(allocator, .{ .name = name_owned, .description = desc_owned }) catch {
            allocator.free(name_owned);
            allocator.free(desc_owned);
            break;
        };
    }

    if (entries.items.len == 0) return null;

    return entries.toOwnedSlice(allocator) catch null;
}

fn freeIndex(allocator: std.mem.Allocator, index: []CapabilityEntry) void {
    for (index) |e| {
        allocator.free(e.name);
        allocator.free(e.description);
    }
    allocator.free(index);
}

fn collectMemberNames(allocator: std.mem.Allocator, members_v: std.json.Value) ![][]const u8 {
    var names: std.ArrayList([]const u8) = .empty;
    errdefer {
        for (names.items) |n| allocator.free(n);
        names.deinit(allocator);
    }
    if (members_v == .array) {
        for (members_v.array.items) |m| {
            if (m != .object) continue;
            const name_v = m.object.get("name") orelse continue;
            if (name_v != .string) continue;
            try names.append(allocator, try allocator.dupe(u8, name_v.string));
        }
    }
    return names.toOwnedSlice(allocator);
}

/// Normalise a capability name: lowercase, replace `-` and `_` with `_`.
fn normalizeName(allocator: std.mem.Allocator, name: []const u8) ![]const u8 {
    const buf = try allocator.alloc(u8, name.len);
    for (name, 0..) |ch, i| {
        buf[i] = if (ch == '-' or ch == '_') '_' else std.ascii.toLower(ch);
    }
    return buf;
}

/// Extract the first balanced `{...}` JSON object from arbitrary text.
fn extractJson(text: []const u8) ?[]const u8 {
    const start = std.mem.indexOfScalar(u8, text, '{') orelse return null;
    var depth: usize = 0;
    var i: usize = start;
    while (i < text.len) : (i += 1) {
        switch (text[i]) {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if (depth == 0) return text[start .. i + 1];
            },
            else => {},
        }
    }
    return null;
}

// =============================================================================
// Tests
// =============================================================================

test "shouldSkipByPath: test files are skipped" {
    try std.testing.expect(shouldSkipByPath("/some/path/foo_tests.zig.json"));
    try std.testing.expect(shouldSkipByPath("/some/path/test_foo.zig.json"));
    try std.testing.expect(!shouldSkipByPath("/some/path/foo.zig.json"));
}

test "extractJson: finds balanced object" {
    const text = "some preamble { \"key\": \"val\" } trailing";
    const result = extractJson(text);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("{ \"key\": \"val\" }", result.?);
}

test "extractJson: nested objects" {
    const text = "{\"a\":{\"b\":1}}";
    const result = extractJson(text);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings(text, result.?);
}

test "normalizeName: dashes and case" {
    const allocator = std.testing.allocator;
    const n = try normalizeName(allocator, "Sync-Engine");
    defer allocator.free(n);
    try std.testing.expectEqualStrings("sync_engine", n);
}

test "freeEvalResult: skip is a no-op" {
    freeEvalResult(std.testing.allocator, .skip);
}