const std = @import("std");
const types = @import("types.zig");
const ast_parser = @import("ast_parser.zig");
const json_store = @import("json_store.zig");
const hash = @import("hash.zig");
const enhancer_mod = @import("enhancer.zig");

pub const SyncProcessor = struct {
    allocator: std.mem.Allocator,
    project_root: []const u8,
    output_dir: []const u8,
    dry_run: bool,
    debug: bool,
    store: json_store.JsonStore,
    /// Optional AI enhancer; when non-null and available, automatically infills
    /// missing comments for modules, structs, enums, and stand-alone functions.
    enhancer: ?enhancer_mod.Enhancer = null,
    /// Optional thinking model enhancer for generating module detail documentation.
    /// Uses the thinking model slot from config. When available, generates
    /// comprehensive module documentation (<800 words) and discovery keywords.
    thinking_enhancer: ?enhancer_mod.Enhancer = null,
    /// Regenerate all comment fields via LLM; ask the LLM to pick the better of old vs new.
    regen_comments: bool = false,
    /// For .zig files with no guidance JSON: create it with LLM-filled comments.
    /// Never replaces existing comments.
    regen_structure: bool = false,
    /// Regenerate module detail documentation (thinking model output).
    regen_detail: bool = false,

    pub fn init(allocator: std.mem.Allocator, project_root: []const u8, output_dir: []const u8, dry_run: bool, debug: bool) SyncProcessor {
        return .{
            .allocator = allocator,
            .project_root = project_root,
            .output_dir = output_dir,
            .dry_run = dry_run,
            .debug = debug,
            .store = json_store.JsonStore.init(allocator),
        };
    }

    pub fn deinit(self: *SyncProcessor) void {
        if (self.enhancer) |*e| e.deinit();
        if (self.thinking_enhancer) |*e| e.deinit();
    }

    fn stripComments(self: *SyncProcessor, members: []types.Member) void {
        for (members) |*m| {
            if (m.comment) |c| {
                self.allocator.free(c);
                m.comment = null;
            }
            if (m.members.len > 0) {
                const nested: []types.Member = @constCast(m.members);
                for (nested) |*nm| {
                    if (nm.comment) |c| {
                        self.allocator.free(c);
                        nm.comment = null;
                    }
                }
            }
        }
    }

    pub fn processFile(self: *SyncProcessor, filepath: []const u8) !types.SyncResult {
        var result: types.SyncResult = .{ .filepath = filepath };

        const file = std.fs.openFileAbsolute(filepath, .{}) catch {
            return error.FileNotFound;
        };
        defer file.close();

        const source = file.readToEndAllocOptions(self.allocator, 10 * 1024 * 1024, null, .@"1", 0) catch {
            return error.ReadError;
        };
        defer self.allocator.free(source);

        var parser = ast_parser.AstParser.init(self.allocator, source) catch {
            return error.ParseError;
        };
        defer parser.deinit();

        if (parser.hasErrors()) {
            if (self.debug) {
                std.debug.print("Parse errors in {s}\n", .{filepath});
            }
            return error.ParseError;
        }

        const source_members = try parser.extractMembers();
        // mergeMembers deep-copies what it needs; free source_members after.
        defer {
            for (source_members) |m| self.store.freeMember(m);
            self.allocator.free(source_members);
        }

        // Note: We do NOT strip AST comments here. Source doc comments (///...)
        // should be captured into JSON. mergeMembers handles comment preservation:
        // - If signature hash unchanged: keep existing JSON comment (or use source)
        // - If signature hash changed: use source comment if present, else mark stale

        const rel_path = if (std.mem.indexOf(u8, filepath, self.project_root)) |idx|
            filepath[idx + self.project_root.len + 1 ..]
        else
            filepath;

        const guidance_path = try std.fmt.allocPrint(self.allocator, "{s}/{s}.json", .{ self.output_dir, rel_path });
        defer self.allocator.free(guidance_path);

        const existing_doc = try self.store.loadGuidance(guidance_path);
        defer if (existing_doc) |ed| self.store.freeGuidanceDoc(ed);

        const existing_members = if (existing_doc) |doc| doc.members else &.{};

        // Preserve existing comments when either infill or regen is active so that
        // infill correctly sees non-null comments and skips them, and regen has the
        // old comment available for score comparison.  Stale members (hash changed)
        // are handled inside mergeMembers regardless of this flag.
        const preserve_comments = self.regen_comments;
        const merge_result = try self.store.mergeMembers(source_members, existing_members, preserve_comments);

        result.members_added = merge_result.members_added;
        result.members_updated = merge_result.members_updated;
        result.members_removed = merge_result.members_removed;
        result.has_changes = merge_result.has_changes;

        if (self.debug and merge_result.members_stale > 0) {
            std.debug.print("[sync] {s}: {} stale comment(s) cleared (hash changed)\n", .{ filepath, merge_result.members_stale });
        }

        // --- AI Enhancement: member-level comments ---
        // Automatically infill comments for key locations when LLM is available:
        // structs, enums, unions, and stand-alone functions.
        // Skip methods (inherit context from parent struct).
        if (self.enhancer) |*enh| {
            if (enh.available()) {
                for (merge_result.members) |*m| {
                    // Skip if comment already present (unless --regen)
                    if (!self.regen_comments) {
                        if (m.comment) |c| if (c.len > 0) continue;
                    }

                    const sig = m.signature orelse m.name;

                    switch (m.type) {
                        // Stand-alone functions only (not methods)
                        .fn_decl, .fn_private => {
                            const er = enh.enhanceFunction(m.name, sig, m.comment, rel_path) catch continue;
                            defer er.deinit(self.allocator);
                            if (er.comment) |new_doc| {
                                const accept = if (m.comment) |old|
                                    enhancer_mod.Enhancer.scoreDocstring(new_doc) > enhancer_mod.Enhancer.scoreDocstring(old)
                                else
                                    true; // no existing comment — always accept
                                if (accept) {
                                    if (m.comment) |old| self.allocator.free(old);
                                    m.comment = try self.allocator.dupe(u8, new_doc);
                                    result.has_changes = true;
                                }
                            }
                            if (er.tags.len > 0) {
                                const merged_tags = try self.mergeTags(m.tags, er.tags);
                                self.allocator.free(m.tags);
                                m.tags = merged_tags;
                            }
                        },
                        .@"struct", .@"enum", .@"union" => {
                            var method_sigs_list: std.ArrayList([]const u8) = .{};
                            defer method_sigs_list.deinit(self.allocator);
                            for (m.members) |mm| {
                                if (mm.signature) |msig| try method_sigs_list.append(self.allocator, msig);
                            }
                            const er = enh.enhanceStruct(m.name, sig, method_sigs_list.items, m.comment, rel_path) catch continue;
                            defer er.deinit(self.allocator);
                            if (er.comment) |new_doc| {
                                const accept = if (m.comment) |old|
                                    enhancer_mod.Enhancer.scoreDocstring(new_doc) > enhancer_mod.Enhancer.scoreDocstring(old)
                                else
                                    true;
                                if (accept) {
                                    if (m.comment) |old| self.allocator.free(old);
                                    m.comment = try self.allocator.dupe(u8, new_doc);
                                    result.has_changes = true;
                                }
                            }
                            if (er.tags.len > 0) {
                                const merged_tags = try self.mergeTags(m.tags, er.tags);
                                self.allocator.free(m.tags);
                                m.tags = merged_tags;
                            }
                        },
                        else => {},
                    }
                }
            }
        }

        // --- Module comment ---
        // Extract module doc (//! comments) from source. These should be captured
        // into JSON. If existing JSON has a comment, preserve it unless --regen.
        const raw_module_doc = try parser.extractModuleDoc();
        defer if (raw_module_doc) |d| self.allocator.free(d);

        var module_comment: ?[]const u8 = blk: {
            // Use source module doc if present
            if (raw_module_doc) |d| {
                break :blk try self.allocator.dupe(u8, d);
            }
            // Preserve existing comment from JSON.
            // --regen still calls the LLM and passes the existing comment for comparison.
            if (existing_doc) |ed| {
                if (ed.comment) |d| {
                    if (d.len > 0) break :blk try self.allocator.dupe(u8, d);
                }
            }
            break :blk null; // Leave null so AI infill can fill it
        };
        // module_comment is owned here; transferred to doc below (freed by freeGuidanceDoc).

        // --- AI Enhancement: file-level comment ---
        // Automatically generate module comment when LLM is available and missing.
        // --regen forces regeneration with score comparison.
        if (self.enhancer) |*enh| {
            const do_file_llm = blk: {
                if (!enh.available()) break :blk false;
                // --regen: always call LLM for comparison
                if (self.regen_comments) break :blk true;
                // Auto-infill: call LLM when comment is missing
                const missing = module_comment == null or
                    (module_comment != null and module_comment.?.len == 0);
                break :blk missing;
            };
            if (do_file_llm) {
                const src_preview = source[0..@min(source.len, 3000)];
                const ai_doc = enh.enhanceFile(rel_path, module_comment, src_preview) catch null;
                if (ai_doc) |new_doc| {
                    if (module_comment) |old| self.allocator.free(old);
                    module_comment = new_doc;
                    result.has_changes = true;
                }
            }
        }

        // --- Skills: add skills when relevant patterns are detected ---
        const has_gof = hasGofPatterns(merge_result.members);
        const has_domain = hasDomainPatterns(merge_result.members);
        const skills = try self.buildSkills(existing_doc, has_gof, has_domain);
        // skills owned here; transferred to doc below.

        // --- Prepend skills prefix to module comment deterministically ---
        // Format: "[skill1, skill2] description text"
        // Skills are computed from the AST; strip any existing prefix before re-adding.
        if (module_comment != null or skills.len > 0) {
            const prefixed = try self.buildCommentWithSkills(module_comment, skills);
            if (module_comment) |old| self.allocator.free(old);
            module_comment = prefixed;
        }

        // --- Module detail generation (thinking model) ---
        // Generate comprehensive module documentation using the thinking model.
        // Only when thinking_enhancer is available and detail is missing or --regen-detail.
        var module_detail: ?[]const u8 = null;
        var module_keywords: []const []const u8 = &.{};
        errdefer {
            if (module_detail) |d| self.allocator.free(d);
            for (module_keywords) |kw| self.allocator.free(kw);
            self.allocator.free(module_keywords);
        }

        // Preserve existing detail from JSON if not regenerating
        if (existing_doc) |ed| {
            if (ed.detail) |d| {
                if (d.len > 0) {
                    module_detail = try self.allocator.dupe(u8, d);
                }
            }
            if (ed.keywords.len > 0) {
                module_keywords = try self.store.dupeStrings(ed.keywords);
            }
        }

        // Generate detail if thinking model is available and detail is missing or regen
        if (self.thinking_enhancer) |*th| {
            const do_detail_llm = blk: {
                if (!th.available()) break :blk false;
                // --regen-detail: always regenerate
                if (self.regen_detail) break :blk true;
                // Auto-generate: call LLM when detail is missing
                const missing = module_detail == null or module_detail.?.len == 0;
                break :blk missing;
            };
            if (do_detail_llm) {
                // Build member signatures for context
                var member_sigs: std.ArrayList([]const u8) = .{};
                defer member_sigs.deinit(self.allocator);
                for (merge_result.members) |m| {
                    if (m.signature) |sig| {
                        try member_sigs.append(self.allocator, try self.allocator.dupe(u8, sig));
                    }
                }

                // Build capabilities string
                var caps_buf: std.ArrayList(u8) = .{};
                defer caps_buf.deinit(self.allocator);
                for (skills) |s| {
                    if (caps_buf.items.len > 0) try caps_buf.append(self.allocator, '\n');
                    try caps_buf.appendSlice(self.allocator, s.ref);
                }

                // Build skills string
                var skills_buf: std.ArrayList(u8) = .{};
                defer skills_buf.deinit(self.allocator);
                for (skills) |s| {
                    if (skills_buf.items.len > 0) try skills_buf.append(self.allocator, '\n');
                    try skills_buf.appendSlice(self.allocator, s.ref);
                }

                const detail_result = th.enhanceModuleDetail(
                    rel_path,
                    source,
                    member_sigs.items,
                    caps_buf.items,
                    skills_buf.items,
                    module_comment,
                ) catch |err| blk: {
                    if (self.debug) std.debug.print("detail generation failed: {}\n", .{err});
                    break :blk null;
                };

                if (detail_result) |dr| {
                    defer dr.deinit(self.allocator);

                    // Free old values
                    if (module_detail) |d| self.allocator.free(d);
                    for (module_keywords) |kw| self.allocator.free(kw);
                    self.allocator.free(module_keywords);

                    // Store new values
                    module_detail = try self.allocator.dupe(u8, dr.detail);
                    module_keywords = try self.store.dupeStrings(dr.keywords);
                    result.has_changes = true;

                    if (self.debug) {
                        std.debug.print("Generated detail for {s} ({} chars, {} keywords)\n", .{
                            rel_path,
                            module_detail.?.len,
                            module_keywords.len,
                        });
                    }
                }
            }
        }

        // --- Reverse dependencies (used_by) ---
        const used_by = try self.findReverseDeps(rel_path);
        // used_by owned here; transferred to doc below.

        var doc: types.GuidanceDoc = .{
            .meta = .{
                .module = try self.pathToModule(rel_path),
                .source = try self.allocator.dupe(u8, rel_path),
            },
            .comment = module_comment,
            .detail = module_detail,
            .keywords = module_keywords,
            .skills = skills,
            .used_by = used_by,
            .members = merge_result.members,
        };
        defer self.store.freeGuidanceDoc(doc);

        if (existing_doc) |existing| {
            if (existing.hashtags.len > 0) {
                doc.hashtags = try self.store.dupeStrings(existing.hashtags);
            }
        }

        // Check if detail changed
        const detail_changed = blk: {
            const existing_detail = if (existing_doc) |ed| ed.detail else null;
            const new_detail = doc.detail;
            if (existing_detail == null and new_detail == null) break :blk false;
            if (existing_detail == null or new_detail == null) break :blk true;
            break :blk !std.mem.eql(u8, existing_detail.?, new_detail.?);
        };

        // Also write when the file-level comment changed (e.g. previously missing).
        const comment_changed = blk: {
            const existing_comment = if (existing_doc) |ed| ed.comment else null;
            const new_comment = doc.comment;
            if (existing_comment == null and new_comment == null) break :blk false;
            if (existing_comment == null or new_comment == null) break :blk true;
            break :blk !std.mem.eql(u8, existing_comment.?, new_comment.?);
        };
        // Leaked prompts on disk must be cleared even when the LLM produces no
        // replacement (--regen without LLM, or LLM rejection).  The flag is set
        // by loadGuidance whenever isLeakedPrompt discards a stored comment.
        const leaked_on_disk = self.store.leaked_prompts_found;
        const needs_write = merge_result.has_changes or comment_changed or leaked_on_disk;
        if (self.debug and needs_write) {
            std.debug.print("[needs-write] has_changes={} comment_changed={} added={} updated={} removed={}\n", .{
                merge_result.has_changes,     comment_changed,
                merge_result.members_added,   merge_result.members_updated,
                merge_result.members_removed,
            });
        }
        if (comment_changed) result.has_changes = true;

        if (self.dry_run) {
            if (needs_write) {
                std.debug.print("[DRY-RUN] Would update: {s}\n", .{guidance_path});
            } else {
                std.debug.print("[DRY-RUN] No changes: {s}\n", .{guidance_path});
            }
        } else if (needs_write) {
            try self.store.saveGuidance(guidance_path, doc);
            std.debug.print("Generated: {s}\n", .{guidance_path});
        }

        return result;
    }

    pub fn processDirectory(self: *SyncProcessor, dir_path: []const u8) !usize {
        var dir = std.fs.openDirAbsolute(dir_path, .{ .iterate = true }) catch {
            return error.DirectoryNotFound;
        };
        defer dir.close();

        var count: usize = 0;
        var walker = dir.walk(self.allocator) catch return 0;
        defer walker.deinit();

        while (try walker.next()) |entry| {
            if (entry.kind == .file and std.mem.endsWith(u8, entry.basename, ".zig")) {
                const full_path = try std.fs.path.join(self.allocator, &.{ dir_path, entry.path });
                defer self.allocator.free(full_path);

                // --regen-structure: only process files that have no guidance JSON yet.
                if (self.regen_structure) {
                    const rel = relPath(full_path, self.project_root);
                    const json_path = try std.fmt.allocPrint(self.allocator, "{s}/{s}.json", .{ self.output_dir, rel });
                    defer self.allocator.free(json_path);
                    const exists = if (std.fs.openFileAbsolute(json_path, .{})) |f| blk: {
                        f.close();
                        break :blk true;
                    } else |_| false;
                    if (exists) continue; // guidance JSON already present — skip
                }

                _ = self.processFile(full_path) catch continue;
                count += 1;
            }
        }

        // Note: STRUCTURE.md is now regenerated by bin/guidance.py structure (Python orchestrator).

        return count;
    }

    fn relPath(abs: []const u8, root: []const u8) []const u8 {
        if (std.mem.startsWith(u8, abs, root)) {
            var rel = abs[root.len..];
            if (rel.len > 0 and rel[0] == '/') rel = rel[1..];
            return rel;
        }
        return abs;
    }

    fn pathToModule(self: *SyncProcessor, rel_path: []const u8) ![]const u8 {
        var result: std.ArrayList(u8) = .{};
        defer result.deinit(self.allocator);

        var parts = std.mem.splitScalar(u8, rel_path, std.fs.path.sep);
        var first = true;
        while (parts.next()) |part| {
            if (!first) try result.append(self.allocator, '.');
            first = false;

            const without_ext = if (std.mem.endsWith(u8, part, ".zig"))
                part[0 .. part.len - 4]
            else
                part;

            try result.appendSlice(self.allocator, without_ext);
        }

        return result.toOwnedSlice(self.allocator);
    }

    /// Build auto-generated module comment: "module_name: N structs, M functions (name1, name2, ...)"
    fn buildModuleCommentFallback(self: *SyncProcessor, rel_path: []const u8, members: []const types.Member) !?[]const u8 {
        if (members.len == 0) return null;

        var structs: usize = 0;
        var functions: usize = 0;
        var names: std.ArrayList([]const u8) = .{};
        defer names.deinit(self.allocator);

        for (members) |m| {
            switch (m.type) {
                .@"struct", .@"enum", .@"union" => structs += 1,
                .fn_decl, .fn_private => functions += 1,
                else => {},
            }
            if (names.items.len < 5) try names.append(self.allocator, m.name);
        }

        var buf: std.ArrayList(u8) = .{};
        defer buf.deinit(self.allocator);
        const w = buf.writer(self.allocator);

        // Use the final path component (without extension) as the module name prefix.
        const basename = std.fs.path.basename(rel_path);
        const stem = if (std.mem.endsWith(u8, basename, ".zig")) basename[0 .. basename.len - 4] else basename;
        try w.writeAll(stem);
        try w.writeAll(": ");

        var first = true;
        if (structs > 0) {
            try w.print("{} struct{s}", .{ structs, if (structs == 1) "" else "s" });
            first = false;
        }
        if (functions > 0) {
            if (!first) try w.writeAll(", ");
            try w.print("{} function{s}", .{ functions, if (functions == 1) "" else "s" });
            first = false;
        }

        if (names.items.len > 0) {
            try w.writeAll(" (");
            for (names.items, 0..) |n, i| {
                if (i > 0) try w.writeAll(", ");
                try w.writeAll(n);
            }
            try w.writeByte(')');
        }

        const owned = try buf.toOwnedSlice(self.allocator);
        return @as(?[]const u8, owned);
    }

    /// Returns true when any member (or nested member) has at least one GoF pattern.
    fn hasGofPatterns(members: []const types.Member) bool {
        for (members) |m| {
            for (m.patterns) |p| {
                if (p.type == .GoF) return true;
            }
            if (hasGofPatterns(m.members)) return true;
        }
        return false;
    }

    fn hasDomainPatterns(members: []const types.Member) bool {
        for (members) |m| {
            for (m.patterns) |p| {
                if (p.type == .Domain) return true;
            }
            if (hasDomainPatterns(m.members)) return true;
        }
        return false;
    }

    /// Build the skills slice: add domain-patterns only when domain patterns are detected;
    /// add gof-patterns when GoF patterns are detected.
    /// Preserves any extra skills that were already in the existing guidance.
    fn buildSkills(self: *SyncProcessor, existing_doc: ?types.GuidanceDoc, has_gof: bool, has_domain: bool) ![]const types.Skill {
        var skills: std.ArrayList(types.Skill) = .{};
        errdefer {
            for (skills.items) |s| {
                self.allocator.free(s.ref);
                if (s.context) |c| self.allocator.free(c);
            }
            skills.deinit(self.allocator);
        }

        const domain_ref = ".skills/domain-patterns/SKILL.md";
        const gof_ref = ".skills/gof-patterns/SKILL.md";

        // Start from existing skills so manual additions are preserved.
        if (existing_doc) |ed| {
            for (ed.skills) |s| {
                // Normalise legacy refs to new path form before deduplication.
                const normalised = if (std.mem.eql(u8, s.ref, "guidance/skills/domain_patterns/SKILL.md") or
                    std.mem.eql(u8, s.ref, "guidance/skills/domain-patterns/SKILL.md"))
                    domain_ref
                else if (std.mem.eql(u8, s.ref, "guidance/skills/gof-patterns/SKILL.md"))
                    gof_ref
                else
                    s.ref;
                // Skip baseline refs — we will re-add them below conditionally.
                if (std.mem.eql(u8, normalised, domain_ref) or std.mem.eql(u8, normalised, gof_ref)) continue;
                try skills.append(self.allocator, .{
                    .ref = try self.allocator.dupe(u8, normalised),
                    .context = if (s.context) |c| try self.allocator.dupe(u8, c) else null,
                });
            }
        }

        // Add domain-patterns only when domain patterns are detected.
        if (has_domain) {
            try skills.append(self.allocator, .{
                .ref = try self.allocator.dupe(u8, domain_ref),
                .context = try self.allocator.dupe(u8, "Domain patterns detected"),
            });
        }

        // Add gof-patterns only when GoF patterns are detected.
        if (has_gof) {
            try skills.append(self.allocator, .{
                .ref = try self.allocator.dupe(u8, gof_ref),
                .context = try self.allocator.dupe(u8, "GoF patterns detected"),
            });
        }

        return skills.toOwnedSlice(self.allocator);
    }

    /// Prepend a `[skill1, skill2] ` prefix to the module comment using the
    /// deterministically-computed skills slice.  Any existing `[...]` prefix is
    /// stripped first so re-run s remain idempotent.
    ///
    /// Returns null when there's no actual comment content (skill tags alone
    /// are NOT a valid comment - they should only annotate real descriptions).
    fn buildCommentWithSkills(
        self: *SyncProcessor,
        comment: ?[]const u8,
        skills: []const types.Skill,
    ) !?[]const u8 {
        // Strip any existing `[...]` prefix from the comment.
        const bare: []const u8 = blk: {
            const c = comment orelse break :blk "";
            if (c.len > 0 and c[0] == '[') {
                const close = std.mem.indexOfScalar(u8, c, ']') orelse break :blk c;
                const after = std.mem.trimLeft(u8, c[close + 1 ..], " ");
                break :blk after;
            }
            break :blk c;
        };

        // No actual comment content - return null to signal infill is needed.
        // Skill tags alone are NOT a valid comment; they annotate real descriptions.
        if (bare.len == 0) return null;

        // If no skills to prepend, return the bare comment as-is.
        if (skills.len == 0) {
            return try self.allocator.dupe(u8, bare);
        }

        // Extract skill names from refs like ".skills/gof-patterns/SKILL.md"
        // or short refs like "zig-current".
        var buf: std.ArrayList(u8) = .{};
        errdefer buf.deinit(self.allocator);
        const w = buf.writer(self.allocator);

        try w.writeByte('[');
        for (skills, 0..) |skill, i| {
            if (i > 0) try w.writeAll(", ");
            // Extract the directory name between "skills/" and "/SKILL.md".
            const ref = skill.ref;
            const name: []const u8 = name_blk: {
                const prefix = "skills/";
                const suffix = "/SKILL.md";
                if (std.mem.indexOf(u8, ref, prefix)) |p| {
                    const after = ref[p + prefix.len ..];
                    if (std.mem.endsWith(u8, after, suffix)) {
                        break :name_blk after[0 .. after.len - suffix.len];
                    }
                    // No trailing suffix — use everything after "skills/"
                    break :name_blk after;
                }
                // Fallback: use the ref as-is (already a bare name).
                break :name_blk ref;
            };
            try w.writeAll(name);
        }
        try w.writeByte(']');
        try w.writeByte(' ');
        try w.writeAll(bare);

        return try buf.toOwnedSlice(self.allocator);
    }

    /// Merge new tags from the LLM into the existing tag slice, deduplicating.
    /// Returns a newly allocated slice owned by the caller.
    fn mergeTags(self: *SyncProcessor, existing: []const []const u8, new_tags: []const []const u8) ![]const []const u8 {
        var merged: std.ArrayList([]const u8) = .{};
        errdefer {
            for (merged.items) |t| self.allocator.free(t);
            merged.deinit(self.allocator);
        }

        for (existing) |t| {
            try merged.append(self.allocator, try self.allocator.dupe(u8, t));
        }

        for (new_tags) |nt| {
            // Skip if already present (case-insensitive).
            var found = false;
            for (existing) |et| {
                if (std.ascii.eqlIgnoreCase(et, nt)) {
                    found = true;
                    break;
                }
            }
            if (!found) {
                try merged.append(self.allocator, try self.allocator.dupe(u8, nt));
            }
        }

        return merged.toOwnedSlice(self.allocator);
    }

    /// Core infill logic for a single guidance JSON file (absolute path).
    /// Loads the JSON, fills blank member/module comments via the Enhancer, and
    /// writes the file back if anything changed.  Returns true when the file was
    /// updated, false when it was unchanged or the enhancer is unavailable.
    ///
    /// Called automatically during processFile when LLM is available.
    fn infillOneFile(self: *SyncProcessor, json_path: []const u8) !bool {
        if (self.enhancer == null) return false;
        if (!self.enhancer.?.available()) return false;

        var doc = (try self.store.loadGuidance(json_path)) orelse return false;
        defer self.store.freeGuidanceDoc(doc);

        var file_changed = false;

        // --- Module-level comment ---
        // Auto-infill when missing, or regenerate all when --regen
        const module_needs = self.regen_comments or
            (doc.comment == null or doc.comment.?.len == 0);
        if (module_needs and doc.meta.source.len > 0) {
            const src_path = try std.fs.path.join(self.allocator, &.{ self.project_root, doc.meta.source });
            defer self.allocator.free(src_path);

            const src_preview: ?[]const u8 = blk: {
                const sf = std.fs.openFileAbsolute(src_path, .{}) catch break :blk null;
                defer sf.close();
                const raw = sf.readToEndAlloc(self.allocator, 10 * 1024 * 1024) catch break :blk null;
                defer self.allocator.free(raw);
                break :blk try self.allocator.dupe(u8, raw[0..@min(raw.len, 3000)]);
            };
            defer if (src_preview) |p| self.allocator.free(p);

            const preview = src_preview orelse "";
            if (self.enhancer.?.enhanceFile(doc.meta.source, doc.comment, preview)) |ai_doc_opt| {
                if (ai_doc_opt) |ai_doc| {
                    const accept = if (doc.comment) |old|
                        enhancer_mod.Enhancer.scoreDocstring(ai_doc) > enhancer_mod.Enhancer.scoreDocstring(old)
                    else
                        true;
                    if (accept) {
                        if (doc.comment) |old| self.allocator.free(old);
                        doc.comment = try self.allocator.dupe(u8, ai_doc);
                        file_changed = true;
                    }
                    self.allocator.free(ai_doc);
                }
            } else |err| {
                std.debug.print("warning: LLM file-comment failed for {s}: {}\n", .{ doc.meta.source, err });
            }

            // Prepend skills prefix deterministically after LLM update.
            if (file_changed) {
                const prefixed = try self.buildCommentWithSkills(doc.comment, doc.skills);
                if (doc.comment) |old| self.allocator.free(old);
                doc.comment = prefixed;
            }
        }

        // --- Member-level comments ---
        // Only infill comments for key locations: structs, enums, unions, and
        // stand-alone functions. Skip methods (inherit context from parent struct),
        // tests, fields, constants, etc.
        const mutable_members: []types.Member = @constCast(doc.members);
        for (mutable_members) |*m| {
            // Skip if comment already present (unless --regen)
            if (!self.regen_comments) {
                if (m.comment) |c| if (c.len > 0) continue;
            }

            // Only generate comments for key locations
            const is_key_location = switch (m.type) {
                .@"struct", .@"enum", .@"union" => true,
                .fn_decl, .fn_private => true, // Stand-alone functions
                else => false,
            };
            if (!is_key_location) continue;

            const sig = m.signature orelse m.name;
            switch (m.type) {
                .fn_decl, .fn_private => {
                    if (self.enhancer.?.enhanceFunction(m.name, sig, m.comment, doc.meta.source)) |er| {
                        defer er.deinit(self.allocator);
                        if (er.comment) |new_doc| {
                            const accept = if (m.comment) |old|
                                enhancer_mod.Enhancer.scoreDocstring(new_doc) > enhancer_mod.Enhancer.scoreDocstring(old)
                            else
                                true;
                            if (accept) {
                                if (m.comment) |old| self.allocator.free(old);
                                m.comment = try self.allocator.dupe(u8, new_doc);
                                file_changed = true;
                            }
                        }
                    } else |err| {
                        std.debug.print("warning: LLM fn-comment failed for {s} in {s}: {}\n", .{ m.name, doc.meta.source, err });
                    }
                },
                .@"struct", .@"enum", .@"union" => {
                    if (self.enhancer.?.enhanceStruct(m.name, sig, &.{}, m.comment, doc.meta.source)) |er| {
                        defer er.deinit(self.allocator);
                        if (er.comment) |new_doc| {
                            const accept = if (m.comment) |old|
                                enhancer_mod.Enhancer.scoreDocstring(new_doc) > enhancer_mod.Enhancer.scoreDocstring(old)
                            else
                                true;
                            if (accept) {
                                if (m.comment) |old| self.allocator.free(old);
                                m.comment = try self.allocator.dupe(u8, new_doc);
                                file_changed = true;
                            }
                        }
                    } else |err| {
                        std.debug.print("warning: LLM type-comment failed for {s} in {s}: {}\n", .{ m.name, doc.meta.source, err });
                    }
                },
                else => {},
            }
        }

        if (file_changed) {
            if (!self.dry_run) {
                try self.store.saveGuidance(json_path, doc);
            } else {
                std.debug.print("[DRY-RUN] Would update (cross-lang): {s}\n", .{json_path});
            }
        }

        return file_changed;
    }

    /// Infill a single guidance JSON file by absolute path.  Suitable for
    /// Makefile per-file dependency targets (`make sync --file src/foo.zig`).
    /// Returns true when the file was updated.
    ///
    /// No-op (returns false) when the enhancer is absent or unreachable.
    pub fn infillJsonFile(self: *SyncProcessor, json_path: []const u8) !bool {
        if (self.enhancer == null) return false;
        if (!self.enhancer.?.available()) return false;
        return self.infillOneFile(json_path);
    }

    /// Walk guidance_dir for all *.json files (excluding those in `skip_paths`)
    /// and fill blank comment fields via the configured Enhancer.
    /// Returns the count of files that were changed.
    ///
    /// No-op (returns 0) when the enhancer is absent or unreachable.
    pub fn infillAllJson(
        self: *SyncProcessor,
        guidance_dir: []const u8,
        skip_paths: *const std.StringHashMapUnmanaged(void),
    ) !usize {
        if (self.enhancer == null) return 0;
        if (!self.enhancer.?.available()) return 0;

        var dir = std.fs.openDirAbsolute(guidance_dir, .{ .iterate = true }) catch return 0;
        defer dir.close();

        var walker = dir.walk(self.allocator) catch return 0;
        defer walker.deinit();

        var changed: usize = 0;

        while (try walker.next()) |entry| {
            if (entry.kind != .file) continue;
            if (!std.mem.endsWith(u8, entry.basename, ".json")) continue;

            const full_path = try std.fs.path.join(self.allocator, &.{ guidance_dir, entry.path });
            defer self.allocator.free(full_path);

            // Skip files already processed in this sync run.
            if (skip_paths.contains(full_path)) continue;

            if (try self.infillOneFile(full_path)) changed += 1;
        }

        return changed;
    }

    /// Check if a file path matches common test file patterns (language-agnostic).
    fn isTestPath(rel_path: []const u8) bool {
        const basename = std.fs.path.basename(rel_path);
        const stem = blk: {
            const ext = std.fs.path.extension(basename);
            break :blk if (ext.len > 0) basename[0 .. basename.len - ext.len] else basename;
        };
        if (std.mem.endsWith(u8, stem, "_test")) return true;
        if (std.mem.startsWith(u8, stem, "test_")) return true;
        if (std.mem.eql(u8, stem, "tests")) return true;
        if (std.mem.indexOf(u8, rel_path, "/test/") != null) return true;
        if (std.mem.indexOf(u8, rel_path, "/tests/") != null) return true;
        if (std.mem.indexOf(u8, rel_path, "\\test\\") != null) return true;
        if (std.mem.indexOf(u8, rel_path, "\\tests\\") != null) return true;
        return false;
    }

    /// Scan src/ directory for Zig files that @import the given module.
    /// rel_path is the relative path of the file being processed (e.g. "src/foo.zig").
    fn findReverseDeps(self: *SyncProcessor, rel_path: []const u8) ![]const []const u8 {
        const src_dir_path = try std.fs.path.join(self.allocator, &.{ self.project_root, "src" });
        defer self.allocator.free(src_dir_path);

        var src_dir = std.fs.openDirAbsolute(src_dir_path, .{ .iterate = true }) catch return &.{};
        defer src_dir.close();

        // Derive the stem (filename without .zig) and @import pattern to search for.
        const basename = std.fs.path.basename(rel_path);
        const stem = if (std.mem.endsWith(u8, basename, ".zig")) basename[0 .. basename.len - 4] else basename;

        // Build the import pattern: @import("stem.zig") or @import("../stem.zig")
        const import_pattern = try std.fmt.allocPrint(self.allocator, "@import(\"{s}.zig\")", .{stem});
        defer self.allocator.free(import_pattern);

        var found: std.ArrayList([]const u8) = .{};
        errdefer {
            for (found.items) |s| self.allocator.free(s);
            found.deinit(self.allocator);
        }

        var walker = src_dir.walk(self.allocator) catch return &.{};
        defer walker.deinit();

        while (try walker.next()) |entry| {
            if (entry.kind != .file) continue;
            if (!std.mem.endsWith(u8, entry.basename, ".zig")) continue;

            // Skip test files - we don't want them in used_by.
            if (isTestPath(entry.path)) continue;

            const full_path = try std.fs.path.join(self.allocator, &.{ src_dir_path, entry.path });
            defer self.allocator.free(full_path);

            // Skip the file itself.
            if (std.mem.endsWith(u8, full_path, rel_path)) continue;

            const content = blk: {
                const f = std.fs.openFileAbsolute(full_path, .{}) catch continue;
                defer f.close();
                break :blk f.readToEndAlloc(self.allocator, 1024 * 1024) catch continue;
            };
            defer self.allocator.free(content);

            if (std.mem.indexOf(u8, content, import_pattern) != null) {
                const rel = try std.fmt.allocPrint(self.allocator, "src/{s}", .{entry.path});
                try found.append(self.allocator, rel);
            }
        }

        // Sort for determinism.
        std.mem.sort([]const u8, found.items, {}, struct {
            fn lessThan(_: void, a: []const u8, b: []const u8) bool {
                return std.mem.lessThan(u8, a, b);
            }
        }.lessThan);

        return found.toOwnedSlice(self.allocator);
    }
};
