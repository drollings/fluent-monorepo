/// lexer.zig — Streaming Turtle (Terse RDF Triple Language) Lexer
///
/// Design:
///   - Zero-allocation: returns slices into the caller-owned source buffer.
///   - Tracks line/column for error diagnostics.
///   - Caller is responsible for source buffer lifetime.
///
/// Token types: IRI, PrefixedName, BlankNode, Literal, Punctuation, Keyword,
///              LangTag, DatatypeMarker (^^), EOF.
///
/// Usage:
///   var lex = Lexer.init(source);
///   while (true) {
///       const tok = try lex.nextToken();
///       if (tok.type == .eof) break;
///   }
const std = @import("std");

/// Defines a token type for Zig keywords, managing ownership and ensuring correct usage patterns.
pub const TokenType = enum {
    iri, // <http://example.org/foo>
    prefixed_name, // ex:foo or :bar
    blank_node, // _:node1
    blank_node_open, // [
    blank_node_close, // ]
    literal, // "text"
    lang_tag, // @en
    datatype_marker, // ^^
    keyword, // @prefix, @base, a
    dot, // .
    semicolon, // ;
    comma, // ,
    open_paren, // (
    close_paren, // )
    eof,
};

/// Represents a keyword in Zig's lexer, tracking ownership and invariants for parsing.
pub const Token = struct {
    type: TokenType,
    /// Slice into the original source buffer (not owned).
    value: []const u8,
    line: u32,
    col: u32,
};

pub const LexError = error{
    UnterminatedIRI,
    UnterminatedLiteral,
    InvalidEscape,
    UnexpectedChar,
    UnterminatedComment,
};

/// Represents a lexer component for Zig, managing token parsing and ownership; ensures correct structure and invariants.
pub const Lexer = struct {
    src: []const u8,
    pos: usize,
    line: u32,
    col: u32,

    pub fn init(source: []const u8) Lexer {
        return .{ .src = source, .pos = 0, .line = 1, .col = 1 };
    }

    /// Advance past whitespace and comments.
    fn skipWhitespaceAndComments(self: *Lexer) void {
        while (self.pos < self.src.len) {
            const c = self.src[self.pos];
            if (c == '#') {
                // Comment — skip to end of line
                while (self.pos < self.src.len and self.src[self.pos] != '\n') {
                    self.pos += 1;
                    self.col += 1;
                }
            } else if (c == '\n') {
                self.pos += 1;
                self.line += 1;
                self.col = 1;
            } else if (c == '\r') {
                self.pos += 1;
                // col stays (will be reset by \n)
            } else if (c == ' ' or c == '\t') {
                self.pos += 1;
                self.col += 1;
            } else {
                break;
            }
        }
    }

    fn advance(self: *Lexer) void {
        if (self.pos < self.src.len) {
            if (self.src[self.pos] == '\n') {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            self.pos += 1;
        }
    }

    pub fn nextToken(self: *Lexer) LexError!Token {
        self.skipWhitespaceAndComments();

        if (self.pos >= self.src.len) {
            return Token{ .type = .eof, .value = "", .line = self.line, .col = self.col };
        }

        const start_line = self.line;
        const start_col = self.col;
        const c = self.src[self.pos];

        // IRI: <...>
        if (c == '<') {
            return self.lexIRI(start_line, start_col);
        }

        // Literal: "..."  or """..."""
        if (c == '"') {
            return self.lexLiteral(start_line, start_col);
        }

        // Blank node: _:name or []
        if (c == '_' and self.pos + 1 < self.src.len and self.src[self.pos + 1] == ':') {
            return self.lexBlankNode(start_line, start_col);
        }

        // [ blank node open
        if (c == '[') {
            const start = self.pos;
            self.advance();
            return Token{ .type = .blank_node_open, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
        }
        // ] blank node close
        if (c == ']') {
            const start = self.pos;
            self.advance();
            return Token{ .type = .blank_node_close, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
        }

        // @keyword: @prefix, @base, or lang tag after literal
        if (c == '@') {
            return self.lexAtDirective(start_line, start_col);
        }

        // ^^ datatype marker
        if (c == '^' and self.pos + 1 < self.src.len and self.src[self.pos + 1] == '^') {
            const start = self.pos;
            self.advance();
            self.advance();
            return Token{ .type = .datatype_marker, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
        }

        // Punctuation
        if (c == '.') {
            const start = self.pos;
            self.advance();
            return Token{ .type = .dot, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
        }
        if (c == ';') {
            const start = self.pos;
            self.advance();
            return Token{ .type = .semicolon, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
        }
        if (c == ',') {
            const start = self.pos;
            self.advance();
            return Token{ .type = .comma, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
        }
        if (c == '(') {
            const start = self.pos;
            self.advance();
            return Token{ .type = .open_paren, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
        }
        if (c == ')') {
            const start = self.pos;
            self.advance();
            return Token{ .type = .close_paren, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
        }

        // Keyword 'a' (rdf:type shorthand) — must be standalone word
        if (c == 'a' and (self.pos + 1 >= self.src.len or isNameEndChar(self.src[self.pos + 1]))) {
            const start = self.pos;
            self.advance();
            return Token{ .type = .keyword, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
        }

        // Bare numeric literals: [+-]?[0-9]+([.][0-9]*)? or [+-]?[.][0-9]+
        // Emitted as .literal with the raw numeric text as value (no quotes).
        if (c == '+' or c == '-' or (c >= '0' and c <= '9') or
            (c == '.' and self.pos + 1 < self.src.len and
                self.src[self.pos + 1] >= '0' and self.src[self.pos + 1] <= '9'))
        {
            return self.lexNumericLiteral(start_line, start_col);
        }

        // Prefixed name or bare keyword (true, false, PREFIX, BASE in SPARQL-style)
        if (isPrefixStartChar(c)) {
            return self.lexPrefixedName(start_line, start_col);
        }

        return error.UnexpectedChar;
    }

    fn lexIRI(self: *Lexer, start_line: u32, start_col: u32) LexError!Token {
        const start = self.pos;
        self.advance(); // consume '<'
        while (self.pos < self.src.len) {
            const ch = self.src[self.pos];
            if (ch == '>') {
                self.advance(); // consume '>'
                return Token{ .type = .iri, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
            }
            if (ch == '\\') {
                // IRI escape: \uXXXX or \UXXXXXXXX
                self.advance();
                if (self.pos >= self.src.len) return error.InvalidEscape;
                const esc = self.src[self.pos];
                if (esc != 'u' and esc != 'U') return error.InvalidEscape;
                self.advance();
            } else if (ch == '\n' or ch == '\r') {
                return error.UnterminatedIRI;
            } else {
                self.advance();
            }
        }
        return error.UnterminatedIRI;
    }

    fn lexLiteral(self: *Lexer, start_line: u32, start_col: u32) LexError!Token {
        const start = self.pos;
        self.advance(); // consume first '"'

        // Check for triple-quoted
        const triple = self.pos + 1 < self.src.len and
            self.src[self.pos] == '"' and self.src[self.pos + 1] == '"';
        if (triple) {
            self.advance();
            self.advance();
            // Scan until closing """
            while (self.pos + 2 < self.src.len) {
                if (self.src[self.pos] == '"' and
                    self.src[self.pos + 1] == '"' and
                    self.src[self.pos + 2] == '"')
                {
                    self.advance();
                    self.advance();
                    self.advance();
                    return Token{ .type = .literal, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
                }
                if (self.src[self.pos] == '\\') {
                    self.advance();
                    if (self.pos >= self.src.len) return error.InvalidEscape;
                }
                self.advance();
            }
            return error.UnterminatedLiteral;
        }

        // Single-quoted literal
        while (self.pos < self.src.len) {
            const ch = self.src[self.pos];
            if (ch == '"') {
                self.advance();
                return Token{ .type = .literal, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
            }
            if (ch == '\\') {
                self.advance();
                if (self.pos >= self.src.len) return error.InvalidEscape;
                const esc = self.src[self.pos];
                // Valid escape chars: t n r " ' \ u U
                const valid = esc == 't' or esc == 'n' or esc == 'r' or
                    esc == '"' or esc == '\'' or esc == '\\' or
                    esc == 'u' or esc == 'U';
                if (!valid) return error.InvalidEscape;
                self.advance();
            } else if (ch == '\n' or ch == '\r') {
                return error.UnterminatedLiteral;
            } else {
                self.advance();
            }
        }
        return error.UnterminatedLiteral;
    }

    /// Lex a bare numeric literal: integer, decimal, or double.
    /// Emitted as .literal so the parser can handle them uniformly.
    fn lexNumericLiteral(self: *Lexer, start_line: u32, start_col: u32) LexError!Token {
        const start = self.pos;
        // Optional sign
        if (self.pos < self.src.len and (self.src[self.pos] == '+' or self.src[self.pos] == '-')) {
            self.advance();
        }
        // Integer digits before optional decimal point
        while (self.pos < self.src.len and self.src[self.pos] >= '0' and self.src[self.pos] <= '9') {
            self.advance();
        }
        // Optional fractional part
        if (self.pos < self.src.len and self.src[self.pos] == '.') {
            // Only consume '.' if followed by a digit (otherwise it is the statement terminator)
            if (self.pos + 1 < self.src.len and
                self.src[self.pos + 1] >= '0' and self.src[self.pos + 1] <= '9')
            {
                self.advance(); // '.'
                while (self.pos < self.src.len and self.src[self.pos] >= '0' and self.src[self.pos] <= '9') {
                    self.advance();
                }
            }
        }
        // Optional exponent (e.g. 1e10, 1.5E-3)
        if (self.pos < self.src.len and (self.src[self.pos] == 'e' or self.src[self.pos] == 'E')) {
            self.advance();
            if (self.pos < self.src.len and (self.src[self.pos] == '+' or self.src[self.pos] == '-')) {
                self.advance();
            }
            while (self.pos < self.src.len and self.src[self.pos] >= '0' and self.src[self.pos] <= '9') {
                self.advance();
            }
        }
        return Token{ .type = .literal, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
    }

    fn lexBlankNode(self: *Lexer, start_line: u32, start_col: u32) LexError!Token {
        const start = self.pos;
        self.advance(); // '_'
        self.advance(); // ':'
        while (self.pos < self.src.len and isNameChar(self.src[self.pos])) {
            self.advance();
        }
        return Token{ .type = .blank_node, .value = self.src[start..self.pos], .line = start_line, .col = start_col };
    }

    fn lexAtDirective(self: *Lexer, start_line: u32, start_col: u32) LexError!Token {
        const start = self.pos;
        self.advance(); // consume '@'
        while (self.pos < self.src.len and isLangChar(self.src[self.pos])) {
            self.advance();
        }
        const word = self.src[start..self.pos];
        // @prefix and @base are keywords; others are language tags
        const is_keyword = std.mem.eql(u8, word, "@prefix") or
            std.mem.eql(u8, word, "@base") or
            std.mem.eql(u8, word, "@PREFIX") or
            std.mem.eql(u8, word, "@BASE");
        return Token{
            .type = if (is_keyword) .keyword else .lang_tag,
            .value = word,
            .line = start_line,
            .col = start_col,
        };
    }

    fn lexPrefixedName(self: *Lexer, start_line: u32, start_col: u32) LexError!Token {
        const start = self.pos;
        // Consume prefix part (up to and including ':')
        while (self.pos < self.src.len and isPrefixChar(self.src[self.pos])) {
            self.advance();
        }
        // Consume ':' if present (making it a prefixed name, not a keyword)
        if (self.pos < self.src.len and self.src[self.pos] == ':') {
            self.advance();
            // Consume local part
            while (self.pos < self.src.len and isLocalNameChar(self.src[self.pos])) {
                self.advance();
            }
        }
        const word = self.src[start..self.pos];
        // Check for sparql-style PREFIX / BASE keywords
        const is_keyword = std.mem.eql(u8, word, "PREFIX") or
            std.mem.eql(u8, word, "BASE") or
            std.mem.eql(u8, word, "true") or
            std.mem.eql(u8, word, "false");
        return Token{
            .type = if (is_keyword) .keyword else .prefixed_name,
            .value = word,
            .line = start_line,
            .col = start_col,
        };
    }

    // -------------------------------------------------------------------------
    // Character class helpers
    // -------------------------------------------------------------------------

    fn isNameEndChar(ch: u8) bool {
        // Characters that cannot follow a bare name token
        return ch == ' ' or ch == '\t' or ch == '\n' or ch == '\r' or
            ch == '.' or ch == ';' or ch == ',' or ch == ')' or ch == ']';
    }

    fn isPrefixStartChar(ch: u8) bool {
        return (ch >= 'A' and ch <= 'Z') or (ch >= 'a' and ch <= 'z') or ch > 127;
    }

    fn isPrefixChar(ch: u8) bool {
        return isPrefixStartChar(ch) or (ch >= '0' and ch <= '9') or ch == '-' or ch == '_';
    }

    fn isLocalNameChar(ch: u8) bool {
        // Turtle PN_LOCAL: letters, digits, '_', '-', '.', ':', '%' (percent-enc).
        // Explicitly excludes Turtle punctuation: ',', ';', '.', '(', ')', '[', ']'.
        // '.' is allowed mid-name but handled via a two-pass rule in lexPrefixedName.
        return isPrefixChar(ch) or ch == '.' or ch == '%' or ch == ':';
    }

    fn isNameChar(ch: u8) bool {
        return (ch >= 'a' and ch <= 'z') or (ch >= 'A' and ch <= 'Z') or
            (ch >= '0' and ch <= '9') or ch == '_' or ch == '-' or ch == '.';
    }

    fn isLangChar(ch: u8) bool {
        return (ch >= 'a' and ch <= 'z') or (ch >= 'A' and ch <= 'Z') or ch == '-';
    }
};

// =============================================================================
// Tests — Milestone 1.1
// =============================================================================

test "lex IRI basic" {
    const src = "<http://example.org/foo>";
    var lex = Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(TokenType.iri, tok.type);
    try std.testing.expectEqualStrings("<http://example.org/foo>", tok.value);
    const eof = try lex.nextToken();
    try std.testing.expectEqual(TokenType.eof, eof.type);
}

test "lex prefixed name" {
    const src = "ex:foo";
    var lex = Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(TokenType.prefixed_name, tok.type);
    try std.testing.expectEqualStrings("ex:foo", tok.value);
}

test "lex literal basic" {
    const src = "\"hello world\"";
    var lex = Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(TokenType.literal, tok.type);
    try std.testing.expectEqualStrings("\"hello world\"", tok.value);
}

test "lex literal with language tag" {
    const src = "\"bonjour\"@fr";
    var lex = Lexer.init(src);
    const lit = try lex.nextToken();
    try std.testing.expectEqual(TokenType.literal, lit.type);
    const lang = try lex.nextToken();
    try std.testing.expectEqual(TokenType.lang_tag, lang.type);
    try std.testing.expectEqualStrings("@fr", lang.value);
}

test "lex literal with datatype" {
    const src = "\"42\"^^xsd:integer";
    var lex = Lexer.init(src);
    const lit = try lex.nextToken();
    try std.testing.expectEqual(TokenType.literal, lit.type);
    const marker = try lex.nextToken();
    try std.testing.expectEqual(TokenType.datatype_marker, marker.type);
    const dt = try lex.nextToken();
    try std.testing.expectEqual(TokenType.prefixed_name, dt.type);
    try std.testing.expectEqualStrings("xsd:integer", dt.value);
}

test "lex blank node" {
    const src = "_:node1";
    var lex = Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(TokenType.blank_node, tok.type);
    try std.testing.expectEqualStrings("_:node1", tok.value);
}

test "lex comment skipping" {
    const src = "# this is a comment\n<http://foo>";
    var lex = Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(TokenType.iri, tok.type);
}

test "lex at-prefix keyword" {
    const src = "@prefix ex: <http://example.org/> .";
    var lex = Lexer.init(src);
    const kw = try lex.nextToken();
    try std.testing.expectEqual(TokenType.keyword, kw.type);
    try std.testing.expectEqualStrings("@prefix", kw.value);
}

test "lex keyword a" {
    const src = "<s> a <Class> .";
    var lex = Lexer.init(src);
    _ = try lex.nextToken(); // <s>
    const a = try lex.nextToken();
    try std.testing.expectEqual(TokenType.keyword, a.type);
    try std.testing.expectEqualStrings("a", a.value);
}

test "lex punctuation" {
    const src = ". ; ,";
    var lex = Lexer.init(src);
    const dot = try lex.nextToken();
    try std.testing.expectEqual(TokenType.dot, dot.type);
    const semi = try lex.nextToken();
    try std.testing.expectEqual(TokenType.semicolon, semi.type);
    const comma = try lex.nextToken();
    try std.testing.expectEqual(TokenType.comma, comma.type);
}

test "lex error unterminated IRI" {
    const src = "<http://example.org/foo";
    var lex = Lexer.init(src);
    const result = lex.nextToken();
    try std.testing.expectError(error.UnterminatedIRI, result);
}

test "lex error unterminated literal" {
    const src = "\"unterminated";
    var lex = Lexer.init(src);
    const result = lex.nextToken();
    try std.testing.expectError(error.UnterminatedLiteral, result);
}

test "lex literal with escape sequence" {
    const src = "\"line1\\nline2\"";
    var lex = Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(TokenType.literal, tok.type);
}

test "lex blank node brackets" {
    const src = "[ ]";
    var lex = Lexer.init(src);
    const open = try lex.nextToken();
    try std.testing.expectEqual(TokenType.blank_node_open, open.type);
    const close = try lex.nextToken();
    try std.testing.expectEqual(TokenType.blank_node_close, close.type);
}



