/// parser.zig — Streaming Recursive-Descent Turtle Parser
///
/// Produces RDF triples one at a time via an iterator.
/// No full AST materialization — memory-efficient for large files.
///
/// Supports:
///   - Simple triples: <s> <p> <o> .
///   - Predicate-object lists: <s> <p1> <o1> ; <p2> <o2> .
///   - Object lists: <s> <p> <o1> , <o2> .
///   - 'a' shorthand for rdf:type
///   - @prefix and @base directives
///   - Blank nodes: _:id and [ <p> <o> ]
///
/// Usage:
///   var parser = try Parser.init(allocator, source);
///   defer parser.deinit();
///   while (try parser.next()) |triple| { ... triple.deinit(allocator); }
const std = @import("std");
const lexer_mod = @import("lexer.zig");
const Lexer = lexer_mod.Lexer;
const Token = lexer_mod.Token;
const TokenType = lexer_mod.TokenType;

pub const RDF_TYPE = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// Represents a term type with enum definitions; managed via ownership model; key invariant is term structure integrity.
pub const TermType = enum { iri, blank_node, literal };

/// Represents a structured literal value in the parser, managing ownership and invariants for consistent interpretation.
pub const Literal = struct {
    value: []const u8,
    lang: ?[]const u8,
    datatype: ?[]const u8,

    pub fn deinit(self: Literal, allocator: std.mem.Allocator) void {
        allocator.free(self.value);
        if (self.lang) |l| allocator.free(l);
        if (self.datatype) |d| allocator.free(d);
    }
};

/// Represents a union of keywords in Zig, managing ownership and invariants for safe term evaluation.
pub const Term = union(TermType) {
    iri: []const u8,
    blank_node: []const u8,
    literal: Literal,

    pub fn deinit(self: Term, allocator: std.mem.Allocator) void {
        switch (self) {
            .iri => |s| allocator.free(s),
            .blank_node => |s| allocator.free(s),
            .literal => |l| l.deinit(allocator),
        }
    }
};

/// Represents a triple structure with ownership model and invariants; manages fixed-size buffers.
pub const Triple = struct {
    subject: Term,
    predicate: Term,
    object: Term,

    pub fn deinit(self: Triple, allocator: std.mem.Allocator) void {
        self.subject.deinit(allocator);
        self.predicate.deinit(allocator);
        self.object.deinit(allocator);
    }
};

pub const ParseError = error{
    UnexpectedToken,
    UnexpectedEOF,
    OutOfMemory,
    UnterminatedIRI,
    UnterminatedLiteral,
    InvalidEscape,
    UnexpectedChar,
    UnterminatedComment,
    InvalidPrefix,
};

/// Handles RDF parsing with a keyword structure, managing ownership and invariants for reliable processing.
pub const Parser = struct {
    // All fields first
    allocator: std.mem.Allocator,
    lex: Lexer,
    peeked: ?Token,
    prefix_map: std.StringHashMap([]const u8),
    base: ?[]const u8,
    blank_counter: u64,
    /// Queue of triples produced from a single subject's predicate-object list.
    queue: std.ArrayList(Triple),

    pub fn init(allocator: std.mem.Allocator, source: []const u8) !Parser {
        return Parser{
            .allocator = allocator,
            .lex = Lexer.init(source),
            .peeked = null,
            .prefix_map = std.StringHashMap([]const u8).init(allocator),
            .base = null,
            .blank_counter = 0,
            .queue = .{},
        };
    }

    pub fn deinit(self: *Parser) void {
        var it = self.prefix_map.iterator();
        while (it.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            self.allocator.free(entry.value_ptr.*);
        }
        self.prefix_map.deinit();
        if (self.base) |b| self.allocator.free(b);
        for (self.queue.items) |t| t.deinit(self.allocator);
        self.queue.deinit(self.allocator);
    }

    // -------------------------------------------------------------------------
    // Token stream helpers
    // -------------------------------------------------------------------------

    fn peekTok(self: *Parser) ParseError!Token {
        if (self.peeked == null) self.peeked = try self.lex.nextToken();
        return self.peeked.?;
    }

    fn consumeTok(self: *Parser) ParseError!Token {
        if (self.peeked) |tok| {
            self.peeked = null;
            return tok;
        }
        return self.lex.nextToken();
    }

    fn expectTok(self: *Parser, tt: TokenType) ParseError!Token {
        const tok = try self.consumeTok();
        if (tok.type != tt) return error.UnexpectedToken;
        return tok;
    }

    // -------------------------------------------------------------------------
    // Public iterator — returns null at EOF
    // -------------------------------------------------------------------------

    pub fn next(self: *Parser) ParseError!?Triple {
        while (true) {
            // Drain queue first
            if (self.queue.items.len > 0) return self.queue.orderedRemove(0);

            const tok = try self.peekTok();
            switch (tok.type) {
                .eof => return null,
                .keyword => {
                    const kw = try self.consumeTok();
                    if (std.mem.eql(u8, kw.value, "@prefix") or
                        std.mem.eql(u8, kw.value, "PREFIX"))
                    {
                        try self.parsePrefix();
                    } else if (std.mem.eql(u8, kw.value, "@base") or
                        std.mem.eql(u8, kw.value, "BASE"))
                    {
                        try self.parseBase();
                    } else {
                        return error.UnexpectedToken;
                    }
                },
                else => {
                    // Parse subject + predicate-object list into queue
                    try self.parseStatement();
                    // If queue is still empty (e.g. bare dot), loop again
                },
            }
        }
    }

    // -------------------------------------------------------------------------
    // Directives
    // -------------------------------------------------------------------------

    fn parsePrefix(self: *Parser) ParseError!void {
        const name_tok = try self.consumeTok();
        if (name_tok.type != .prefixed_name and name_tok.type != .keyword) {
            // bare ':' is emitted as prefixed_name with value ":"
            return error.UnexpectedToken;
        }
        const iri_tok = try self.expectTok(.iri);
        _ = try self.expectTok(.dot);

        const raw = name_tok.value;
        const colon_pos = std.mem.lastIndexOfScalar(u8, raw, ':') orelse return error.InvalidPrefix;
        const label = raw[0..colon_pos];
        const iri_inner = try self.extractIRI(iri_tok.value);

        if (self.prefix_map.getPtr(label)) |v| {
            self.allocator.free(v.*);
            v.* = iri_inner;
        } else {
            const owned = try self.allocator.dupe(u8, label);
            errdefer self.allocator.free(owned);
            try self.prefix_map.put(owned, iri_inner);
        }
    }

    fn parseBase(self: *Parser) ParseError!void {
        const iri_tok = try self.expectTok(.iri);
        _ = try self.expectTok(.dot);
        const new_base = try self.extractIRI(iri_tok.value);
        if (self.base) |old| self.allocator.free(old);
        self.base = new_base;
    }

    // -------------------------------------------------------------------------
    // Statement → subject predicate-object list .
    // -------------------------------------------------------------------------

    fn parseStatement(self: *Parser) ParseError!void {
        const subj = (try self.parseTerm()) orelse return error.UnexpectedEOF;
        defer subj.deinit(self.allocator);

        try self.collectPredicateObjectList(subj);

        // Consume trailing '.'
        const pk = try self.peekTok();
        if (pk.type == .dot) _ = try self.consumeTok();
    }

    /// Fill self.queue with all triples from one predicate-object list.
    fn collectPredicateObjectList(self: *Parser, subj: Term) ParseError!void {
        while (true) {
            // Parse verb
            const verb_opt = try self.parseVerb();
            if (verb_opt == null) break;
            const verb = verb_opt.?;
            defer verb.deinit(self.allocator);

            // Parse one or more objects
            while (true) {
                // Record queue length before parseTerm — inline blank nodes
                // may enqueue inner triples; we want the outer triple first.
                const insert_pos = self.queue.items.len;
                const obj = (try self.parseTerm()) orelse break;
                const sc = try self.cloneTerm(subj);
                const pc = try self.cloneTerm(verb);
                try self.queue.insert(self.allocator, insert_pos, Triple{ .subject = sc, .predicate = pc, .object = obj });

                // ',' → more objects for same verb
                const pk = try self.peekTok();
                if (pk.type == .comma) {
                    _ = try self.consumeTok();
                } else break;
            }

            // ';' → more verb-object pairs
            const pk = try self.peekTok();
            if (pk.type == .semicolon) {
                _ = try self.consumeTok();
                // Trailing ';' before '.' or EOF is allowed
                const pk2 = try self.peekTok();
                if (pk2.type == .dot or pk2.type == .eof or pk2.type == .blank_node_close) break;
            } else break;
        }
    }

    fn parseVerb(self: *Parser) ParseError!?Term {
        const tok = try self.peekTok();
        if (tok.type == .dot or tok.type == .eof or tok.type == .blank_node_close) return null;
        if (tok.type == .keyword and std.mem.eql(u8, tok.value, "a")) {
            _ = try self.consumeTok();
            return Term{ .iri = try self.allocator.dupe(u8, RDF_TYPE) };
        }
        return self.parseTerm();
    }

    fn parseTerm(self: *Parser) ParseError!?Term {
        const tok = try self.peekTok();
        switch (tok.type) {
            .eof, .dot, .semicolon, .comma, .blank_node_close => return null,
            .iri => {
                _ = try self.consumeTok();
                return Term{ .iri = try self.extractIRI(tok.value) };
            },
            .prefixed_name => {
                _ = try self.consumeTok();
                return Term{ .iri = try self.expandPrefixedName(tok.value) };
            },
            .blank_node => {
                _ = try self.consumeTok();
                return Term{ .blank_node = try self.allocator.dupe(u8, tok.value[2..]) };
            },
            .blank_node_open => return self.parseInlineBlankNode(),
            .literal => {
                _ = try self.consumeTok();
                const lit = try self.parseLiteralTerm(tok);
                return lit;
            },
            .keyword => {
                const kw = tok.value;
                if (std.mem.eql(u8, kw, "true") or std.mem.eql(u8, kw, "false")) {
                    _ = try self.consumeTok();
                    return Term{ .literal = Literal{
                        .value = try self.allocator.dupe(u8, kw),
                        .lang = null,
                        .datatype = try self.allocator.dupe(u8, "http://www.w3.org/2001/XMLSchema#boolean"),
                    } };
                }
                return null;
            },
            else => return null,
        }
    }

    fn parseLiteralTerm(self: *Parser, lit_tok: Token) ParseError!Term {
        const raw = lit_tok.value;
        // Bare numeric literal (no surrounding quotes)
        if (raw.len > 0 and raw[0] != '"') {
            const value = try self.allocator.dupe(u8, raw);
            errdefer self.allocator.free(value);
            // Infer XSD datatype from content
            const has_dot = std.mem.indexOfScalar(u8, raw, '.') != null;
            const has_exp = std.mem.indexOfAny(u8, raw, "eE") != null;
            const dt = if (has_dot or has_exp)
                "http://www.w3.org/2001/XMLSchema#double"
            else
                "http://www.w3.org/2001/XMLSchema#integer";
            return Term{ .literal = Literal{
                .value = value,
                .lang = null,
                .datatype = try self.allocator.dupe(u8, dt),
            } };
        }
        const content = extractLiteralContent(raw);
        const value = try self.allocator.dupe(u8, content);
        errdefer self.allocator.free(value);

        const pk = try self.peekTok();
        if (pk.type == .lang_tag) {
            _ = try self.consumeTok();
            const lang = try self.allocator.dupe(u8, pk.value[1..]);
            return Term{ .literal = Literal{ .value = value, .lang = lang, .datatype = null } };
        }
        if (pk.type == .datatype_marker) {
            _ = try self.consumeTok();
            const dt_tok = try self.consumeTok();
            const dt_iri = switch (dt_tok.type) {
                .iri => try self.extractIRI(dt_tok.value),
                .prefixed_name => try self.expandPrefixedName(dt_tok.value),
                else => return error.UnexpectedToken,
            };
            return Term{ .literal = Literal{ .value = value, .lang = null, .datatype = dt_iri } };
        }
        return Term{ .literal = Literal{ .value = value, .lang = null, .datatype = null } };
    }

    fn parseInlineBlankNode(self: *Parser) ParseError!?Term {
        _ = try self.expectTok(.blank_node_open);
        self.blank_counter += 1;
        const id_str = try std.fmt.allocPrint(self.allocator, "b{d}", .{self.blank_counter});
        const bn_term = Term{ .blank_node = id_str };

        // Check if empty brackets [ ]
        const pk = try self.peekTok();
        if (pk.type != .blank_node_close) {
            // Parse inner predicate-object list into queue
            const bn_copy = try self.cloneTerm(bn_term);
            defer bn_copy.deinit(self.allocator);
            try self.collectPredicateObjectList(bn_copy);
        }

        _ = try self.expectTok(.blank_node_close);
        return bn_term;
    }

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    fn extractIRI(self: *Parser, raw: []const u8) ![]const u8 {
        if (raw.len >= 2 and raw[0] == '<' and raw[raw.len - 1] == '>') {
            return self.allocator.dupe(u8, raw[1 .. raw.len - 1]);
        }
        return self.allocator.dupe(u8, raw);
    }

    fn expandPrefixedName(self: *Parser, raw: []const u8) ![]const u8 {
        const colon = std.mem.indexOfScalar(u8, raw, ':') orelse {
            const base_iri = self.prefix_map.get("") orelse "";
            return std.fmt.allocPrint(self.allocator, "{s}{s}", .{ base_iri, raw });
        };
        const prefix_label = raw[0..colon];
        const local = raw[colon + 1 ..];
        const base_iri = self.prefix_map.get(prefix_label) orelse {
            return self.allocator.dupe(u8, raw);
        };
        return std.fmt.allocPrint(self.allocator, "{s}{s}", .{ base_iri, local });
    }

    fn cloneTerm(self: *Parser, t: Term) !Term {
        return switch (t) {
            .iri => |s| Term{ .iri = try self.allocator.dupe(u8, s) },
            .blank_node => |s| Term{ .blank_node = try self.allocator.dupe(u8, s) },
            .literal => |l| Term{ .literal = Literal{
                .value = try self.allocator.dupe(u8, l.value),
                .lang = if (l.lang) |ln| try self.allocator.dupe(u8, ln) else null,
                .datatype = if (l.datatype) |d| try self.allocator.dupe(u8, d) else null,
            } },
        };
    }

    fn extractLiteralContent(raw: []const u8) []const u8 {
        if (raw.len >= 6 and
            std.mem.startsWith(u8, raw, "\"\"\"") and
            std.mem.endsWith(u8, raw, "\"\"\""))
        {
            return raw[3 .. raw.len - 3];
        }
        if (raw.len >= 2 and raw[0] == '"' and raw[raw.len - 1] == '"') {
            return raw[1 .. raw.len - 1];
        }
        return raw;
    }
};

// =============================================================================
// Tests — Milestone 1.2
// =============================================================================

const testing = std.testing;

test "parse simple triple" {
    const src = "<http://s> <http://p> <http://o> .";
    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();
    const t = (try p.next()).?;
    defer t.deinit(testing.allocator);
    try testing.expectEqualStrings("http://s", t.subject.iri);
    try testing.expectEqualStrings("http://p", t.predicate.iri);
    try testing.expectEqualStrings("http://o", t.object.iri);
    try testing.expect((try p.next()) == null);
}

test "parse predicate-object list" {
    const src = "<http://s> <http://p1> <http://o1> ; <http://p2> <http://o2> .";
    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();
    const t1 = (try p.next()).?;
    defer t1.deinit(testing.allocator);
    const t2 = (try p.next()).?;
    defer t2.deinit(testing.allocator);
    try testing.expectEqualStrings("http://p1", t1.predicate.iri);
    try testing.expectEqualStrings("http://p2", t2.predicate.iri);
    try testing.expect((try p.next()) == null);
}

test "parse object list" {
    const src = "<http://s> <http://p> <http://o1> , <http://o2> .";
    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();
    const t1 = (try p.next()).?;
    defer t1.deinit(testing.allocator);
    const t2 = (try p.next()).?;
    defer t2.deinit(testing.allocator);
    try testing.expectEqualStrings("http://o1", t1.object.iri);
    try testing.expectEqualStrings("http://o2", t2.object.iri);
}

test "parse a shorthand" {
    const src = "<http://s> a <http://Class> .";
    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();
    const t = (try p.next()).?;
    defer t.deinit(testing.allocator);
    try testing.expectEqualStrings(RDF_TYPE, t.predicate.iri);
}

test "parse prefix expansion" {
    const src =
        \\@prefix ex: <http://example.org/> .
        \\ex:foo a ex:Thing .
    ;
    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();
    const t = (try p.next()).?;
    defer t.deinit(testing.allocator);
    try testing.expectEqualStrings("http://example.org/foo", t.subject.iri);
    try testing.expectEqualStrings("http://example.org/Thing", t.object.iri);
}

test "parse blank node subject" {
    const src = "_:b1 <http://p> <http://o> .";
    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();
    const t = (try p.next()).?;
    defer t.deinit(testing.allocator);
    try testing.expectEqualStrings("b1", t.subject.blank_node);
}

test "parse literal object" {
    const src = "<http://s> <http://p> \"hello\" .";
    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();
    const t = (try p.next()).?;
    defer t.deinit(testing.allocator);
    try testing.expectEqualStrings("hello", t.object.literal.value);
}

test "parse inline blank node" {
    const src = "<http://s> <http://p> [ <http://p2> <http://o2> ] .";
    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();
    const t1 = (try p.next()).?;
    defer t1.deinit(testing.allocator);
    // subject of t1 is <s>, object is blank node
    try testing.expectEqual(TermType.blank_node, @as(TermType, t1.object));
    const t2 = (try p.next()).?;
    defer t2.deinit(testing.allocator);
    try testing.expectEqualStrings("http://p2", t2.predicate.iri);
}

test "parse multiple subjects" {
    const src =
        \\<http://a> <http://p> <http://x> .
        \\<http://b> <http://p> <http://y> .
    ;
    var p = try Parser.init(testing.allocator, src);
    defer p.deinit();
    const t1 = (try p.next()).?;
    defer t1.deinit(testing.allocator);
    const t2 = (try p.next()).?;
    defer t2.deinit(testing.allocator);
    try testing.expectEqualStrings("http://a", t1.subject.iri);
    try testing.expectEqualStrings("http://b", t2.subject.iri);
}

// =============================================================================
// Phase 2.5 — YAGO 4.5 Tiny Dataset Validation
// =============================================================================
// Reads the YAGO 4.5 tiny TTL file and parses the first 100 triples without
// error.  Skipped gracefully when the data file is absent.

const YAGO_TINY_PATH = "data/yago-4.5.0.2-tiny/yago-tiny.ttl";
const YAGO_VALIDATE_MAX: usize = 100;

test "YAGO 4.5 tiny: first 100 triples parse without errors" {
    const file = std.fs.cwd().openFile(YAGO_TINY_PATH, .{}) catch |err| {
        if (err == error.FileNotFound) return;
        return err;
    };
    defer file.close();

    // 32 KB is ample for 100 YAGO triples.
    const buf = try testing.allocator.alloc(u8, 32 * 1024);
    defer testing.allocator.free(buf);
    const source = buf[0..try file.read(buf)];

    var p = try Parser.init(testing.allocator, source);
    defer p.deinit();

    var count: usize = 0;
    while (count < YAGO_VALIDATE_MAX) {
        const triple = (try p.next()) orelse break;
        triple.deinit(testing.allocator);
        count += 1;
    }

    try testing.expectEqual(YAGO_VALIDATE_MAX, count);
}
