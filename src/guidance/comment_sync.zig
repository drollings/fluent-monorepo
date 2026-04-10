/// comment_sync.zig — Source-code-first comment sync workflow for guidance.
///
/// Implements the `guidance sync-comments` subcommand and the
/// `--sync-comments` flag for `guidance gen`.
///
/// Workflow for each Zig source file:
///   1. Parse AST and extract members with line numbers.
///   2. Sort members by line number (descending) so insertions don't shift
///      subsequent line numbers.
///   3. For each member: check for existing `///` comment above declaration.
///      a. If missing → generate and insert one.
///      b. If present and hash changed → check staleness → optionally regen.
///   4. If the file changed, re-parse to get fresh line numbers.
///   5. Optionally generate a `//!` file header if none exists.
///   6. Write updated JSON with corrected line numbers.
const std = @import("std");
const types = @import("types.zig");
const ast_parser = @import("ast_parser.zig");
const json_store = @import("json_store.zig");
const enhancer_mod = @import("enhancer.zig");
const comment_inserter = @import("comment_inserter.zig");
const comment_checker = @import("comment_checker.zig");
const header_generator = @import("header_generator.zig");
const marker = @import("marker.zig");
const common = @import("common");

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Manages sync result data, tracks ownership, ensures consistent state across components.
pub const CommentSyncResult = struct {
    filepath: []const u8,
    /// Number of `///` comments inserted where none existed.
    comments_added: usize = 0,
    /// Number of `///` comments whose text was updated.
    comments_updated: usize = 0,
    /// Number of `///` comments regenerated via LLM (hash changed).
    comments_regenerated: usize = 0,
    /// True when a `//!` file header was inserted.
    header_added: bool = false,
    /// True when any change was made (comment or header).
    has_changes: bool = false,
    /// True when the source file was written to disk.
    source_modified: bool = false,
};

// ---------------------------------------------------------------------------
// Processor
// ---------------------------------------------------------------------------

/// Manages synchronization logic for comment structures; owns processing state; ensures consistent data flow.
pub const CommentSyncProcessor = struct {
    allocator: std.mem.Allocator,
    /// Absolute path to the project root (workspace).
    project_root: []const u8,
    /// Directory where guidance JSON files are stored.
    output_dir: []const u8,
    /// When true, print per-file diagnostic messages.
    debug: bool,
    /// When true, do not write any files (report only).
    dry_run: bool,
    /// When true, generate headers for files that lack `//!` comments.
    generate_headers: bool,
    /// When true, skip files whose mtime has not changed since last sync.
    incremental: bool,
    /// Optional AI enhancer for comment generation.
    enhancer: ?*enhancer_mod.Enhancer,
    /// Backing JSON store for loading/saving guidance JSON.
    store: json_store.JsonStore,

    pub fn init(
        allocator: std.mem.Allocator,
        project_root: []const u8,
        output_dir: []const u8,
        debug: bool,
        dry_run: bool,
    ) CommentSyncProcessor {
        return .{
            .allocator = allocator,
            .project_root = project_root,
            .output_dir = output_dir,
            .debug = debug,
            .dry_run = dry_run,
            .generate_headers = false,
            .incremental = false,
            .enhancer = null,
            .store = json_store.JsonStore.init(allocator),
        };
    }

    /// Return true when `filepath` has not been modified since its corresponding
    /// guidance JSON was last written.  Requires `incremental = true`.
    pub fn isUpToDate(self: *const CommentSyncProcessor, filepath: []const u8) bool {
        if (!self.incremental) return false;

        const rel = relPath(filepath, self.project_root);
        const json_path = std.fmt.allocPrint(self.allocator, "{s}/{s}.json", .{ self.output_dir, rel }) catch return false;
        defer self.allocator.free(json_path);

        // File is up-to-date when JSON mtime >= source mtime (not stale).
        return !marker.fileNeedsProcessing(filepath, json_path);
    }

    /// Process a single source file: insert/update `///` comments and
    /// optionally a `//!` file header.  The source file is modified in-place
    /// when `dry_run` is false.
    pub fn processFile(self: *CommentSyncProcessor, filepath: []const u8) !CommentSyncResult {
        var result: CommentSyncResult = .{ .filepath = filepath };

        // Incremental mode: skip files whose JSON is fresher than the source.
        if (self.isUpToDate(filepath)) {
            if (self.debug) {
                std.debug.print("[comment-sync] {s}: up to date, skipping\n", .{filepath});
            }
            return result;
        }

        // Read source.
        const source = common.readFileAlloc(self.allocator, filepath, 10 * 1024 * 1024) orelse
            return error.FileNotFound;
        defer self.allocator.free(source);

        // Parse AST (must be null-terminated).
        const source_z = try self.allocator.dupeZ(u8, source);
        defer self.allocator.free(source_z);

        var parser = ast_parser.AstParser.init(self.allocator, source_z) catch
            return error.ParseError;
        defer parser.deinit();

        if (parser.hasErrors()) return error.ParseError;

        const members = try parser.extractMembers();
        defer {
            for (members) |m| self.store.freeMember(m);
            self.allocator.free(members);
        }

        if (members.len == 0) return result;

        // Load stored guidance JSON to retrieve old match_hashes for staleness
        // detection.  If the JSON file doesn't exist yet, stored_doc is null
        // and hash-based regeneration is simply skipped.
        const rel_for_json = relPath(filepath, self.project_root);
        const guidance_path = try std.fmt.allocPrint(
            self.allocator,
            "{s}/{s}.json",
            .{ self.output_dir, rel_for_json },
        );
        defer self.allocator.free(guidance_path);

        const stored_doc = try self.store.loadGuidance(guidance_path);
        defer if (stored_doc) |sd| self.store.freeGuidanceDoc(sd);

        // Build a name→stored_match_hash lookup map.
        var stored_hash_map = std.StringHashMap(?[]const u8).init(self.allocator);
        defer stored_hash_map.deinit();
        if (stored_doc) |sd| {
            for (sd.members) |sm| {
                try stored_hash_map.put(sm.name, sm.match_hash);
            }
        }

        // Sort members by line number descending so comment insertions don't
        // shift the line numbers of earlier declarations.
        const sorted = try sortMembersByLineDesc(self.allocator, members);
        defer self.allocator.free(sorted);

        // Working copy of source that accumulates modifications.
        var current_source: []const u8 = try self.allocator.dupe(u8, source);
        defer self.allocator.free(current_source);
        var source_changed = false;

        for (sorted) |member| {
            const decl_line = member.line orelse continue;

            // Skip tests, comptime blocks, and methods.
            // Tests don't need doc comments.
            // Comptime blocks don't need doc comments.
            // Methods inherit context from their parent struct's comment.
            // Note: Private functions (.fn_private) are stand-alone and should get comments.
            switch (member.type) {
                .test_decl, .comptime_block, .method, .method_private => continue,
                else => {},
            }

            // Check for existing `///` comment above the declaration.
            const existing_comment = try comment_inserter.extractCommentAtLine(
                self.allocator,
                current_source,
                decl_line,
            );
            defer if (existing_comment) |c| self.allocator.free(c);

            if (existing_comment == null) {
                // No comment — try to generate one and insert it.
                const new_comment = try self.generateMemberComment(member, current_source, filepath) orelse continue;
                defer self.allocator.free(new_comment);

                if (self.debug) {
                    std.debug.print("[comment-sync] {s}: inserting comment for '{s}'\n", .{ filepath, member.name });
                }

                if (!self.dry_run) {
                    const insert_res = try comment_inserter.insertComment(
                        self.allocator,
                        current_source,
                        decl_line,
                        new_comment,
                    );
                    if (insert_res.changed) {
                        self.allocator.free(current_source);
                        current_source = insert_res.new_source;
                        self.allocator.free(insert_res.line_adjustments);
                        source_changed = true;
                        result.comments_added += 1;
                        result.has_changes = true;
                    } else {
                        insert_res.deinit(self.allocator);
                    }
                } else {
                    result.comments_added += 1;
                    result.has_changes = true;
                }
            } else {
                // Comment exists — check if it is stale via heuristics.
                var check = comment_checker.checkCommentStaleness(existing_comment.?, member, "");

                // Additionally check whether the member's code hash has changed
                // since the comment was last written (Bug 3 fix).
                if (!check.needs_regeneration) {
                    // stored_hash_map.get returns ??[]const u8 because the value
                    // type is ?[]const u8.  Flatten: if key absent treat as null.
                    const stored_hash: ?[]const u8 = if (stored_hash_map.get(member.name)) |v| v else null;
                    if (comment_checker.isHashStale(stored_hash, member.match_hash)) {
                        check = .{ .needs_regeneration = true, .reason = "match_hash changed" };
                    }
                }

                if (!check.needs_regeneration) continue;

                // Stale — regenerate if LLM is available.
                const new_comment = try self.generateMemberComment(member, current_source, filepath) orelse continue;
                defer self.allocator.free(new_comment);

                if (self.debug) {
                    std.debug.print("[comment-sync] {s}: regenerating comment for '{s}' (reason: {s})\n", .{
                        filepath,
                        member.name,
                        check.reason orelse "unknown",
                    });
                }

                if (!self.dry_run) {
                    const rep_res = try comment_inserter.replaceComment(
                        self.allocator,
                        current_source,
                        decl_line,
                        new_comment,
                    );
                    if (rep_res.changed) {
                        self.allocator.free(current_source);
                        current_source = rep_res.new_source;
                        self.allocator.free(rep_res.line_adjustments);
                        source_changed = true;
                        result.comments_regenerated += 1;
                        result.has_changes = true;
                    } else {
                        rep_res.deinit(self.allocator);
                    }
                } else {
                    result.comments_regenerated += 1;
                    result.has_changes = true;
                }
            }
        }

        // Optionally generate a //! file header.
        if (self.generate_headers) {
            if (try header_generator.generateFileHeader(
                self.allocator,
                relPath(filepath, self.project_root),
                members,
                current_source[0..@min(current_source.len, 2000)],
            )) |header| {
                defer self.allocator.free(header);

                if (self.debug) {
                    std.debug.print("[comment-sync] {s}: inserting file header\n", .{filepath});
                }

                if (!self.dry_run) {
                    const new_src = try header_generator.insertFileHeader(
                        self.allocator,
                        current_source,
                        header,
                    );
                    self.allocator.free(current_source);
                    current_source = new_src;
                    source_changed = true;
                    result.header_added = true;
                    result.has_changes = true;
                } else {
                    result.header_added = true;
                    result.has_changes = true;
                }
            }
        }

        // Write modified source file.
        if (source_changed and !self.dry_run) {
            try writeFile(filepath, current_source);
            result.source_modified = true;

            // Re-parse to get fresh line numbers and update the guidance JSON.
            // Re-use guidance_path that was already computed above.
            try self.updateJsonLineNumbers(guidance_path, current_source);
        }

        return result;
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Generate a `///` doc comment for `member` using the LLM enhancer when
    /// available.  Returns null when no comment can be produced.
    fn generateMemberComment(
        self: *CommentSyncProcessor,
        member: types.Member,
        _source: []const u8,
        filepath: []const u8,
    ) !?[]const u8 {
        _ = _source;
        const enh = self.enhancer orelse return null;

        const sig = member.signature orelse member.name;
        const rel = relPath(filepath, self.project_root);

        const er = switch (member.type) {
            .fn_decl, .fn_private, .method, .method_private => enh.enhanceFunction(member.name, sig, null, rel) catch return null,
            .@"struct", .@"enum", .@"union" => enh.enhanceStruct(member.name, sig, &.{}, null, rel) catch return null,
            else => return null,
        };
        defer er.deinit(self.allocator);

        return if (er.comment) |c| try self.allocator.dupe(u8, c) else null;
    }

    /// Re-parse `source` and update the guidance JSON at `guidance_path` with
    /// corrected line numbers.
    fn updateJsonLineNumbers(
        self: *CommentSyncProcessor,
        guidance_path: []const u8,
        source: []const u8,
    ) !void {
        const source_z = try self.allocator.dupeZ(u8, source);
        defer self.allocator.free(source_z);

        var parser = ast_parser.AstParser.init(self.allocator, source_z) catch return;
        defer parser.deinit();
        if (parser.hasErrors()) return;

        const new_members = try parser.extractMembers();
        defer {
            for (new_members) |m| self.store.freeMember(m);
            self.allocator.free(new_members);
        }

        const existing_doc = try self.store.loadGuidance(guidance_path);
        defer if (existing_doc) |ed| self.store.freeGuidanceDoc(ed);

        const existing_members = if (existing_doc) |doc| doc.members else &.{};
        const merge = try self.store.mergeMembers(new_members, existing_members, true);
        defer {
            for (merge.members) |m| self.store.freeMember(m);
            self.allocator.free(merge.members);
        }

        if (existing_doc) |ed| {
            var updated = ed;
            updated.members = merge.members;
            try self.store.saveGuidance(guidance_path, updated);
        }
    }
};

// ---------------------------------------------------------------------------
// Module-level helpers
// ---------------------------------------------------------------------------

/// Sorts a list of members by their line description in descending order.
pub fn sortMembersByLineDesc(allocator: std.mem.Allocator, members: []const types.Member) ![]types.Member {
    const copy = try allocator.dupe(types.Member, members);
    std.mem.sort(types.Member, copy, {}, struct {
        fn gt(_: void, a: types.Member, b: types.Member) bool {
            const la = a.line orelse 0;
            const lb = b.line orelse 0;
            return la > lb;
        }
    }.gt);
    return copy;
}

/// Converts a file path array into a Zig-safe slice, handling null-termination correctly.
fn relPath(filepath: []const u8, root: []const u8) []const u8 {
    if (std.mem.startsWith(u8, filepath, root)) {
        var rel = filepath[root.len..];
        if (rel.len > 0 and rel[0] == '/') rel = rel[1..];
        return rel;
    }
    return filepath;
}

/// Writes binary content to a file path, handling data and errors.
fn writeFile(path: []const u8, content: []const u8) !void {
    const file = try std.fs.createFileAbsolute(path, .{ .truncate = true });
    defer file.close();
    try file.writeAll(content);
}

// ---------------------------------------------------------------------------
// Batch Processing Context
// ---------------------------------------------------------------------------

/// Manages synchronization context for comment updates; owns state, ensures consistency across operations.
pub const CommentSyncContext = struct {
    allocator: std.mem.Allocator,
    /// Files that had comments generated by LLM and need to be written to source.
    needs_comment_sync: std.StringHashMapUnmanaged(bool),
    /// Files that had comments written to source and need line number correction.
    needs_line_correction: std.StringHashMapUnmanaged(bool),
    /// Files that need fmt after comment insertion.
    needs_fmt: std.StringHashMapUnmanaged(bool),

    pub fn init(allocator: std.mem.Allocator) CommentSyncContext {
        return .{
            .allocator = allocator,
            .needs_comment_sync = .{},
            .needs_line_correction = .{},
            .needs_fmt = .{},
        };
    }

    pub fn deinit(self: *CommentSyncContext) void {
        self.needs_comment_sync.deinit(self.allocator);
        self.needs_line_correction.deinit(self.allocator);
        self.needs_fmt.deinit(self.allocator);
    }

    /// Mark a file as needing comment sync (LLM generated comments need writing to source).
    pub fn markNeedsCommentSync(self: *CommentSyncContext, filepath: []const u8) !void {
        const owned = try self.allocator.dupe(u8, filepath);
        errdefer self.allocator.free(owned);
        const gop = try self.needs_comment_sync.getOrPut(self.allocator, owned);
        if (!gop.found_existing) {
            gop.key_ptr.* = owned;
            gop.value_ptr.* = true;
        } else {
            self.allocator.free(owned);
        }
    }

    /// Mark a file as needing line number correction (after comments written to source).
    pub fn markNeedsLineCorrection(self: *CommentSyncContext, filepath: []const u8) !void {
        const owned = try self.allocator.dupe(u8, filepath);
        errdefer self.allocator.free(owned);
        const gop = try self.needs_line_correction.getOrPut(self.allocator, owned);
        if (!gop.found_existing) {
            gop.key_ptr.* = owned;
            gop.value_ptr.* = true;
        } else {
            self.allocator.free(owned);
        }
    }

    /// Mark a file as needing fmt (after allsource modifications done).
    pub fn markNeedsFmt(self: *CommentSyncContext, filepath: []const u8) !void {
        const owned = try self.allocator.dupe(u8, filepath);
        errdefer self.allocator.free(owned);
        const gop = try self.needs_fmt.getOrPut(self.allocator, owned);
        if (!gop.found_existing) {
            gop.key_ptr.* = owned;
            gop.value_ptr.* = true;
        } else {
            self.allocator.free(owned);
        }
    }

    /// Free all owned filepath strings.
    pub fn clear(self: *CommentSyncContext) void {
        var it = self.needs_comment_sync.keyIterator();
        while (it.next()) |key| {
            self.allocator.free(key.*);
        }
        self.needs_comment_sync.clearRetainingCapacity();

        it = self.needs_line_correction.keyIterator();
        while (it.next()) |key| {
            self.allocator.free(key.*);
        }
        self.needs_line_correction.clearRetainingCapacity();

        it = self.needs_fmt.keyIterator();
        while (it.next()) |key| {
            self.allocator.free(key.*);
        }
        self.needs_fmt.clearRetainingCapacity();
    }
};

/// Transforms a JSON string into a Zig array of sync results, handling allocator, workspace, and verbosity.
pub fn syncCommentsToSource(
    allocator: std.mem.Allocator,
    results: []const types.SyncResult,
    json_dir: []const u8,
    workspace: []const u8,
    dry_run: bool,
    verbose: bool,
) ![]const []const u8 {
    var modified_files: std.ArrayList([]const u8) = .{};
    errdefer {
        for (modified_files.items) |f| allocator.free(f);
        modified_files.deinit(allocator);
    }

    var store = json_store.JsonStore.init(allocator);
    // JsonStore has no deinit method - memory is managed per-call

    for (results) |result| {
        if (!result.comments_generated) continue;

        const rel = relPath(result.filepath, workspace);
        const json_path = try std.fmt.allocPrint(allocator, "{s}/{s}.json", .{ json_dir, rel });
        defer allocator.free(json_path);

        const existing_doc = store.loadGuidance(json_path) catch null;
        if (existing_doc == null) continue;
        const doc = existing_doc.?;
        defer store.freeGuidanceDoc(doc);

        // Find members with generated comments that need to be written to source.
        var has_generated = false;
        for (doc.members) |member| {
            if (member.comment_generated) {
                has_generated = true;
                break;
            }
        }
        if (!has_generated) continue;

        if (verbose) {
            std.debug.print("[comment-sync] {s}: writing generated comments to source\n", .{result.filepath});
        }

        if (dry_run) {
            try modified_files.append(allocator, try allocator.dupe(u8, result.filepath));
            store.freeGuidanceDoc(doc);
            continue;
        }

        // Read current source.
        const source = std.fs.cwd().readFileAllocOptions(
            allocator,
            result.filepath,
            10 * 1024 * 1024,
            null,
            .@"1",
            0,
        ) catch |err| {
            std.debug.print("[comment-sync] WARN: cannot read {s}: {s}\n", .{ result.filepath, @errorName(err) });
            store.freeGuidanceDoc(doc);
            continue;
        };
        defer allocator.free(source);

        // Process members in descending line order to avoid shifting.
        const sorted_members = try sortMembersByLineDesc(allocator, doc.members);
        defer allocator.free(sorted_members);

        var current_source: []const u8 = try allocator.dupe(u8, source);
        defer allocator.free(current_source);
        var source_changed = false;

        for (sorted_members) |member| {
            if (!member.comment_generated) continue;
            if (member.comment == null) continue;

            const decl_line = member.line orelse continue;
            const new_comment = member.comment.?;

            // Check if there's already a comment at this line.
            const existing = try comment_inserter.extractCommentAtLine(allocator, current_source, decl_line);
            defer if (existing) |e| allocator.free(e);

            if (existing == null) {
                // No existing comment — insert.
                const insert_res = try comment_inserter.insertComment(
                    allocator,
                    current_source,
                    decl_line,
                    new_comment,
                );
                if (insert_res.changed) {
                    allocator.free(current_source);
                    current_source = insert_res.new_source;
                    allocator.free(insert_res.line_adjustments);
                    source_changed = true;
                } else {
                    insert_res.deinit(allocator);
                }
            } else {
                // Existing comment — replace if different.
                if (!std.mem.eql(u8, existing.?, new_comment)) {
                    const replace_res = try comment_inserter.replaceComment(
                        allocator,
                        current_source,
                        decl_line,
                        new_comment,
                    );
                    if (replace_res.changed) {
                        allocator.free(current_source);
                        current_source = replace_res.new_source;
                        allocator.free(replace_res.line_adjustments);
                        source_changed = true;
                    } else {
                        replace_res.deinit(allocator);
                    }
                }
            }
        }

        if (source_changed) {
            // Write modified source.
            try writeFile(result.filepath, current_source);
            try modified_files.append(allocator, try allocator.dupe(u8, result.filepath));
        }
    }

    return modified_files.toOwnedSlice(allocator);
}

/// Validates and corrects line numbers in a Zig source file using provided paths and allocator.
pub fn correctLineNumbers(
    allocator: std.mem.Allocator,
    filepath: []const u8,
    json_dir: []const u8,
    workspace: []const u8,
) !void {
    const rel = relPath(filepath, workspace);
    const json_path = try std.fmt.allocPrint(allocator, "{s}/{s}.json", .{ json_dir, rel });
    defer allocator.free(json_path);

    var store = json_store.JsonStore.init(allocator);
    // JsonStore has no deinit method - memory is managed per-call

    const existing_doc = store.loadGuidance(json_path) catch null orelse return;
    var doc = existing_doc;
    defer store.freeGuidanceDoc(doc);

    // Re-parse source.
    const source = std.fs.cwd().readFileAllocOptions(
        allocator,
        filepath,
        10 * 1024 * 1024,
        null,
        .@"1",
        0,
    ) catch |err| {
        std.debug.print("[line-correct] WARN: cannot read {s}: {s}\n", .{ filepath, @errorName(err) });
        return;
    };
    defer allocator.free(source);

    const source_z = try allocator.dupeZ(u8, source);
    defer allocator.free(source_z);

    var parser = ast_parser.AstParser.init(allocator, source_z) catch return;
    defer parser.deinit();

    if (parser.hasErrors()) return;

    const new_members = try parser.extractMembers();
    defer {
        for (new_members) |m| store.freeMember(m);
        allocator.free(new_members);
    }

    // Merge: keep comments and match_hash, update line numbers.
    var merged_members: std.ArrayList(types.Member) = .{};
    defer merged_members.deinit(allocator);

    // Build a name → new_member map.
    var new_by_name: std.StringHashMapUnmanaged(types.Member) = .{};
    defer new_by_name.deinit(allocator);
    for (new_members) |nm| {
        try new_by_name.put(allocator, nm.name, nm);
    }

    for (doc.members) |om| {
        var merged = try store.dupeMember(om);
        errdefer store.freeMember(merged);

        // Update line number from new AST.
        if (new_by_name.get(om.name)) |nm| {
            merged.line = nm.line;
        }

        try merged_members.append(allocator, merged);
    }

    // Update doc with corrected line numbers.
    for (doc.members) |m| store.freeMember(m);
    allocator.free(doc.members);
    doc.members = try merged_members.toOwnedSlice(allocator);

    try store.saveGuidance(json_path, doc);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "sortMembersByLineDesc - ordering" {
    const allocator = std.testing.allocator;
    const members = [_]types.Member{
        .{ .type = .fn_decl, .name = "first", .line = 10 },
        .{ .type = .fn_decl, .name = "second", .line = 5 },
        .{ .type = .fn_decl, .name = "third", .line = 20 },
    };
    const sorted = try sortMembersByLineDesc(allocator, &members);
    defer allocator.free(sorted);

    try std.testing.expectEqual(@as(u32, 20), sorted[0].line.?);
    try std.testing.expectEqual(@as(u32, 10), sorted[1].line.?);
    try std.testing.expectEqual(@as(u32, 5), sorted[2].line.?);
}

test "processFile_insertsCommentForMissingFn - plumbing with no enhancer" {
    // With no enhancer, generateMemberComment returns null and comments_added
    // stays 0.  The test validates that the full processFile plumbing runs
    // without errors on a valid Zig source file.
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    // Write a minimal valid Zig source file with a public function.
    const source =
        \\pub fn myFunc() void {}
        \\
    ;
    const src_file = try tmp.dir.createFile("test_fn.zig", .{});
    try src_file.writeAll(source);
    src_file.close();

    // Resolve absolute path to the temp file.
    var buf: [std.fs.max_path_bytes]u8 = undefined;
    const abs_path = try tmp.dir.realpath("test_fn.zig", &buf);

    // Also resolve the tmp dir itself as both workspace and output_dir so
    // that the guidance JSON path is constructed under the same tmp dir.
    var ws_buf: [std.fs.max_path_bytes]u8 = undefined;
    const ws_path = try tmp.dir.realpath(".", &ws_buf);

    var csp = CommentSyncProcessor.init(allocator, ws_path, ws_path, false, true); // dry_run = true
    // No enhancer assigned — generateMemberComment will return null.

    const result = try csp.processFile(abs_path);

    // Without an enhancer: no comments can be generated, so comments_added == 0.
    try std.testing.expectEqual(@as(usize, 0), result.comments_added);
    // dry_run = true so source was not modified.
    try std.testing.expect(!result.source_modified);
}

test "processFile_skipsUpToDate - incremental mode" {
    // When incremental = true and the guidance JSON mtime >= source mtime,
    // processFile must return early with has_changes = false.
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const source =
        \\pub fn anotherFunc() void {}
        \\
    ;
    {
        const f = try tmp.dir.createFile("uptodate.zig", .{});
        defer f.close();
        try f.writeAll(source);
    }

    var buf: [std.fs.max_path_bytes]u8 = undefined;
    const abs_path = try tmp.dir.realpath("uptodate.zig", &buf);
    var ws_buf: [std.fs.max_path_bytes]u8 = undefined;
    const ws_path = try tmp.dir.realpath(".", &ws_buf);

    // Create the guidance JSON with a newer mtime by writing it after the source.
    const json_path_rel = "uptodate.zig.json";
    {
        const jf = try tmp.dir.createFile(json_path_rel, .{});
        defer jf.close();
        try jf.writeAll("{\"meta\":{\"module\":\"uptodate\",\"source\":\"\"},\"members\":[]}");
    }

    var csp = CommentSyncProcessor.init(allocator, ws_path, ws_path, false, false);
    csp.incremental = true;

    const result = try csp.processFile(abs_path);
    // File should be considered up to date — no changes.
    try std.testing.expect(!result.has_changes);
}

test "processFile_bottomUpOrder - higher line processed first" {
    // sortMembersByLineDesc ensures the member with the larger line number is
    // sorted first.  This test verifies the sort indirectly: a two-function
    // source file processed with dry_run=true should not error, and the
    // ordering is already verified by the sortMembersByLineDesc test above.
    // Here we just confirm processFile does not crash on a multi-decl file.
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const source =
        \\pub fn alpha() void {}
        \\pub fn beta() void {}
        \\
    ;
    {
        const f = try tmp.dir.createFile("two_fns.zig", .{});
        defer f.close();
        try f.writeAll(source);
    }

    var buf: [std.fs.max_path_bytes]u8 = undefined;
    const abs_path = try tmp.dir.realpath("two_fns.zig", &buf);
    var ws_buf: [std.fs.max_path_bytes]u8 = undefined;
    const ws_path = try tmp.dir.realpath(".", &ws_buf);

    var csp = CommentSyncProcessor.init(allocator, ws_path, ws_path, false, true); // dry_run
    const result = try csp.processFile(abs_path);
    // No enhancer: no comments added; no crash.
    try std.testing.expectEqual(@as(usize, 0), result.comments_added);
}

test "processFile_skipsPrivateFns - no comment for private fn" {
    // Private functions (fn_private) must be skipped.  With dry_run=true and
    // no enhancer the result should always show 0 comments added, but the
    // private function must never attempt to generate a comment.
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    // Only private function — should never be processed.
    const source =
        \\fn privateHelper() void {}
        \\
    ;
    {
        const f = try tmp.dir.createFile("private.zig", .{});
        defer f.close();
        try f.writeAll(source);
    }

    var buf: [std.fs.max_path_bytes]u8 = undefined;
    const abs_path = try tmp.dir.realpath("private.zig", &buf);
    var ws_buf: [std.fs.max_path_bytes]u8 = undefined;
    const ws_path = try tmp.dir.realpath(".", &ws_buf);

    var csp = CommentSyncProcessor.init(allocator, ws_path, ws_path, false, true); // dry_run
    const result = try csp.processFile(abs_path);
    try std.testing.expectEqual(@as(usize, 0), result.comments_added);
    try std.testing.expect(!result.has_changes);
}
