use crate::RdfError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Iri,
    PrefixedName,
    BlankNode,
    BlankNodeOpen,
    BlankNodeClose,
    Literal,
    LangTag,
    DatatypeMarker,
    Keyword,
    Dot,
    Semicolon,
    Comma,
    OpenParen,
    CloseParen,
    Eof,
}

#[derive(Debug, Clone, Copy)]
pub struct Token<'a> {
    pub kind: TokenKind,
    pub value: &'a str,
    pub line: u32,
    pub col: u32,
}

pub struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    line: u32,
    col: u32,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn skip_whitespace_and_comments(&mut self) {
        while self.pos < self.src.len() {
            let c = self.src.as_bytes()[self.pos];
            if c == b'#' {
                while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b'\n' {
                    self.pos += 1;
                    self.col += 1;
                }
            } else if c == b'\n' {
                self.pos += 1;
                self.line += 1;
                self.col = 1;
            } else if c == b'\r' {
                self.pos += 1;
            } else if c == b' ' || c == b'\t' {
                self.pos += 1;
                self.col += 1;
            } else {
                break;
            }
        }
    }

    fn advance(&mut self) {
        if self.pos < self.src.len() {
            if self.src.as_bytes()[self.pos] == b'\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    pub fn next_token(&mut self) -> Result<Token<'a>, RdfError> {
        self.skip_whitespace_and_comments();

        if self.pos >= self.src.len() {
            return Ok(Token {
                kind: TokenKind::Eof,
                value: "",
                line: self.line,
                col: self.col,
            });
        }

        let start_line = self.line;
        let start_col = self.col;
        let c = self.peek().unwrap();

        match c {
            b'<' => self.lex_iri(start_line, start_col),
            b'"' => self.lex_literal(start_line, start_col),
            b'_' if self.src.as_bytes().get(self.pos + 1) == Some(&b':') => {
                self.lex_blank_node(start_line, start_col)
            }
            b'[' => {
                let start = self.pos;
                self.advance();
                Ok(Token {
                    kind: TokenKind::BlankNodeOpen,
                    value: &self.src[start..self.pos],
                    line: start_line,
                    col: start_col,
                })
            }
            b']' => {
                let start = self.pos;
                self.advance();
                Ok(Token {
                    kind: TokenKind::BlankNodeClose,
                    value: &self.src[start..self.pos],
                    line: start_line,
                    col: start_col,
                })
            }
            b'@' => self.lex_at_directive(start_line, start_col),
            b':' => self.lex_prefixed_name(start_line, start_col),
            b'^' if self.src.as_bytes().get(self.pos + 1) == Some(&b'^') => {
                let start = self.pos;
                self.advance();
                self.advance();
                Ok(Token {
                    kind: TokenKind::DatatypeMarker,
                    value: &self.src[start..self.pos],
                    line: start_line,
                    col: start_col,
                })
            }
            b'.' if self.pos + 1 < self.src.len()
                && self.src.as_bytes()[self.pos + 1].is_ascii_digit() =>
            {
                self.lex_numeric_literal(start_line, start_col)
            }
            b'.' => {
                let start = self.pos;
                self.advance();
                Ok(Token {
                    kind: TokenKind::Dot,
                    value: &self.src[start..self.pos],
                    line: start_line,
                    col: start_col,
                })
            }
            b';' => {
                let start = self.pos;
                self.advance();
                Ok(Token {
                    kind: TokenKind::Semicolon,
                    value: &self.src[start..self.pos],
                    line: start_line,
                    col: start_col,
                })
            }
            b',' => {
                let start = self.pos;
                self.advance();
                Ok(Token {
                    kind: TokenKind::Comma,
                    value: &self.src[start..self.pos],
                    line: start_line,
                    col: start_col,
                })
            }
            b'(' => {
                let start = self.pos;
                self.advance();
                Ok(Token {
                    kind: TokenKind::OpenParen,
                    value: &self.src[start..self.pos],
                    line: start_line,
                    col: start_col,
                })
            }
            b')' => {
                let start = self.pos;
                self.advance();
                Ok(Token {
                    kind: TokenKind::CloseParen,
                    value: &self.src[start..self.pos],
                    line: start_line,
                    col: start_col,
                })
            }
            b'a' if self.pos + 1 >= self.src.len()
                || is_name_end_char(self.src.as_bytes()[self.pos + 1]) =>
            {
                let start = self.pos;
                self.advance();
                Ok(Token {
                    kind: TokenKind::Keyword,
                    value: &self.src[start..self.pos],
                    line: start_line,
                    col: start_col,
                })
            }
            b'+' | b'-' | b'0'..=b'9' => self.lex_numeric_literal(start_line, start_col),
            _ if is_prefix_start_char(c) => self.lex_prefixed_name(start_line, start_col),
            _ => Err(RdfError::UnexpectedChar {
                line: start_line,
                col: start_col,
            }),
        }
    }

    fn lex_iri(&mut self, start_line: u32, start_col: u32) -> Result<Token<'a>, RdfError> {
        let start = self.pos;
        self.advance();
        while self.pos < self.src.len() {
            let ch = self.src.as_bytes()[self.pos];
            if ch == b'>' {
                self.advance();
                return Ok(Token {
                    kind: TokenKind::Iri,
                    value: &self.src[start..self.pos],
                    line: start_line,
                    col: start_col,
                });
            }
            if ch == b'\\' {
                self.advance();
                if self.pos >= self.src.len() {
                    return Err(RdfError::InvalidEscape);
                }
                let esc = self.src.as_bytes()[self.pos];
                if esc != b'u' && esc != b'U' {
                    return Err(RdfError::InvalidEscape);
                }
                self.advance();
            } else if ch == b'\n' || ch == b'\r' {
                return Err(RdfError::UnterminatedIRI);
            } else {
                self.advance();
            }
        }
        Err(RdfError::UnterminatedIRI)
    }

    fn lex_literal(&mut self, start_line: u32, start_col: u32) -> Result<Token<'a>, RdfError> {
        let start = self.pos;
        self.advance();

        let triple = self.pos + 1 < self.src.len()
            && self.src.as_bytes()[self.pos] == b'"'
            && self.src.as_bytes()[self.pos + 1] == b'"';

        if triple {
            self.advance();
            self.advance();
            while self.pos + 2 < self.src.len() {
                if self.src.as_bytes()[self.pos] == b'"'
                    && self.src.as_bytes()[self.pos + 1] == b'"'
                    && self.src.as_bytes()[self.pos + 2] == b'"'
                {
                    self.advance();
                    self.advance();
                    self.advance();
                    return Ok(Token {
                        kind: TokenKind::Literal,
                        value: &self.src[start..self.pos],
                        line: start_line,
                        col: start_col,
                    });
                }
                if self.src.as_bytes()[self.pos] == b'\\' {
                    self.advance();
                    if self.pos >= self.src.len() {
                        return Err(RdfError::InvalidEscape);
                    }
                }
                self.advance();
            }
            return Err(RdfError::UnterminatedLiteral);
        }

        while self.pos < self.src.len() {
            let ch = self.src.as_bytes()[self.pos];
            if ch == b'"' {
                self.advance();
                return Ok(Token {
                    kind: TokenKind::Literal,
                    value: &self.src[start..self.pos],
                    line: start_line,
                    col: start_col,
                });
            }
            if ch == b'\\' {
                self.advance();
                if self.pos >= self.src.len() {
                    return Err(RdfError::InvalidEscape);
                }
                let esc = self.src.as_bytes()[self.pos];
                let valid = matches!(esc, b't' | b'n' | b'r' | b'"' | b'\'' | b'\\' | b'u' | b'U');
                if !valid {
                    return Err(RdfError::InvalidEscape);
                }
                self.advance();
            } else if ch == b'\n' || ch == b'\r' {
                return Err(RdfError::UnterminatedLiteral);
            } else {
                self.advance();
            }
        }
        Err(RdfError::UnterminatedLiteral)
    }

    fn lex_numeric_literal(
        &mut self,
        start_line: u32,
        start_col: u32,
    ) -> Result<Token<'a>, RdfError> {
        let start = self.pos;
        if let Some(c) = self.peek() {
            if c == b'+' || c == b'-' {
                self.advance();
            }
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.advance();
            } else {
                break;
            }
        }
        if let Some(c) = self.peek() {
            if c == b'.'
                && self.pos + 1 < self.src.len()
                && self.src.as_bytes()[self.pos + 1].is_ascii_digit()
            {
                self.advance();
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
        }
        if let Some(c) = self.peek() {
            if c == b'e' || c == b'E' {
                self.advance();
                if let Some(c) = self.peek() {
                    if c == b'+' || c == b'-' {
                        self.advance();
                    }
                }
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(Token {
            kind: TokenKind::Literal,
            value: &self.src[start..self.pos],
            line: start_line,
            col: start_col,
        })
    }

    fn lex_blank_node(&mut self, start_line: u32, start_col: u32) -> Result<Token<'a>, RdfError> {
        let start = self.pos;
        self.advance();
        self.advance();
        while self.pos < self.src.len() && is_name_char(self.src.as_bytes()[self.pos]) {
            self.advance();
        }
        Ok(Token {
            kind: TokenKind::BlankNode,
            value: &self.src[start..self.pos],
            line: start_line,
            col: start_col,
        })
    }

    fn lex_at_directive(&mut self, start_line: u32, start_col: u32) -> Result<Token<'a>, RdfError> {
        let start = self.pos;
        self.advance();
        while self.pos < self.src.len() && is_lang_char(self.src.as_bytes()[self.pos]) {
            self.advance();
        }
        let word = &self.src[start..self.pos];
        let is_keyword = matches!(word, "@prefix" | "@base" | "@PREFIX" | "@BASE");
        Ok(Token {
            kind: if is_keyword {
                TokenKind::Keyword
            } else {
                TokenKind::LangTag
            },
            value: word,
            line: start_line,
            col: start_col,
        })
    }

    fn lex_prefixed_name(
        &mut self,
        start_line: u32,
        start_col: u32,
    ) -> Result<Token<'a>, RdfError> {
        let start = self.pos;
        while self.pos < self.src.len() && is_prefix_char(self.src.as_bytes()[self.pos]) {
            self.advance();
        }
        if self.pos < self.src.len() && self.src.as_bytes()[self.pos] == b':' {
            self.advance();
            while self.pos < self.src.len() && is_local_name_char(self.src.as_bytes()[self.pos]) {
                self.advance();
            }
        }
        let word = &self.src[start..self.pos];
        let is_keyword = matches!(word, "PREFIX" | "BASE" | "true" | "false");
        Ok(Token {
            kind: if is_keyword {
                TokenKind::Keyword
            } else {
                TokenKind::PrefixedName
            },
            value: word,
            line: start_line,
            col: start_col,
        })
    }
}

fn is_name_end_char(ch: u8) -> bool {
    matches!(
        ch,
        b' ' | b'\t' | b'\n' | b'\r' | b'.' | b';' | b',' | b')' | b']'
    )
}

fn is_prefix_start_char(ch: u8) -> bool {
    ch.is_ascii_alphabetic() || ch > 127
}

fn is_prefix_char(ch: u8) -> bool {
    is_prefix_start_char(ch) || ch.is_ascii_digit() || ch == b'-' || ch == b'_'
}

fn is_local_name_char(ch: u8) -> bool {
    is_prefix_char(ch) || ch == b'.' || ch == b'%' || ch == b':'
}

fn is_name_char(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-' || ch == b'.'
}

fn is_lang_char(ch: u8) -> bool {
    ch.is_ascii_alphabetic() || ch == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lex_iri_basic() {
        let src = "<http://example.org/foo>";
        let mut lex = Lexer::new(src);
        let tok = lex.next_token().unwrap();
        assert_eq!(tok.kind, TokenKind::Iri);
        assert_eq!(tok.value, "<http://example.org/foo>");
        let eof = lex.next_token().unwrap();
        assert_eq!(eof.kind, TokenKind::Eof);
    }

    #[test]
    fn test_lex_prefixed_name() {
        let src = "ex:foo";
        let mut lex = Lexer::new(src);
        let tok = lex.next_token().unwrap();
        assert_eq!(tok.kind, TokenKind::PrefixedName);
        assert_eq!(tok.value, "ex:foo");
    }

    #[test]
    fn test_lex_literal_basic() {
        let src = "\"hello world\"";
        let mut lex = Lexer::new(src);
        let tok = lex.next_token().unwrap();
        assert_eq!(tok.kind, TokenKind::Literal);
        assert_eq!(tok.value, "\"hello world\"");
    }

    #[test]
    fn test_lex_literal_with_lang_tag() {
        let src = "\"bonjour\"@fr";
        let mut lex = Lexer::new(src);
        let lit = lex.next_token().unwrap();
        assert_eq!(lit.kind, TokenKind::Literal);
        let lang = lex.next_token().unwrap();
        assert_eq!(lang.kind, TokenKind::LangTag);
        assert_eq!(lang.value, "@fr");
    }

    #[test]
    fn test_lex_literal_with_datatype() {
        let src = "\"42\"^^xsd:integer";
        let mut lex = Lexer::new(src);
        let lit = lex.next_token().unwrap();
        assert_eq!(lit.kind, TokenKind::Literal);
        let marker = lex.next_token().unwrap();
        assert_eq!(marker.kind, TokenKind::DatatypeMarker);
        let dt = lex.next_token().unwrap();
        assert_eq!(dt.kind, TokenKind::PrefixedName);
        assert_eq!(dt.value, "xsd:integer");
    }

    #[test]
    fn test_lex_blank_node() {
        let src = "_:node1";
        let mut lex = Lexer::new(src);
        let tok = lex.next_token().unwrap();
        assert_eq!(tok.kind, TokenKind::BlankNode);
        assert_eq!(tok.value, "_:node1");
    }

    #[test]
    fn test_lex_comment_skipping() {
        let src = "# this is a comment\n<http://foo>";
        let mut lex = Lexer::new(src);
        let tok = lex.next_token().unwrap();
        assert_eq!(tok.kind, TokenKind::Iri);
    }

    #[test]
    fn test_lex_at_prefix_keyword() {
        let src = "@prefix ex: <http://example.org/> .";
        let mut lex = Lexer::new(src);
        let kw = lex.next_token().unwrap();
        assert_eq!(kw.kind, TokenKind::Keyword);
        assert_eq!(kw.value, "@prefix");
    }

    #[test]
    fn test_lex_keyword_a() {
        let src = "<s> a <Class> .";
        let mut lex = Lexer::new(src);
        let _ = lex.next_token().unwrap();
        let a = lex.next_token().unwrap();
        assert_eq!(a.kind, TokenKind::Keyword);
        assert_eq!(a.value, "a");
    }

    #[test]
    fn test_lex_punctuation() {
        let src = ". ; ,";
        let mut lex = Lexer::new(src);
        let dot = lex.next_token().unwrap();
        assert_eq!(dot.kind, TokenKind::Dot);
        let semi = lex.next_token().unwrap();
        assert_eq!(semi.kind, TokenKind::Semicolon);
        let comma = lex.next_token().unwrap();
        assert_eq!(comma.kind, TokenKind::Comma);
    }

    #[test]
    fn test_lex_unterminated_iri() {
        let src = "<http://example.org/foo";
        let mut lex = Lexer::new(src);
        let result = lex.next_token();
        assert!(result.is_err());
        matches!(result.unwrap_err(), RdfError::UnterminatedIRI);
    }

    #[test]
    fn test_lex_unterminated_literal() {
        let src = "\"unterminated";
        let mut lex = Lexer::new(src);
        let result = lex.next_token();
        assert!(result.is_err());
        matches!(result.unwrap_err(), RdfError::UnterminatedLiteral);
    }

    #[test]
    fn test_lex_literal_with_escape() {
        let src = "\"line1\\nline2\"";
        let mut lex = Lexer::new(src);
        let tok = lex.next_token().unwrap();
        assert_eq!(tok.kind, TokenKind::Literal);
    }

    #[test]
    fn test_lex_blank_node_brackets() {
        let src = "[ ]";
        let mut lex = Lexer::new(src);
        let open = lex.next_token().unwrap();
        assert_eq!(open.kind, TokenKind::BlankNodeOpen);
        let close = lex.next_token().unwrap();
        assert_eq!(close.kind, TokenKind::BlankNodeClose);
    }

    #[test]
    fn test_lex_multiple_tokens() {
        let src = "<http://s> <http://p> <http://o> .";
        let mut lex = Lexer::new(src);
        assert_eq!(lex.next_token().unwrap().kind, TokenKind::Iri);
        assert_eq!(lex.next_token().unwrap().kind, TokenKind::Iri);
        assert_eq!(lex.next_token().unwrap().kind, TokenKind::Iri);
        assert_eq!(lex.next_token().unwrap().kind, TokenKind::Dot);
        assert_eq!(lex.next_token().unwrap().kind, TokenKind::Eof);
    }

    #[test]
    fn test_lex_numeric_integer() {
        let src = "42";
        let mut lex = Lexer::new(src);
        let tok = lex.next_token().unwrap();
        assert_eq!(tok.kind, TokenKind::Literal);
        assert_eq!(tok.value, "42");
    }

    #[test]
    fn test_lex_numeric_decimal() {
        let src = "3.14";
        let mut lex = Lexer::new(src);
        let tok = lex.next_token().unwrap();
        assert_eq!(tok.kind, TokenKind::Literal);
        assert_eq!(tok.value, "3.14");
    }

    #[test]
    fn test_lex_numeric_negative() {
        let src = "-5";
        let mut lex = Lexer::new(src);
        let tok = lex.next_token().unwrap();
        assert_eq!(tok.kind, TokenKind::Literal);
        assert_eq!(tok.value, "-5");
    }

    #[test]
    fn test_lex_triple_quoted_literal() {
        let src = "\"\"\"hello\nworld\"\"\"";
        let mut lex = Lexer::new(src);
        let tok = lex.next_token().unwrap();
        assert_eq!(tok.kind, TokenKind::Literal);
        assert!(tok.value.starts_with("\"\"\""));
    }

    #[test]
    fn test_lex_keyword_true_false() {
        let src = "true false";
        let mut lex = Lexer::new(src);
        let t = lex.next_token().unwrap();
        assert_eq!(t.kind, TokenKind::Keyword);
        assert_eq!(t.value, "true");
        let f = lex.next_token().unwrap();
        assert_eq!(f.kind, TokenKind::Keyword);
        assert_eq!(f.value, "false");
    }

    #[test]
    fn test_lex_line_tracking() {
        let src = "\n<http://foo>";
        let mut lex = Lexer::new(src);
        let tok = lex.next_token().unwrap();
        assert_eq!(tok.line, 2);
        assert_eq!(tok.col, 1);
    }
}
