//! test_mover.zig — Move inline tests from source .zig files to <name>_tests.zig.
//!
//! Used by `guidance codehealth --fix [--dry-run]`.
//!
//! Algorithm per source file:
//!   1. Parse with std.zig.Ast to collect top-level names:
//!      - module_imports: const X = @import("Y")  (always accessible via build system)
//!      - pub_names: explicitly pub fn/const/var names  (accessible after move)
//!      - private_names: non-pub, non-import names     (inaccessible after move)
//!   2. Classify each test_decl:
//!      - movable:   body does not reference any private_name
//!      - unmovable: body references at least one private_name
//!   3. For movable tests: apply pub-symbol qualification (replace bare `sym` → `mod.sym`)
//!      because usingnamespace was removed in Zig 0.15.
//!   4. Build <name>_tests.zig:
//!      - Header: const std, module imports used by moved tests,
//!                const <stem>_mod = @import("<stem>.zig")
//!      - Body: qualified test blocks
//!   5. Rewrite source with movable test blocks removed.
//!   6. Run `zig fmt` on both files.
//!   7. Update the tests.zig aggregator to include the new _tests.zig.

const std = @import("std");
const common = @import("common");

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A single test block extracted from a source file.
pub const TestBlock = struct {
    /// Test name without quotes, or empty string for anonymous tests.
    name: []const u8,
    /// Line-aligned source text: from start-of-line of `test` to end-of-line of `}`.
    text: []const u8,
    /// Byte offset of `text` in the original source.
    start_offset: usize,
    /// Byte offset one past the last byte of `text`.
    end_offset: usize,
};

/// A test that was NOT moved because its body references a private (non-pub) symbol.
pub const SkippedTest = struct {
    name: []const u8,
    /// Name of the first private symbol found in the test body.
    private_symbol: []const u8,
};

/// Aggregate statistics from fixAll().
pub const FixStats = struct {
    files_with_tests: usize,
    tests_moved: usize,
    tests_skipped: usize,
    files_created: usize,
    files_updated: usize,
};

// ---------------------------------------------------------------------------
// Internal: import alias record
// ---------------------------------------------------------------------------

const ImportAlias = struct {
    alias: []const u8, // e.g. "vector"
    path: []const u8, // e.g. "vector" or "../config.zig"
};

// ---------------------------------------------------------------------------
// Internal: declaration analysis
// ---------------------------------------------------------------------------

/// Collect top-level declaration info from a parsed AST.
///
/// - `out_imports`:  const X = @import("Y") aliases.
/// - `out_pub`:      pub fn/const/var names.
/// - `out_private`:  non-pub, non-import names (prevent test moving if referenced).
///
/// All allocations use `aa` (arena allocator).
fn collectDeclInfo(
    aa: std.mem.Allocator,
    tree: *const std.zig.Ast,
    out_imports: *std.ArrayList(ImportAlias),
    out_pub: *std.StringHashMap(void),
    out_private: *std.StringHashMap(void),
) !void {
    for (tree.rootDecls()) |decl| {
        const tag = tree.nodeTag(decl);
        if (tag == .test_decl) continue;
        if (tag == .@"comptime") continue;

        switch (tag) {
            .simple_var_decl,
            .local_var_decl,
            .global_var_decl,
            .aligned_var_decl,
            => {
                const full = tree.fullVarDecl(decl) orelse continue;
                const name_tok = full.ast.mut_token + 1;
                const name = tree.tokenSlice(name_tok);
                const is_pub = full.visib_token != null;

                // Check for @import initializer.
                if (full.ast.init_node != .none) {
                    const init_opt = full.ast.init_node.unwrap();
                    if (init_opt) |init_node| {
                        const itag = tree.nodeTag(init_node);
                        const is_import = switch (itag) {
                            .builtin_call_two, .builtin_call_two_comma => blk: {
                                const bt = tree.nodeMainToken(init_node);
                                break :blk std.mem.eql(u8, tree.tokenSlice(bt), "@import");
                            },
                            else => false,
                        };
                        if (is_import) {
                            const opts = tree.nodeData(init_node).opt_node_and_opt_node;
                            const path: []const u8 = if (opts[0].unwrap()) |arg_node|
                                if (tree.nodeTag(arg_node) == .string_literal) blk: {
                                    const raw = tree.tokenSlice(tree.nodeMainToken(arg_node));
                                    break :blk if (raw.len >= 2) raw[1 .. raw.len - 1] else raw;
                                } else ""
                            else
                                "";
                            try out_imports.append(aa, .{
                                .alias = try aa.dupe(u8, name),
                                .path = try aa.dupe(u8, path),
                            });
                            continue;
                        }
                    }
                }

                if (is_pub) {
                    try out_pub.put(try aa.dupe(u8, name), {});
                } else {
                    try out_private.put(try aa.dupe(u8, name), {});
                }
            },

            .fn_decl => {
                var buf: [1]std.zig.Ast.Node.Index = undefined;
                const proto_node = tree.nodeData(decl).node_and_node[0];
                const proto = tree.fullFnProto(&buf, proto_node) orelse continue;
                const name = if (proto.name_token) |nt| tree.tokenSlice(nt) else continue;
                if (proto.visib_token != null) {
                    try out_pub.put(try aa.dupe(u8, name), {});
                } else {
                    try out_private.put(try aa.dupe(u8, name), {});
                }
            },

            else => continue,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal: test block classification
// ---------------------------------------------------------------------------

/// Return the name of the first private symbol referenced in the token range
/// [first_tok, last_tok], or null if none.
fn firstPrivateUsage(
    tree: *const std.zig.Ast,
    first_tok: std.zig.Ast.TokenIndex,
    last_tok: std.zig.Ast.TokenIndex,
    private_names: *const std.StringHashMap(void),
) ?[]const u8 {
    const tok_tags = tree.tokens.items(.tag);
    var tok = first_tok;
    while (tok <= last_tok) : (tok += 1) {
        if (tok_tags[tok] != .identifier) continue;
        const name = tree.tokenSlice(tok);
        if (private_names.contains(name)) return name;
    }
    return null;
}

/// Classify rootDecl test nodes into movable and skipped.
/// All arena-allocated.
fn classifyTests(
    aa: std.mem.Allocator,
    source: []const u8,
    tree: *const std.zig.Ast,
    private_names: *const std.StringHashMap(void),
    out_movable: *std.ArrayList(TestBlock),
    out_skipped: *std.ArrayList(SkippedTest),
) !void {
    const tok_starts = tree.tokens.items(.start);
    const tok_tags = tree.tokens.items(.tag);

    for (tree.rootDecls()) |decl| {
        if (tree.nodeTag(decl) != .test_decl) continue;

        const main_tok = tree.nodeMainToken(decl);
        const last_tok = tree.lastToken(decl);

        // Extract test name from the string literal token after `test`.
        const name_tok = main_tok + 1;
        const name: []const u8 = if (name_tok < tree.tokens.len and
            tok_tags[name_tok] == .string_literal)
        blk: {
            const raw = tree.tokenSlice(name_tok);
            break :blk if (raw.len >= 2) raw[1 .. raw.len - 1] else "";
        } else "";

        // Skip test if it references a private symbol.
        if (firstPrivateUsage(tree, main_tok + 1, last_tok, private_names)) |priv| {
            try out_skipped.append(aa, .{
                .name = try aa.dupe(u8, name),
                .private_symbol = try aa.dupe(u8, priv),
            });
            continue;
        }

        // Line-aligned byte range.
        const start_byte = tok_starts[main_tok];
        const end_byte = tok_starts[last_tok] + tree.tokenSlice(last_tok).len;

        var line_start = start_byte;
        while (line_start > 0 and source[line_start - 1] != '\n') line_start -= 1;

        var line_end = end_byte;
        while (line_end < source.len and source[line_end] != '\n') line_end += 1;
        if (line_end < source.len) line_end += 1;

        try out_movable.append(aa, .{
            .name = try aa.dupe(u8, name),
            .text = try aa.dupe(u8, source[line_start..line_end]),
            .start_offset = line_start,
            .end_offset = line_end,
        });
    }
}

// ---------------------------------------------------------------------------
// Internal: pub-symbol qualification (replaces usingnamespace)
// ---------------------------------------------------------------------------

/// Rewrite `test_text` so that bare identifiers that are pub symbols in the
/// source module become `<mod_alias>.<identifier>`.
///
/// Rule: qualify `.identifier` tokens that (a) are in `pub_names` AND (b) are
/// NOT immediately preceded by a `.` token (i.e. not a field/method name).
///
/// `test_text_z` must be null-terminated. Returns allocator-owned slice.
pub fn qualifyPubRefs(
    allocator: std.mem.Allocator,
    test_text_z: [:0]const u8,
    pub_names: *const std.StringHashMap(void),
    mod_alias: []const u8,
) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(allocator);

    var tokenizer = std.zig.Tokenizer.init(test_text_z);
    var prev_tag: std.zig.Token.Tag = .eof;
    var pos: usize = 0; // cursor into test_text_z

    while (true) {
        const tok = tokenizer.next();
        if (tok.tag == .eof) break;

        // Emit verbatim text between last token and this one (whitespace / comments).
        if (tok.loc.start > pos) {
            try out.appendSlice(allocator, test_text_z[pos..tok.loc.start]);
        }
        pos = tok.loc.end;

        const tok_text = test_text_z[tok.loc.start..tok.loc.end];

        if (tok.tag == .identifier and
            prev_tag != .period and
            pub_names.contains(tok_text))
        {
            try out.appendSlice(allocator, mod_alias);
            try out.append(allocator, '.');
        }
        try out.appendSlice(allocator, tok_text);
        prev_tag = tok.tag;
    }
    // Append any trailing bytes (shouldn't happen for well-formed Zig, but be safe).
    if (pos < test_text_z.len) {
        try out.appendSlice(allocator, test_text_z[pos..]);
    }
    return out.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Internal: determine module imports used in test texts
// ---------------------------------------------------------------------------

/// Return the subset of `imports` whose alias appears as a token in any block text.
/// "std" is excluded (always added separately).
fn usedImports(
    aa: std.mem.Allocator,
    imports: []const ImportAlias,
    blocks: []const TestBlock,
) ![]ImportAlias {
    var used: std.ArrayList(ImportAlias) = .empty;
    for (imports) |imp| {
        if (std.mem.eql(u8, imp.alias, "std")) continue;
        for (blocks) |blk| {
            if (std.mem.indexOf(u8, blk.text, imp.alias) != null) {
                try used.append(aa, imp);
                break;
            }
        }
    }
    return used.toOwnedSlice(aa);
}

// ---------------------------------------------------------------------------
// Internal: source rewriting
// ---------------------------------------------------------------------------

/// Remove test blocks from `source`. `blocks` must be sorted ascending by start_offset.
fn removeBlocks(allocator: std.mem.Allocator, source: []const u8, blocks: []const TestBlock) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(allocator);

    var pos: usize = 0;
    for (blocks) |blk| {
        try out.appendSlice(allocator, source[pos..blk.start_offset]);
        pos = blk.end_offset;
    }
    try out.appendSlice(allocator, source[pos..]);

    const raw = try out.toOwnedSlice(allocator);
    defer allocator.free(raw);
    return collapseBlankLines(allocator, raw);
}

/// Reduce three or more consecutive newlines to two (one blank line between sections).
fn collapseBlankLines(allocator: std.mem.Allocator, src: []const u8) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(allocator);
    var i: usize = 0;
    while (i < src.len) {
        if (src[i] == '\n') {
            var j = i;
            while (j < src.len and src[j] == '\n') j += 1;
            try out.appendNTimes(allocator, '\n', @min(j - i, 2));
            i = j;
        } else {
            try out.append(allocator, src[i]);
            i += 1;
        }
    }
    return out.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Internal: _tests.zig content assembly
// ---------------------------------------------------------------------------

/// Build content for a `<stem>_tests.zig` file.
///
/// `existing_content` = null → new file (header is written).
/// `existing_content` = some slice → append mode.
/// `source_basename`  = "main.zig"
/// `mod_alias`        = "main_mod"
/// `module_imports`   = non-std imports from the source file that are used in blocks
/// `blocks`           = already-qualified test texts
fn assembleTestsFile(
    allocator: std.mem.Allocator,
    source_basename: []const u8,
    mod_alias: []const u8,
    module_imports: []const ImportAlias,
    blocks: []const []const u8, // qualified test texts
    existing_content: ?[]const u8,
) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(allocator);

    if (existing_content == null) {
        try out.appendSlice(allocator, "//! Tests for ");
        try out.appendSlice(allocator, source_basename);
        try out.appendSlice(allocator, ".\n");
        try out.appendSlice(allocator, "//! Moved by `guidance codehealth --fix`. Edit as needed.\n");
        try out.appendSlice(allocator, "const std = @import(\"std\");\n");
        for (module_imports) |imp| {
            if (imp.path.len == 0) continue;
            try out.appendSlice(allocator, "const ");
            try out.appendSlice(allocator, imp.alias);
            try out.appendSlice(allocator, " = @import(\"");
            try out.appendSlice(allocator, imp.path);
            try out.appendSlice(allocator, "\");\n");
        }
        try out.appendSlice(allocator, "const ");
        try out.appendSlice(allocator, mod_alias);
        try out.appendSlice(allocator, " = @import(\"");
        try out.appendSlice(allocator, source_basename);
        try out.appendSlice(allocator, "\");\n\n");
    } else {
        try out.appendSlice(allocator, existing_content.?);
        if (out.items.len > 0 and out.items[out.items.len - 1] != '\n')
            try out.append(allocator, '\n');
        try out.append(allocator, '\n');
    }

    for (blocks) |text| {
        try out.appendSlice(allocator, text);
        if (out.items.len > 0 and out.items[out.items.len - 1] != '\n')
            try out.append(allocator, '\n');
    }

    return out.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Internal: zig fmt runner
// ---------------------------------------------------------------------------

fn runZigFmt(allocator: std.mem.Allocator, path: []const u8) void {
    _ = std.process.run(allocator, common.io.singleIo(), .{
        .argv = &[_][]const u8{ "zig", "fmt", path },
    }) catch return;
}

// ---------------------------------------------------------------------------
// Public: tests.zig aggregator update
// ---------------------------------------------------------------------------

/// Add `import_line` (e.g. `_ = @import("codehealth/main_tests.zig");`) to the
/// comptime block in `tests_zig_path`, if not already present.
pub fn addToTestsZig(
    allocator: std.mem.Allocator,
    tests_zig_path: []const u8,
    import_line: []const u8,
) !void {
    const io = std.Io.Threaded.global_single_threaded.io();
    const content = std.Io.Dir.cwd().readFileAlloc(io, tests_zig_path, allocator, .limited(5 * 1024 * 1024)) catch return;
    defer allocator.free(content);

    if (std.mem.indexOf(u8, content, import_line) != null) return;

    // Find `\n}` that closes the comptime block (last occurrence before EOF).
    const close = std.mem.lastIndexOf(u8, content, "\n}") orelse return;

    var buf: [256]u8 = undefined;
    const f = try std.Io.Dir.cwd().createFile(io, tests_zig_path, .{});
    var fw = f.writer(io, &buf);
    defer f.close(io);
    try fw.interface.writeAll(content[0 .. close + 1]); // up to the '\n' before '}'
    try fw.interface.print("    {s}\n", .{import_line});
    try fw.interface.writeAll(content[close + 1 ..]); // '}' onward
    try fw.interface.flush();
}

// ---------------------------------------------------------------------------
// Internal: path helpers
// ---------------------------------------------------------------------------

fn stemOf(path: []const u8) []const u8 {
    if (std.mem.endsWith(u8, path, ".zig")) return path[0 .. path.len - 4];
    return path;
}

/// Derive a module alias from a basename like "main.zig" → "main_mod".
fn modAlias(aa: std.mem.Allocator, basename: []const u8) ![]u8 {
    const stem = stemOf(basename);
    return std.fmt.allocPrint(aa, "{s}_mod", .{stem});
}

/// Return `target` relative to `from_dir` (both workspace-relative).
fn relPath(allocator: std.mem.Allocator, from_dir: []const u8, target: []const u8) ![]const u8 {
    if (from_dir.len == 0) return allocator.dupe(u8, target);
    const prefix = try std.fmt.allocPrint(allocator, "{s}/", .{from_dir});
    defer allocator.free(prefix);
    if (std.mem.startsWith(u8, target, prefix)) return allocator.dupe(u8, target[prefix.len..]);
    return allocator.dupe(u8, target);
}

// ---------------------------------------------------------------------------
// Public: fix all files in workspace
// ---------------------------------------------------------------------------

/// Walk all .zig source files under `workspace` and move their inline tests to
/// `<name>_tests.zig` files.
///
/// `tests_zig_rel` is the workspace-relative path to the tests.zig aggregator
/// to update, or null to skip that step.
pub fn fixAll(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    dry_run: bool,
    tests_zig_rel: ?[]const u8,
) !FixStats {
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const aa = arena.allocator();

    var stats = FixStats{
        .files_with_tests = 0,
        .tests_moved = 0,
        .tests_skipped = 0,
        .files_created = 0,
        .files_updated = 0,
    };

    const io = std.Io.Threaded.global_single_threaded.io();
    var src_dir = std.Io.Dir.cwd().openDir(io, workspace, .{ .iterate = true }) catch |err| {
        std.debug.print("[test_mover] cannot open workspace '{s}': {s}\n", .{ workspace, @errorName(err) });
        return stats;
    };
    defer src_dir.close(io);

    var walker = try src_dir.walk(aa);
    defer walker.deinit();

    while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, ".zig")) continue;
        if (std.mem.startsWith(u8, entry.path, "zig-")) continue;
        if (std.mem.endsWith(u8, entry.path, "_tests.zig")) continue;
        if (std.mem.endsWith(u8, entry.path, "/tests.zig")) continue;
        if (std.mem.eql(u8, entry.path, "tests.zig")) continue;
        if (std.mem.eql(u8, entry.path, "build.zig")) continue;

        const rel_path = entry.path;
        const abs_path = try std.fmt.allocPrint(aa, "{s}/{s}", .{ workspace, rel_path });

        const src_raw = std.Io.Dir.cwd().readFileAlloc(std.Io.Threaded.global_single_threaded.io(), abs_path, aa, .limited(10 * 1024 * 1024)) catch continue;
        const src_z = try aa.dupeZ(u8, src_raw);

        var tree = std.zig.Ast.parse(aa, src_z, .zig) catch continue;
        defer tree.deinit(aa);

        var imports: std.ArrayList(ImportAlias) = .empty;
        var pub_names = std.StringHashMap(void).init(aa);
        var private_names = std.StringHashMap(void).init(aa);
        collectDeclInfo(aa, &tree, &imports, &pub_names, &private_names) catch continue;

        var movable: std.ArrayList(TestBlock) = .empty;
        var skipped_list: std.ArrayList(SkippedTest) = .empty;
        classifyTests(aa, src_raw, &tree, &private_names, &movable, &skipped_list) catch continue;

        stats.tests_skipped += skipped_list.items.len;
        if (movable.items.len == 0) continue;

        stats.files_with_tests += 1;
        stats.tests_moved += movable.items.len;

        if (dry_run) {
            std.debug.print("[test_mover] [dry-run] {s}: would move {d} test(s), skip {d}\n", .{
                rel_path, movable.items.len, skipped_list.items.len,
            });
            continue;
        }

        const stem = stemOf(rel_path);
        const tests_rel = try std.fmt.allocPrint(aa, "{s}_tests.zig", .{stem});
        const tests_abs = try std.fmt.allocPrint(aa, "{s}/{s}", .{ workspace, tests_rel });
        const source_basename = std.fs.path.basename(rel_path);
        const alias = try modAlias(aa, source_basename);

        const existing = std.Io.Dir.cwd().readFileAlloc(io, tests_abs, aa, .limited(10 * 1024 * 1024)) catch null;
        const is_new = existing == null;

        // Qualify pub-symbol references in each movable test.
        var qualified_texts: std.ArrayList([]const u8) = .empty;
        for (movable.items) |blk| {
            const blk_z = try aa.dupeZ(u8, blk.text);
            const qualified = qualifyPubRefs(aa, blk_z, &pub_names, alias) catch
                try aa.dupe(u8, blk.text);
            try qualified_texts.append(aa, qualified);
        }

        // Determine which module imports the moved tests actually use.
        const used = usedImports(aa, imports.items, movable.items) catch &.{};

        // Build _tests.zig content.
        const tests_content = assembleTestsFile(
            aa,
            source_basename,
            alias,
            used,
            qualified_texts.items,
            existing,
        ) catch continue;

        // Write _tests.zig.
        {
            const parent = std.fs.path.dirname(tests_abs) orelse ".";
            std.Io.Dir.cwd().createDirPath(io, parent) catch {};

            var buf: [256]u8 = undefined;
            const f = std.Io.Dir.cwd().createFile(io, tests_abs, .{}) catch continue;
            var fw = f.writer(io, &buf);
            fw.interface.writeAll(tests_content) catch {
                f.close(io);
                continue;
            };
            fw.interface.flush() catch {};
            f.close(io);
        }

        if (is_new) {
            stats.files_created += 1;
        } else {
            stats.files_updated += 1;
        }

        // Rewrite source with movable test blocks removed.
        const sorted = try aa.dupe(TestBlock, movable.items);
        std.sort.block(TestBlock, sorted, {}, struct {
            fn lt(_: void, a: TestBlock, b: TestBlock) bool {
                return a.start_offset < b.start_offset;
            }
        }.lt);

        const new_source = removeBlocks(aa, src_raw, sorted) catch continue;
        {
            var buf: [256]u8 = undefined;
            const f = std.Io.Dir.cwd().createFile(io, abs_path, .{}) catch continue;
            var fw = f.writer(io, &buf);
            fw.interface.writeAll(new_source) catch {
                f.close(io);
                continue;
            };
            fw.interface.flush() catch {};
            f.close(io);
        }

        runZigFmt(aa, abs_path);
        runZigFmt(aa, tests_abs);

        std.debug.print("[test_mover] {s}: moved {d} → {s}; {d} kept (private)\n", .{
            rel_path, movable.items.len, tests_rel, skipped_list.items.len,
        });

        // Update tests.zig aggregator.
        if (tests_zig_rel) |tz_rel| {
            if (is_new) {
                const tz_abs = try std.fmt.allocPrint(aa, "{s}/{s}", .{ workspace, tz_rel });
                const tz_dir = std.fs.path.dirname(tz_rel) orelse "";
                const import_path = relPath(aa, tz_dir, tests_rel) catch tests_rel;
                const import_line = try std.fmt.allocPrint(aa, "_ = @import(\"{s}\");", .{import_path});
                addToTestsZig(aa, tz_abs, import_line) catch |err| {
                    std.debug.print("[test_mover] warning: could not update {s}: {s}\n", .{
                        tz_rel, @errorName(err),
                    });
                };
            }
        }
    }

    return stats;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "test_mover: stemOf strips .zig extension" {
    try std.testing.expectEqualStrings("src/foo/bar", stemOf("src/foo/bar.zig"));
    try std.testing.expectEqualStrings("src/foo/bar", stemOf("src/foo/bar"));
}

test "test_mover: collapseBlankLines reduces triple newlines" {
    const allocator = std.testing.allocator;
    const input = "a\n\n\n\nb\n";
    const out = try collapseBlankLines(allocator, input);
    defer allocator.free(out);
    try std.testing.expectEqualStrings("a\n\nb\n", out);
}

test "test_mover: collapseBlankLines preserves double newlines" {
    const allocator = std.testing.allocator;
    const input = "a\n\nb\n";
    const out = try collapseBlankLines(allocator, input);
    defer allocator.free(out);
    try std.testing.expectEqualStrings("a\n\nb\n", out);
}

test "test_mover: removeBlocks removes a single test block" {
    const allocator = std.testing.allocator;
    const source = "pub fn foo() void {}\n\ntest \"foo\" {\n    _ = 1;\n}\n";
    const blocks = [_]TestBlock{.{
        .name = "foo",
        .text = "test \"foo\" {\n    _ = 1;\n}\n",
        .start_offset = 22,
        .end_offset = source.len,
    }};
    const result = try removeBlocks(allocator, source, &blocks);
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "test") == null);
    try std.testing.expect(std.mem.indexOf(u8, result, "pub fn foo") != null);
}

test "test_mover: collectDeclInfo identifies private and pub names" {
    const allocator = std.testing.allocator;
    const source: [:0]const u8 =
        \\const std = @import("std");
        \\const vec = @import("vector");
        \\pub fn pubFunc() void {}
        \\fn privateFunc() void {}
        \\const PRIVATE_CONST = 42;
        \\pub const PUB_CONST = 1;
    ;
    var tree = try std.zig.Ast.parse(allocator, source, .zig);
    defer tree.deinit(allocator);

    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const aa = arena.allocator();

    var imports: std.ArrayList(ImportAlias) = .empty;
    var pub_names = std.StringHashMap(void).init(aa);
    var private_names = std.StringHashMap(void).init(aa);
    try collectDeclInfo(aa, &tree, &imports, &pub_names, &private_names);

    // Imports should not appear in private.
    try std.testing.expect(!private_names.contains("std"));
    try std.testing.expect(!private_names.contains("vec"));
    // Private should be detected.
    try std.testing.expect(private_names.contains("privateFunc"));
    try std.testing.expect(private_names.contains("PRIVATE_CONST"));
    // Pub should be detected.
    try std.testing.expect(pub_names.contains("pubFunc"));
    try std.testing.expect(pub_names.contains("PUB_CONST"));
    // Pub should NOT be in private.
    try std.testing.expect(!private_names.contains("pubFunc"));
    // Import aliases collected.
    var found_std = false;
    var found_vec = false;
    for (imports.items) |imp| {
        if (std.mem.eql(u8, imp.alias, "std")) found_std = true;
        if (std.mem.eql(u8, imp.alias, "vec")) found_vec = true;
    }
    try std.testing.expect(found_std);
    try std.testing.expect(found_vec);
}

test "test_mover: classifyTests splits movable from skipped" {
    const allocator = std.testing.allocator;
    const source: [:0]const u8 =
        \\const std = @import("std");
        \\fn privHelper() bool { return true; }
        \\pub fn pubFn() void {}
        \\
        \\test "uses private" {
        \\    _ = privHelper();
        \\}
        \\
        \\test "uses only pub" {
        \\    pubFn();
        \\}
    ;
    var tree = try std.zig.Ast.parse(allocator, source, .zig);
    defer tree.deinit(allocator);

    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const aa = arena.allocator();

    var imports: std.ArrayList(ImportAlias) = .empty;
    var pub_names = std.StringHashMap(void).init(aa);
    var private_names = std.StringHashMap(void).init(aa);
    try collectDeclInfo(aa, &tree, &imports, &pub_names, &private_names);

    var movable: std.ArrayList(TestBlock) = .empty;
    var skipped: std.ArrayList(SkippedTest) = .empty;
    try classifyTests(aa, source, &tree, &private_names, &movable, &skipped);

    try std.testing.expectEqual(@as(usize, 1), movable.items.len);
    try std.testing.expectEqualStrings("uses only pub", movable.items[0].name);
    try std.testing.expectEqual(@as(usize, 1), skipped.items.len);
    try std.testing.expectEqualStrings("uses private", skipped.items[0].name);
    try std.testing.expectEqualStrings("privHelper", skipped.items[0].private_symbol);
}

test "test_mover: assembleTestsFile creates correct header" {
    const allocator = std.testing.allocator;
    const blocks = [_][]const u8{
        "test \"foo\" {\n    try std.testing.expect(true);\n}\n",
    };
    const imp = [_]ImportAlias{.{ .alias = "common", .path = "common" }};
    const content = try assembleTestsFile(allocator, "foo.zig", "foo_mod", &imp, &blocks, null);
    defer allocator.free(content);

    try std.testing.expect(std.mem.indexOf(u8, content, "const foo_mod = @import(\"foo.zig\")") != null);
    try std.testing.expect(std.mem.indexOf(u8, content, "const common = @import(\"common\")") != null);
    try std.testing.expect(std.mem.indexOf(u8, content, "const std = @import(\"std\")") != null);
    try std.testing.expect(std.mem.indexOf(u8, content, "test \"foo\"") != null);
}
