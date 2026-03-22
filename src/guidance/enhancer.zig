/// AI Docstring Enhancer for Zig guidance generation.
///
/// Mirrors Python's AIDocstringEnhancer class in guidance.py.
/// Generates descriptions (≤240 chars) for functions, structs, and files
/// by calling the configured LLM endpoint.  The enhancer is opt-in: sync
/// runs without it by default; pass --upgrade-comments or --regen_comments to
/// activate.
///
/// Behaviour mirrors Python AIDocstringEnhancer:
///   - infill_comments: enhance only new members or those whose API hash changed.
///   - regen_comments: exhaustive mode — regenerate for all members, keep best score.
///   - File-level documentation is generated from the module name + member list.
///   - Tags (e.g. #ring-buffer) are extracted from the last line "Tags: #a #b".
///   - Hallucination guard: responses containing terms absent from source are rejected.
const std = @import("std");
const llm = @import("common");

/// Default max_tokens for non-thinking models.
const DEFAULT_MAX_TOKENS: usize = 1000;

/// Max_tokens for thinking models (generous for local models).
const THINKING_MAX_TOKENS: usize = 4000;

/// Result returned by each enhancement call.
pub const EnrichmentResult = struct {
    /// The generated or preserved description text (≤240 chars enforced by prompt).
    /// Owned by the caller; free with allocator.free().
    comment: ?[]const u8,
    /// Hashtags extracted from the "Tags: #a #b" line.  Each tag is heap-allocated.
    tags: []const []const u8,

    pub fn deinit(self: EnrichmentResult, allocator: std.mem.Allocator) void {
        if (self.comment) |d| allocator.free(d);
        for (self.tags) |t| allocator.free(t);
        allocator.free(self.tags);
    }
};

/// Manages static configuration structures; owned by the module; ensures consistent initialization state.
pub const Enhancer = struct {
    allocator: std.mem.Allocator,
    client: llm.LlmClient,
    config: llm.LlmConfig,
    /// Owns the api_url string if we allocated it; null if config.api_url is static.
    owned_url: ?[]const u8,
    debug: bool,

    pub fn init(allocator: std.mem.Allocator, config: llm.LlmConfig) !Enhancer {
        // Dupe the api_url so we own it and can free it in deinit.
        // This is necessary because LlmConfig.api_url is a slice that might
        // point to temporary memory (e.g., from std.fmt.allocPrint).
        const owned_url = try allocator.dupe(u8, config.api_url);
        var owned_config = config;
        owned_config.api_url = owned_url;

        return .{
            .allocator = allocator,
            .client = try llm.LlmClient.init(allocator, owned_config),
            .config = owned_config,
            .owned_url = owned_url,
            .debug = config.debug,
        };
    }

    pub fn deinit(self: *Enhancer) void {
        self.client.deinit();
        if (self.owned_url) |url| {
            self.allocator.free(url);
        }
    }

    /// Check whether the LLM endpoint is reachable.
    pub fn available(self: *Enhancer) bool {
        return self.client.available();
    }

    /// Get appropriate max_tokens for this model.
    /// Thinking models need more tokens for chain-of-thought.
    fn maxTokens(self: Enhancer, base_tokens: usize) usize {
        if (self.config.isThinkingModel()) {
            return THINKING_MAX_TOKENS;
        }
        return base_tokens;
    }

    // -------------------------------------------------------------------------
    // Public enhancement API
    // -------------------------------------------------------------------------

    /// Generate a description for a Zig function / method.
    ///
    /// Returns an EnrichmentResult owned by the caller.
    /// Falls back to `existing_doc` (duped) if the LLM is unavailable or
    /// produces a hallucinated response.
    pub fn enhanceFunction(
        self: *Enhancer,
        name: []const u8,
        signature: []const u8,
        existing_doc: ?[]const u8,
        module_context: []const u8,
    ) !EnrichmentResult {
        const prompt = try self.buildFunctionPrompt(name, signature, existing_doc, module_context);
        defer self.allocator.free(prompt);

        return self.runEnhancement(name, prompt, existing_doc);
    }

    /// Generate a description for a Zig struct / enum / union.
    pub fn enhanceStruct(
        self: *Enhancer,
        name: []const u8,
        signature: []const u8,
        method_sigs: []const []const u8,
        existing_doc: ?[]const u8,
        module_context: []const u8,
    ) !EnrichmentResult {
        const prompt = try self.buildStructPrompt(name, signature, method_sigs, existing_doc, module_context);
        defer self.allocator.free(prompt);

        return self.runEnhancement(name, prompt, existing_doc);
    }

    /// Generate a one-line file-level description (≤200 chars) for STRUCTURE.md.
    pub fn enhanceFile(
        self: *Enhancer,
        rel_path: []const u8,
        existing_doc: ?[]const u8,
        source_preview: []const u8,
    ) !?[]const u8 {
        const prompt = try self.buildFilePrompt(rel_path, existing_doc, source_preview);
        defer self.allocator.free(prompt);

        if (self.debug) std.debug.print("[enhancer] generating file doc for {s}\n", .{rel_path});
        if (self.debug) std.debug.print("[enhancer] prompt (len={}):\n{s}\n", .{ prompt.len, prompt });

        const raw = self.client.complete(prompt, self.maxTokens(600), 0.2, null) catch |err| {
            if (self.debug) std.debug.print("[enhancer] LLM error for file doc: {}\n", .{err});
            return if (existing_doc) |d| try self.allocator.dupe(u8, d) else null;
        };
        const response = raw orelse {
            if (self.debug) std.debug.print("[enhancer] LLM returned null for file doc\n", .{});
            return if (existing_doc) |d| try self.allocator.dupe(u8, d) else null;
        };
        defer self.allocator.free(response);

        if (self.debug) std.debug.print("[enhancer] raw response for {s}:\n{s}\n---\n", .{ rel_path, response });

        // Extract <comment>...</comment> tag.  The prompts mandate this format;
        // if the tag is absent the response is unusable.
        const tagged = llm.extractCommentTag(response) orelse {
            if (self.debug) std.debug.print("[enhancer] no <comment> tag in response for {s}\n", .{rel_path});
            return if (existing_doc) |d| try self.allocator.dupe(u8, d) else null;
        };

        // Reject chain-of-thought that leaked into the tag.
        if (llm.isMalformedResponse(tagged)) {
            if (self.debug) std.debug.print("[enhancer] malformed file tag content for {s}: '{s}'\n", .{ rel_path, tagged });
            return if (existing_doc) |d| try self.allocator.dupe(u8, d) else null;
        }

        if (self.debug) std.debug.print("[enhancer] tag-extracted for {s}: '{s}'\n", .{ rel_path, tagged });
        // Enforce 200-char cap for STRUCTURE.md inline comments.
        const cap: usize = 200;
        const truncated = if (tagged.len > cap) tagged[0..cap] else tagged;
        return try self.allocator.dupe(u8, truncated);
    }

    // -------------------------------------------------------------------------
    // Score a description for quality (mirrors Python's _score_comment).
    // -------------------------------------------------------------------------

    pub fn scoreDocstring(text: []const u8) u32 {
        if (text.len == 0) return 0;
        var score: u32 = 0;
        if (text.len > 50) score += 1;
        const check_len = @min(text.len, 512);
        var buf: [512]u8 = undefined;
        const lower = std.ascii.lowerString(buf[0..check_len], text[0..check_len]);
        if (std.mem.indexOf(u8, lower, "args:") != null or
            std.mem.indexOf(u8, lower, "parameters") != null) score += 2;
        if (std.mem.indexOf(u8, lower, "returns:") != null or
            std.mem.indexOf(u8, lower, "return") != null) score += 2;
        if (std.mem.indexOf(u8, lower, "error") != null or
            std.mem.indexOf(u8, lower, "raises:") != null) score += 1;
        var newline_count: u32 = 0;
        for (text) |c| if (c == '\n') {
            newline_count += 1;
        };
        if (newline_count > 2) score += 1;
        return score;
    }

    // -------------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------------

    fn runEnhancement(
        self: *Enhancer,
        name: []const u8,
        prompt: []const u8,
        existing_doc: ?[]const u8,
    ) !EnrichmentResult {
        if (self.debug) std.debug.print("[enhancer] enhancing {s}\n", .{name});
        if (self.debug) std.debug.print("[enhancer] prompt:\n{s}\n---\n", .{prompt});

        const raw = self.client.complete(prompt, self.maxTokens(DEFAULT_MAX_TOKENS), 0.3, null) catch {
            if (self.debug) std.debug.print("[enhancer] LLM unavailable for {s}\n", .{name});
            return self.fallback(existing_doc);
        };
        const response = raw orelse return self.fallback(existing_doc);
        defer self.allocator.free(response);

        if (self.debug) std.debug.print("[enhancer] raw response for {s}:\n{s}\n---\n", .{ name, response });

        // Extract <comment>...</comment> tag.  The prompts mandate this format;
        // if the tag is absent the response is unusable — fall back to existing.
        const tagged = llm.extractCommentTag(response) orelse {
            if (self.debug) std.debug.print("[enhancer] no <comment> tag in response for {s}\n", .{name});
            return self.fallback(existing_doc);
        };

        // Reject chain-of-thought that leaked into the tag (e.g. reasoning models
        // emitting "we need to write a comment..." inside <comment> tags).
        if (llm.isMalformedResponse(tagged)) {
            if (self.debug) std.debug.print("[enhancer] malformed tag content for {s}: '{s}'\n", .{ name, tagged });
            return self.fallback(existing_doc);
        }

        if (self.debug) std.debug.print("[enhancer] tag-extracted for {s}: '{s}'\n", .{ name, tagged });
        const tags = try self.extractTags(tagged);
        const stripped = try self.stripTagsLine(tagged);
        return .{ .comment = stripped, .tags = tags };
    }

    fn fallback(self: *Enhancer, existing_doc: ?[]const u8) !EnrichmentResult {
        const doc = if (existing_doc) |d| try self.allocator.dupe(u8, d) else null;
        const tags = try self.allocator.alloc([]const u8, 0);
        return .{ .comment = doc, .tags = tags };
    }

    /// Extract #hashtags from a "Tags: #a #b" line at the end of the response.
    fn extractTags(self: *Enhancer, text: []const u8) ![]const []const u8 {
        var tags: std.ArrayList([]const u8) = .{};
        errdefer {
            for (tags.items) |t| self.allocator.free(t);
            tags.deinit(self.allocator);
        }

        // Find the last occurrence of "tags:" (case-insensitive).
        const lower_text = try std.ascii.allocLowerString(self.allocator, text);
        defer self.allocator.free(lower_text);

        const tags_pos = lastIndexOf(lower_text, "tags:") orelse return tags.toOwnedSlice(self.allocator);
        const after_tags = text[tags_pos + 5 ..];

        var it = std.mem.tokenizeScalar(u8, after_tags, ' ');
        while (it.next()) |tok| {
            const trimmed = std.mem.trim(u8, tok, " \t\r\n,;");
            if (trimmed.len == 0) continue;
            if (trimmed[0] == '#') {
                // Strip the '#' and lowercase.
                const tag_word = trimmed[1..];
                if (tag_word.len == 0) continue;
                const lower_tag = try std.ascii.allocLowerString(self.allocator, tag_word);
                try tags.append(self.allocator, lower_tag);
            }
        }

        return tags.toOwnedSlice(self.allocator);
    }

    /// Remove the trailing "Tags: #a #b" line from text.
    fn stripTagsLine(self: *Enhancer, text: []const u8) !?[]const u8 {
        const lower = try std.ascii.allocLowerString(self.allocator, text);
        defer self.allocator.free(lower);

        // Find "tags:" in the last 80 chars.
        const search_start: usize = if (text.len > 80) text.len - 80 else 0;
        const tags_pos = std.mem.indexOf(u8, lower[search_start..], "tags:") orelse {
            return try self.allocator.dupe(u8, std.mem.trim(u8, text, " \t\r\n"));
        };
        const abs_pos = search_start + tags_pos;

        // Walk back to the start of that line.
        var line_start = abs_pos;
        while (line_start > 0 and text[line_start - 1] != '\n') {
            line_start -= 1;
        }

        const stripped = std.mem.trim(u8, text[0..line_start], " \t\r\n");
        return try self.allocator.dupe(u8, stripped);
    }

    // -------------------------------------------------------------------------
    // Prompt builders
    // -------------------------------------------------------------------------

    fn buildFunctionPrompt(
        self: *Enhancer,
        name: []const u8,
        signature: []const u8,
        existing_doc: ?[]const u8,
        module_context: []const u8,
    ) ![]const u8 {
        var buf: std.ArrayList(u8) = .{};
        const w = buf.writer(self.allocator);

        try w.print("Zig function in {s}:\n  {s}\n\n", .{ module_context, signature });

        if (existing_doc) |d| {
            if (d.len > 0) {
                try w.print("Existing comment: {s}\n\n", .{d});
            }
        }

        try w.writeAll(
            \\Write a single-line comment for this function.
            \\Rules:
            \\- Plain English, technically specific — state what it does, key args, return value or error
            \\- Max 200 characters
            \\- No boilerplate openers ("This function", "A function that")
            \\
            \\Wrap your answer in <comment> tags. Example:
            \\<comment>Parses a null-terminated C string into an owned Zig slice.</comment>
            \\
        );
        try w.print("Function: {s}\n", .{name});

        return buf.toOwnedSlice(self.allocator);
    }

    fn buildStructPrompt(
        self: *Enhancer,
        name: []const u8,
        signature: []const u8,
        method_sigs: []const []const u8,
        existing_doc: ?[]const u8,
        module_context: []const u8,
    ) ![]const u8 {
        var buf: std.ArrayList(u8) = .{};
        const w = buf.writer(self.allocator);

        try w.print("Zig type in {s}:\n  {s}\n", .{ module_context, signature });

        const limit = @min(method_sigs.len, 6);
        if (limit > 0) {
            try w.writeAll("Methods:\n");
            for (method_sigs[0..limit]) |sig| {
                try w.print("  {s}\n", .{sig});
            }
            if (method_sigs.len > 6) {
                try w.print("  ... and {} more\n", .{method_sigs.len - 6});
            }
        }

        if (existing_doc) |d| {
            if (d.len > 0) {
                try w.print("\nExisting comment: {s}\n\n", .{d});
            }
        }

        try w.writeAll(
            \\
            \\Write a single-line comment for this type.
            \\Rules:
            \\- Plain English, technically specific — state purpose, ownership model, key invariants
            \\- Max 200 characters
            \\- No boilerplate openers ("This struct", "A type that")
            \\
            \\Wrap your answer in <comment> tags. Example:
            \\<comment>Owns a pool of fixed-size buffers; init/deinit pair; not thread-safe.</comment>
            \\
        );
        try w.print("Type: {s}\n", .{name});

        return buf.toOwnedSlice(self.allocator);
    }

    fn buildFilePrompt(
        self: *Enhancer,
        rel_path: []const u8,
        existing_doc: ?[]const u8,
        source_preview: []const u8,
    ) ![]const u8 {
        var buf: std.ArrayList(u8) = .{};
        const w = buf.writer(self.allocator);

        // First 3000 chars of source gives the model the module doc comment and
        // enough declarations to understand what the file actually does.
        const preview_len = @min(source_preview.len, 3000);
        if (preview_len > 0) {
            try w.writeAll("Source:\n");
            try w.writeAll(source_preview[0..preview_len]);
            try w.writeByte('\n');
        }

        try w.print("\nFile: {s}\n", .{rel_path});

        if (existing_doc) |d| {
            if (d.len > 0) {
                try w.print("Existing comment: {s}\n\n", .{d});
            }
        }

        try w.writeAll(
            \\
            \\Write a single-line description for this file.
            \\Rules:
            \\- Plain English, technically specific: key types, algorithms, or responsibilities
            \\- Max 200 chars
            \\- No boilerplate openers ("This file", "A module that")
            \\- Do NOT include a skills prefix — that is added automatically
            \\
            \\Wrap your answer in <comment> tags. Example:
            \\<comment>Parses Zig AST and extracts public member signatures for guidance generation.</comment>
            \\
        );

        return buf.toOwnedSlice(self.allocator);
    }

    // -------------------------------------------------------------------------
    // Module detail generation (thinking model)
    // -------------------------------------------------------------------------

    /// Result of module detail generation.
    pub const ModuleDetailResult = struct {
        detail: []const u8,
        keywords: []const []const u8,
        comment: []const u8,

        pub fn deinit(self: ModuleDetailResult, allocator: std.mem.Allocator) void {
            allocator.free(self.detail);
            for (self.keywords) |kw| allocator.free(kw);
            allocator.free(self.keywords);
            allocator.free(self.comment);
        }
    };

    /// Generate comprehensive module documentation using the thinking model.
    /// Input: source code, member signatures, capabilities, skills.
    /// Output: detail (<800 words), keywords (5-10), comment (≤200 chars).
    pub fn enhanceModuleDetail(
        self: *Enhancer,
        module_name: []const u8,
        source_content: []const u8,
        member_signatures: []const []const u8,
        capabilities: []const u8,
        skills: []const u8,
        existing_comment: ?[]const u8,
    ) !ModuleDetailResult {
        // Build prompt for thinking model
        const prompt = try self.buildModuleDetailPrompt(module_name, source_content, member_signatures, capabilities, skills, existing_comment);
        defer self.allocator.free(prompt);

        if (self.debug) std.debug.print("[enhancer] generating module detail for {s}\n", .{module_name});

        // Use thinking model (higher max_tokens for chain-of-thought)
        const raw = self.client.complete(prompt, self.maxTokens(2400), 0.3, null) catch {
            return self.fallbackModuleDetail(existing_comment);
        };
        const response = raw orelse return self.fallbackModuleDetail(existing_comment);
        defer self.allocator.free(response);

        if (self.debug) std.debug.print("[enhancer] raw response for {s} (len={})\n", .{ module_name, response.len });

        // Parse response: <detail>...</detail> <keywords>...</keywords> <comment>...</comment>
        return self.parseModuleDetailResponse(response, existing_comment);
    }

    fn buildModuleDetailPrompt(
        self: *Enhancer,
        module_name: []const u8,
        source_content: []const u8,
        member_signatures: []const []const u8,
        capabilities: []const u8,
        skills: []const u8,
        existing_comment: ?[]const u8,
    ) ![]const u8 {
        var buf: std.ArrayList(u8) = .{};
        const w = buf.writer(self.allocator);

        try w.print("You are documenting a Zig module for an AI coding assistant.\n\n", .{});
        try w.print("MODULE: {s}\n\n", .{module_name});

        // Source preview (first 4000 chars)
        const src_preview = source_content[0..@min(4000, source_content.len)];
        try w.print("SOURCE CODE:\n{s}\n\n", .{src_preview});

        // Member signatures
        if (member_signatures.len > 0) {
            try w.writeAll("PUBLIC API:\n");
            const limit = @min(member_signatures.len, 15);
            for (member_signatures[0..limit]) |sig| {
                try w.print("  {s}\n", .{sig});
            }
            try w.writeByte('\n');
        }

        // Capabilities
        if (capabilities.len > 0) {
            try w.print("RELATED CAPABILITIES:\n{s}\n\n", .{capabilities});
        }

        // Skills
        if (skills.len > 0) {
            try w.print("RELATED SKILLS:\n{s}\n\n", .{skills});
        }

        if (existing_comment) |c| {
            if (c.len > 0) {
                try w.print("EXISTING COMMENT: {s}\n\n", .{c});
            }
        }

        try w.writeAll(
            \\Generate comprehensive module documentation. Output three sections:
            \\
            \\1. <detail>...</detail> — A detailed description (under 800 words) covering:
            \\   - Module purpose and architecture
            \\   - Key abstractions and their relationships
            \\   - Public API and usage patterns
            \\   - Important implementation details
            \\   - Design patterns used
            \\
            \\2. <keywords>...</keywords> — 5-10 discovery keywords (comma-separated) that would help find this module:
            \\   - Unique API names (structs, functions)
            \\   - Domain concepts
            \\   - Design patterns
            \\   - Technical terms
            \\
            \\3. <comment>...</comment> — A concise one-line description (≤200 chars) for the module.
            \\
        );

        return buf.toOwnedSlice(self.allocator);
    }

    fn parseModuleDetailResponse(
        self: *Enhancer,
        response: []const u8,
        existing_comment: ?[]const u8,
    ) !ModuleDetailResult {
        // Extract <detail> tag
        const detail = blk: {
            const start = std.mem.indexOf(u8, response, "<detail>") orelse break :blk null;
            const end = std.mem.indexOf(u8, response[start..], "</detail>") orelse break :blk null;
            const content = std.mem.trim(u8, response[start + 8 .. start + end], " \t\n\r");
            break :blk try self.allocator.dupe(u8, content);
        };

        // Extract <keywords> tag
        var keywords: std.ArrayList([]const u8) = .{};
        errdefer {
            for (keywords.items) |kw| self.allocator.free(kw);
            keywords.deinit(self.allocator);
        }

        if (std.mem.indexOf(u8, response, "<keywords>")) |start| {
            if (std.mem.indexOf(u8, response[start..], "</keywords>")) |end_offset| {
                const end = start + end_offset;
                const kw_text = std.mem.trim(u8, response[start + 10 .. end], " \t\n\r");
                var it = std.mem.splitAny(u8, kw_text, ",\n");
                var count: usize = 0;
                while (it.next()) |kw| {
                    if (count >= 10) break;
                    const trimmed = std.mem.trim(u8, kw, " \t\r");
                    if (trimmed.len > 0 and trimmed.len < 50) {
                        try keywords.append(self.allocator, try self.allocator.dupe(u8, trimmed));
                        count += 1;
                    }
                }
            }
        }

        // Extract <comment> tag
        const comment = blk: {
            const start = std.mem.indexOf(u8, response, "<comment>") orelse break :blk null;
            const end = std.mem.indexOf(u8, response[start..], "</comment>") orelse break :blk null;
            const content = std.mem.trim(u8, response[start + 9 .. start + end], " \t\n\r");
            // Cap at 200 chars
            const capped = content[0..@min(200, content.len)];
            break :blk try self.allocator.dupe(u8, capped);
        };

        // Fallbacks
        const final_detail = detail orelse try self.allocator.dupe(u8, "");
        const final_keywords = try keywords.toOwnedSlice(self.allocator);
        const final_comment = comment orelse if (existing_comment) |c| try self.allocator.dupe(u8, c) else try self.allocator.dupe(u8, "");

        return .{
            .detail = final_detail,
            .keywords = final_keywords,
            .comment = final_comment,
        };
    }

    fn fallbackModuleDetail(self: *Enhancer, existing_comment: ?[]const u8) !ModuleDetailResult {
        const comment = if (existing_comment) |c| try self.allocator.dupe(u8, c) else try self.allocator.dupe(u8, "");
        return .{
            .detail = try self.allocator.dupe(u8, ""),
            .keywords = try self.allocator.alloc([]const u8, 0),
            .comment = comment,
        };
    }
};

// ---------------------------------------------------------------------------
// Pure utilities (no allocator needed)
// ---------------------------------------------------------------------------

fn lastIndexOf(haystack: []const u8, needle: []const u8) ?usize {
    if (needle.len > haystack.len) return null;
    var i: usize = haystack.len - needle.len + 1;
    while (i > 0) {
        i -= 1;
        if (std.mem.eql(u8, haystack[i .. i + needle.len], needle)) return i;
    }
    return null;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "scoreDocstring empty" {
    try std.testing.expectEqual(@as(u32, 0), Enhancer.scoreDocstring(""));
}

test "scoreDocstring quality" {
    const good = "Parses JSON from a slice.\n\nArgs:\n  data: input slice\nReturns: parsed value\nRaises: error on malformed input";
    const score = Enhancer.scoreDocstring(good);
    try std.testing.expect(score >= 4);
}

test "extractTags parses hashtags" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    var e = try Enhancer.init(gpa.allocator(), .{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "test",
    });
    defer e.deinit();
    const alloc = gpa.allocator();

    const text = "Parses a JSON slice and returns a Value.\nTags: #json #parser #zig";
    const tags = try e.extractTags(text);
    defer {
        for (tags) |t| alloc.free(t);
        alloc.free(tags);
    }
    try std.testing.expectEqual(@as(usize, 3), tags.len);
    try std.testing.expectEqualStrings("json", tags[0]);
    try std.testing.expectEqualStrings("parser", tags[1]);
    try std.testing.expectEqualStrings("zig", tags[2]);
}

test "extractTags returns empty when no Tags line" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    var e = try Enhancer.init(gpa.allocator(), .{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "test",
    });
    defer e.deinit();
    const alloc = gpa.allocator();

    const tags = try e.extractTags("Just a plain description with no tags.");
    defer {
        for (tags) |t| alloc.free(t);
        alloc.free(tags);
    }
    try std.testing.expectEqual(@as(usize, 0), tags.len);
}

test "stripTagsLine removes Tags line" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    var e = try Enhancer.init(gpa.allocator(), .{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "test",
    });
    defer e.deinit();
    const alloc = gpa.allocator();

    const text = "Parses JSON from a byte slice.\nTags: #json #zig";
    const stripped = try e.stripTagsLine(text);
    defer if (stripped) |s| alloc.free(s);
    try std.testing.expectEqualStrings("Parses JSON from a byte slice.", stripped.?);
}

test "lastIndexOf basic" {
    try std.testing.expectEqual(@as(?usize, 6), lastIndexOf("hello tags: #a", "tags:"));
    try std.testing.expectEqual(@as(?usize, null), lastIndexOf("no match", "tags:"));
}

