//! Tests for token_budget.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const token_budget_mod = @import("token_budget.zig");

test "ProportionalBudget: validate passes on correct fractions" {
    const b = token_budget_mod.ProportionalBudget{ .total = 1000 };
    try b.validate();
}
