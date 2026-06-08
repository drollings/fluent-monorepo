//! AGENTS.md content generator for guidance init.
const std = @import("std");

pub const AGENTS_INSERTION: []const u8 =
    \\---
    \\
    \\## guidance Integration
    \\
    \\This project uses guidance for AST-guided code navigation.
    \\
    \\```
    \\# Initialize and run
    \\guidance init
    \\guidance check
    \\```
    \\
;

pub fn generateAgentsMdContent(allocator: std.mem.Allocator, guidance_dir: []const u8) ![]const u8 {
    var buf: std.ArrayList(u8) = .empty;
    defer buf.deinit(allocator);

    try buf.appendSlice(allocator,
        \\# guidance Integration
        \\
        \\This project uses guidance for AST-guided code navigation and documentation.
        \\
        \\## Quick Start
        \\
        \\```
        \\# Initialize guidance configuration
        \\guidance init
        \\
        \\# Run the full RALPH loop (build → test → lint → fmt → guidance)
        \\guidance check
        \\
        \\# Query the codebase
        \\guidance explain "how does X work?"
        \\```
        \\
        \\## Key Files
        \\
        \\# Agent Bootloader — guidance
        \\
        \\**Context**: guidance is a Zig-native, deterministic-first AST-guided vector search
        \\database generator with local AI enhancement.  When used to search the
        \\codebase's capabilities and code, it can save over 90% of the tokens and tool
        \\calls compared to the orchestrating AI coder using other tools.
        \\
        \\## Prime Directive
        \\
        \\1. **Never guess**: use `guidance explain "<query text>" for guidance, and
        \\follow instructions for any queries of interest
        \\
        \\---
        \\
        \\## Quick Start: RALPH Loop (Discovery → Implementation)
        \\
        \\```
        \\1. DISCOVER (guidance):  guidance explain "<keywords or a short question>"
        \\                         Prefer keywords: "cmdExplain"
        \\                         Or, prefer a short question: "How do we sync guidance?"
        \\                         Scan: module purpose, pattern type, skill list
        \\
        \\2. UNDERSTAND (MCP):     Read the primary source file(s) from step 1
        \\                         Grep callers: who @import's this file?
        \\                         Ask: do the listed skills actually apply?
        \\
        \\3. DECIDE:               If skills match → read them
        \\                         If not → proceed to implementation
        \\
        \\4. IMPLEMENT:            Write to src/guidance/ or bin/ (for Python or
        \\                         other languages apart from Zig, i.e.  guidance-py)
        \\                         Follow source patterns and applicable skills only
        \\
        \\5. VERIFY (make):        make pre-commit
        \\                         build → test → lint → guidance gen → STRUCTURE.md
        \\```
        \\---
        \\
        \\## Source Layout
        \\
        \\```
        \\
    );
    try buf.appendSlice(allocator, "- `");
    try buf.appendSlice(allocator, guidance_dir);
    try buf.appendSlice(allocator, "/guidance-config.json` — Model and provider configuration\n");
    try buf.appendSlice(allocator, "- `");
    try buf.appendSlice(allocator, guidance_dir);
    try buf.appendSlice(allocator, "/src/` — Generated guidance JSON files\n");
    try buf.appendSlice(allocator,
        \\- `.guidance.db` — SQLite vector search database
        \\- `STRUCTURE.md` — Project structure documentation (auto-generated)
        \\
        \\---
        \\
        \\**DO:**
        \\- Run `guidance explain "<query>"` and read the results
        \\- Ask: "What capabilitity is used here?" before consulting skills
        \\
        \\**DON'T:**
        \\- Assume skills apply without validating against source code
        \\- Write any code in Zig without reading `doc/skills/zig-current/SKILL.md` first
    );

    return try buf.toOwnedSlice(allocator);
}
