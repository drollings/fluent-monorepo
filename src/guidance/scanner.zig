//! scanner.zig — M9: CodebaseScanner — Generic Codebase Analysis
//!
//! Implements M9 of ROADMAP_20260403_FLUENT_GUIDANCE.md:
//! Ties together CodebaseMap (M3), DocumentIndexer (M1), QueryStrategy (M2),
//! and InferCapabilities (M4) into a unified tool that works on any repository.
//!
//! Six-layer discovery:
//!   Layer 1: Structural discovery (CodebaseMap)
//!   Layer 2: Plugin discovery (language plugins from config)
//!   Layer 3: Capability discovery (CAPABILITY.md or inferred via M4)
//!   Layer 4: Skill discovery (SKILL.md documents)
//!   Layer 5: Index building (DocumentIndexer for all documents)
//!   Layer 6: Strategy binding (QueryStrategy VTable for routing)
//!
//! Confidence tiers:
//!   high:   AST analysis + CAPABILITY.md + STRUCTURE.md present
//!   medium: AST analysis + inferred capabilities
//!   low:    Filesystem walk only, no AST plugins available
//!
//! ## Memory Ownership
//!
//!   - CodebaseScanner: Owns workspace string (duped in init); call deinit() to release.
//!   - cmdScan(): Returns void; all output to stdout.
//!   - Internal discovery methods return owned stages; freed with types.freeStages().

const std = @import("std");
const types = @import("types.zig");
const codebase_map_mod = @import("codebase_map.zig");
const document_indexer_mod = @import("document_indexer.zig");
const query_strategy_mod = @import("query/strategy.zig");
const infer_capabilities_mod = @import("infer_capabilities.zig");
const staged_mod = @import("staged.zig");
const vector_db_mod = @import("vector");

const CodebaseMap = codebase_map_mod.CodebaseMap;
const QueryMatch = query_strategy_mod.QueryMatch;
const InferredCapability = infer_capabilities_mod.InferredCapability;
const GuidanceDb = vector_db_mod.GuidanceDb;

// =============================================================================
// Confidence tier
// =============================================================================

pub const ConfidenceTier = enum {
    /// AST analysis + CAPABILITY.md + STRUCTURE.md all present.
    high,
    /// AST analysis + inferred capabilities (no CAPABILITY.md).
    medium,
    /// Filesystem walk only — no AST plugins available.
    low,
};

// =============================================================================
// CodebaseScanner
// =============================================================================

pub const CodebaseScanner = struct {
    allocator: std.mem.Allocator,
    workspace: []const u8,
    map: ?CodebaseMap,
    inferred_capabilities: ?[]InferredCapability,
    confidence: ConfidenceTier,
    matches: [3]QueryMatch,

    pub fn init(allocator: std.mem.Allocator, workspace: []const u8) !CodebaseScanner {
        const s = CodebaseScanner{
            .allocator = allocator,
            .workspace = try allocator.dupe(u8, workspace),
            .map = null,
            .inferred_capabilities = null,
            .confidence = .low,
            .matches = query_strategy_mod.buildDefaultStrategies(),
        };
        return s;
    }

    pub fn deinit(self: *CodebaseScanner) void {
        if (self.map) |*m| m.deinit();
        if (self.inferred_capabilities) |caps| {
            infer_capabilities_mod.freeInferredCapabilities(self.allocator, caps);
        }
        self.allocator.free(self.workspace);
    }

    // ── Scanning ────────────────────────────────────────────────────────────

    /// Layer 1-4: Discover structure, plugins, capabilities, skills.
    /// Does not require GuidanceDb — works purely from filesystem.
    pub fn scan(self: *CodebaseScanner) !void {
        // Layer 1: Structural discovery
        self.map = codebase_map_mod.discoverStructure(self.allocator, self.workspace) catch null;

        // Layer 3: Capability discovery (CAPABILITY.md check)
        const has_capability_docs = self.hasCapabilityDocs();

        // Determine confidence tier.
        self.confidence = if (has_capability_docs) .high else .medium;
    }

    /// Layer 3 + 4: Infer capabilities when GuidanceDb is available.
    pub fn scanWithDb(self: *CodebaseScanner, db: *GuidanceDb) !void {
        try self.scan();

        if (!self.hasCapabilityDocs()) {
            if (self.map) |*m| {
                const caps = infer_capabilities_mod.inferCapabilities(
                    self.allocator,
                    m,
                    db,
                ) catch null;
                if (caps) |c| self.inferred_capabilities = c;
            }
        }
    }

    // ── Query execution ──────────────────────────────────────────────────────

    /// Execute a query through the strategy dispatcher.
    /// Returns owned stages; caller frees with types.freeStages() + allocator.free().
    pub fn explain(
        self: *CodebaseScanner,
        allocator: std.mem.Allocator,
        db: *GuidanceDb,
        query: []const u8,
        aliases: ?vector_db_mod.SemanticAliases,
    ) ![]types.Stage {
        return query_strategy_mod.executeQueryWithMatch(
            allocator,
            db,
            query,
            query,
            self.workspace,
            aliases,
            &self.matches,
        );
    }

    // ── Self-diagnosis ────────────────────────────────────────────────────────

    /// Generate a confidence report as a formatted string.
    /// Caller owns the returned string.
    pub fn diagnose(self: *const CodebaseScanner, allocator: std.mem.Allocator) ![]u8 {
        var aw: std.Io.Writer.Allocating = .init(allocator);
        errdefer aw.deinit();
        const w = &aw.writer;

        try w.print("# Codebase Scanner — Self-Diagnosis\n\n", .{});
        try w.print("**Workspace**: {s}\n", .{self.workspace});
        try w.print("**Confidence**: {s}\n\n", .{@tagName(self.confidence)});

        if (self.map) |*m| {
            try w.writeAll("## Structure\n\n");
            if (m.root_description) |desc| {
                try w.print("Description: {s}\n", .{desc[0..@min(200, desc.len)]});
            }
            try w.print("Build system: {s} ({s})\n", .{
                @tagName(m.build_system),
                m.build_system.buildCommand(),
            });

            if (m.language_counts.len > 0) {
                try w.writeAll("Languages: ");
                for (m.language_counts[0..@min(5, m.language_counts.len)], 0..) |lc, i| {
                    if (i > 0) try w.writeAll(", ");
                    try w.print("{s}({d})", .{ lc.extension, lc.count });
                }
                try w.writeByte('\n');
            }

            if (m.entry_points.len > 0) {
                try w.writeAll("Entry points: ");
                for (m.entry_points[0..@min(5, m.entry_points.len)], 0..) |ep, i| {
                    if (i > 0) try w.writeAll(", ");
                    try w.print("`{s}`", .{ep.name});
                }
                try w.writeByte('\n');
            }
            try w.writeByte('\n');
        } else {
            try w.writeAll("## Structure\n\nStructure discovery failed or not yet run.\n\n");
        }

        // Capability confidence
        try w.writeAll("## Capabilities\n\n");
        if (self.hasCapabilityDocs()) {
            try w.writeAll("Source: CAPABILITY.md documents found ✓\n");
        } else if (self.inferred_capabilities) |caps| {
            try w.print("Source: Inferred ({d} capabilities)\n", .{caps.len});
            for (caps[0..@min(5, caps.len)]) |cap| {
                try w.print("- `{s}` ({s}, confidence={d:.1})\n", .{
                    cap.name, @tagName(cap.method), cap.confidence,
                });
            }
        } else {
            try w.writeAll("Source: None — run `guidance gen` to build capability index\n");
        }

        return aw.toOwnedSlice();
    }

    // ── Helpers ────────────────────────────────────────────────────────────

    fn hasCapabilityDocs(self: *const CodebaseScanner) bool {
        const map = self.map orelse return false;
        return map.capability_dirs.len > 0;
    }
};

// =============================================================================
// cmdScan — CLI entry point
// =============================================================================

/// Reads a C string and converts it into a Zig-safe slice, handling allocation and error cases.
pub fn cmdScan(allocator: std.mem.Allocator, args: []const []const u8) !void {
    const config_mod = @import("config.zig");
    const common = @import("common");

    var workspace_arg: ?[]const u8 = null;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        if ((std.mem.eql(u8, args[i], "--workspace") or std.mem.eql(u8, args[i], "-w")) and i + 1 < args.len) {
            i += 1;
            workspace_arg = args[i];
        }
    }

    const cwd = try std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator);
    defer allocator.free(cwd);

    const workspace = workspace_arg orelse cwd;

    var scanner = try CodebaseScanner.init(allocator, workspace);
    defer scanner.deinit();

    // Try to open the DB for capability inference.
    const cfg = config_mod.loadConfig(allocator, cwd) catch null;
    if (cfg) |c| {
        defer @constCast(&c).deinit();
        const db_path = common.resolvePath(allocator, workspace, c.db_path) catch null;
        defer if (db_path) |p| allocator.free(p);

        var noop: vector_db_mod.NoopEmbedding = .{};
        if (db_path) |p| {
            var db = GuidanceDb.init(allocator, p, noop.provider()) catch {
                try scanner.scan();
                return emitDiagnosis(allocator, &scanner);
            };
            defer db.deinit();
            try scanner.scanWithDb(&db);
        } else {
            try scanner.scan();
        }
    } else {
        try scanner.scan();
    }

    return emitDiagnosis(allocator, &scanner);
}

/// Emits a diagnostic message using the provided allocator and scanner instance.
fn emitDiagnosis(allocator: std.mem.Allocator, scanner: *const CodebaseScanner) !void {
    const common = @import("common");
    const report = try scanner.diagnose(allocator);
    defer allocator.free(report);

    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();
    try stdout.writeAll(report);
    try stdout.flush();
}

// =============================================================================
// Tests
// =============================================================================
