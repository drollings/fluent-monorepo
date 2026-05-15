//! root.zig — Public re-exports for the subagent module.
//!
//! The subagent implements a deterministic-first FSM that orchestrates
//! tool calls for `guidance todo run`. It resolves 70%+ of iterations
//! without any LLM call by pattern-matching checklist items to tool
//! actions, filling parameters via in-process `guidance explain`, and
//! routing unknown items through a single grammar-constrained batch
//! LLM call.

pub const types = @import("types.zig");
pub const grammar = @import("grammar.zig");
pub const reflect = @import("reflect.zig");
pub const fsm = @import("fsm.zig");
pub const builder_mod = @import("builder.zig");
pub const classify = @import("classify.zig");
pub const route = @import("route.zig");
pub const validate_mod = @import("validate.zig");
pub const execute = @import("execute.zig");
pub const synthesize_mod = @import("synthesize.zig");
pub const guardrails_mod = @import("guardrails.zig");
pub const todo_mod = @import("todo.zig");

pub const FsmState = types.FsmState;
pub const ActionType = types.ActionType;
pub const ChecklistItem = types.ChecklistItem;
pub const ToolParams = types.ToolParams;
pub const ToolResult = types.ToolResult;
pub const BatchClassifyResult = types.BatchClassifyResult;
pub const BatchClassifyEntry = types.BatchClassifyEntry;
pub const ScratchpadEntry = types.ScratchpadEntry;
pub const SubagentConfig = types.SubagentConfig;
pub const SummarizedContext = types.SummarizedContext;
pub const Evidence = types.Evidence;
pub const SubagentResult = types.SubagentResult;
pub const IterationState = types.IterationState;
pub const IterationId = types.IterationId;
pub const StepId = types.StepId;
pub const Citation = types.Citation;
pub const ExplainStage = types.ExplainStage;
pub const ExplainStageKind = types.ExplainStageKind;
pub const GuardrailState = types.GuardrailState;
pub const ExplainFn = types.ExplainFn;
pub const ExplainResultType = types.ExplainResult;
pub const BackendMode = types.BackendMode;
pub const IterationProfile = types.IterationProfile;

pub const Scratchpad = reflect.Scratchpad;
pub const appendObservation = reflect.appendObservation;

pub const SubagentBuilder = builder_mod.SubagentBuilder;
pub const BuilderError = builder_mod.BuilderError;
pub const BuilderPhase = builder_mod.BuilderPhase;
pub const builder = builder_mod.builder;

pub const classificationGrammar = grammar.classification_grammar;
pub const routeGrammar = grammar.route_grammar;
pub const synthGrammar = grammar.synth_grammar;

pub const classifyDeterministic = classify.classifyDeterministic;
pub const classifyAllDeterministic = classify.classifyAllDeterministic;
pub const batchClassifyDeterministic = classify.batchClassifyDeterministic;
pub const parseBatchClassifyResult = classify.parseBatchClassifyResult;
pub const constitutionallyValidate = classify.constitutionallyValidate;
pub const classifyBatchViaLlm = classify.classifyBatchViaLlm;
pub const LlmClassifyFn = classify.LlmClassifyFn;
pub const TodoAction = classify.TodoAction;
pub const classifiers = classify.classifiers;

pub const RouteResult = route.RouteResult;
pub const RouteSource = route.RouteSource;
pub const ExplainResult = route.ExplainResult;
pub const ExplainCache = route.ExplainCache;
pub const routeParams = route.routeParams;
pub const routeParamsCached = route.routeParamsCached;

pub const ValidationResult = validate_mod.ValidationResult;
pub const ValidationError = validate_mod.ValidationError;
pub const validateParams = validate_mod.validateParams;

pub const ToolVTable = execute.ToolVTable;
pub const SubagentTool = execute.SubagentTool;
pub const BashTool = execute.BashTool;
pub const ReadTool = execute.ReadTool;
pub const ExplainTool = execute.ExplainTool;
pub const EditTool = execute.EditTool;
pub const DiaryTool = execute.DiaryTool;
pub const ChecklistTool = execute.ChecklistTool;
pub const toolName = execute.toolName;

pub const SynthesizeResult = synthesize_mod.SynthesizeResult;
pub const LlmFn = synthesize_mod.LlmFn;
pub const synthesize = synthesize_mod.synthesize;

pub const GuardrailCheck = guardrails_mod.GuardrailCheck;
pub const checkGuardrails = guardrails_mod.checkGuardrails;
pub const OutputHashTracker = guardrails_mod.OutputHashTracker;

pub const cmdTodoRun = todo_mod.cmdTodoRun;

pub const runSubagent = fsm.runSubagent;
pub const runSubagentWithBackend = fsm.runSubagentWithBackend;
pub const parseChecklistItems = fsm.parseChecklistItems;
pub const toggleChecklistItem = fsm.toggleChecklistItem;
pub const RunCallbacks = fsm.RunCallbacks;

// ── Tests ─────────────────────────────────────────────────────────────────────

const std = @import("std");

test "types: FsmState transitions" {
    const t = std.testing;
    var state: types.FsmState = .intake;
    state = .classify;
    try t.expectEqual(types.FsmState.classify, state);
    state = .route;
    try t.expectEqual(types.FsmState.route, state);
    state = .validate;
    try t.expectEqual(types.FsmState.validate, state);
    state = .execute;
    try t.expectEqual(types.FsmState.execute, state);
    state = .reflect;
    try t.expectEqual(types.FsmState.reflect, state);
    state = .synth;
    try t.expectEqual(types.FsmState.synth, state);
    state = .done;
    try t.expectEqual(types.FsmState.done, state);

    state = .intake;
    state = .classify;
    state = .batch_classify;
    try t.expectEqual(types.FsmState.batch_classify, state);

    state = .route;
    state = .route_llm;
    try t.expectEqual(types.FsmState.route_llm, state);

    state = .validate;
    state = .escalate;
    try t.expectEqual(types.FsmState.escalate, state);
}

test "types: ActionType round trips" {
    const t = std.testing;
    const actions = [_]types.ActionType{ .bash, .read, .explain, .edit, .diary, .checklist, .unknown };
    for (actions) |action| {
        const name = @tagName(action);
        const back = std.meta.stringToEnum(types.ActionType, name).?;
        try t.expectEqual(action, back);
    }
}

test "types: ToolParams.isComplete" {
    const t = std.testing;
    var p: types.ToolParams = .{ .action = .bash };
    try t.expect(!p.isComplete());
    p.command = "make test";
    try t.expect(p.isComplete());

    p = .{ .action = .read };
    try t.expect(!p.isComplete());
    p.path = "src/main.zig";
    try t.expect(p.isComplete());

    p = .{ .action = .explain };
    try t.expect(!p.isComplete());
    p.query = "how does filterStages work";
    try t.expect(p.isComplete());

    p = .{ .action = .edit };
    try t.expect(!p.isComplete());
    p.path = "src/main.zig";
    try t.expect(!p.isComplete());
    p.content = "fix: update handler";
    try t.expect(p.isComplete());

    p = .{ .action = .unknown };
    try t.expect(!p.isComplete());
}

test "types: GuardrailState failure counting" {
    const t = std.testing;
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var gs = types.GuardrailState.init(allocator, 3, 5, 20);
    defer gs.deinit();

    try t.expectEqual(@as(?void, null), gs.recordFailure("bash"));
    try t.expectEqual(@as(?void, null), gs.recordFailure("bash"));
    const halted = gs.recordFailure("bash");
    try t.expect(halted != null);

    gs.clearFailure("bash");
    const after_clear = gs.recordFailure("bash");
    try t.expect(after_clear == null);
}

test "types: GuardrailState no-progress detection" {
    const t = std.testing;
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var gs = types.GuardrailState.init(allocator, 5, 3, 20);
    defer gs.deinit();

    try t.expect(!gs.recordNoProgress("item1"));
    try t.expect(!gs.recordNoProgress("item1"));
    try t.expect(gs.recordNoProgress("item1"));

    try t.expect(!gs.recordNoProgress("item2"));
}

test "types: IterationProfile default values" {
    const t = std.testing;
    const profile: types.IterationProfile = .{
        .iteration = 1,
        .state_entered = .route,
        .deterministic_time_us = 50,
        .llm_time_us = 0,
        .total_time_us = 55,
        .action = .bash,
        .used_cache = false,
    };
    try t.expectEqual(@as(u16, 1), profile.iteration);
    try t.expectEqual(types.FsmState.route, profile.state_entered);
    try t.expectEqual(@as(u64, 50), profile.deterministic_time_us);
    try t.expect(!profile.used_cache);
}

test "types: BackendMode enum values" {
    try std.testing.expectEqual(types.BackendMode.sync, .sync);
    try std.testing.expectEqual(types.BackendMode.zio, .zio);
}

test "reflect: Scratchpad append and ring eviction" {
    const t = std.testing;
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var sp = reflect.Scratchpad.init(allocator, 3);
    defer sp.deinit();

    try sp.append(.{ .iteration = 1, .item_text = "first", .action = .bash, .observation = "obs1", .reasoning = "", .success = true });
    try sp.append(.{ .iteration = 2, .item_text = "second", .action = .read, .observation = "obs2", .reasoning = "", .success = true });
    try sp.append(.{ .iteration = 3, .item_text = "third", .action = .explain, .observation = "obs3", .reasoning = "", .success = false });
    try t.expectEqual(@as(usize, 3), sp.ring.items.len);

    try sp.append(.{ .iteration = 4, .item_text = "fourth", .action = .edit, .observation = "obs4", .reasoning = "", .success = true });
    try t.expectEqual(@as(usize, 3), sp.ring.items.len);
    try t.expectEqual(@as(u16, 3), sp.count);
}

test "reflect: Scratchpad formatContext" {
    const t = std.testing;
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var sp = reflect.Scratchpad.init(allocator, 10);
    defer sp.deinit();

    try sp.append(.{ .iteration = 1, .item_text = "run tests", .action = .bash, .observation = "all passed", .reasoning = "", .success = true });
    const ctx = try sp.formatContext(allocator);
    defer allocator.free(ctx);
    try t.expect(std.mem.indexOf(u8, ctx, "run tests") != null);
    try t.expect(std.mem.indexOf(u8, ctx, "bash") != null);
    try t.expect(std.mem.indexOf(u8, ctx, "all passed") != null);
}
