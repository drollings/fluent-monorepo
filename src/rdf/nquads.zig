/// nquads.zig — N-Quads / N-Triples Parser (line-based, no prefix expansion)
///
/// Supports:
///   <s> <p> <o> <g> .   — quad with named graph
///   <s> <p> "literal" . — triple (no graph)
///   _:bn <p> <o> .      — blank node subject
///
/// Usage:
///   var parser = NQuadsParser.init(allocator, source);
///   defer parser.deinit();
///   while (try parser.next()) |quad| { ... quad.deinit(allocator); }
const std = @import("std");
const parser_mod = @import("parser.zig");
const Term = parser_mod.Term;
const Literal = parser_mod.Literal;
const TermType = parser_mod.TermType;

/// Represents a quad structure for spatial queries, managed with a fixed ownership model; key invariant is consistent indexing.
pub const Quad = struct {
    subject: Term,
    predicate: Term,
    object: Term,
    graph: ?Term, // null → default graph

    pub fn deinit(self: Quad, allocator: std.mem.Allocator) void {
        self.subject.deinit(allocator);
        self.predicate.deinit(allocator);
        self.object.deinit(allocator);
        if (self.graph) |g| g.deinit(allocator);
    }
};

pub const NQuadsError = error{
    MalformedLine,
    OutOfMemory,
};

/// Manages NQuad parsing structures, owns parsing state, supports fixed-size buffers; not thread-safe.
pub const NQuadsParser = struct {
    allocator: std.mem.Allocator,
    lines: std.mem.SplitIterator(u8, .scalar),

    pub fn init(allocator: std.mem.Allocator, source: []const u8) NQuadsParser {
        return .{
            .allocator = allocator,
            .lines = std.mem.splitScalar(u8, source, '\n'),
        };
    }

    pub fn deinit(self: *NQuadsParser) void {
        _ = self;
    }

    /// Returns next Quad or null at EOF. Caller must call quad.deinit(allocator).
    pub fn next(self: *NQuadsParser) NQuadsError!?Quad {
        while (self.lines.next()) |raw_line| {
            const line = std.mem.trim(u8, raw_line, " \t\r");
            if (line.len == 0 or line[0] == '#') continue;
            const quad = try self.parseLine(line);
            return quad;
        }
        return null;
    }

    fn parseLine(self: *NQuadsParser, line: []const u8) NQuadsError!Quad {
        var pos: usize = 0;

        const subj = try self.parseTerm(line, &pos);
        errdefer subj.deinit(self.allocator);
        skipWs(line, &pos);

        const pred = try self.parseTerm(line, &pos);
        errdefer pred.deinit(self.allocator);
        skipWs(line, &pos);

        const obj = try self.parseTerm(line, &pos);
        errdefer obj.deinit(self.allocator);
        skipWs(line, &pos);

        // Optional graph IRI
        var graph: ?Term = null;
        if (pos < line.len and line[pos] == '<') {
            graph = try self.parseTerm(line, &pos);
            skipWs(line, &pos);
        }

        // Expect trailing '.'
        if (pos >= line.len or line[pos] != '.') return error.MalformedLine;

        return Quad{ .subject = subj, .predicate = pred, .object = obj, .graph = graph };
    }

    fn skipWs(line: []const u8, pos: *usize) void {
        while (pos.* < line.len and (line[pos.*] == ' ' or line[pos.*] == '\t')) {
            pos.* += 1;
        }
    }

    fn parseTerm(self: *NQuadsParser, line: []const u8, pos: *usize) NQuadsError!Term {
        skipWs(line, pos);
        if (pos.* >= line.len) return error.MalformedLine;

        const c = line[pos.*];
        if (c == '<') return self.parseIRI(line, pos);
        if (c == '_') return self.parseBlankNode(line, pos);
        if (c == '"') return self.parseLiteral(line, pos);
        return error.MalformedLine;
    }

    fn parseIRI(self: *NQuadsParser, line: []const u8, pos: *usize) NQuadsError!Term {
        pos.* += 1; // skip '<'
        const start = pos.*;
        while (pos.* < line.len and line[pos.*] != '>') pos.* += 1;
        if (pos.* >= line.len) return error.MalformedLine;
        const iri = self.allocator.dupe(u8, line[start..pos.*]) catch return error.OutOfMemory;
        pos.* += 1; // skip '>'
        return Term{ .iri = iri };
    }

    fn parseBlankNode(self: *NQuadsParser, line: []const u8, pos: *usize) NQuadsError!Term {
        if (pos.* + 1 >= line.len or line[pos.* + 1] != ':') return error.MalformedLine;
        pos.* += 2; // skip '_:'
        const start = pos.*;
        while (pos.* < line.len and line[pos.*] != ' ' and line[pos.*] != '\t' and line[pos.*] != '.') {
            pos.* += 1;
        }
        const id = self.allocator.dupe(u8, line[start..pos.*]) catch return error.OutOfMemory;
        return Term{ .blank_node = id };
    }

    fn parseLiteral(self: *NQuadsParser, line: []const u8, pos: *usize) NQuadsError!Term {
        pos.* += 1; // skip opening '"'
        const start = pos.*;
        // Scan for closing '"', handling backslash escapes
        while (pos.* < line.len) {
            if (line[pos.*] == '\\') {
                pos.* += 2;
            } else if (line[pos.*] == '"') {
                break;
            } else {
                pos.* += 1;
            }
        }
        if (pos.* >= line.len) return error.MalformedLine;
        const value = self.allocator.dupe(u8, line[start..pos.*]) catch return error.OutOfMemory;
        pos.* += 1; // skip closing '"'
        errdefer self.allocator.free(value);

        // Check for @lang or ^^datatype
        if (pos.* < line.len and line[pos.*] == '@') {
            pos.* += 1;
            const lang_start = pos.*;
            while (pos.* < line.len and line[pos.*] != ' ' and line[pos.*] != '\t' and line[pos.*] != '.') {
                pos.* += 1;
            }
            const lang = self.allocator.dupe(u8, line[lang_start..pos.*]) catch return error.OutOfMemory;
            return Term{ .literal = Literal{ .value = value, .lang = lang, .datatype = null } };
        }
        if (pos.* + 1 < line.len and line[pos.*] == '^' and line[pos.* + 1] == '^') {
            pos.* += 2;
            const dt_term = try self.parseIRI(line, pos);
            const dt_iri = dt_term.iri;
            return Term{ .literal = Literal{ .value = value, .lang = null, .datatype = dt_iri } };
        }
        return Term{ .literal = Literal{ .value = value, .lang = null, .datatype = null } };
    }
};

// =============================================================================
// Tests — Milestone 1.4
// =============================================================================

const testing = std.testing;

test "nquads quad with graph" {
    const src = "<http://s> <http://p> <http://o> <http://g> .";
    var p = NQuadsParser.init(testing.allocator, src);
    defer p.deinit();
    const q = (try p.next()).?;
    defer q.deinit(testing.allocator);
    try testing.expectEqualStrings("http://s", q.subject.iri);
    try testing.expectEqualStrings("http://g", q.graph.?.iri);
}

test "nquads triple without graph" {
    const src = "<http://s> <http://p> <http://o> .";
    var p = NQuadsParser.init(testing.allocator, src);
    defer p.deinit();
    const q = (try p.next()).?;
    defer q.deinit(testing.allocator);
    try testing.expect(q.graph == null);
}

test "nquads literal object" {
    const src = "<http://s> <http://p> \"hello world\" .";
    var p = NQuadsParser.init(testing.allocator, src);
    defer p.deinit();
    const q = (try p.next()).?;
    defer q.deinit(testing.allocator);
    try testing.expectEqualStrings("hello world", q.object.literal.value);
}

test "nquads comment and blank line skipped" {
    const src = "# comment\n\n<http://s> <http://p> <http://o> .";
    var p = NQuadsParser.init(testing.allocator, src);
    defer p.deinit();
    const q = (try p.next()).?;
    defer q.deinit(testing.allocator);
    try testing.expectEqualStrings("http://s", q.subject.iri);
}

test "nquads malformed line error" {
    const src = "this is not valid";
    var p = NQuadsParser.init(testing.allocator, src);
    defer p.deinit();
    const result = p.next();
    try testing.expectError(error.MalformedLine, result);
}

test "nquads blank node subject" {
    const src = "_:b1 <http://p> <http://o> .";
    var p = NQuadsParser.init(testing.allocator, src);
    defer p.deinit();
    const q = (try p.next()).?;
    defer q.deinit(testing.allocator);
    try testing.expectEqualStrings("b1", q.subject.blank_node);
}


