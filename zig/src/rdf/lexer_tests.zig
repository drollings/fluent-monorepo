//! Tests for lexer.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const lexer_mod = @import("lexer.zig");

test "lex IRI basic" {
    const src = "<http://example.org/foo>";
    var lex = lexer_mod.Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.iri, tok.type);
    try std.testing.expectEqualStrings("<http://example.org/foo>", tok.value);
    const eof = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.eof, eof.type);
}
test "lex prefixed name" {
    const src = "ex:foo";
    var lex = lexer_mod.Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.prefixed_name, tok.type);
    try std.testing.expectEqualStrings("ex:foo", tok.value);
}
test "lex literal basic" {
    const src = "\"hello world\"";
    var lex = lexer_mod.Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.literal, tok.type);
    try std.testing.expectEqualStrings("\"hello world\"", tok.value);
}
test "lex literal with language tag" {
    const src = "\"bonjour\"@fr";
    var lex = lexer_mod.Lexer.init(src);
    const lit = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.literal, lit.type);
    const lang = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.lang_tag, lang.type);
    try std.testing.expectEqualStrings("@fr", lang.value);
}
test "lex literal with datatype" {
    const src = "\"42\"^^xsd:integer";
    var lex = lexer_mod.Lexer.init(src);
    const lit = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.literal, lit.type);
    const marker = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.datatype_marker, marker.type);
    const dt = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.prefixed_name, dt.type);
    try std.testing.expectEqualStrings("xsd:integer", dt.value);
}
test "lex blank node" {
    const src = "_:node1";
    var lex = lexer_mod.Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.blank_node, tok.type);
    try std.testing.expectEqualStrings("_:node1", tok.value);
}
test "lex comment skipping" {
    const src = "# this is a comment\n<http://foo>";
    var lex = lexer_mod.Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.iri, tok.type);
}
test "lex at-prefix keyword" {
    const src = "@prefix ex: <http://example.org/> .";
    var lex = lexer_mod.Lexer.init(src);
    const kw = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.keyword, kw.type);
    try std.testing.expectEqualStrings("@prefix", kw.value);
}
test "lex keyword a" {
    const src = "<s> a <Class> .";
    var lex = lexer_mod.Lexer.init(src);
    _ = try lex.nextToken(); // <s>
    const a = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.keyword, a.type);
    try std.testing.expectEqualStrings("a", a.value);
}
test "lex punctuation" {
    const src = ". ; ,";
    var lex = lexer_mod.Lexer.init(src);
    const dot = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.dot, dot.type);
    const semi = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.semicolon, semi.type);
    const comma = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.comma, comma.type);
}
test "lex error unterminated IRI" {
    const src = "<http://example.org/foo";
    var lex = lexer_mod.Lexer.init(src);
    const result = lex.nextToken();
    try std.testing.expectError(error.UnterminatedIRI, result);
}
test "lex error unterminated literal" {
    const src = "\"unterminated";
    var lex = lexer_mod.Lexer.init(src);
    const result = lex.nextToken();
    try std.testing.expectError(error.UnterminatedLiteral, result);
}
test "lex literal with escape sequence" {
    const src = "\"line1\\nline2\"";
    var lex = lexer_mod.Lexer.init(src);
    const tok = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.literal, tok.type);
}
test "lex blank node brackets" {
    const src = "[ ]";
    var lex = lexer_mod.Lexer.init(src);
    const open = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.blank_node_open, open.type);
    const close = try lex.nextToken();
    try std.testing.expectEqual(lexer_mod.TokenType.blank_node_close, close.type);
}
