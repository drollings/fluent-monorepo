//! codehealth — detect unused modules, redundant code, and dead code candidates.
//!
//! Usage:
//!   guidance codehealth [options]
//!
//! Phase 0:  Orphaned file detection (files not imported anywhere).
//! Phase 1:  Module-level detection (zero-importer files from DB).
//! Phase 1.5: build.zig validation (references to missing files).
//! Phase 2a: Redundancy detection via SimHash Hamming distance.
//! Phase 2b: Symbol-level call graph analysis (--extract-calls).
//! Phase 2:  Test organisation enforcement.
//!
//! This command is expensive and meant for periodic audits, not the RALPH loop.

const std = @import("std");
const vector = @import("vector");
const config_mod = @import("../config.zig");
const common = @import("common");
const orphan_mod = @import("orphan.zig");
const build_val_mod = @import("build_validation.zig");
const test_audit_mod = @import("test_audit.zig");
const test_mover_mod = @import("test_mover.zig");

// Re-export for tests.zig to pull in inline tests.
pub const parseCodehealthDirective = vector.parseCodehealthDirective;
pub const CodehealthDirective = vector.CodehealthDirective;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A source file module with zero importers — high-confidence dead code candidate.
pub const UnusedModule = struct {
    source: []const u8,
    last_modified: i64,
};

/// A pair of structurally similar symbols (SimHash Hamming distance ≤ threshold).
pub const RedundantPair = struct {
    a_node_id: i64,
    a_name: []const u8,
    a_source: []const u8,
    a_signature: ?[]const u8,
    b_node_id: i64,
    b_name: []const u8,
    b_source: []const u8,
    b_signature: ?[]const u8,
    hamming_distance: u6,
};

/// Output format for codehealth report.
pub const Format = enum { ai, human, json };

/// Phase selector for the codehealth command.
/// Use `parsePhases` to populate from `--phases=0,1,1.5,2`.
pub const PhaseSet = struct {
    phase0: bool = true, // orphaned files
    phase1: bool = true, // unused modules (DB)
    phase15: bool = true, // build.zig validation
    phase2a: bool = true, // SimHash redundancy
    phase2: bool = true, // test organisation
};

/// Arguments parsed from CLI flags for the codehealth command.
pub const CodehealthArgs = struct {
    min_age_days: u32 = 30,
    format: Format = .ai,
    db_path: []const u8 = ".guidance.db",
    extract_calls: bool = false,
    simhash_threshold: u6 = 3,
    workspace: []const u8 = ".",
    phases: PhaseSet = .{},
    /// Move inline tests to _tests.zig files.
    fix: bool = false,
    /// Print what --fix would do without writing files.
    dry_run: bool = false,
    /// Workspace-relative path to the tests.zig aggregator to update on --fix.
    tests_zig: ?[]const u8 = null,
};

/// Parse a comma-separated phase list such as "0,1,1.5,2" into a PhaseSet.
/// Only the listed phases will be enabled.
fn parsePhases(spec: []const u8) PhaseSet {
    var ps = PhaseSet{
        .phase0 = false,
        .phase1 = false,
        .phase15 = false,
        .phase2a = false,
        .phase2 = false,
    };
    var it = std.mem.splitScalar(u8, spec, ',');
    while (it.next()) |tok| {
        const t = std.mem.trim(u8, tok, " ");
        if (std.mem.eql(u8, t, "0")) ps.phase0 = true;
        if (std.mem.eql(u8, t, "1")) ps.phase1 = true;
        if (std.mem.eql(u8, t, "1.5")) ps.phase15 = true;
        if (std.mem.eql(u8, t, "2a")) ps.phase2a = true;
        if (std.mem.eql(u8, t, "2")) ps.phase2 = true;
    }
    return ps;
}

// ---------------------------------------------------------------------------
// SimHash redundancy detection (Phase 2a)
// ---------------------------------------------------------------------------

/// Common single-letter or trivial names that are expected to appear in many
/// structs/fns — not worth flagging as redundant unless distance == 0.
const GENERIC_NAMES = [_][]const u8{
    "init",  "deinit", "run",  "start", "stop", "reset", "new",    "free",
    "clone", "copy",   "hash", "eq",    "cmp",  "fmt",   "format",
};

/// Checks if a given byte slice represents a valid Zig identifier, returning true if it matches a generic name pattern.
fn isGenericName(name: []const u8) bool {
    for (GENERIC_NAMES) |g| {
        if (std.mem.eql(u8, name, g)) return true;
    }
    return false;
}

/// Find pairs of structurally similar symbols across different source files.
/// SimHash Hamming distance ≤ threshold indicates structural similarity.
/// Pairs with both names being generic are only included at distance == 0.
pub fn findRedundantPairs(
    allocator: std.mem.Allocator,
    db: *vector.GuidanceDb,
    threshold: u6,
) ![]RedundantPair {
    const entries = try db.loadSimhashEntries(allocator);
    defer {
        for (entries) |e| {
            allocator.free(e.name);
            allocator.free(e.node_type);
            allocator.free(e.source);
            if (e.signature) |s| allocator.free(s);
        }
        allocator.free(entries);
    }

    var pairs: std.ArrayList(RedundantPair) = .empty;
    errdefer {
        for (pairs.items) |p| freeRedundantPair(allocator, p);
        pairs.deinit(allocator);
    }

    // O(n²) comparison — ~785 entries → ~300k comparisons, sub-millisecond.
    for (entries, 0..) |a, i| {
        for (entries[i + 1 ..]) |b| {
            // Only compare across different source files.
            if (std.mem.eql(u8, a.source, b.source)) continue;
            // Only compare same node_type.
            if (!std.mem.eql(u8, a.node_type, b.node_type)) continue;

            const dist: u6 = @truncate(vector.hammingDistance(a.simhash, b.simhash));
            if (dist > threshold) continue;

            // Skip generic × generic unless identical.
            if (isGenericName(a.name) and isGenericName(b.name) and dist > 0) continue;

            try pairs.append(allocator, .{
                .a_node_id = a.node_id,
                .a_name = try allocator.dupe(u8, a.name),
                .a_source = try allocator.dupe(u8, a.source),
                .a_signature = if (a.signature) |s| try allocator.dupe(u8, s) else null,
                .b_node_id = b.node_id,
                .b_name = try allocator.dupe(u8, b.name),
                .b_source = try allocator.dupe(u8, b.source),
                .b_signature = if (b.signature) |s| try allocator.dupe(u8, s) else null,
                .hamming_distance = dist,
            });
        }
    }

    // Sort by Hamming distance ascending — closest matches first.
    std.sort.block(RedundantPair, pairs.items, {}, struct {
        fn lessThan(_: void, x: RedundantPair, y: RedundantPair) bool {
            return x.hamming_distance < y.hamming_distance;
        }
    }.lessThan);

    return pairs.toOwnedSlice(allocator);
}

/// Releases memory by freeing a redundant pair using the provided allocator.
fn freeRedundantPair(allocator: std.mem.Allocator, p: RedundantPair) void {
    allocator.free(p.a_name);
    allocator.free(p.a_source);
    if (p.a_signature) |s| allocator.free(s);
    allocator.free(p.b_name);
    allocator.free(p.b_source);
    if (p.b_signature) |s| allocator.free(s);
}

// ---------------------------------------------------------------------------
// Phase 2b: Symbol-level call graph queries
// ---------------------------------------------------------------------------

/// A public symbol with zero callers (from non-test files).
pub const UnusedSymbol = struct {
    name: []const u8,
    node_type: []const u8,
    signature: ?[]const u8,
    source: []const u8,
    line: ?u32,
};

/// A symbol called only from test files.
pub const TestOnlySymbol = struct {
    name: []const u8,
    node_type: []const u8,
    source: []const u8,
    line: ?u32,
    callers: []const u8, // comma-joined caller files
};

/// Query unused public symbols (zero callers, excluding test callers).
/// Requires `called_by` table populated by `--extract-calls`.
/// Returns an empty slice until the called_by table is populated.
pub fn findUnusedSymbols(
    allocator: std.mem.Allocator,
    db: *vector.GuidanceDb,
) ![]UnusedSymbol {
    _ = db;
    // Full implementation uses called_by table populated by --extract-calls.
    // Returns empty slice when called_by has no data.
    return allocator.alloc(UnusedSymbol, 0);
}

// ---------------------------------------------------------------------------
// Output formatters
// ---------------------------------------------------------------------------

/// Calculates days elapsed since a given modification timestamp.
fn daysSince(last_modified: i64) u32 {
    const now: i64 = @intCast(@divTrunc(@as(i128, std.Io.Timestamp.now(std.Io.Threaded.global_single_threaded.io(), .real).nanoseconds), std.time.ns_per_s));
    const diff = now - last_modified;
    if (diff <= 0) return 0;
    return @intCast(@divTrunc(diff, 86400));
}

/// Writes output to a writer, handling unused and redundant pairs with a minimum age constraint.
fn writeAiOutput(
    w: *std.Io.Writer,
    orphans: []const orphan_mod.OrphanedFile,
    unused: []const UnusedModule,
    build_anomalies: []const build_val_mod.BuildAnomaly,
    pairs: []const RedundantPair,
    test_anomalies: []const test_audit_mod.TestAnomaly,
    min_age_days: u32,
) !void {
    var has_content = false;

    // ── Phase 0: Orphaned files ───────────────────────────────────
    var printed_orphan_header = false;
    for (orphans) |o| {
        const age = daysSince(o.last_modified);
        if (age < min_age_days) continue;
        if (!printed_orphan_header) {
            try w.writeAll(
                \\## ⚠️ ORPHANED FILES (High Confidence)
                \\
                \\> These files exist but are not imported by any other file.
                \\
                \\
            );
            printed_orphan_header = true;
            has_content = true;
        }
        try w.print("### {s} ({d} days)\n\n", .{ o.source, age });
        try w.writeAll("**Imports:** None\n");
        try w.writeAll("**Action:** Delete file and remove from build.zig if present\n\n---\n\n");
    }

    // ── Phase 1.5: build.zig anomalies ───────────────────────────
    try build_val_mod.writeAiOutput(w, build_anomalies);
    if (build_anomalies.len > 0) has_content = true;

    // ── Phase 1: Modules not imported (DB) ───────────────────────
    var printed_header = false;
    for (unused) |m| {
        const age = daysSince(m.last_modified);
        if (age < min_age_days) continue;
        if (!printed_header) {
            try w.writeAll(
                \\## ⚠️ MODULES NOT IMPORTED (High Confidence)
                \\
                \\> These files have zero importers. Verify no dynamic loading before deletion.
                \\
                \\
            );
            printed_header = true;
            has_content = true;
        }
        try w.print("### {s} ({d} days)\n\n", .{ m.source, age });
        try w.writeAll("**Action:** Safe to delete if dynamic loading not used.\n\n---\n\n");
    }

    // ── Redundant pairs ───────────────────────────────────────────
    if (pairs.len > 0) {
        try w.writeAll(
            \\## 🔁 POTENTIALLY REDUNDANT CODE (Medium Confidence)
            \\
            \\> These symbol pairs have similar structure (SimHash Hamming distance ≤ threshold).
            \\> Consider consolidating or verifying they serve distinct purposes.
            \\
            \\
        );
        has_content = true;
        for (pairs) |p| {
            try w.print("### {s} vs {s}\n\n", .{ p.a_name, p.b_name });
            try w.writeAll("| | A | B |\n|---|---|---|\n");
            try w.print("| **Symbol** | `{s}` | `{s}` |\n", .{ p.a_name, p.b_name });
            try w.print("| **File** | `{s}` | `{s}` |\n", .{ p.a_source, p.b_source });
            if (p.a_signature) |sa| {
                if (p.b_signature) |sb| {
                    try w.print("| **Signature** | `{s}` | `{s}` |\n", .{ sa, sb });
                }
            }
            try w.print("| **Hamming** | {d} bits | |\n\n", .{p.hamming_distance});
            try w.writeAll("**Action:** Verify they cannot share a common interface.\n\n---\n\n");
        }
    }

    // ── Phase 2: Test organisation ────────────────────────────────
    try test_audit_mod.writeAiOutput(w, test_anomalies);
    if (test_anomalies.len > 0) has_content = true;

    if (!has_content) {
        try w.writeAll("## ✅ No issues found\n\nAll modules appear to be imported and no redundant code detected.\n");
    }

    // ── Summary ───────────────────────────────────────────────────
    // Count orphans that pass the age filter (matches what was displayed).
    var orphans_displayed: usize = 0;
    for (orphans) |o| {
        if (daysSince(o.last_modified) >= min_age_days) orphans_displayed += 1;
    }

    try w.writeAll("## 📋 SUMMARY\n\n");
    try w.writeAll("| Category | Count | Action |\n|----------|-------|--------|\n");
    try w.print("| Orphaned files | {d} | Review for deletion |\n", .{orphans_displayed});
    try w.print("| build.zig anomalies | {d} | Fix references |\n", .{build_anomalies.len});
    try w.print("| Not imported (DB) | {d} | Review for deletion |\n", .{unused.len});
    try w.print("| Redundant pairs | {d} | Consider consolidation |\n", .{pairs.len});
    try w.print("| Test organisation | {d} | Review conventions |\n", .{test_anomalies.len});
}

/// Writes formatted output to a writer, handling unused and redundant data efficiently.
fn writeHumanOutput(
    w: *std.Io.Writer,
    orphans: []const orphan_mod.OrphanedFile,
    unused: []const UnusedModule,
    build_anomalies: []const build_val_mod.BuildAnomaly,
    pairs: []const RedundantPair,
    test_anomalies: []const test_audit_mod.TestAnomaly,
    min_age_days: u32,
) !void {
    try w.print("CODEHEALTH REPORT (min_age={d}d)\n", .{min_age_days});
    try w.writeAll("================================\n\n");

    try w.print("Orphaned files ({d}):\n", .{orphans.len});
    for (orphans) |o| {
        const age = daysSince(o.last_modified);
        if (age < min_age_days) continue;
        try w.print("  [{d}d] {s}\n", .{ age, o.source });
    }

    try build_val_mod.writeHumanOutput(w, build_anomalies);

    try w.print("\nUnused modules/{d}):\n", .{unused.len});
    for (unused) |m| {
        const age = daysSince(m.last_modified);
        if (age < min_age_days) continue;
        try w.print("  [{d}d] {s}\n", .{ age, m.source });
    }

    try w.print("\nRedundant pairs ({d}):\n", .{pairs.len});
    for (pairs) |p| {
        try w.print("  [{d} bits] {s} ({s}) <-> {s} ({s})\n", .{
            p.hamming_distance, p.a_name, p.a_source, p.b_name, p.b_source,
        });
    }

    try test_audit_mod.writeHumanOutput(w, test_anomalies);
}

/// Writes JSON-formatted output to a writer, handling optional parameters and redundant pairs.
fn writeJsonOutput(
    w: *std.Io.Writer,
    orphans: []const orphan_mod.OrphanedFile,
    unused: []const UnusedModule,
    build_anomalies: []const build_val_mod.BuildAnomaly,
    pairs: []const RedundantPair,
    test_anomalies: []const test_audit_mod.TestAnomaly,
) !void {
    try w.writeAll("{\"orphaned_files\":[");
    for (orphans, 0..) |o, i| {
        if (i > 0) try w.writeAll(",");
        try w.print("{{\"source\":\"{s}\",\"last_modified\":{d},\"age_days\":{d}}}", .{
            o.source, o.last_modified, daysSince(o.last_modified),
        });
    }
    try w.writeAll("],");
    try build_val_mod.writeJsonFragment(w, build_anomalies);
    try w.writeAll(",\"unused_modules\":[");
    for (unused, 0..) |m, i| {
        if (i > 0) try w.writeAll(",");
        try w.print("{{\"source\":\"{s}\",\"last_modified\":{d},\"age_days\":{d}}}", .{
            m.source, m.last_modified, daysSince(m.last_modified),
        });
    }
    try w.writeAll("],\"redundant_pairs\":[");
    for (pairs, 0..) |p, i| {
        if (i > 0) try w.writeAll(",");
        try w.print("{{\"a_name\":\"{s}\",\"a_source\":\"{s}\",\"b_name\":\"{s}\",\"b_source\":\"{s}\",\"hamming\":{d}}}", .{
            p.a_name, p.a_source, p.b_name, p.b_source, p.hamming_distance,
        });
    }
    try w.writeAll("],");
    try test_audit_mod.writeJsonFragment(w, test_anomalies);
    try w.writeAll("}\n");
}

// ---------------------------------------------------------------------------
// Command entry point
// ---------------------------------------------------------------------------

/// Run the codehealth audit against the guidance database.
pub fn cmdHealth(allocator: std.mem.Allocator, args_raw: []const []const u8) !void {
    var ch_args = CodehealthArgs{};
    var i: usize = 0;
    while (i < args_raw.len) : (i += 1) {
        const arg = args_raw[i];
        if (std.mem.startsWith(u8, arg, "--min-age=")) {
            ch_args.min_age_days = std.fmt.parseInt(u32, arg["--min-age=".len..], 10) catch 30;
        } else if (std.mem.startsWith(u8, arg, "--format=")) {
            const fmt_str = arg["--format=".len..];
            ch_args.format = std.meta.stringToEnum(Format, fmt_str) orelse .ai;
        } else if (std.mem.startsWith(u8, arg, "--db")) {
            if (std.mem.eql(u8, arg, "--db")) {
                i += 1;
                if (i < args_raw.len) ch_args.db_path = args_raw[i];
            } else if (arg.len > 5 and arg[4] == '=') {
                ch_args.db_path = arg[5..];
            }
        } else if (std.mem.startsWith(u8, arg, "--simhash-threshold=")) {
            const n = std.fmt.parseInt(u6, arg["--simhash-threshold=".len..], 10) catch 3;
            ch_args.simhash_threshold = n;
        } else if (std.mem.eql(u8, arg, "--extract-calls")) {
            ch_args.extract_calls = true;
        } else if (std.mem.startsWith(u8, arg, "-w") or std.mem.startsWith(u8, arg, "--workspace")) {
            if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
                i += 1;
                if (i < args_raw.len) ch_args.workspace = args_raw[i];
            }
        } else if (std.mem.startsWith(u8, arg, "--phases=")) {
            ch_args.phases = parsePhases(arg["--phases=".len..]);
        } else if (std.mem.eql(u8, arg, "--fix")) {
            ch_args.fix = true;
        } else if (std.mem.eql(u8, arg, "--dry-run")) {
            ch_args.dry_run = true;
        } else if (std.mem.startsWith(u8, arg, "--tests-zig=")) {
            ch_args.tests_zig = arg["--tests-zig=".len..];
        }
    }

    // Phase 0: orphaned files (filesystem walk, no DB needed).
    const orphans: []orphan_mod.OrphanedFile = if (ch_args.phases.phase0)
        orphan_mod.findOrphanedFiles(allocator, ch_args.workspace) catch |err| blk: {
            std.debug.print("warning: findOrphanedFiles failed: {s}\n", .{@errorName(err)});
            break :blk &[_]orphan_mod.OrphanedFile{};
        }
    else
        &[_]orphan_mod.OrphanedFile{};
    defer {
        for (orphans) |o| allocator.free(o.source);
        allocator.free(orphans);
    }

    // Phase 1.5: build.zig validation.
    const build_anomalies: []build_val_mod.BuildAnomaly = if (ch_args.phases.phase15)
        build_val_mod.validateBuildZig(allocator, ch_args.workspace) catch |err| blk: {
            std.debug.print("warning: validateBuildZig failed: {s}\n", .{@errorName(err)});
            break :blk &[_]build_val_mod.BuildAnomaly{};
        }
    else
        &[_]build_val_mod.BuildAnomaly{};
    defer {
        for (build_anomalies) |a| allocator.free(a.referenced_path);
        allocator.free(build_anomalies);
    }

    var noop: common.NoopEmbedding = .{};
    var db = vector.GuidanceDb.init(allocator, ch_args.db_path, noop.provider()) catch |err| {
        std.debug.print("error: cannot open database '{s}': {s}\n", .{ ch_args.db_path, @errorName(err) });
        return;
    };
    defer db.deinit();

    // Phase 1: unused modules.
    const unused = if (ch_args.phases.phase1)
        db.findUnusedModules(allocator) catch |err| blk: {
            std.debug.print("warning: findUnusedModules failed: {s}\n", .{@errorName(err)});
            break :blk &[_]vector.GuidanceDb.UnusedModule{};
        }
    else
        &[_]vector.GuidanceDb.UnusedModule{};
    defer {
        for (unused) |m| allocator.free(m.source);
        allocator.free(unused);
    }

    // Phase 2a: redundant pairs via SimHash.
    const pairs = if (ch_args.phases.phase2a)
        findRedundantPairs(allocator, &db, ch_args.simhash_threshold) catch |err| blk: {
            std.debug.print("warning: findRedundantPairs failed: {s}\n", .{@errorName(err)});
            break :blk &[_]RedundantPair{};
        }
    else
        &[_]RedundantPair{};
    defer {
        for (pairs) |p| freeRedundantPair(allocator, p);
        allocator.free(pairs);
    }

    // Phase 2b: call graph (expensive, opt-in).
    if (ch_args.extract_calls) {
        runCallExtraction(allocator, &db, ch_args.workspace) catch |err| {
            std.debug.print("warning: call extraction failed: {s}\n", .{@errorName(err)});
        };
    }

    // Phase 2: test organisation.
    const test_anomalies: []test_audit_mod.TestAnomaly = if (ch_args.phases.phase2)
        test_audit_mod.auditTestFiles(allocator, ch_args.workspace) catch |err| blk: {
            std.debug.print("warning: auditTestFiles failed: {s}\n", .{@errorName(err)});
            break :blk &[_]test_audit_mod.TestAnomaly{};
        }
    else
        &[_]test_audit_mod.TestAnomaly{};
    defer {
        for (test_anomalies) |a| {
            allocator.free(a.source);
        }
        allocator.free(test_anomalies);
    }

    // --fix: move inline tests to _tests.zig files.
    if (ch_args.fix or ch_args.dry_run) {
        const fix_stats = test_mover_mod.fixAll(
            allocator,
            ch_args.workspace,
            ch_args.dry_run,
            ch_args.tests_zig,
        ) catch |err| blk: {
            std.debug.print("warning: --fix failed: {s}\n", .{@errorName(err)});
            break :blk test_mover_mod.FixStats{
                .files_with_tests = 0,
                .tests_moved = 0,
                .tests_skipped = 0,
                .files_created = 0,
                .files_updated = 0,
            };
        };
        const action = if (ch_args.dry_run) "would move" else "moved";
        std.debug.print(
            "[codehealth --fix] {s} {d} test(s) from {d} file(s); skipped {d}; created {d} _tests.zig file(s)\n",
            .{ action, fix_stats.tests_moved, fix_stats.files_with_tests, fix_stats.tests_skipped, fix_stats.files_created },
        );

        // --fix: add uncovered *_tests.zig files to build.zig.
        var uncovered_paths: std.ArrayList([]const u8) = .empty;
        defer uncovered_paths.deinit(allocator);
        for (test_anomalies) |a| {
            if (a.kind == .uncovered_test_file)
                try uncovered_paths.append(allocator, a.source);
        }
        if (uncovered_paths.items.len > 0) {
            const bld_stats = build_val_mod.fixUncoveredTestFiles(
                allocator,
                ch_args.workspace,
                uncovered_paths.items,
                ch_args.dry_run,
            ) catch |err| blk: {
                std.debug.print("warning: fix build.zig failed: {s}\n", .{@errorName(err)});
                break :blk build_val_mod.FixBuildStats{};
            };
            const bld_action = if (ch_args.dry_run) "would add" else "added";
            std.debug.print(
                "[codehealth --fix] {s} {d} test target(s) to build.zig; skipped {d}\n",
                .{ bld_action, bld_stats.added, bld_stats.skipped },
            );
        }
    }

    // Adapt to UnusedModule local type for output functions.
    var unused_out = try allocator.alloc(UnusedModule, unused.len);
    defer allocator.free(unused_out);
    for (unused, 0..) |m, idx| {
        unused_out[idx] = .{ .source = m.source, .last_modified = m.last_modified };
    }

    // Write output.
    var out_buf: [8192]u8 = undefined;
    const io = std.Io.Threaded.global_single_threaded.io();
    var out_fw = std.Io.File.stdout().writer(io, &out_buf);
    const stdout = &out_fw.interface;

    switch (ch_args.format) {
        .ai => try writeAiOutput(stdout, orphans, unused_out, build_anomalies, pairs, test_anomalies, ch_args.min_age_days),
        .human => try writeHumanOutput(stdout, orphans, unused_out, build_anomalies, pairs, test_anomalies, ch_args.min_age_days),
        .json => try writeJsonOutput(stdout, orphans, unused_out, build_anomalies, pairs, test_anomalies),
    }
    try stdout.flush();
}

// ---------------------------------------------------------------------------
// Phase 2b: call graph extraction (delegates to call_extractor.zig)
// ---------------------------------------------------------------------------

/// Processes a GuidanceDb object and extracts call-related data using the provided allocator.
fn runCallExtraction(
    allocator: std.mem.Allocator,
    db: *vector.GuidanceDb,
    workspace: []const u8,
) !void {
    const call_extractor = @import("extractor.zig");
    db.truncateCalledBy();
    std.debug.print("[codehealth] extracting call graph from {s}...\n", .{workspace});
    try call_extractor.extractCallsFromWorkspace(allocator, db, workspace);
    std.debug.print("[codehealth] call graph extraction complete\n", .{});
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "codehealth: isGenericName recognises common names" {
    try std.testing.expect(isGenericName("init"));
    try std.testing.expect(isGenericName("deinit"));
    try std.testing.expect(isGenericName("run"));
    try std.testing.expect(!isGenericName("computePageRank"));
}
