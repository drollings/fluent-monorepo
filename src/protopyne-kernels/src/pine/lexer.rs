//! Pine lexer: one scanner for the egui-overlap token set plus v6 needs.
//!
//! [`lex`] tokenizes a full source file into [`PineToken`]s. The overlap
//! with the reference lexer (`Lexer`/`Token`/`TokenKind` in the egui-charts
//! `scripting` module, reference only — never a dependency) is behaviorally
//! identical: same token kinds, same greedy rules (dotted names lex whole,
//! `.5` leads a number, unknown characters skip silently), same 1-based
//! line/column tracking. The v6 additions are strictly additive: the
//! `:=` operator, `import`/`export`/`enum`/`type`/`method`/`const`/`simple`/
//! `switch` keywords. Anything else new stays an [`Identifier`](TokenKind::Identifier)
//! until the parser stage owns it (`as` is a contextual import marker, not
//! a keyword).
//!
//! Two egui behaviors are mirrored deliberately, not fixed here: a numeric
//! literal that fails to parse yields `0.0`, and unknown characters are
//! skipped without error. Literal validation and precise syntax errors
//! belong to the later parser stage, which sees the full token stream.

use std::fmt;

/// One lexical kind. The egui-overlap subset carries egui's names and
/// shapes; the v6 subset (`ColonEqual`, `Import`, `Export`, `Enum`, `Type`,
/// `Method`, `Const`, `Simple`, `Switch`) is additive.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// Numeric literal (scientific notation included).
    Number(f64),
    /// Quoted literal (escapes resolved).
    String(String),
    /// Boolean literal. Reserved for later stages: this scanner emits the
    /// `true`/`false` keywords (egui parity) and never this variant.
    Boolean(bool),
    /// Names and dotted names (`length`, `ta.sma`, `UTIL.Smoothings.WMA`).
    Identifier(String),
    /// `if`.
    If,
    /// `else`.
    Else,
    /// `for`.
    For,
    /// `while`.
    While,
    /// `true`.
    True,
    /// `false`.
    False,
    /// `var`.
    Var,
    /// `varip`.
    Varip,
    /// `open`.
    Open,
    /// `high`.
    High,
    /// `low`.
    Low,
    /// `close`.
    Close,
    /// `volume`.
    Volume,
    /// `time`.
    Time,
    /// `import`.
    Import,
    /// `export`.
    Export,
    /// `enum`.
    Enum,
    /// `type`.
    Type,
    /// `method`.
    Method,
    /// `const`.
    Const,
    /// `simple`.
    Simple,
    /// `switch`.
    Switch,
    /// `break` (loop exit; a statement, never a value).
    Break,
    /// `+`.
    Plus,
    /// `-`.
    Minus,
    /// `*`.
    Star,
    /// `/`.
    Slash,
    /// `%`.
    Percent,
    /// `=`.
    Equal,
    /// `:=` (v6 reassignment).
    ColonEqual,
    /// `+=` (augmented assignment, desugared at parse).
    PlusEqual,
    /// `-=` (augmented assignment, desugared at parse).
    MinusEqual,
    /// `*=` (augmented assignment, desugared at parse).
    StarEqual,
    /// `/=` (augmented assignment, desugared at parse).
    SlashEqual,
    /// `%=` (augmented assignment, desugared at parse).
    PercentEqual,
    /// `==`.
    EqualEqual,
    /// `!=`.
    NotEqual,
    /// `<`.
    Less,
    /// `<=`.
    LessEqual,
    /// `>`.
    Greater,
    /// `>=`.
    GreaterEqual,
    /// `and`.
    And,
    /// `or`.
    Or,
    /// `not`.
    Not,
    /// `?`.
    Question,
    /// `:`.
    Colon,
    /// `(`.
    LeftParen,
    /// `)`.
    RightParen,
    /// `[`.
    LeftBracket,
    /// `]`.
    RightBracket,
    /// `,`.
    Comma,
    /// `.`.
    Dot,
    /// `=>`.
    Arrow,
    /// Line break (significant in Pine).
    Newline,
    /// End of input (always the final token).
    Eof,
    /// `//` comment to end of line, trimmed.
    Comment(String),
    /// `#rrggbb` / `#rrggbbaa` color literal (digits only, `#` excluded).
    ColorHex(String),
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "Number({n:?})"),
            Self::String(s) => write!(f, "String({s:?})"),
            Self::Boolean(b) => write!(f, "Boolean({b})"),
            Self::Identifier(id) => write!(f, "Identifier({id})"),
            Self::Comment(c) => write!(f, "Comment({c})"),
            Self::ColorHex(digits) => write!(f, "#{digits}"),
            Self::If => write!(f, "if"),
            Self::Else => write!(f, "else"),
            Self::For => write!(f, "for"),
            Self::While => write!(f, "while"),
            Self::True => write!(f, "true"),
            Self::False => write!(f, "false"),
            Self::Var => write!(f, "var"),
            Self::Varip => write!(f, "varip"),
            Self::Open => write!(f, "open"),
            Self::High => write!(f, "high"),
            Self::Low => write!(f, "low"),
            Self::Close => write!(f, "close"),
            Self::Volume => write!(f, "volume"),
            Self::Time => write!(f, "time"),
            Self::Import => write!(f, "import"),
            Self::Export => write!(f, "export"),
            Self::Enum => write!(f, "enum"),
            Self::Type => write!(f, "type"),
            Self::Method => write!(f, "method"),
            Self::Const => write!(f, "const"),
            Self::Simple => write!(f, "simple"),
            Self::Switch => write!(f, "switch"),
            Self::Break => write!(f, "break"),
            Self::Plus => write!(f, "+"),
            Self::Minus => write!(f, "-"),
            Self::Star => write!(f, "*"),
            Self::Slash => write!(f, "/"),
            Self::Percent => write!(f, "%"),
            Self::Equal => write!(f, "="),
            Self::ColonEqual => write!(f, ":="),
            Self::PlusEqual => write!(f, "+="),
            Self::MinusEqual => write!(f, "-="),
            Self::StarEqual => write!(f, "*="),
            Self::SlashEqual => write!(f, "/="),
            Self::PercentEqual => write!(f, "%="),
            Self::EqualEqual => write!(f, "=="),
            Self::NotEqual => write!(f, "!="),
            Self::Less => write!(f, "<"),
            Self::LessEqual => write!(f, "<="),
            Self::Greater => write!(f, ">"),
            Self::GreaterEqual => write!(f, ">="),
            Self::And => write!(f, "and"),
            Self::Or => write!(f, "or"),
            Self::Not => write!(f, "not"),
            Self::Question => write!(f, "?"),
            Self::Colon => write!(f, ":"),
            Self::LeftParen => write!(f, "("),
            Self::RightParen => write!(f, ")"),
            Self::LeftBracket => write!(f, "["),
            Self::RightBracket => write!(f, "]"),
            Self::Comma => write!(f, ","),
            Self::Dot => write!(f, "."),
            Self::Arrow => write!(f, "=>"),
            Self::Newline => write!(f, "newline"),
            Self::Eof => write!(f, "eof"),
        }
    }
}

/// One token with its 1-based source position.
#[derive(Debug, Clone, PartialEq)]
pub struct PineToken {
    /// Lexical kind.
    pub kind: TokenKind,
    /// 1-based line.
    pub line: usize,
    /// 1-based column (one step per character).
    pub column: usize,
}

impl PineToken {
    /// Attach a position to a kind.
    #[must_use]
    pub fn new(kind: TokenKind, line: usize, column: usize) -> Self {
        Self { kind, line, column }
    }
}

/// Tokenize `source` into a terminated stream (always ending in [`Eof`](TokenKind::Eof)).
#[must_use]
pub fn lex(source: &str) -> Vec<PineToken> {
    PineLexer::new(source).tokenize()
}

/// Cursor scanner behind [`lex`]. Single pass over characters; positions
/// track the reference lexer's rule (one column step per character, lines
/// reset the column).
pub struct PineLexer {
    input: Vec<char>,
    position: usize,
    line: usize,
    column: usize,
}

impl PineLexer {
    /// Prepare to scan `input`.
    #[must_use]
    pub fn new(input: &str) -> Self {
        Self {
            input: input.chars().collect(),
            position: 0,
            line: 1,
            column: 1,
        }
    }

    /// Scan to termination, including the final [`Eof`](TokenKind::Eof).
    #[must_use]
    pub fn tokenize(mut self) -> Vec<PineToken> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next_token();
            let done = token.kind == TokenKind::Eof;
            tokens.push(token);
            if done {
                break;
            }
        }
        tokens
    }

    /// Scan one token, skipping insignificant whitespace first.
    pub fn next_token(&mut self) -> PineToken {
        self.skip_whitespace();

        let line = self.line;
        let column = self.column;

        if self.is_at_end() {
            return PineToken::new(TokenKind::Eof, line, column);
        }

        let ch = self.current();

        if ch == '/' && self.peek() == Some('/') {
            return self.read_comment();
        }

        // `#rrggbb[aa]` color literals; a `#` without six hex digits keeps
        // the legacy skip-silently behavior.
        if ch == '#' {
            if let Some(color) = self.read_color_hex() {
                return color;
            }
            self.advance();
            return self.next_token();
        }

        if ch == '\n' {
            self.advance();
            self.line += 1;
            self.column = 1;
            return PineToken::new(TokenKind::Newline, line, column);
        }

        if ch == '"' || ch == '\'' {
            return self.read_string(ch);
        }

        if ch.is_ascii_digit() || (ch == '.' && self.peek().is_some_and(|c| c.is_ascii_digit())) {
            return self.read_number();
        }

        if ch.is_alphabetic() || ch == '_' {
            return self.read_identifier();
        }

        let kind = match ch {
            '+' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::PlusEqual
                } else {
                    TokenKind::Plus
                }
            }
            '-' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::MinusEqual
                } else {
                    TokenKind::Minus
                }
            }
            '*' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::StarEqual
                } else {
                    TokenKind::Star
                }
            }
            '/' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::SlashEqual
                } else {
                    TokenKind::Slash
                }
            }
            '%' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::PercentEqual
                } else {
                    TokenKind::Percent
                }
            }
            '(' => TokenKind::LeftParen,
            ')' => TokenKind::RightParen,
            '[' => TokenKind::LeftBracket,
            ']' => TokenKind::RightBracket,
            ',' => TokenKind::Comma,
            '.' => TokenKind::Dot,
            '?' => TokenKind::Question,
            ':' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::ColonEqual
                } else {
                    TokenKind::Colon
                }
            }
            '=' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::EqualEqual
                } else if self.peek() == Some('>') {
                    self.advance();
                    TokenKind::Arrow
                } else {
                    TokenKind::Equal
                }
            }
            '!' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::NotEqual
                } else {
                    TokenKind::Not
                }
            }
            '<' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::LessEqual
                } else {
                    TokenKind::Less
                }
            }
            '>' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::GreaterEqual
                } else {
                    TokenKind::Greater
                }
            }
            _ => {
                // Unknown character: skip it and keep scanning, so one
                // stray character (annotations, emoji outside literals)
                // never aborts the stream.
                self.advance();
                return self.next_token();
            }
        };

        self.advance();
        PineToken::new(kind, line, column)
    }

    fn read_comment(&mut self) -> PineToken {
        let (line, column) = (self.line, self.column);
        self.advance();
        self.advance();

        let mut comment = String::new();
        while !self.is_at_end() && self.current() != '\n' {
            comment.push(self.current());
            self.advance();
        }

        PineToken::new(TokenKind::Comment(comment.trim().to_owned()), line, column)
    }

    /// Scan a `#rrggbb[aa]` color literal. Returns `None` (consuming
    /// nothing) unless six hex digits follow the `#`; eight digits win over
    /// six when present.
    fn read_color_hex(&mut self) -> Option<PineToken> {
        let (line, column) = (self.line, self.column);
        let mut digits = String::new();
        let mut index = self.position + 1;
        while index < self.input.len()
            && digits.len() < 8
            && self.input[index].is_ascii_hexdigit()
        {
            digits.push(self.input[index]);
            index += 1;
        }
        // One advance for the `#` plus one per kept digit; digits never
        // contain newlines, so only the column moves.
        let take = if digits.len() >= 8 {
            8
        } else if digits.len() >= 6 {
            6
        } else {
            return None;
        };
        digits.truncate(take);
        for _ in 0..=take {
            self.advance();
        }
        Some(PineToken::new(TokenKind::ColorHex(digits), line, column))
    }

    fn read_string(&mut self, quote: char) -> PineToken {
        let (line, column) = (self.line, self.column);
        self.advance();

        let mut string = String::new();
        while !self.is_at_end() && self.current() != quote {
            if self.current() == '\\' {
                self.advance();
                if !self.is_at_end() {
                    let escaped = match self.current() {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '\\' => '\\',
                        '"' => '"',
                        '\'' => '\'',
                        c => c,
                    };
                    string.push(escaped);
                    self.advance();
                }
            } else {
                string.push(self.current());
                self.advance();
            }
        }

        if !self.is_at_end() {
            self.advance();
        }

        PineToken::new(TokenKind::String(string), line, column)
    }

    fn read_number(&mut self) -> PineToken {
        let (line, column) = (self.line, self.column);
        let mut number = String::new();

        while !self.is_at_end()
            && (self.current().is_ascii_digit() || self.current() == '.')
        {
            number.push(self.current());
            self.advance();
        }

        if !self.is_at_end() && (self.current() == 'e' || self.current() == 'E') {
            number.push(self.current());
            self.advance();

            if !self.is_at_end() && (self.current() == '+' || self.current() == '-') {
                number.push(self.current());
                self.advance();
            }

            while !self.is_at_end() && self.current().is_ascii_digit() {
                number.push(self.current());
                self.advance();
            }
        }

        PineToken::new(TokenKind::Number(number.parse().unwrap_or(0.0)), line, column)
    }

    fn read_identifier(&mut self) -> PineToken {
        let (line, column) = (self.line, self.column);
        let mut ident = String::new();

        while !self.is_at_end()
            && (self.current().is_alphanumeric() || self.current() == '_' || self.current() == '.')
        {
            ident.push(self.current());
            self.advance();
        }

        let kind = match ident.as_str() {
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "for" => TokenKind::For,
            "while" => TokenKind::While,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "var" => TokenKind::Var,
            "varip" => TokenKind::Varip,
            "open" => TokenKind::Open,
            "high" => TokenKind::High,
            "low" => TokenKind::Low,
            "close" => TokenKind::Close,
            "volume" => TokenKind::Volume,
            "time" => TokenKind::Time,
            "and" => TokenKind::And,
            "or" => TokenKind::Or,
            "not" => TokenKind::Not,
            "import" => TokenKind::Import,
            "export" => TokenKind::Export,
            "enum" => TokenKind::Enum,
            "type" => TokenKind::Type,
            "method" => TokenKind::Method,
            "const" => TokenKind::Const,
            "simple" => TokenKind::Simple,
            "switch" => TokenKind::Switch,
            "break" => TokenKind::Break,
            _ => TokenKind::Identifier(ident),
        };

        PineToken::new(kind, line, column)
    }

    fn skip_whitespace(&mut self) {
        while !self.is_at_end() {
            let ch = self.current();
            if ch == ' ' || ch == '\t' || ch == '\r' {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn current(&self) -> char {
        self.input[self.position]
    }

    fn peek(&self) -> Option<char> {
        if self.position + 1 < self.input.len() {
            Some(self.input[self.position + 1])
        } else {
            None
        }
    }

    fn advance(&mut self) {
        if !self.is_at_end() {
            self.position += 1;
            self.column += 1;
        }
    }

    fn is_at_end(&self) -> bool {
        self.position >= self.input.len()
    }
}

#[cfg(test)]
#[path = "../../tests/pine_lexer.rs"]
mod tests;
