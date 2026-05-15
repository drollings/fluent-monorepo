//! types.zig — Core type definitions for the subagent FSM.
//!
//! Defines FsmState, ActionType, ChecklistItem, ToolParams, ToolResult,
//! Scratchpad, SummarizedContext, Evidence, SubagentResult, IterationState,
//! and GuardrailState.

const std = @import("std");

pub const IterationId = enum(i64) { _ };
pub const StepId = enum(i64) { _ };

pub const FsmState = enum {
    intake,
    classify,
    batch_classify,
    route,
    route_llm,
    validate,
    execute,
    reflect,
    synth,
    done,
    escalate,
};

pub const ActionType = enum {
    bash,
    read,
    explain,
    edit,
    diary,
    checklist,
    unknown,
};

pub const ChecklistItem = struct {
    index: usize,
    text: []const u8,
    completed: bool,
    line_number: usize,
};

pub const ToolParams = struct {
    action: ActionType,
    command: ?[]const u8 = null,
    path: ?[]const u8 = null,
    line_start: ?u32 = null,
    line_end: ?u32 = null,
    content: ?[]const u8 = null,
    query: ?[]const u8 = null,
    item_index: ?usize = null,

    pub fn isComplete(self: *const ToolParams) bool {
        return switch (self.action) {
            .bash => self.command != null,
            .read => self.path != null,
            .edit => self.path != null and self.content != null,
            .explain => self.query != null,
            .diary => self.content != null,
            .checklist => self.item_index != null,
            .unknown => false,
        };
    }
};

pub const BatchClassifyEntry = struct {
    item_index: usize,
    action: ActionType,
    params: ?ToolParams = null,
};

pub const BatchClassifyResult = struct {
    classified: []BatchClassifyEntry,
    unknown_indices: []const usize,
    llm_results: []BatchClassifyEntry,
    llm_calls_used: u16,
};

pub const Citation = struct {
    file: ?[]const u8 = null,
    line: ?u32 = null,
    member: ?[]const u8 = null,
};

pub const ExplainStage = struct {
    kind: ExplainStageKind,
    content: []const u8,
    source: []const u8,
    line: ?u32 = null,
    relevance: f64 = 1.0,
};

pub const ExplainStageKind = enum {
    prose,
    code,
    metadata,
    insight,
    skill_doc,
    capability_doc,
    not_found,
};

pub const ToolResult = struct {
    action: ActionType,
    success: bool,
    structured: ?[]const u8 = null,
    raw: ?[]const u8 = null,
    stages: ?[]ExplainStage = null,
    citations: []const Citation = &.{},
    token_estimate: u32 = 0,

    pub fn deinit(self: *ToolResult, allocator: std.mem.Allocator) void {
        if (self.structured) |s| allocator.free(s);
        if (self.raw) |r| allocator.free(r);
        if (self.stages) |stages| {
            for (stages) |s| {
                allocator.free(s.content);
                allocator.free(s.source);
            }
            allocator.free(stages);
        }
        for (self.citations) |c| {
            if (c.file) |f| allocator.free(f);
            if (c.member) |m| allocator.free(m);
        }
        allocator.free(self.citations);
    }
};

pub const ScratchpadEntry = struct {
    iteration: u16,
    item_text: []const u8,
    action: ActionType,
    observation: []const u8,
    reasoning: []const u8,
    success: bool,
};

pub const SubagentConfig = struct {
    workspace: []const u8,
    db_path: []const u8,
    guidance_dir: []const u8,
    api_url: []const u8,
    model: []const u8,
    max_iterations: u16 = 20,
    scratchpad_max_entries: u16 = 10,
    allow_edit: bool = false,
    backend_mode: BackendMode = .sync,
    tool_fn: ?*const ToolFn = null,
    command_allowlist: []const []const u8 = &.{
        "make", "zig build", "cargo", "npm",  "git",
        "ls",   "cat",       "head",  "tail", "grep",
        "find", "echo",      "wc",    "sort", "uniq",
        "diff",
    },
};

pub const SummarizedContext = struct {
    summary: []const u8,
    facts: []const []const u8,
    citations: []const Citation,
    followup: ?ActionType = null,
    token_cost: u32,
};

pub const Evidence = struct {
    iteration: IterationId,
    step: StepId,
    action: ActionType,
    item_text: []const u8,
    summary: []const u8,
    citations: []const Citation,
};

pub const SubagentResult = struct {
    status: enum { completed, escalated, halted },
    summary: []const u8,
    completed_items: usize,
    total_items: usize,
    evidence: []const Evidence,
    iterations: u16,
    llm_calls: u16,
    deterministic_calls: u16,
    profiles: []const IterationProfile = &.{},

    pub fn deinit(self: *SubagentResult, allocator: std.mem.Allocator) void {
        allocator.free(self.summary);
        for (self.evidence) |e| {
            allocator.free(e.item_text);
            allocator.free(e.summary);
        }
        allocator.free(self.evidence);
        if (self.profiles.len > 0) {
            allocator.free(@constCast(self.profiles));
        }
    }
};

pub const IterationState = struct {
    iteration_id: IterationId,
    item: ChecklistItem,
    classified_action: ActionType,
    params: ToolParams,
    result: ?ToolResult = null,
    summary: ?SummarizedContext = null,
    llm_calls_this_iteration: u16 = 0,
};

pub const GuardrailState = struct {
    failure_counts: std.StringHashMap(usize),
    no_progress_counts: std.StringHashMap(usize),
    max_consecutive_failures: u16,
    max_no_progress: u16,
    max_iterations: u16,

    pub fn init(allocator: std.mem.Allocator, max_consecutive_failures: u16, max_no_progress: u16, max_iterations: u16) GuardrailState {
        return .{
            .failure_counts = std.StringHashMap(usize).init(allocator),
            .no_progress_counts = std.StringHashMap(usize).init(allocator),
            .max_consecutive_failures = max_consecutive_failures,
            .max_no_progress = max_no_progress,
            .max_iterations = max_iterations,
        };
    }

    pub fn deinit(self: *GuardrailState) void {
        self.failure_counts.deinit();
        self.no_progress_counts.deinit();
    }

    pub fn recordFailure(self: *GuardrailState, key: []const u8) ?void {
        const entry = self.failure_counts.getOrPut(key) catch return null;
        if (entry.found_existing) {
            entry.value_ptr.* += 1;
            if (entry.value_ptr.* >= self.max_consecutive_failures) return {};
        } else {
            entry.value_ptr.* = 1;
        }
        return null;
    }

    pub fn recordNoProgress(self: *GuardrailState, key: []const u8) bool {
        const entry = self.no_progress_counts.getOrPut(key) catch return false;
        if (entry.found_existing) {
            entry.value_ptr.* += 1;
            if (entry.value_ptr.* >= self.max_no_progress) return true;
        } else {
            entry.value_ptr.* = 1;
        }
        return false;
    }

    pub fn clearFailure(self: *GuardrailState, key: []const u8) void {
        _ = self.failure_counts.remove(key);
    }
};

pub const ExplainFn = fn (allocator: std.mem.Allocator, query: []const u8, db_path: []const u8, workspace: []const u8) ?ExplainResult;

pub const ToolFn = fn (allocator: std.mem.Allocator, params: *const ToolParams, io: *std.Io) anyerror!ToolResult;

pub const ExplainResult = struct {
    path: ?[]const u8 = null,
    line: ?u32 = null,
    content: ?[]const u8 = null,
    query: []const u8,
};

// ── Backend mode (SyncBackend for tests, ZioBackend for production) ────────

pub const BackendMode = enum {
    sync,
    zio,
};

// ── Iteration profiling ───────────────────────────────────────────────────

pub const IterationProfile = struct {
    iteration: u16,
    state_entered: FsmState,
    deterministic_time_us: u64 = 0,
    llm_time_us: u64 = 0,
    total_time_us: u64 = 0,
    action: ActionType,
    used_cache: bool = false,
};
