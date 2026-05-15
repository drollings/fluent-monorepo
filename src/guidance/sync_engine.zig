//! sync_engine.zig — init, commit, gen, status, clean, pipeline, and utility commands.
//!
//! Extracted from main.zig (M1.6) to keep file sizes navigable.
//! M2.1: gen/check/pipeline helpers extracted to sync/gen_files.zig and sync/ralph.zig.
//! All public functions are called from main.zig's command dispatch switch.
//!
//! ## Memory Ownership
//!
//!   - cmdGen(): Delegates to gen_files.cmdGenImpl(); all allocations scoped to the command.
//!   - cmdCheck(): Delegates to ralph.cmdCheck(); arena-backed for phase orchestration.
//!   - cmdCommit(): Delegates to commit_mod.cmdCommit(); LlmClient is ephemeral.
//!   - cmdSyncCapabilities() / cmdDiscoverCapabilitySources(): Own CapabilityEntry results;
//!     freed with allocator.free() at end of function.
//!   - cmdStatus() / cmdClean() / cmdInit(): Minimal allocation, all scoped.
//!   - Delegation re-exports (GenArgs, cmdCommit, cmdCheck, etc.) delegate ownership
//!     to their respective sub-modules (gen_files.zig, commit.zig, ralph.zig).

const std = @import("std");
const types = @import("types.zig");
const vector_db_mod = @import("vector");
const vector_mod = @import("vector");
const common = @import("common");
const config_mod = @import("config.zig");
const marker_mod = @import("sync/marker.zig");
const comment_sync_mod = @import("comments/sync.zig");
const json_store_mod = @import("sync/json_store.zig");
const comment_inserter_mod = @import("comments/inserter.zig");
const todo_mod = @import("todo.zig");
const doc_parser_mod = @import("doc_parser.zig");
const commit_mod = @import("sync/commit.zig");
const agents_md_mod = @import("agents_md.zig");
const gen_files_mod = @import("sync/gen_files.zig");
const GuidanceDb = vector_db_mod.GuidanceDb;
const stepPrint = types.stepPrint;

// =============================================================================
// init — create default configuration
// =============================================================================

/// Manages initialization arguments for sync engine; owns configuration; ensures stable state across calls.
const InitArgs = struct {
    guidance_dir: ?[]const u8 = null,
    db_path: ?[]const u8 = null,

    fn parse(args: []const []const u8) error{MissingValue}!InitArgs {
        var ia: InitArgs = .{};
        var i: usize = 0;
        while (i < args.len) : (i += 1) {
            const arg = args[i];
            if (std.mem.eql(u8, arg, "--guidance-dir") or std.mem.eql(u8, arg, "-g")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ia.guidance_dir = args[i];
            } else if (std.mem.eql(u8, arg, "--db") or std.mem.eql(u8, arg, "-o")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ia.db_path = args[i];
            }
        }
        return ia;
    }
};

/// Initializes the sync engine with allocator and command arguments, preparing for Zig command execution.
pub fn cmdInit(allocator: std.mem.Allocator, args: []const []const u8) !void {
    const ia = InitArgs.parse(args) catch |err| {
        std.debug.print("error: init flag missing value ({s})\n", .{@errorName(err)});
        return err;
    };

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const options: config_mod.InitOptions = .{
        .guidance_dir = ia.guidance_dir,
        .db_path = ia.db_path,
    };

    const created = try config_mod.initConfig(allocator, cwd, options);

    // Handle AGENTS.md
    const guidance_dir = ia.guidance_dir orelse config_mod.DEFAULT_GUIDANCE_DIR;
    const agents_path = try std.fs.path.join(allocator, &.{ cwd, "AGENTS.md" });
    defer allocator.free(agents_path);

    const agents_exists = blk: {
        std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), agents_path, .{}) catch break :blk false;
        break :blk true;
    };

    if (agents_exists) {
        // Read existing content
        const existing = blk: {
            const io = std.Io.Threaded.global_single_threaded.io();
            const file = std.Io.Dir.openFileAbsolute(io, agents_path, .{}) catch break :blk null;
            defer file.close(io);
            break :blk std.Io.Dir.cwd().readFileAlloc(io, agents_path, allocator, .limited(1024 * 1024)) catch null;
        };

        // Check if already has guidance integration
        if (existing) |e| {
            if (std.mem.indexOf(u8, e, "guidance Integration") != null) {
                std.debug.print("AGENTS.md already contains guidance integration.\n", .{});
                allocator.free(e);
            } else {
                // Prepend insertion
                const insertion = try std.fmt.allocPrint(allocator,
                    \\---
                    \\
                    \\## guidance Integration
                    \\
                    \\This project uses guidance for AST-guided code navigation.
                    \\
                    \\```
                    \\guidance init
                    \\guidance check
                    \\```
                    \\
                    \\Config: `{s}/guidance-config.json`
                    \\
                    \\
                , .{guidance_dir});
                defer allocator.free(insertion);

                const new_content = try std.fmt.allocPrint(allocator, "{s}{s}", .{ insertion, e });
                defer allocator.free(new_content);

                // Write new file
                const io = std.Io.Threaded.global_single_threaded.io();
                const file = std.Io.Dir.createFileAbsolute(io, agents_path, .{}) catch |err| {
                    std.debug.print("Warning: could not update AGENTS.md: {any}\n", .{err});
                    allocator.free(e);
                    return;
                };
                defer file.close(io);
                var wbuf: [4096]u8 = undefined;
                var writer = file.writer(io, &wbuf);
                try writer.interface.writeAll(new_content);
                try writer.interface.flush();
                allocator.free(e);

                std.debug.print("AGENTS.md updated with guidance integration.\n", .{});

                // Offer to open $EDITOR
                if (std.c.getenv("EDITOR")) |editor_ptr| {
                    const editor = std.mem.span(editor_ptr);
                    std.debug.print("Review changes with: {s} AGENTS.md\n", .{editor});
                }
            }
        }
    } else {
        // Create new AGENTS.md
        const agents_content = try agents_md_mod.generateAgentsMdContent(allocator, guidance_dir);
        defer allocator.free(agents_content);

        const io = std.Io.Threaded.global_single_threaded.io();
        const file = std.Io.Dir.createFileAbsolute(io, agents_path, .{}) catch |err| {
            std.debug.print("Warning: could not create AGENTS.md: {any}\n", .{err});
            return;
        };
        defer file.close(io);
        {
            var wbuf: [4096]u8 = undefined;
            var writer = file.writer(io, &wbuf);
            try writer.interface.writeAll(agents_content);
            try writer.interface.flush();
        }
        std.debug.print("Created AGENTS.md\n", .{});
    }

    if (created) {
        std.debug.print("\nConfiguration created at {s}/{s}/guidance-config.json\n", .{ cwd, guidance_dir });
    }
}

// =============================================================================
// commit — delegated to sync/commit.zig (M2.1 extraction)
// =============================================================================

pub const cmdCommit = commit_mod.cmdCommit;

pub const GenArgs = gen_files_mod.GenArgs;
pub const guidanceJsonPath = gen_files_mod.guidanceJsonPath;
pub const guidanceDbIsUpToDatePub = gen_files_mod.guidanceDbIsUpToDate;

// =============================================================================
// gen — delegates to sync/gen_files.zig (M2.1 extraction)
// =============================================================================

pub fn cmdGen(allocator: std.mem.Allocator, args: []const []const u8) !void {
    return gen_files_mod.cmdGen(allocator, args, syncCapabilitiesIfStale);
}

// ---------------------------------------------------------------------------
// sync-capabilities-if-stale — stays in sync_engine (calls local cmds)
// ---------------------------------------------------------------------------

/// Checks and updates Zig capabilities if they're outdated using provided paths and allocator.
fn syncCapabilitiesIfStale(
    allocator: std.mem.Allocator,
    json_dir: []const u8,
    db_path: []const u8,
    capabilities_dir: []const u8,
    verbose: bool,
    newly_created: []const []const u8,
) !void {
    const index_path = std.fs.path.join(allocator, &.{ json_dir, "capability-index.json" }) catch return;
    defer allocator.free(index_path);

    const index_mtime = marker_mod.fileMtime(index_path);

    // Force re-run when new CAPABILITY.md files were just created this cycle.
    if (newly_created.len > 0) {
        if (verbose) {
            for (newly_created) |path| {
                std.debug.print("[sync] new capability created: {s}\n", .{path});
            }
        }
        if (verbose) std.debug.print("[sync] capability-index.json forced stale ({d} new)\n", .{newly_created.len});
    }

    // Check if any CAPABILITY.md is newer than the index.
    var stale = newly_created.len > 0 or index_mtime == null; // newly created or missing index → always stale
    if (!stale) {
        const idx_mtime = index_mtime.?;
        const io = std.Io.Threaded.global_single_threaded.io();
        std.Io.Dir.accessAbsolute(io, capabilities_dir, .{}) catch return; // no cap dir → nothing to do
        var cap_dir = std.Io.Dir.openDirAbsolute(io, capabilities_dir, .{ .iterate = true }) catch return;
        defer cap_dir.close(io);
        var walker = cap_dir.walk(allocator) catch return;
        defer walker.deinit();
        while (true) {
            const entry = walker.next(io) catch continue;
            if (entry) |e| {
                if (e.kind != .file) continue;
                if (!std.mem.endsWith(u8, e.basename, "CAPABILITY.md")) continue;
                const full = std.fs.path.join(allocator, &.{ capabilities_dir, e.path }) catch continue;
                defer allocator.free(full);
                const m = marker_mod.fileMtime(full) orelse continue;
                if (m > idx_mtime) {
                    stale = true;
                    break;
                }
            } else break;
        }
    }

    if (!stale) {
        if (verbose) std.debug.print("[sync] capability-index.json is up to date\n", .{});
        return;
    }

    if (verbose) std.debug.print("[sync] capability-index.json is stale — running sync-capabilities\n", .{});
    stepPrint("gen: sync-capabilities\n", .{});

    cmdSyncCapabilities(allocator, &[_][]const u8{}) catch |err| {
        std.debug.print("[sync] WARN: sync-capabilities failed: {s}\n", .{@errorName(err)});
        return;
    };

    if (verbose) std.debug.print("[sync] pruning dead sources from AUTO-SOURCES sections\n", .{});
    stepPrint("gen: prune-capability-sources\n", .{});
    pruneCapabilitySources(allocator, json_dir, capabilities_dir, verbose);

    if (verbose) std.debug.print("[sync] running discover-capability-sources\n", .{});
    stepPrint("gen: discover-capability-sources\n", .{});

    cmdDiscoverCapabilitySources(allocator, &[_][]const u8{ "--db", db_path }) catch |err| {
        std.debug.print("[sync] WARN: discover-capability-sources failed: {s}\n", .{@errorName(err)});
    };
}

// =============================================================================
// status
// =============================================================================

/// Validates and processes Zig command arguments, returning a status update.
pub fn cmdStatus(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var json_dir_arg: ?[]const u8 = null;
    var db_path_arg: ?[]const u8 = null;
    var verbose = false; // verbose defaults false; overridden by --verbose arg
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--json-dir")) {
            i += 1;
            if (i >= args.len) return;
            json_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) return;
            db_path_arg = args[i];
        } else if (std.mem.eql(u8, arg, "--verbose") or std.mem.eql(u8, arg, "--debug")) {
            verbose = true;
        }
    }

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const json_dir = try common.resolvePath(allocator, cwd, json_dir_arg orelse config_mod.DEFAULT_GUIDANCE_DIR);
    defer allocator.free(json_dir);

    const db_path = try common.resolvePath(allocator, cwd, db_path_arg orelse config_mod.DEFAULT_GUIDANCE_DB_PATH);
    defer allocator.free(db_path);

    // Count JSON files in json_dir/src/.
    const json_src_dir = try std.fs.path.join(allocator, &.{ json_dir, "src" });
    defer allocator.free(json_src_dir);

    var json_count: usize = 0;
    if (std.Io.Dir.openDirAbsolute(std.Io.Threaded.global_single_threaded.io(), json_src_dir, .{ .iterate = true })) |*jdir_ptr| {
        var jdir = jdir_ptr.*;
        defer jdir.close(std.Io.Threaded.global_single_threaded.io());
        var walker = try jdir.walk(allocator);
        defer walker.deinit();
        while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
            if (entry.kind == .file and std.mem.endsWith(u8, entry.path, ".json"))
                json_count += 1;
        }
    } else |_| {}

    const db_exists = if (std.Io.Dir.openFileAbsolute(std.Io.Threaded.global_single_threaded.io(), db_path, .{})) |f| blk: {
        f.close(std.Io.Threaded.global_single_threaded.io());
        break :blk true;
    } else |_| false;

    std.debug.print("guidance status:\n", .{});
    std.debug.print("  json_dir:   {s}\n", .{json_dir});
    std.debug.print("  json files: {d}\n", .{json_count});
    std.debug.print("  db_path:    {s}\n", .{db_path});
    std.debug.print("  db_exists:  {any}\n", .{db_exists});

    // Show embedding statistics if database exists and --verbose is set
    if (db_exists and verbose) {
        var noop: vector_mod.NoopEmbedding = .{};
        var db = GuidanceDb.init(allocator, db_path, noop.provider()) catch |err| {
            std.debug.print("  (could not open db: {any})\n", .{err});
            return;
        };
        defer db.deinit();

        if (db.getEmbeddingStats()) |stats| {
            std.debug.print("  embeddings:\n", .{});
            std.debug.print("    ast_nodes:      {d}\n", .{stats.ast_nodes_with_embeddings});
            std.debug.print("    alias:          {d}\n", .{stats.alias_embeddings});
            std.debug.print("    keywords:       {d}\n", .{stats.keyword_embeddings});
            std.debug.print("    embedding_cache: {d}\n", .{stats.embedding_cache_entries});
        } else {
            std.debug.print("  (could not read embedding stats)\n", .{});
        }
    }
}

// =============================================================================
// clean
// =============================================================================

/// Cleans up memory by allocating and returning a cleaned Zig allocation.
pub fn cmdClean(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var json_dir_arg: ?[]const u8 = null;
    var db_path_arg: ?[]const u8 = null;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--json-dir")) {
            i += 1;
            if (i >= args.len) return;
            json_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) return;
            db_path_arg = args[i];
        }
    }

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const json_dir = try common.resolvePath(allocator, cwd, json_dir_arg orelse config_mod.DEFAULT_GUIDANCE_DIR);
    defer allocator.free(json_dir);

    const db_path = try common.resolvePath(allocator, cwd, db_path_arg orelse config_mod.DEFAULT_GUIDANCE_DB_PATH);
    defer allocator.free(db_path);

    // Remove the database.
    std.Io.Dir.deleteFileAbsolute(std.Io.Threaded.global_single_threaded.io(), db_path) catch |err| {
        if (err != error.FileNotFound)
            std.debug.print("warning: could not remove {s}: {any}\n", .{ db_path, err });
    };
    std.debug.print("clean: removed {s}\n", .{db_path});

    // Remove the generated JSON src tree only (preserve config and skills).
    const json_src = try std.fs.path.join(allocator, &.{ json_dir, "src" });
    defer allocator.free(json_src);
    std.Io.Dir.cwd().deleteTree(std.Io.Threaded.global_single_threaded.io(), json_src) catch |err| {
        if (err != error.FileNotFound)
            std.debug.print("warning: could not remove {s}: {any}\n", .{ json_src, err });
    };
    std.debug.print("clean: removed {s}\n", .{json_src});
}

// ---------------------------------------------------------------------------
// map-capabilities — regenerate capability-mapping.json from CAPABILITY.md files
// ---------------------------------------------------------------------------

/// Transforms allocation parameters into a Zig command map, handling allocator and argument inputs.
pub fn cmdMapCapabilities(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var guidance_dir_arg: []const u8 = config_mod.DEFAULT_GUIDANCE_DIR;
    var workspace: ?[]const u8 = null;
    var dry_run = false;
    var i: usize = 0;

    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--guidance-dir") or std.mem.eql(u8, arg, "-g")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            guidance_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "--workspace") or std.mem.eql(u8, arg, "-w")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--dry-run")) {
            dry_run = true;
        }
    }

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const ws = workspace orelse cwd;
    const guidance_abs = try common.resolvePath(allocator, ws, guidance_dir_arg);
    defer allocator.free(guidance_abs);

    const cap_dir = try std.fs.path.join(allocator, &.{ guidance_abs, "capabilities" });
    defer allocator.free(cap_dir);

    const mapping_path = try std.fs.path.join(allocator, &.{ guidance_abs, "capability-mapping.json" });
    defer allocator.free(mapping_path);

    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const fa = arena.allocator();

    // ------------------------------------------------------------------
    // Step 1: Walk capabilities dir, extract keywords from each CAPABILITY.md.
    // ------------------------------------------------------------------
    var new_cap_keywords: std.StringHashMapUnmanaged([]const []const u8) = .empty;
    defer new_cap_keywords.deinit(fa);

    var cap_d = std.Io.Dir.cwd().openDir(std.Io.Threaded.global_single_threaded.io(), cap_dir, .{ .iterate = true }) catch |err| {
        std.debug.print("map-capabilities: cannot open {s}: {s}\n", .{ cap_dir, @errorName(err) });
        return;
    };
    defer cap_d.close(std.Io.Threaded.global_single_threaded.io());

    var walker = try cap_d.walk(fa);
    defer walker.deinit();

    var cap_count: usize = 0;

    while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, "CAPABILITY.md")) continue;

        const abs_path = try std.fmt.allocPrint(fa, "{s}/{s}", .{ cap_dir, entry.path });
        const io = std.Io.Threaded.global_single_threaded.io();
        const content = std.Io.Dir.cwd().readFileAlloc(io, abs_path, fa, .limited(512 * 1024)) catch continue;

        // Use doc_parser for unified frontmatter parsing (name, description, anchors)
        const excerpt = doc_parser_mod.parseCapabilityDocContent(fa, content, false) catch continue;
        defer doc_parser_mod.freeDocExcerpt(fa, excerpt);

        const cap_name: []const u8 = excerpt.name orelse std.fs.path.basename(std.fs.path.dirname(abs_path) orelse abs_path);

        // Body is content after frontmatter for keyword extraction
        var body: []const u8 = content;
        if (std.mem.startsWith(u8, content, "---\n")) {
            const end = std.mem.indexOf(u8, content[4..], "\n---\n") orelse 0;
            if (end > 0) {
                body = content[4 + end + 5 ..];
            }
        }

        // Extract code-fence identifiers: backtick-delimited tokens that look like
        // Zig/Python identifiers (camelCase, PascalCase, snake_case, no punctuation).
        var kw_set: std.StringHashMapUnmanaged(void) = .empty;
        defer kw_set.deinit(fa);

        // Add anchors as keywords (these are high-confidence identifiers)
        for (excerpt.anchors) |anchor| {
            if (!kw_set.contains(anchor)) {
                try kw_set.put(fa, try fa.dupe(u8, anchor), {});
            }
        }

        // Scan backtick spans and identifier-like tokens from the body.
        var tok_it = std.mem.tokenizeAny(u8, body, " \t\n\r`()[]{}:,;\"'");
        while (tok_it.next()) |tok| {
            if (tok.len < 2 or tok.len > 80) continue;
            if (!isCapabilityKeywordToken(tok)) continue;
            if (!kw_set.contains(tok)) {
                try kw_set.put(fa, try fa.dupe(u8, tok), {});
            }
        }

        var kw_list: std.ArrayList([]const u8) = .empty;
        var kwit = kw_set.keyIterator();
        while (kwit.next()) |k| {
            try kw_list.append(fa, k.*);
        }

        try new_cap_keywords.put(fa, try fa.dupe(u8, cap_name), try kw_list.toOwnedSlice(fa));
        cap_count += 1;
    }

    // ------------------------------------------------------------------
    // Step 2: Load existing mapping to preserve "mapping" section.
    //         Merge: keep existing capability_keywords, add new capabilities.
    // ------------------------------------------------------------------
    var existing_mapping: std.StringHashMapUnmanaged(std.json.Value) = .empty;
    defer existing_mapping.deinit(fa);

    var existing_cap_keywords: std.StringHashMapUnmanaged(std.json.Value) = .empty;
    defer existing_cap_keywords.deinit(fa);

    const io = std.Io.Threaded.global_single_threaded.io();
    const existing_content = std.Io.Dir.cwd().readFileAlloc(io, mapping_path, fa, .limited(2 * 1024 * 1024)) catch null;
    if (existing_content) |ec| {
        if (std.json.parseFromSlice(std.json.Value, fa, ec, .{ .ignore_unknown_fields = true })) |parsed| {
            defer parsed.deinit();
            if (parsed.value == .object) {
                // Preserve existing "mapping" section as-is.
                if (parsed.value.object.get("mapping")) |m| {
                    if (m == .object) {
                        var mit = m.object.iterator();
                        while (mit.next()) |mentry| {
                            try existing_mapping.put(fa, mentry.key_ptr.*, mentry.value_ptr.*);
                        }
                    }
                }
                // Preserve existing capability_keywords (hand-crafted).
                if (parsed.value.object.get("capability_keywords")) |ck| {
                    if (ck == .object) {
                        var ckit = ck.object.iterator();
                        while (ckit.next()) |ckentry| {
                            try existing_cap_keywords.put(fa, ckentry.key_ptr.*, ckentry.value_ptr.*);
                        }
                    }
                }
            }
        } else |_| {
            std.debug.print("map-capabilities: cannot parse existing {s}, preserving\n", .{mapping_path});
        }
    }

    // Merge: for capabilities without existing keywords, use extracted ones.
    var it = new_cap_keywords.iterator();
    while (it.next()) |entry| {
        const cap_name = entry.key_ptr.*;
        if (!existing_cap_keywords.contains(cap_name)) {
            // Build a json array from the extracted keyword list.
            var arr = std.json.Array.init(fa);
            for (entry.value_ptr.*) |kw| {
                try arr.append(.{ .string = kw });
            }
            try existing_cap_keywords.put(fa, cap_name, .{ .array = arr });
            std.debug.print("map-capabilities: added keywords for new capability '{s}'\n", .{cap_name});
        }
    }

    // ------------------------------------------------------------------
    // Step 3: Build output JSON object and write.
    // ------------------------------------------------------------------
    var mapping_obj_out = std.json.ObjectMap.init(fa);
    var file_to_caps_obj = std.json.ObjectMap.init(fa);
    {
        var mit = existing_mapping.iterator();
        while (mit.next()) |mentry| {
            try file_to_caps_obj.put(mentry.key_ptr.*, mentry.value_ptr.*);
        }
    }
    var cap_kw_obj = std.json.ObjectMap.init(fa);
    {
        var ckit = existing_cap_keywords.iterator();
        while (ckit.next()) |ckentry| {
            try cap_kw_obj.put(ckentry.key_ptr.*, ckentry.value_ptr.*);
        }
    }
    try mapping_obj_out.put("mapping", .{ .object = file_to_caps_obj });
    try mapping_obj_out.put("capability_keywords", .{ .object = cap_kw_obj });

    const json_out = try common.jsonStringifyAlloc(fa, std.json.Value{ .object = mapping_obj_out });

    if (dry_run) {
        std.debug.print("map-capabilities (dry-run): would write {d} bytes to {s}\n", .{ json_out.len, mapping_path });
        return;
    }

    const file = try std.Io.Dir.cwd().createFile(mapping_path, .{});
    defer file.close(io);
    var wbuf: [8192]u8 = undefined;
    var fw = file.writer(&wbuf);
    try fw.interface.writeAll(json_out);
    try fw.interface.flush();

    std.debug.print("map-capabilities: wrote {s} ({d} capabilities)\n", .{ mapping_path, cap_count });
}

/// Stopwords that should be filtered from capability keywords.
const STOPWORDS = [_][]const u8{
    "the",   "a",       "an",    "and",     "or",     "but",    "in",    "on",     "at",    "to",      "for",
    "of",    "with",    "by",    "from",    "as",     "is",     "was",   "are",    "were",  "this",    "that",
    "these", "those",   "it",    "its",     "node",   "key",    "const", "module", "type",  "used",    "each",
    "file",  "files",   "using", "into",    "when",   "where",  "which", "while",  "then",  "than",    "only",
    "over",  "such",    "both",  "through", "during", "before", "after", "above",  "below", "between", "under",
    "again", "further",
};

/// Checks if a given token is a stopword and returns true or false.
fn isStopword(tok: []const u8) bool {
    for (STOPWORDS) |sw| {
        if (std.ascii.eqlIgnoreCase(tok, sw)) return true;
    }
    return false;
}

/// Checks if a token is a capability keyword token, returning true or false.
fn isCapabilityKeywordToken(tok: []const u8) bool {
    // Must start with an ASCII letter or underscore.
    if (tok.len == 0) return false;
    if (!std.ascii.isAlphabetic(tok[0]) and tok[0] != '_') return false;

    // Check for stopwords (case-insensitive).
    if (isStopword(tok)) return false;

    var has_upper = false;
    var has_underscore = false;
    var has_dot = false;

    for (tok) |ch| {
        if (std.ascii.isUpper(ch)) has_upper = true;
        if (ch == '_') has_underscore = true;
        if (ch == '.') has_dot = true;
        // Reject if contains non-identifier characters (except _ and .).
        if (!std.ascii.isAlphanumeric(ch) and ch != '_' and ch != '.') return false;
    }

    // Accept: camelCase/PascalCase (has uppercase), snake_case (has underscore),
    // dotted paths (has dot), or short known identifiers (len >= 4).
    return has_upper or has_underscore or has_dot or tok.len >= 4;
}

pub const CapabilityEntry = struct {
    name: []const u8,
    description: ?[]const u8,
    anchors: []const []const u8,
    keywords: []const []const u8,
    source: []const u8,
};

/// Validates and processes sync capabilities arguments, returning a void result.
pub fn cmdSyncCapabilities(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var guidance_dir_arg: []const u8 = config_mod.DEFAULT_GUIDANCE_DIR;
    var workspace: ?[]const u8 = null;
    var dry_run = false;
    var verbose = false;
    var i: usize = 0;

    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--guidance-dir") or std.mem.eql(u8, arg, "-g")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            guidance_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "--workspace") or std.mem.eql(u8, arg, "-w")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--dry-run")) {
            dry_run = true;
        } else if (std.mem.eql(u8, arg, "--verbose") or std.mem.eql(u8, arg, "-v")) {
            verbose = true;
        }
    }

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const ws = workspace orelse cwd;
    const guidance_abs = try common.resolvePath(allocator, ws, guidance_dir_arg);
    defer allocator.free(guidance_abs);

    const cap_dir = try std.fs.path.join(allocator, &.{ guidance_abs, "capabilities" });
    defer allocator.free(cap_dir);

    const index_path = try std.fs.path.join(allocator, &.{ guidance_abs, "capability-index.json" });
    defer allocator.free(index_path);

    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const fa = arena.allocator();

    if (verbose) {
        std.debug.print("[sync-capabilities] scanning {s}\n", .{cap_dir});
    }

    // ------------------------------------------------------------------
    // Step 1: Walk capabilities dir, parse each CAPABILITY.md.
    // ------------------------------------------------------------------
    var capabilities: std.ArrayList(CapabilityEntry) = .empty;
    defer {
        for (capabilities.items) |cap| {
            fa.free(cap.name);
            if (cap.description) |d| fa.free(d);
            for (cap.anchors) |a| fa.free(a);
            fa.free(cap.anchors);
            for (cap.keywords) |k| fa.free(k);
            fa.free(cap.keywords);
            fa.free(cap.source);
        }
        capabilities.deinit(fa);
    }

    var cap_d = std.Io.Dir.cwd().openDir(std.Io.Threaded.global_single_threaded.io(), cap_dir, .{ .iterate = true }) catch |err| {
        std.debug.print("[sync-capabilities] cannot open {s}: {s}\n", .{ cap_dir, @errorName(err) });
        return;
    };
    defer cap_d.close(std.Io.Threaded.global_single_threaded.io());

    var walker = try cap_d.walk(fa);
    defer walker.deinit();

    var cap_count: usize = 0;
    var cap_with_anchors: usize = 0;
    var cap_without_anchors: usize = 0;

    while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, "CAPABILITY.md")) continue;

        const abs_path = try std.fmt.allocPrint(fa, "{s}/{s}", .{ cap_dir, entry.path });
        const io = std.Io.Threaded.global_single_threaded.io();
        const content = std.Io.Dir.cwd().readFileAlloc(io, abs_path, fa, .limited(512 * 1024)) catch continue;

        // Use doc_parser for unified frontmatter parsing
        const excerpt = doc_parser_mod.parseCapabilityDocContent(fa, content, verbose) catch continue;
        errdefer doc_parser_mod.freeDocExcerpt(fa, excerpt);

        // Default name from directory if not in frontmatter
        const cap_name = if (excerpt.name) |n| n else std.fs.path.basename(std.fs.path.dirname(abs_path) orelse abs_path);

        // Body is content after frontmatter for keyword extraction
        var body: []const u8 = content;
        if (std.mem.startsWith(u8, content, "---\n")) {
            const end = std.mem.indexOf(u8, content[4..], "\n---\n") orelse 0;
            if (end > 0) {
                body = content[4 + end + 5 ..];
            }
        }

        // Extract keywords from body
        var kw_set: std.StringHashMapUnmanaged(void) = .empty;
        defer kw_set.deinit(fa);

        // Add anchors as keywords (high-confidence identifiers)
        for (excerpt.anchors) |anchor| {
            if (!kw_set.contains(anchor)) {
                try kw_set.put(fa, try fa.dupe(u8, anchor), {});
            }
        }

        // Scan body for identifier-like tokens
        var tok_it = std.mem.tokenizeAny(u8, body, " \t\n\r`()[]{}:,;\"'");
        while (tok_it.next()) |tok| {
            if (tok.len < 2 or tok.len > 80) continue;
            if (!isCapabilityKeywordToken(tok)) continue;
            if (!kw_set.contains(tok)) {
                try kw_set.put(fa, try fa.dupe(u8, tok), {});
            }
        }

        // Build keyword list
        var kw_list: std.ArrayList([]const u8) = .empty;
        var kwit = kw_set.keyIterator();
        while (kwit.next()) |k| {
            try kw_list.append(fa, try fa.dupe(u8, k.*));
        }

        // Build anchors list
        var anchors_list: std.ArrayList([]const u8) = .empty;
        for (excerpt.anchors) |a| {
            try anchors_list.append(fa, try fa.dupe(u8, a));
        }

        const cap_entry: CapabilityEntry = .{
            .name = try fa.dupe(u8, cap_name),
            .description = if (excerpt.description) |d| try fa.dupe(u8, d) else null,
            .anchors = try anchors_list.toOwnedSlice(fa),
            .keywords = try kw_list.toOwnedSlice(fa),
            .source = try fa.dupe(u8, entry.path),
        };

        try capabilities.append(fa, cap_entry);
        cap_count += 1;
        if (cap_entry.anchors.len > 0) {
            cap_with_anchors += 1;
        } else {
            cap_without_anchors += 1;
        }

        if (verbose) {
            std.debug.print("[sync-capabilities] parsing {s} → {d} anchors, {d} keywords\n", .{
                entry.path,
                cap_entry.anchors.len,
                cap_entry.keywords.len,
            });
        }
    }

    // ------------------------------------------------------------------
    // Step 2: Build and write capability-index.json.
    // ------------------------------------------------------------------
    var index_obj = std.json.ObjectMap.init(fa, &[_][]const u8{}, &[_]std.json.Value{}) catch unreachable;
    try index_obj.put(fa, "version", .{ .integer = 1 });

    const io = std.Io.Threaded.global_single_threaded.io();
    const timestamp: i128 = @as(i128, std.Io.Timestamp.now(io, .real).nanoseconds);
    const timestamp_str = try std.fmt.allocPrint(fa, "{d}", .{timestamp});
    try index_obj.put(fa, "generated", .{ .string = timestamp_str });

    var capabilities_arr = std.json.Array.init(fa);
    for (capabilities.items) |cap| {
        var cap_obj = std.json.ObjectMap.init(fa, &[_][]const u8{}, &[_]std.json.Value{}) catch unreachable;
        try cap_obj.put(fa, "name", .{ .string = cap.name });
        if (cap.description) |d| {
            try cap_obj.put(fa, "description", .{ .string = d });
        }

        var anchors_arr = std.json.Array.init(fa);
        for (cap.anchors) |a| {
            try anchors_arr.append(.{ .string = a });
        }
        try cap_obj.put(fa, "anchors", .{ .array = anchors_arr });

        var keywords_arr = std.json.Array.init(fa);
        for (cap.keywords) |k| {
            try keywords_arr.append(.{ .string = k });
        }
        try cap_obj.put(fa, "keywords", .{ .array = keywords_arr });

        try cap_obj.put(fa, "source", .{ .string = cap.source });

        try capabilities_arr.append(.{ .object = cap_obj });
    }
    try index_obj.put(fa, "capabilities", .{ .array = capabilities_arr });

    const json_out = try common.jsonStringifyAlloc(fa, std.json.Value{ .object = index_obj });

    if (dry_run) {
        std.debug.print("[sync-capabilities] (dry-run) would write {d} bytes to {s}\n", .{ json_out.len, index_path });
        return;
    }

    // ── Lifecycle detection (compare with previous index) ─────────────────
    _ = reportCapabilityLifecycle(fa, index_path, capabilities.items, verbose) catch null;

    const file = try std.Io.Dir.cwd().createFile(io, index_path, .{});
    defer file.close(io);
    var wbuf: [8192]u8 = undefined;
    var fw = file.writer(io, &wbuf);
    try fw.interface.writeAll(json_out);
    try fw.interface.flush();

    std.debug.print("[sync-capabilities] wrote {s} ({d} capabilities)\n", .{ index_path, cap_count });

    // ------------------------------------------------------------------
    // Step 3: Quality assessment warnings.
    // ------------------------------------------------------------------
    if (cap_without_anchors > 0) {
        std.debug.print("[sync-capabilities] WARN: {d} capabilities have no anchors — cannot auto-discover source files\n", .{cap_without_anchors});
    }

    // Check for empty descriptions
    for (capabilities.items) |cap| {
        if (cap.description == null or cap.description.?.len == 0) {
            if (verbose) {
                std.debug.print("[sync-capabilities] WARN: capability '{s}' has empty description\n", .{cap.name});
            }
        }
    }

    // Check for duplicate names
    var seen_names: std.StringHashMapUnmanaged(void) = .empty;
    defer seen_names.deinit(fa);
    for (capabilities.items) |cap| {
        if (seen_names.contains(cap.name)) {
            std.debug.print("[sync-capabilities] WARN: duplicate capability name '{s}'\n", .{cap.name});
        } else {
            try seen_names.put(fa, cap.name, {});
        }
    }
}

/// Tracks changes in capability counts during allocation lifecycle, returning updated statistics.
pub fn reportCapabilityLifecycle(
    allocator: std.mem.Allocator,
    prev_index_path: []const u8,
    current_caps: []const CapabilityEntry,
    verbose: bool,
) !struct {
    new_count: usize,
    updated_count: usize,
    removed_count: usize,
    unchanged_count: usize,
} {
    var new_count: usize = 0;
    var updated_count: usize = 0;
    var removed_count: usize = 0;
    var unchanged_count: usize = 0;

    // Load previous index if exists
    const prev_content = std.Io.Dir.cwd().readFileAlloc(std.Io.Threaded.global_single_threaded.io(), prev_index_path, allocator, .limited(2 * 1024 * 1024)) catch null;
    defer if (prev_content) |pc| allocator.free(pc);

    var prev_caps: std.StringHashMapUnmanaged(struct {
        anchors: []const []const u8,
    }) = .empty;
    defer {
        var it = prev_caps.iterator();
        while (it.next()) |entry| {
            allocator.free(entry.key_ptr.*);
            for (entry.value_ptr.anchors) |a| allocator.free(a);
            allocator.free(entry.value_ptr.anchors);
        }
        prev_caps.deinit(allocator);
    }

    if (prev_content) |pc| {
        const parsed = std.json.parseFromSlice(std.json.Value, allocator, pc, .{ .ignore_unknown_fields = true }) catch null;
        if (parsed) |p| {
            defer p.deinit();
            if (p.value == .object) {
                if (p.value.object.get("capabilities")) |caps_val| {
                    if (caps_val == .array) {
                        for (caps_val.array.items) |cap_item| {
                            if (cap_item != .object) continue;
                            const cap_obj = cap_item.object;
                            const name = (cap_obj.get("name") orelse continue).string;

                            var anchors_list: std.ArrayList([]const u8) = .empty;
                            if (cap_obj.get("anchors")) |a| {
                                if (a == .array) {
                                    for (a.array.items) |anchor| {
                                        if (anchor == .string) {
                                            try anchors_list.append(allocator, try allocator.dupe(u8, anchor.string));
                                        }
                                    }
                                }
                            }

                            try prev_caps.put(allocator, try allocator.dupe(u8, name), .{
                                .anchors = try anchors_list.toOwnedSlice(allocator),
                            });
                        }
                    }
                }
            }
        }
    }

    // Track which previous capabilities we've seen
    var seen_prev: std.StringHashMapUnmanaged(void) = .empty;
    defer seen_prev.deinit(allocator);

    // Check each current capability against previous
    for (current_caps) |cap| {
        try seen_prev.put(allocator, cap.name, {});

        const prev_entry = prev_caps.get(cap.name);
        if (prev_entry == null) {
            // NEW capability
            new_count += 1;
            if (verbose) {
                std.debug.print("[capabilities] NEW: {s}\n", .{cap.name});
            }
        } else {
            // Check if anchors changed
            const prev = prev_entry.?;
            const anchors_changed = blk: {
                if (cap.anchors.len != prev.anchors.len) break :blk true;
                for (cap.anchors, 0..) |a, i| {
                    if (!std.mem.eql(u8, a, prev.anchors[i])) break :blk true;
                }
                break :blk false;
            };

            if (anchors_changed) {
                updated_count += 1;
                if (verbose) {
                    std.debug.print("[capabilities] UPDATED: {s} (anchors changed)\n", .{cap.name});
                }
            } else {
                unchanged_count += 1;
            }
        }
    }

    // Find removed capabilities
    var prev_it = prev_caps.iterator();
    while (prev_it.next()) |entry| {
        const name = entry.key_ptr.*;
        if (!seen_prev.contains(name)) {
            removed_count += 1;
            if (verbose) {
                std.debug.print("[capabilities] REMOVED: {s}\n", .{name});
            }
        }
    }

    std.debug.print("[capabilities] lifecycle: {d} new, {d} updated, {d} removed, {d} unchanged\n", .{
        new_count,
        updated_count,
        removed_count,
        unchanged_count,
    });

    return .{
        .new_count = new_count,
        .updated_count = updated_count,
        .removed_count = removed_count,
        .unchanged_count = unchanged_count,
    };
}

// ---------------------------------------------------------------------------
// discover-capability-sources — anchor-based source discovery
// ---------------------------------------------------------------------------

/// Parsed source row from AUTO-SOURCES table.
const ParsedSourceRow = struct {
    source_path: []const u8,
    confidence: f32,
    reason: []const u8,
};

/// Prunes dead source file references from AUTO-SOURCES sections in CAPABILITY.md files.
/// Runs before cmdDiscoverCapabilitySources to remove references to files that no longer exist.
fn pruneCapabilitySources(
    allocator: std.mem.Allocator,
    json_dir: []const u8,
    capabilities_dir: []const u8,
    verbose: bool,
) void {
    const workspace_root = std.fs.path.dirname(json_dir) orelse return;

    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const fa = arena.allocator();

    const io = std.Io.Threaded.global_single_threaded.io();
    std.Io.Dir.accessAbsolute(io, capabilities_dir, .{}) catch return;
    var cap_dir = std.Io.Dir.openDirAbsolute(io, capabilities_dir, .{ .iterate = true }) catch return;
    defer cap_dir.close(io);
    var walker = cap_dir.walk(allocator) catch return;
    defer walker.deinit();

    while (walker.next(std.Io.Threaded.global_single_threaded.io()) catch null) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, "CAPABILITY.md")) continue;

        const md_abs = std.fs.path.join(allocator, &.{ capabilities_dir, entry.path }) catch continue;
        defer allocator.free(md_abs);

        const content = std.Io.Dir.cwd().readFileAlloc(std.Io.Threaded.global_single_threaded.io(), md_abs, fa, .limited(512 * 1024)) catch continue;
        const marker = "<!-- AUTO-SOURCES:";
        const marker_pos = std.mem.indexOf(u8, content, marker);

        const auto_section_start = if (marker_pos) |p| p + marker.len else continue;
        const after_marker = content[auto_section_start..];
        const nl_pos = std.mem.indexOfScalar(u8, after_marker, '\n') orelse 0;
        const table_content = common.trimLeft(u8, after_marker[nl_pos..], "\n ");

        var rows = std.mem.splitScalar(u8, table_content, '\n');
        var parsed_rows: std.ArrayList(ParsedSourceRow) = .empty;
        defer parsed_rows.deinit(fa);

        var header_found = false;
        while (rows.next()) |row| {
            const trimmed = std.mem.trim(u8, row, " \t\r");
            if (trimmed.len == 0) continue;
            if (!header_found) {
                if (std.mem.startsWith(u8, trimmed, "| File")) header_found = true;
                continue;
            }
            if (!std.mem.startsWith(u8, trimmed, "|")) continue;
            if (std.mem.startsWith(u8, trimmed, "|------")) continue;

            const inner = trimmed[1..];
            const last_bar = std.mem.lastIndexOfScalar(u8, inner, '|') orelse continue;
            const first_cell = std.mem.trim(u8, inner[0..last_bar], " \t|");
            const second_cell = std.mem.trim(u8, inner[last_bar + 1 ..], " \t|");

            const path_start = std.mem.indexOfScalar(u8, first_cell, '`') orelse continue;
            const path_end = std.mem.lastIndexOfScalar(u8, first_cell, '`') orelse continue;
            if (path_end <= path_start) continue;
            const source_path = first_cell[path_start + 1 .. path_end];

            const conf = std.fmt.parseFloat(f32, std.mem.trim(u8, second_cell, " \t")) catch continue;
            const reason = std.mem.trim(u8, inner[last_bar + 1 ..], " \t|");

            parsed_rows.append(fa, .{
                .source_path = fa.dupe(u8, source_path) catch continue,
                .confidence = conf,
                .reason = fa.dupe(u8, reason) catch continue,
            }) catch continue;
        }

        var filtered_rows: std.ArrayList(ParsedSourceRow) = .empty;
        defer filtered_rows.deinit(fa);
        var removed_count: usize = 0;

        for (parsed_rows.items) |row| {
            const full_path = std.fs.path.join(fa, &.{ workspace_root, row.source_path }) catch continue;
            std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), full_path, .{}) catch {
                removed_count += 1;
                continue;
            };
            filtered_rows.append(fa, row) catch continue;
        }

        if (removed_count == 0) continue;

        if (verbose) {
            const cap_name = std.fs.path.basename(std.fs.path.dirname(md_abs) orelse md_abs);
            std.debug.print("[prune] {s}: removed {d} dead source(s)\n", .{ cap_name, removed_count });
        }

        const deduped = pruneCapabilitySourcesDedup(fa, filtered_rows.items) catch continue;
        const md_abs_alloc = allocator.dupe(u8, md_abs) catch continue;
        defer allocator.free(md_abs_alloc);
        updateCapabilitySourcesSection(allocator, md_abs_alloc, deduped, verbose) catch |err| {
            if (verbose) std.debug.print("[prune] WARN: failed to update {s}: {s}\n", .{ md_abs, @errorName(err) });
        };
    }
}

/// Removes duplicate source rows from a list, returning a cleaned DiscoveredSource slice.
fn pruneCapabilitySourcesDedup(allocator: std.mem.Allocator, rows: []const ParsedSourceRow) ![]const DiscoveredSource {
    var seen: std.StringHashMapUnmanaged(usize) = .empty;
    defer seen.deinit(allocator);
    var result: std.ArrayList(DiscoveredSource) = .empty;
    errdefer result.deinit(allocator);

    for (rows) |row| {
        if (seen.get(row.source_path)) |idx| {
            if (row.confidence > result.items[idx].confidence) {
                result.items[idx] = .{ .capability_name = "", .source_path = row.source_path, .confidence = row.confidence, .reason = row.reason };
            }
        } else {
            try seen.put(allocator, row.source_path, result.items.len);
            try result.append(allocator, .{ .capability_name = "", .source_path = row.source_path, .confidence = row.confidence, .reason = row.reason });
        }
    }
    return result.toOwnedSlice(allocator);
}

/// Manages confidence level metadata; owned by the sync engine; ensures invariant accuracy across runs.
const ConfidenceLevels = struct {
    const defines_anchor: f32 = 1.0;
    const used_by: f32 = 0.9;
    const keyword_overlap: f32 = 0.7;
    const pattern_match: f32 = 0.6;
    const path_heuristic: f32 = 0.4;
};

const DiscoveredSource = struct {
    capability_name: []const u8,
    source_path: []const u8,
    confidence: f32,
    reason: []const u8,
};

/// Updates the capability sources section with provided allocator, paths, sources, and verbosity flag.
fn updateCapabilitySourcesSection(
    allocator: std.mem.Allocator,
    cap_md_path: []const u8,
    sources: []const DiscoveredSource,
    verbose: bool,
) !void {
    // Read existing content.
    const io = std.Io.Threaded.global_single_threaded.io();
    const content = std.Io.Dir.cwd().readFileAlloc(io, cap_md_path, allocator, .limited(512 * 1024)) catch return;
    defer allocator.free(content);

    const marker = "<!-- AUTO-SOURCES:";
    const marker_pos = std.mem.indexOf(u8, content, marker);
    const base = if (marker_pos) |p| content[0..p] else content;
    const base_trimmed = common.trimRight(u8, base, " \t\r\n");

    // Deduplicate by source_path, keeping highest confidence per path.
    var seen_paths: std.StringHashMapUnmanaged(usize) = .empty;
    defer seen_paths.deinit(allocator);
    var deduped: std.ArrayList(DiscoveredSource) = .empty;
    defer deduped.deinit(allocator);
    for (sources) |src| {
        if (seen_paths.get(src.source_path)) |idx| {
            if (src.confidence > deduped.items[idx].confidence) {
                deduped.items[idx] = src;
            }
        } else {
            try seen_paths.put(allocator, src.source_path, deduped.items.len);
            try deduped.append(allocator, src);
        }
    }

    var out_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer out_aw.deinit();
    const w = &out_aw.writer;

    try w.writeAll(base_trimmed);
    try w.writeAll("\n\n<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->\n");
    try w.print("## Sources ({d} file{s}, auto-discovered)\n\n", .{
        deduped.items.len,
        if (deduped.items.len == 1) @as([]const u8, "") else "s",
    });
    try w.writeAll("| File | Confidence | Reason |\n");
    try w.writeAll("|------|-----------|--------|\n");
    for (deduped.items) |src| {
        try w.print("| `{s}` | {d:.1} | {s} |\n", .{ src.source_path, src.confidence, src.reason });
    }
    try w.writeAll("\n");

    const file = try std.Io.Dir.cwd().createFile(io, cap_md_path, .{});
    defer file.close(io);
    var wbuf: [8192]u8 = undefined;
    var fw = file.writer(io, &wbuf);
    try fw.interface.writeAll(out_aw.written());
    try fw.interface.flush();

    if (verbose) {
        const dir_name = std.fs.path.basename(std.fs.path.dirname(cap_md_path) orelse cap_md_path);
        std.debug.print("[capabilities] updated sources section in {s}/CAPABILITY.md ({d} files)\n", .{ dir_name, deduped.items.len });
    }
}

/// Processes allocation parameters to discover capability sources for the sync engine.
pub fn cmdDiscoverCapabilitySources(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var guidance_dir_arg: []const u8 = config_mod.DEFAULT_GUIDANCE_DIR;
    var db_path: []const u8 = config_mod.DEFAULT_DB_PATH;
    var verbose = false;
    var i: usize = 0;

    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--guidance-dir") or std.mem.eql(u8, arg, "-g")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            guidance_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "--db") or std.mem.eql(u8, arg, "-o")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            db_path = args[i];
        } else if (std.mem.eql(u8, arg, "--verbose") or std.mem.eql(u8, arg, "-v")) {
            verbose = true;
        }
    }

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const guidance_abs = try common.resolvePath(allocator, cwd, guidance_dir_arg);
    defer allocator.free(guidance_abs);

    const db_abs = try common.resolvePath(allocator, cwd, db_path);
    defer allocator.free(db_abs);

    const cap_index_path = try std.fs.path.join(allocator, &.{ guidance_abs, "capability-index.json" });
    defer allocator.free(cap_index_path);

    const src_dir_path = try std.fs.path.join(allocator, &.{ guidance_abs, "src" });
    defer allocator.free(src_dir_path);

    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const fa = arena.allocator();

    if (verbose) {
        std.debug.print("[discover] loading capability index from {s}\n", .{cap_index_path});
    }

    // Load capability index
    const io = std.Io.Threaded.global_single_threaded.io();
    const cap_index_content = std.Io.Dir.cwd().readFileAlloc(io, cap_index_path, fa, .limited(2 * 1024 * 1024)) catch |err| {
        std.debug.print("[discover] ERROR: cannot read {s}: {s}\n", .{ cap_index_path, @errorName(err) });
        std.debug.print("[discover] HINT: run 'guidance sync-capabilities' first\n", .{});
        return;
    };

    const parsed = std.json.parseFromSlice(std.json.Value, fa, cap_index_content, .{ .ignore_unknown_fields = true }) catch |err| {
        std.debug.print("[discover] ERROR: cannot parse capability index: {s}\n", .{@errorName(err)});
        return;
    };
    defer parsed.deinit();

    const capabilities_arr = parsed.value.object.get("capabilities") orelse {
        std.debug.print("[discover] ERROR: capability index missing 'capabilities' array\n", .{});
        return;
    };
    if (capabilities_arr != .array) {
        std.debug.print("[discover] ERROR: capabilities is not an array\n", .{});
        return;
    }

    // Build anchor -> capability map
    var anchor_to_cap: std.StringHashMapUnmanaged([]const u8) = .empty;
    defer anchor_to_cap.deinit(fa);

    var cap_keywords: std.StringHashMapUnmanaged([]const []const u8) = .empty;
    defer cap_keywords.deinit(fa);

    // M5.4: map capability name → source field (relative MD path within cap_dir)
    var cap_name_to_md_rel: std.StringHashMapUnmanaged([]const u8) = .empty;
    defer cap_name_to_md_rel.deinit(fa);

    for (capabilities_arr.array.items) |cap_item| {
        if (cap_item != .object) continue;
        const cap_obj = cap_item.object;
        const cap_name = (cap_obj.get("name") orelse continue).string;
        const anchors_arr = if (cap_obj.get("anchors")) |a| a.array else continue;

        for (anchors_arr.items) |anchor| {
            if (anchor != .string) continue;
            try anchor_to_cap.put(fa, try fa.dupe(u8, anchor.string), try fa.dupe(u8, cap_name));
        }

        // Store keywords for this capability
        const kw_val = cap_obj.get("keywords") orelse continue;
        if (kw_val != .array) continue;
        var kw_list: std.ArrayList([]const u8) = .empty;
        for (kw_val.array.items) |kw| {
            if (kw != .string) continue;
            try kw_list.append(fa, try fa.dupe(u8, kw.string));
        }
        try cap_keywords.put(fa, try fa.dupe(u8, cap_name), try kw_list.toOwnedSlice(fa));

        // Store source path for auto-sources section updates (M5.4)
        if (cap_obj.get("source")) |sv| {
            if (sv == .string) {
                try cap_name_to_md_rel.put(fa, try fa.dupe(u8, cap_name), try fa.dupe(u8, sv.string));
            }
        }
    }

    if (verbose) {
        std.debug.print("[discover] loaded {d} anchors from {d} capabilities\n", .{ anchor_to_cap.size, capabilities_arr.array.items.len });
    }

    // Load guidance JSON files and extract members
    var member_to_file: std.StringHashMapUnmanaged([]const u8) = .empty;
    defer member_to_file.deinit(fa);

    var file_to_members: std.StringHashMapUnmanaged([]const []const u8) = .empty;
    defer file_to_members.deinit(fa);

    var file_to_used_by: std.StringHashMapUnmanaged([]const []const u8) = .empty;
    defer file_to_used_by.deinit(fa);

    var src_dir = std.Io.Dir.cwd().openDir(std.Io.Threaded.global_single_threaded.io(), src_dir_path, .{ .iterate = true }) catch |err| {
        std.debug.print("[discover] ERROR: cannot open {s}: {s}\n", .{ src_dir_path, @errorName(err) });
        return;
    };
    defer src_dir.close(io);

    var walker = try src_dir.walk(fa);
    defer walker.deinit();

    var file_count: usize = 0;
    while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, ".json")) continue;

        const json_path = try std.fs.path.join(fa, &.{ src_dir_path, entry.path });
        const json_content = std.Io.Dir.cwd().readFileAlloc(io, json_path, fa, .limited(10 * 1024 * 1024)) catch continue;

        const json_parsed = std.json.parseFromSlice(std.json.Value, fa, json_content, .{ .ignore_unknown_fields = true }) catch continue;
        defer json_parsed.deinit();

        if (json_parsed.value != .object) continue;
        const json_obj = json_parsed.value.object;

        // Get source from meta object
        const rel_path = blk: {
            const meta_val = json_obj.get("meta") orelse continue;
            if (meta_val != .object) continue;
            const source_val = meta_val.object.get("source") orelse continue;
            if (source_val != .string) continue;
            break :blk source_val.string;
        };

        var members: std.ArrayList([]const u8) = .empty;
        if (json_obj.get("members")) |m| {
            if (m == .array) {
                for (m.array.items) |member| {
                    if (member != .object) continue;
                    const name_val = member.object.get("name") orelse continue;
                    if (name_val != .string) continue;
                    try members.append(fa, try fa.dupe(u8, name_val.string));
                }
            }
        }

        if (members.items.len > 0) {
            // First populate member_to_file, then toOwnedSlice
            for (members.items) |mbr| {
                try member_to_file.put(fa, mbr, try fa.dupe(u8, rel_path));
            }
            try file_to_members.put(fa, try fa.dupe(u8, rel_path), try members.toOwnedSlice(fa));
        }

        // Extract used_by
        if (json_obj.get("used_by")) |ub| {
            if (ub == .array) {
                var used_by_list: std.ArrayList([]const u8) = .empty;
                for (ub.array.items) |u| {
                    if (u != .string) continue;
                    try used_by_list.append(fa, try fa.dupe(u8, u.string));
                }
                if (used_by_list.items.len > 0) {
                    try file_to_used_by.put(fa, try fa.dupe(u8, rel_path), try used_by_list.toOwnedSlice(fa));
                }
            }
        }

        file_count += 1;
    }

    if (verbose) {
        std.debug.print("[discover] indexed {d} guidance JSON files\n", .{file_count});
    }

    // Open database
    var noop_embedder = common.NoopEmbedding{};
    const provider = noop_embedder.provider();
    var db = GuidanceDb.init(allocator, db_abs, provider) catch |err| {
        std.debug.print("[discover] ERROR: cannot open database {s}: {s}\n", .{ db_abs, @errorName(err) });
        return;
    };
    defer db.deinit();

    // Build capability -> sources map
    var cap_to_sources: std.StringHashMapUnmanaged(std.ArrayList(DiscoveredSource)) = .{};
    defer {
        var it = cap_to_sources.iterator();
        while (it.next()) |entry| {
            entry.value_ptr.deinit(fa);
        }
        cap_to_sources.deinit(fa);
    }

    var cap_stats: struct {
        anchor_defining: usize = 0,
        used_by: usize = 0,
        keyword_overlap: usize = 0,
        path_heuristic: usize = 0,
    } = .{};

    // Initialize source lists for each capability
    var cap_it = cap_keywords.iterator();
    while (cap_it.next()) |entry| {
        const sources_list: std.ArrayList(DiscoveredSource) = .empty;
        try cap_to_sources.put(fa, entry.key_ptr.*, sources_list);
    }

    // Pass 1: Anchor match (1.0)
    var anchor_it = anchor_to_cap.iterator();
    while (anchor_it.next()) |entry| {
        const anchor = entry.key_ptr.*;
        const cap_name = entry.value_ptr.*;

        // Find file that defines this anchor
        if (member_to_file.get(anchor)) |source_file| {
            const ds: DiscoveredSource = .{
                .capability_name = try fa.dupe(u8, cap_name),
                .source_path = try fa.dupe(u8, source_file),
                .confidence = ConfidenceLevels.defines_anchor,
                .reason = "defines_anchor",
            };
            if (cap_to_sources.getPtr(cap_name)) |sources_list| {
                try sources_list.append(fa, ds);
            }
            cap_stats.anchor_defining += 1;
        }
    }

    // Pass 2: Reverse import traversal (0.9)
    cap_it = cap_keywords.iterator();
    while (cap_it.next()) |entry| {
        const cap_name = entry.key_ptr.*;

        // Find anchor-defining files for this capability
        var anchor_files: std.StringHashMapUnmanaged(void) = .empty;
        defer anchor_files.deinit(fa);

        anchor_it = anchor_to_cap.iterator();
        while (anchor_it.next()) |ae| {
            if (std.mem.eql(u8, ae.value_ptr.*, cap_name)) {
                if (member_to_file.get(ae.key_ptr.*)) |af| {
                    try anchor_files.put(fa, af, {});
                }
            }
        }

        // For each anchor-defining file, find files that import it (used_by)
        var af_it = anchor_files.iterator();
        while (af_it.next()) |af_entry| {
            const anchor_file = af_entry.key_ptr.*;
            if (file_to_used_by.get(anchor_file)) |users| {
                for (users) |user_file| {
                    const ds: DiscoveredSource = .{
                        .capability_name = try fa.dupe(u8, cap_name),
                        .source_path = try fa.dupe(u8, user_file),
                        .confidence = ConfidenceLevels.used_by,
                        .reason = "used_by",
                    };
                    if (cap_to_sources.getPtr(cap_name)) |sources_list| {
                        // Check for duplicates
                        var is_dup = false;
                        for (sources_list.items) |existing| {
                            if (std.mem.eql(u8, existing.source_path, user_file)) {
                                is_dup = true;
                                break;
                            }
                        }
                        if (!is_dup) {
                            try sources_list.append(fa, ds);
                            cap_stats.used_by += 1;
                        }
                    }
                }
            }
        }
    }

    // Pass 3: Keyword overlap (0.7)
    cap_it = cap_keywords.iterator();
    while (cap_it.next()) |entry| {
        const cap_name = entry.key_ptr.*;
        const keywords = entry.value_ptr.*;

        if (keywords.len < 3) continue; // Skip if less than 3 keywords

        // For each guidance file, count keyword overlap
        var file_it = file_to_members.iterator();
        while (file_it.next()) |file_entry| {
            const source_file = file_entry.key_ptr.*;
            const file_members = file_entry.value_ptr.*;

            // Count keyword overlap
            var overlap: usize = 0;
            for (keywords) |kw| {
                for (file_members) |mbr| {
                    if (std.mem.eql(u8, kw, mbr)) {
                        overlap += 1;
                        break;
                    }
                }
            }

            if (overlap >= 3) {
                const ds: DiscoveredSource = .{
                    .capability_name = try fa.dupe(u8, cap_name),
                    .source_path = try fa.dupe(u8, source_file),
                    .confidence = ConfidenceLevels.keyword_overlap,
                    .reason = "keyword_overlap",
                };
                if (cap_to_sources.getPtr(cap_name)) |sources_list| {
                    // Check for duplicates
                    var is_dup = false;
                    for (sources_list.items) |existing| {
                        if (std.mem.eql(u8, existing.source_path, source_file)) {
                            is_dup = true;
                            break;
                        }
                    }
                    if (!is_dup) {
                        try sources_list.append(fa, ds);
                        cap_stats.keyword_overlap += 1;
                    }
                }
            }
        }
    }

    // Pass 5: Path heuristic (0.4)
    cap_it = cap_keywords.iterator();
    while (cap_it.next()) |entry| {
        const cap_name = entry.key_ptr.*;
        // Normalize capability name for path matching (kebab-case to words)
        const cap_lower = try std.ascii.allocLowerString(fa, cap_name);
        defer fa.free(cap_lower);

        // Extract key part of capability name (e.g., "embedding" from "embedding-providers")
        var cap_key: []const u8 = cap_lower;
        if (std.mem.indexOf(u8, cap_lower, "-")) |dash_pos| {
            cap_key = cap_lower[0..dash_pos];
        }

        var file_it = file_to_members.iterator();
        while (file_it.next()) |file_entry| {
            const source_file = file_entry.key_ptr.*;
            const file_lower = try std.ascii.allocLowerString(fa, source_file);
            defer fa.free(file_lower);

            if (std.mem.indexOf(u8, file_lower, cap_key) != null) {
                const ds: DiscoveredSource = .{
                    .capability_name = try fa.dupe(u8, cap_name),
                    .source_path = try fa.dupe(u8, source_file),
                    .confidence = ConfidenceLevels.path_heuristic,
                    .reason = "path_heuristic",
                };
                if (cap_to_sources.getPtr(cap_name)) |sources_list| {
                    // Check for duplicates
                    var is_dup = false;
                    for (sources_list.items) |existing| {
                        if (std.mem.eql(u8, existing.source_path, source_file)) {
                            is_dup = true;
                            break;
                        }
                    }
                    if (!is_dup) {
                        try sources_list.append(fa, ds);
                        cap_stats.path_heuristic += 1;
                    }
                }
            }
        }
    }

    // Upsert into database
    var total_joins: usize = 0;
    var cap_count: usize = 0;

    var src_it = cap_to_sources.iterator();
    while (src_it.next()) |entry| {
        const cap_name = entry.key_ptr.*;
        const sources_list = entry.value_ptr.*;

        // Clear existing mappings for this capability
        db.clearCapabilitySources(cap_name) catch {
            // Non-fatal
        };

        // Insert new mappings
        for (sources_list.items) |src| {
            _ = db.upsertCapabilitySource(cap_name, src.source_path, src.confidence, src.reason) catch {
                // Non-fatal
                continue;
            };
            total_joins += 1;
        }

        cap_count += 1;

        if (verbose and sources_list.items.len > 0) {
            std.debug.print("[discover] {s}: {d} sources\n", .{ cap_name, sources_list.items.len });
        }

        // M5.4: Update auto-generated Sources section in the CAPABILITY.md file.
        if (cap_name_to_md_rel.get(cap_name)) |md_rel| {
            const cap_dir_path = try std.fs.path.join(fa, &.{ guidance_abs, "capabilities" });
            const md_abs = try std.fs.path.join(fa, &.{ cap_dir_path, md_rel });
            updateCapabilitySourcesSection(fa, md_abs, sources_list.items, verbose) catch |err| {
                if (verbose) std.debug.print("[capabilities] WARN: cannot update sources in {s}: {s}\n", .{ md_rel, @errorName(err) });
            };
        }
    }

    std.debug.print("[discover] wrote {d} capability_sources joins ({d} capabilities, {d} unique source files)\n", .{ total_joins, cap_count, file_count });
    if (verbose) {
        std.debug.print("[discover] confidence distribution: 1.0={d}, 0.9={d}, 0.7={d}, 0.4={d}\n", .{
            cap_stats.anchor_defining,
            cap_stats.used_by,
            cap_stats.keyword_overlap,
            cap_stats.path_heuristic,
        });
    }
}

// =============================================================================
// sync-comments — insert/update /// doc comments in source files
// =============================================================================

/// Processes a Zig source file to extract and return sync-comment lines.
pub fn cmdSyncComments(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var workspace: ?[]const u8 = null;
    var guidance_dir: ?[]const u8 = null;
    var single_file: ?[]const u8 = null;
    var dry_run = false;
    var debug_mode = false;
    var gen_headers = false;
    var no_llm = false;
    var api_url: []const u8 = config_mod.DEFAULT_API_URL;
    var api_url_set = false;
    var model: []const u8 = config_mod.DEFAULT_MODEL;
    var model_override = false;

    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --workspace requires a value\n", .{});
                return;
            }
            workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--guidance-dir")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --guidance-dir requires a value\n", .{});
                return;
            }
            guidance_dir = args[i];
        } else if (std.mem.eql(u8, arg, "--file")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --file requires a value\n", .{});
                return;
            }
            single_file = args[i];
        } else if (std.mem.eql(u8, arg, "--dry-run")) {
            dry_run = true;
        } else if (std.mem.eql(u8, arg, "--verbose") or std.mem.eql(u8, arg, "--debug")) {
            debug_mode = true;
        } else if (std.mem.eql(u8, arg, "--headers")) {
            gen_headers = true;
        } else if (std.mem.eql(u8, arg, "--no-llm")) {
            no_llm = true;
        } else if (std.mem.eql(u8, arg, "--api-url")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --api-url requires a value\n", .{});
                return;
            }
            api_url = args[i];
            api_url_set = true;
        } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --model requires a value\n", .{});
                return;
            }
            model = args[i];
            model_override = true;
        }
    }

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const ws = try common.resolvePath(allocator, cwd, workspace orelse cwd);
    defer allocator.free(ws);

    const gdir = try common.resolvePath(allocator, ws, guidance_dir orelse config_mod.DEFAULT_GUIDANCE_DIR);
    defer allocator.free(gdir);

    const src_json_dir = try std.fs.path.join(allocator, &.{ gdir, "src" });
    defer allocator.free(src_json_dir);

    var processor = comment_sync_mod.CommentSyncProcessor.init(allocator, ws, src_json_dir, debug_mode, dry_run);
    processor.generate_headers = gen_headers;

    // Wire up the LLM enhancer unless --no-llm was passed.
    if (!no_llm) {
        var cfg = config_mod.loadConfig(allocator, ws) catch
            try config_mod.loadConfig(allocator, cwd);
        defer cfg.deinit();
        const ga_for_enh: GenArgs = .{
            .api_url = api_url,
            .api_url_set = api_url_set,
            .model = model,
            .model_override = model_override,
            .verbose = debug_mode,
            .no_llm = false,
        };
        gen_files_mod.setupCspEnhancer(allocator, ga_for_enh, &cfg, &processor);
    }
    defer gen_files_mod.teardownCspEnhancer(allocator, &processor);

    var total_added: usize = 0;
    var total_regen: usize = 0;
    var total_files: usize = 0;

    if (single_file) |fp| {
        const abs_fp = try common.resolvePath(allocator, cwd, fp);
        defer allocator.free(abs_fp);

        const r = processor.processFile(abs_fp) catch |err| {
            std.debug.print("error processing {s}: {s}\n", .{ fp, @errorName(err) });
            return;
        };
        total_added += r.comments_added;
        total_regen += r.comments_regenerated;
        total_files += 1;
        if (r.has_changes) {
            const action = if (dry_run) "[dry-run] " else "";
            std.debug.print("{s}{s}: +{} comments, {} regenerated{s}\n", .{
                action,
                fp,
                r.comments_added,
                r.comments_regenerated,
                if (r.header_added) ", header added" else "",
            });
        }
    } else {
        // Scan all .zig files under workspace/src.
        const src_dir_path = try std.fs.path.join(allocator, &.{ ws, "src" });
        defer allocator.free(src_dir_path);

        const io = std.Io.Threaded.global_single_threaded.io();
        var src_dir = std.Io.Dir.openDirAbsolute(io, src_dir_path, .{ .iterate = true }) catch {
            std.debug.print("error: cannot open src dir: {s}\n", .{src_dir_path});
            return;
        };
        defer src_dir.close(io);

        var walker = try src_dir.walk(allocator);
        defer walker.deinit();

        while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
            if (entry.kind != .file) continue;
            if (!std.mem.endsWith(u8, entry.path, ".zig")) continue;

            const abs_path = try std.fs.path.join(allocator, &.{ src_dir_path, entry.path });
            defer allocator.free(abs_path);

            const r = processor.processFile(abs_path) catch continue;
            total_added += r.comments_added;
            total_regen += r.comments_regenerated;
            total_files += 1;

            if (r.has_changes and debug_mode) {
                std.debug.print("  {s}: +{} comments, {} regenerated\n", .{
                    entry.path,
                    r.comments_added,
                    r.comments_regenerated,
                });
            }
        }
    }

    const action = if (dry_run) "Would add" else "Added";
    std.debug.print("{s} {} comment(s) across {} file(s); {} regenerated.\n", .{
        action,
        total_added,
        total_files,
        total_regen,
    });
}

// =============================================================================
// migrate-comments — migrate JSON comment fields to source /// comments
// =============================================================================

/// Processes and migrates comment data during sync operations.
pub fn cmdMigrateComments(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var workspace: ?[]const u8 = null;
    var guidance_dir: ?[]const u8 = null;
    var dry_run = false;

    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --workspace requires a value\n", .{});
                return;
            }
            workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--guidance-dir")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --guidance-dir requires a value\n", .{});
                return;
            }
            guidance_dir = args[i];
        } else if (std.mem.eql(u8, arg, "--dry-run")) {
            dry_run = true;
        }
    }

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const ws = try common.resolvePath(allocator, cwd, workspace orelse cwd);
    defer allocator.free(ws);

    const gdir = try common.resolvePath(allocator, ws, guidance_dir orelse config_mod.DEFAULT_GUIDANCE_DIR);
    defer allocator.free(gdir);

    const src_json_dir = try std.fs.path.join(allocator, &.{ gdir, "src" });
    defer allocator.free(src_json_dir);

    var store = json_store_mod.JsonStore.init(allocator);

    var json_dir = std.Io.Dir.openDirAbsolute(std.Io.Threaded.global_single_threaded.io(), src_json_dir, .{ .iterate = true }) catch {
        std.debug.print("error: cannot open guidance src dir: {s}\n", .{src_json_dir});
        return;
    };
    defer json_dir.close();

    var walker = try json_dir.walk(allocator);
    defer walker.deinit();

    var migrated_files: usize = 0;
    var migrated_members: usize = 0;

    while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, ".json")) continue;

        const json_path = try std.fs.path.join(allocator, &.{ src_json_dir, entry.path });
        defer allocator.free(json_path);

        var doc = (try store.loadGuidance(json_path)) orelse continue;
        defer doc.arena.deinit();

        // Only process Zig files.
        if (!std.mem.endsWith(u8, doc.meta.source, ".zig")) continue;

        const abs_source = try std.fs.path.join(allocator, &.{ ws, doc.meta.source });
        defer allocator.free(abs_source);

        // Check if source file exists.
        std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), abs_source, .{}) catch continue;

        // For each member that has a JSON comment but lacks a source comment,
        // insert the JSON comment into the source file.
        const processor = comment_sync_mod.CommentSyncProcessor.init(
            allocator,
            ws,
            src_json_dir,
            false,
            dry_run,
        );

        const source = common.readFileAlloc(allocator, abs_source, 10 * 1024 * 1024) orelse continue;
        defer allocator.free(source);

        var source_changed = false;
        var current_source: []const u8 = try allocator.dupe(u8, source);
        defer allocator.free(current_source);

        // Process in reverse line order to preserve positions.
        const sorted_members = blk: {
            const m = try allocator.dupe(types.Member, doc.members);
            std.mem.sort(types.Member, m, {}, struct {
                fn gt(_: void, a: types.Member, b: types.Member) bool {
                    return (a.line orelse 0) > (b.line orelse 0);
                }
            }.gt);
            break :blk m;
        };
        defer allocator.free(sorted_members);

        for (sorted_members) |member| {
            const decl_line = member.line orelse continue;
            const json_comment = member.comment orelse continue;
            if (json_comment.len == 0) continue;

            // Check if source already has a comment at this line.
            const existing = try comment_inserter_mod.extractCommentAtLine(
                allocator,
                current_source,
                decl_line,
            );
            if (existing != null) {
                allocator.free(existing.?);
                continue; // Source already has a comment — don't overwrite.
            }

            if (!dry_run) {
                const ins = try comment_inserter_mod.insertComment(
                    allocator,
                    current_source,
                    decl_line,
                    json_comment,
                );
                if (ins.changed) {
                    allocator.free(current_source);
                    current_source = ins.new_source;
                    allocator.free(ins.line_adjustments);
                    source_changed = true;
                } else {
                    ins.deinit(allocator);
                }
            }
            migrated_members += 1;
        }

        _ = processor; // used for type context only

        if (source_changed) {
            const io = std.Io.Threaded.global_single_threaded.io();
            const file = try std.Io.Dir.createFileAbsolute(io, abs_source, .{ .truncate = true });
            defer file.close(io);
            try file.writeAll(current_source);
            migrated_files += 1;
        } else if (dry_run and migrated_members > 0) {
            migrated_files += 1;
        }
    }

    const action = if (dry_run) "Would migrate" else "Migrated";
    std.debug.print("{s} {} comment(s) from JSON to source in {} file(s).\n", .{
        action,
        migrated_members,
        migrated_files,
    });
}

// ---------------------------------------------------------------------------
// todo — work item lifecycle
// ---------------------------------------------------------------------------

/// Processes a Zig command string, validating input and preparing execution context.
pub fn cmdTodo(allocator: std.mem.Allocator, args: []const []const u8) !void {
    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const guidance_dir = config_mod.DEFAULT_GUIDANCE_DIR;
    const todo_dir = try std.fs.path.join(allocator, &.{ cwd, guidance_dir, "todo" });
    defer allocator.free(todo_dir);

    // Load config for LLM access.
    var api_url: []const u8 = config_mod.DEFAULT_API_URL;
    var model_thinking: []const u8 = "";
    var model_fast: []const u8 = "";

    if (config_mod.loadConfig(allocator, cwd)) |cfg_val| {
        var cfg = cfg_val;
        defer cfg.deinit();
        const parsed = config_mod.ProjectConfig.parseModelRef(cfg.model_thinking);
        if (parsed) |p| {
            if (cfg.getProvider(p.provider)) |provider| {
                api_url = try std.fmt.allocPrint(allocator, "{s}{s}", .{ provider.base_url, provider.chat_endpoint });
            }
        }
        model_thinking = try allocator.dupe(u8, cfg.model_thinking);
        model_fast = try allocator.dupe(u8, cfg.model_fast);
    } else |_| {}
    defer if (model_thinking.len > 0) allocator.free(model_thinking);
    defer if (model_fast.len > 0) allocator.free(model_fast);
    defer if (!std.mem.eql(u8, api_url, config_mod.DEFAULT_API_URL)) allocator.free(api_url);

    if (args.len == 0) {
        std.debug.print("Usage: guidance todo <new|triage|checklist|status|list|abandon>\n", .{});
        return;
    }

    const subcmd = args[0];
    const sub_args = args[1..];

    if (std.mem.eql(u8, subcmd, "new")) {
        const description = if (sub_args.len > 0) sub_args[0] else "";
        return todo_mod.cmdTodoNew(allocator, description, todo_dir);
    } else if (std.mem.eql(u8, subcmd, "triage")) {
        const thinking_model = if (model_thinking.len > 0) model_thinking else model_fast;
        return todo_mod.cmdTodoTriage(allocator, todo_dir, api_url, thinking_model);
    } else if (std.mem.eql(u8, subcmd, "checklist")) {
        const fast_model = if (model_fast.len > 0) model_fast else model_thinking;
        return todo_mod.cmdTodoChecklist(allocator, todo_dir, api_url, fast_model);
    } else if (std.mem.eql(u8, subcmd, "status")) {
        return todo_mod.cmdTodoStatus(allocator, todo_dir);
    } else if (std.mem.eql(u8, subcmd, "list")) {
        return todo_mod.cmdTodoList(allocator, todo_dir);
    } else if (std.mem.eql(u8, subcmd, "abandon")) {
        return todo_mod.cmdTodoAbandon(allocator, todo_dir);
    } else if (std.mem.eql(u8, subcmd, "run")) {
        const subagent_mod = @import("subagent");
        const workspace = cwd;
        const db_path = try std.fs.path.join(allocator, &.{ cwd, ".guidance.db" });
        defer allocator.free(db_path);
        var max_iterations: u16 = 20;
        var allow_edit = false;
        var i: usize = 0;
        while (i < sub_args.len) : (i += 1) {
            if (std.mem.eql(u8, sub_args[i], "--max-iterations") and i + 1 < sub_args.len) {
                i += 1;
                max_iterations = std.fmt.parseInt(u16, sub_args[i], 10) catch 20;
            } else if (std.mem.eql(u8, sub_args[i], "--allow-edit")) {
                allow_edit = true;
            }
        }
        var result = try subagent_mod.cmdTodoRun(allocator, workspace, db_path, guidance_dir, api_url, if (model_fast.len > 0) model_fast else model_thinking, max_iterations, allow_edit);
        defer result.deinit(allocator);
        std.debug.print("Subagent {s}: {d}/{d} items completed in {d} iterations ({d} LLM calls, {d} deterministic)\n", .{
            @tagName(result.status),
            result.completed_items,
            result.total_items,
            result.iterations,
            result.llm_calls,
            result.deterministic_calls,
        });
        if (result.summary.len > 0) {
            std.debug.print("{s}\n", .{result.summary});
        }
    } else {
        std.debug.print("Unknown todo subcommand: {s}\n", .{subcmd});
        std.debug.print("Usage: guidance todo <new|triage|checklist|status|list|abandon|run>\n", .{});
    }
}

// =============================================================================
// Public wrappers for testing (commit helpers) — delegated to sync/commit.zig
// =============================================================================

pub const parseHunkRangesPub = commit_mod.parseHunkRangesPub;
pub const loadChangedMembersPub = commit_mod.loadChangedMembersPub;
pub const generateCommitMessagePub = commit_mod.generateCommitMessagePub;
pub const chunkIsIgnoredPub = commit_mod.chunkIsIgnoredPub;
pub const chunkFilePathPub = commit_mod.chunkFilePathPub;
pub const splitDiffByFilePub = commit_mod.splitDiffByFilePub;
pub const CommitMemberInfo = commit_mod.CommitMemberInfo;
