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
const llm = @import("common");

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Per-file result from `CommentSyncProcessor.processFile()`.
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
        const source = llm.readFileAlloc(self.allocator, filepath, 10 * 1024 * 1024) orelse
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

            // Skip private members and tests.
            switch (member.type) {
                .fn_private, .method_private, .test_decl, .comptime_block => continue,
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
                const new_comment = try self.generateMemberComment(member, current_source) orelse continue;
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
                // Comment exists — check if it is stale.
                const check = comment_checker.checkCommentStaleness(existing_comment.?, member, "");
                if (!check.needs_regeneration) continue;

                // Stale — regenerate if LLM is available.
                const new_comment = try self.generateMemberComment(member, current_source) orelse continue;
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
            const rel = relPath(filepath, self.project_root);
            const guidance_path = try std.fmt.allocPrint(
                self.allocator,
                "{s}/{s}.json",
                .{ self.output_dir, rel },
            );
            defer self.allocator.free(guidance_path);

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
    ) !?[]const u8 {
        _ = _source;
        const enh = self.enhancer orelse return null;

        const sig = member.signature orelse member.name;
        const rel = relPath(self.project_root, self.project_root); // placeholder

        const er = switch (member.type) {
            .fn_decl, .fn_private, .method, .method_private =>
                enh.enhanceFunction(member.name, sig, null, rel) catch return null,
            .@"struct", .@"enum", .@"union" =>
                enh.enhanceStruct(member.name, sig, &.{}, null, rel) catch return null,
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

/// Sort members by line number in descending order.
fn sortMembersByLineDesc(allocator: std.mem.Allocator, members: []const types.Member) ![]types.Member {
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

fn relPath(filepath: []const u8, root: []const u8) []const u8 {
    if (std.mem.startsWith(u8, filepath, root)) {
        var rel = filepath[root.len..];
        if (rel.len > 0 and rel[0] == '/') rel = rel[1..];
        return rel;
    }
    return filepath;
}

fn writeFile(path: []const u8, content: []const u8) !void {
    const file = try std.fs.createFileAbsolute(path, .{ .truncate = true });
    defer file.close();
    try file.writeAll(content);
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
