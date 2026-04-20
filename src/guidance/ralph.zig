//! ralph.zig — RALPH Loop: Read → Ask → Learn → Plan → Help
//!
//! Implements M6 of ROADMAP_20260403_FLUENT_GUIDANCE.md:
//! A state machine for interactive codebase navigation that integrates
//! CodebaseMap (M3), QueryStrategy (M2), and DocumentIndexer (M1).
//!
//! State transitions:
//!   read  → discovers codebase structure, initialises context
//!   ask   → routes query through QueryStrategy dispatcher
//!   learn → records query history, discovers relationships
//!   plan  → suggests next queries based on capability gaps
//!   help  → emits formatted stages for the current query
//!   done  → terminal state; context is invalid

const std = @import("std");
const types = @import("types.zig");
const codebase_map_mod = @import("codebase_map.zig");
const query_strategy_mod = @import("query/strategy.zig");
const vector_db_mod = @import("vector");

const CodebaseMap = codebase_map_mod.CodebaseMap;
const QueryMatch = query_strategy_mod.QueryMatch;
const QueryIntent = query_strategy_mod.QueryIntent;
const GuidanceDb = vector_db_mod.GuidanceDb;

// =============================================================================
// State machine types
// =============================================================================

/// RALPH loop state enum.
pub const RalphState = enum { read, ask, learn, plan, help, done };

/// A single query record with intent and result count.
pub const QueryRecord = struct {
    query: []const u8,
    intent: QueryIntent,
    result_count: usize,
    had_synthesis: bool,
};

/// A relationship between two code entities (calls, imports, implements, extends).
pub const Relationship = struct {
    from: []const u8,
    to: []const u8,
    kind: enum { calls, imports, implements, extends },
    confidence: f32,
};

/// Context for RALPH loop execution.
pub const RalphContext = struct {
    allocator: std.mem.Allocator,
    state: RalphState,
    workspace: []const u8,
    map: ?CodebaseMap,
    db: *GuidanceDb,
    aliases: ?vector_db_mod.SemanticAliases,
    query_history: std.ArrayList(QueryRecord),
    pending_relationships: std.ArrayList(Relationship),

    // Strategies
    matches: [3]QueryMatch,

    pub fn init(
        allocator: std.mem.Allocator,
        workspace: []const u8,
        db: *GuidanceDb,
        aliases: ?vector_db_mod.SemanticAliases,
    ) !*RalphContext {
        const ctx = try allocator.create(RalphContext);
        ctx.allocator = allocator;
        ctx.state = .read;
        ctx.workspace = workspace;
        ctx.db = db;
        ctx.aliases = aliases;
        ctx.map = null;
        ctx.query_history = .{};
        ctx.pending_relationships = .{};
        ctx.matches = query_strategy_mod.buildDefaultStrategies();
        return ctx;
    }

    pub fn deinit(ctx: *RalphContext) void {
        if (ctx.map) |*m| m.deinit();
        for (ctx.query_history.items) |r| ctx.allocator.free(r.query);
        ctx.query_history.deinit(ctx.allocator);
        for (ctx.pending_relationships.items) |r| {
            ctx.allocator.free(r.from);
            ctx.allocator.free(r.to);
        }
        ctx.pending_relationships.deinit(ctx.allocator);
        ctx.allocator.destroy(ctx);
    }
};

// =============================================================================
// State transitions
// =============================================================================

/// Processes user input to generate a Zig stage, handling context and allocator parameters.
pub fn step(
    ctx: *RalphContext,
    allocator: std.mem.Allocator,
    user_input: []const u8,
) ![]types.Stage {
    return switch (ctx.state) {
        .read => ralphRead(ctx, allocator, user_input),
        .ask => ralphAsk(ctx, allocator, user_input),
        .learn => ralphLearn(ctx, allocator, user_input),
        .plan => ralphPlan(ctx, allocator, user_input),
        .help => ralphHelp(ctx, allocator, user_input),
        .done => allocator.alloc(types.Stage, 0),
    };
}

/// Reads a raw C string into a Zig-safe slice, handling allocation and error cases.
fn ralphRead(ctx: *RalphContext, allocator: std.mem.Allocator, _: []const u8) ![]types.Stage {
    ctx.map = codebase_map_mod.discoverStructure(allocator, ctx.workspace) catch null;
    ctx.state = .ask;
    return formatReadSummary(allocator, ctx);
}

/// Retrieves a stage from the RalphContext using provided allocator and query parameters.
fn ralphAsk(
    ctx: *RalphContext,
    allocator: std.mem.Allocator,
    query: []const u8,
) ![]types.Stage {
    const stages = try query_strategy_mod.executeQueryWithMatch(
        allocator,
        ctx.db,
        query,
        query,
        ctx.workspace,
        ctx.aliases,
        &ctx.matches,
    );
    ctx.state = .learn;
    return stages;
}

/// Processes a Zig code snippet to extract learning data from a context.
fn ralphLearn(
    ctx: *RalphContext,
    allocator: std.mem.Allocator,
    query: []const u8,
) ![]types.Stage {
    // Record the query.
    const intent = classifyIntent(query, &ctx.matches, ctx.db);
    try ctx.query_history.append(allocator, .{
        .query = try allocator.dupe(u8, query),
        .intent = intent,
        .result_count = 0,
        .had_synthesis = false,
    });
    ctx.state = .plan;
    // Return empty (learning is silent).
    return allocator.alloc(types.Stage, 0);
}

/// Transforms a raw C string into a Zig-safe slice with allocation handling.
fn ralphPlan(
    ctx: *RalphContext,
    allocator: std.mem.Allocator,
    _: []const u8,
) ![]types.Stage {
    ctx.state = .help;
    return formatPlanSuggestions(allocator, ctx);
}

/// Processes a Zig code snippet and returns a compiled stage.
fn ralphHelp(
    ctx: *RalphContext,
    allocator: std.mem.Allocator,
    _: []const u8,
) ![]types.Stage {
    ctx.state = .ask; // Ready for next query.
    return formatHelpStages(allocator);
}

// =============================================================================
// Formatting helpers
// =============================================================================

/// Converts a raw C string into a Zig-safe slice for read operations.
fn formatReadSummary(allocator: std.mem.Allocator, ctx: *const RalphContext) ![]types.Stage {
    var stages: std.ArrayList(types.Stage) = .{};
    errdefer {
        types.freeStages(allocator, stages.items);
        stages.deinit(allocator);
    }

    var buf: std.ArrayList(u8) = .{};
    defer buf.deinit(allocator);
    const w = buf.writer(allocator);

    try w.writeAll("# RALPH Loop — Codebase Discovery\n\n");

    if (ctx.map) |*m| {
        try w.print("**Workspace**: {s}\n\n", .{ctx.workspace});

        // Language distribution
        if (m.language_counts.len > 0) {
            try w.writeAll("**Languages**: ");
            for (m.language_counts[0..@min(5, m.language_counts.len)], 0..) |lc, i| {
                if (i > 0) try w.writeAll(", ");
                try w.print("{s}({d})", .{ lc.extension, lc.count });
            }
            try w.writeByte('\n');
        }

        // Build system
        try w.print("**Build**: {s}\n", .{m.build_system.buildCommand()});

        // Entry points
        if (m.entry_points.len > 0) {
            try w.writeAll("**Entry points**: ");
            for (m.entry_points[0..@min(3, m.entry_points.len)], 0..) |ep, i| {
                if (i > 0) try w.writeAll(", ");
                try w.print("`{s}`", .{ep.name});
            }
            try w.writeByte('\n');
        }
    } else {
        try w.print("**Workspace**: {s} (structure discovery failed)\n", .{ctx.workspace});
    }

    try w.writeAll("\nReady. Enter a query to explore the codebase.\n");

    try stages.append(allocator, .{
        .kind = .prose,
        .content = try buf.toOwnedSlice(allocator),
        .source = try allocator.dupe(u8, "guidance"),
    });

    return stages.toOwnedSlice(allocator);
}

/// Converts a RALPH context into a structured plan suggestion slice using the provided allocator.
fn formatPlanSuggestions(allocator: std.mem.Allocator, ctx: *const RalphContext) ![]types.Stage {
    var stages: std.ArrayList(types.Stage) = .{};
    errdefer {
        types.freeStages(allocator, stages.items);
        stages.deinit(allocator);
    }

    if (ctx.query_history.items.len == 0) return stages.toOwnedSlice(allocator);

    var buf: std.ArrayList(u8) = .{};
    defer buf.deinit(allocator);
    const w = buf.writer(allocator);

    try w.writeAll("**Suggested next queries** (based on session history):\n");

    // Suggest related queries for each unique intent seen.
    var seen_identifier = false;
    var seen_concept = false;
    for (ctx.query_history.items) |r| {
        switch (r.intent) {
            .identifier_lookup => {
                if (!seen_identifier) {
                    try w.print("- `{s}` — trace callers and dependencies\n", .{r.query});
                    seen_identifier = true;
                }
            },
            .concept_query => {
                if (!seen_concept) {
                    try w.print("- How does {s} integrate with the system?\n", .{r.query});
                    seen_concept = true;
                }
            },
            else => {},
        }
    }

    if (buf.items.len > 0) {
        try stages.append(allocator, .{
            .kind = .metadata,
            .content = try buf.toOwnedSlice(allocator),
            .source = try allocator.dupe(u8, "guidance"),
        });
    }

    return stages.toOwnedSlice(allocator);
}

/// Converts a Zig stage string into a slice of Stage objects.
fn formatHelpStages(allocator: std.mem.Allocator) ![]types.Stage {
    var stages: std.ArrayList(types.Stage) = .{};
    errdefer {
        types.freeStages(allocator, stages.items);
        stages.deinit(allocator);
    }

    const help_text =
        "**RALPH Loop** — Interactive Codebase Navigation\n\n" ++
        "Commands:\n" ++
        "- Any query text → search the codebase\n" ++
        "- `plan` → get suggested next queries\n" ++
        "- `map` → show codebase structure\n" ++
        "- `help` → this message\n" ++
        "- `quit` / `exit` → done\n";

    try stages.append(allocator, .{
        .kind = .prose,
        .content = try allocator.dupe(u8, help_text),
        .source = try allocator.dupe(u8, "guidance"),
    });

    return stages.toOwnedSlice(allocator);
}

// =============================================================================
// Helpers
// =============================================================================

/// Determines the intent of a query string based on provided strategies and database context.
fn classifyIntent(query: []const u8, matches: []const QueryMatch, db: *GuidanceDb) QueryIntent {
    for (matches) |m| {
        if (m.matches(query, db)) return m.intent;
    }
    return .general_search;
}

// =============================================================================
// Session runner (single query, no interactive loop)
// =============================================================================

/// Executes a database query using the provided allocator, database, and workspace, returning the result stage.
pub fn runQuery(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    workspace: []const u8,
    aliases: ?vector_db_mod.SemanticAliases,
) ![]types.Stage {
    const ctx = try RalphContext.init(allocator, workspace, db, aliases);
    defer ctx.deinit();

    // read → ask
    const read_stages = try step(ctx, allocator, "");
    defer {
        types.freeStages(allocator, read_stages);
        allocator.free(read_stages);
    }

    // ask → learn (with the user query)
    const query_stages = try step(ctx, allocator, query);
    // Caller owns these; don't defer-free.

    // learn → plan (silent, but consumes the query)
    const learn_stages = try step(ctx, allocator, query);
    defer {
        types.freeStages(allocator, learn_stages);
        allocator.free(learn_stages);
    }

    // Return query stages (the ask phase result) plus the plan suggestions.
    const plan_stages = try step(ctx, allocator, "");
    defer {
        types.freeStages(allocator, plan_stages);
        allocator.free(plan_stages);
    }

    // Combine query + plan into one slice.
    var combined: std.ArrayList(types.Stage) = .{};
    errdefer {
        // Don't free individual stage strings here — they're owned by query_stages.
        combined.deinit(allocator);
    }

    for (query_stages) |s| try combined.append(allocator, s);
    // Note: plan_stages strings will be freed by defer above — copy them.
    for (plan_stages) |s| {
        try combined.append(allocator, .{
            .kind = s.kind,
            .content = try allocator.dupe(u8, s.content),
            .source = try allocator.dupe(u8, s.source),
            .line = s.line,
        });
    }

    // query_stages outer slice transfered into combined; free only the spine.
    allocator.free(query_stages);

    return combined.toOwnedSlice(allocator);
}

// =============================================================================
// Tests
// =============================================================================
