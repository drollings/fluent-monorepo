//! Pine parser: egui-overlap grammar plus the v6 surface the targets need.
//!
//! [`parse_program`] turns a source file into a [`PineProgram`]. Expression
//! precedence mirrors the reference parser exactly (ternary, or, and,
//! equality, comparison, addition, multiplication, unary, postfix, primary),
//! and every overlapping statement and expression parses to the same shape.
//! Two deliberate additive divergences, both required by later milestones:
//! call arguments keep their names ([`CallArg`]) instead of being discarded,
//! and `var` declarations carry an optional type. Property refusal is not
//! copied either: dotted names without a call (`size.auto`,
//! `UTIL.Smoothings.WMA`) are plain [`Variables`](PineExpr::Variable).
//!
//! Blocks follow Pine indentation: a compound statement (`=>`, `if`,
//! `for`, `while`, `switch`, `enum`, `type`) owns every following statement
//! indented past its own first column, using the [`positions`](PineToken)
//! the lexer recorded. Multi-line expressions continue after an operator,
//! comma, or opening bracket; everywhere else a newline ends the statement.

use std::fmt;

use super::lexer::{PineToken, TokenKind};
use super::source::{ImportDecl, LibraryId};

/// A parsed Pine file: top-level statements in source order.
#[derive(Debug, Clone, PartialEq)]
pub struct PineProgram {
    /// Top-level statements.
    pub statements: Vec<PineStmt>,
}

impl PineProgram {
    /// Create an empty program.
    #[must_use]
    pub fn new() -> Self {
        Self {
            statements: Vec::new(),
        }
    }
}

impl Default for PineProgram {
    fn default() -> Self {
        Self::new()
    }
}

/// One Pine statement: the egui set plus v6 declarations.
#[derive(Debug, Clone, PartialEq)]
pub enum PineStmt {
    /// `import publisher/name/version as ALIAS` (reuses the loader identity).
    Import(ImportDecl),
    /// `library('name')` header.
    LibraryDecl {
        /// Declared library name.
        name: String,
    },
    /// `indicator(...)` header with preserved arguments.
    IndicatorDecl {
        /// Header arguments (names preserved).
        args: Vec<CallArg>,
    },
    /// `export` wrapping one declaration (function, method, const, enum, type).
    ExportDecl(Box<PineStmt>),
    /// `name(params) =>` body; the body's final expression is the value.
    FuncDecl {
        /// Function name.
        name: String,
        /// Declared parameters.
        params: Vec<Param>,
        /// Indented body.
        body: Vec<PineStmt>,
    },
    /// `method name(Type this, ...) =>` body.
    MethodDecl {
        /// Method name.
        name: String,
        /// Declared parameters (first is the receiver, e.g. `Scheduler this`).
        params: Vec<Param>,
        /// Indented body.
        body: Vec<PineStmt>,
    },
    /// `const type NAME = value`.
    ConstDecl {
        /// Declared type.
        type_name: PineType,
        /// Constant name.
        name: String,
        /// Constant value.
        value: Box<PineExpr>,
    },
    /// `enum Name` with `MEMBER = value` lines.
    EnumDecl {
        /// Enum name.
        name: String,
        /// Members in source order.
        members: Vec<EnumMember>,
    },
    /// `type Name` with typed field lines.
    TypeDecl {
        /// Type name.
        name: String,
        /// Declared fields.
        fields: Vec<FieldDecl>,
    },
    /// `var[name] = value` / `varip [type] name = value`.
    VarDeclaration {
        /// Variable name.
        name: String,
        /// Optional declared type (`var float x = ...`).
        type_name: Option<PineType>,
        /// Initial value.
        value: Box<PineExpr>,
        /// Intrabar persistence (`varip`).
        is_varip: bool,
    },
    /// Typed local: `int x = value`, `array<int> keys = value`.
    Declaration {
        /// Declared type.
        type_name: PineType,
        /// Variable name.
        name: String,
        /// Initial value.
        value: Box<PineExpr>,
    },
    /// Comma-separated locals (`float o = na, h = na`): each item carries
    /// its own optional type and optional initializer.
    MultiDeclaration {
        /// Declared items in source order.
        decls: Vec<MultiDecl>,
    },
    /// Untyped assignment: `name = value`.
    Assignment {
        /// Variable name.
        name: String,
        /// Assigned value.
        value: Box<PineExpr>,
    },
    /// Comma-separated assignments (`o := open, h := high`): each item
    /// carries its own operator and runs left to right through the matching
    /// single-assignment rule.
    MultiAssign {
        /// Assigned items in source order.
        items: Vec<AssignItem>,
    },
    /// Reassignment: `name := value`.
    Reassign {
        /// Variable name.
        name: String,
        /// Reassigned value.
        value: Box<PineExpr>,
    },
    /// Tuple destructuring: `[a, b] = value`.
    DestructureAssign {
        /// Bound names in order.
        targets: Vec<String>,
        /// Tuple value.
        value: Box<PineExpr>,
    },
    /// Bare expression statement (calls, switch values, bare returns).
    Expression(PineExpr),
    /// `if condition` block (`else` block or chained `else if`).
    If {
        /// Branch condition.
        condition: Box<PineExpr>,
        /// Taken branch.
        then_branch: Vec<PineStmt>,
        /// Optional `else` branch.
        else_branch: Option<Vec<PineStmt>>,
    },
    /// `for i = start to end` block.
    For {
        /// Loop variable.
        variable: String,
        /// First value.
        start: Box<PineExpr>,
        /// Last value.
        end: Box<PineExpr>,
        /// Loop body.
        body: Vec<PineStmt>,
    },
    /// `for x in arr` / `for [k, v] in pairs` iteration over collection
    /// values. One target binds each value; a pair binds the integer
    /// index beside the value.
    ForIn {
        /// Bound names: one variable, or a destructured pair.
        targets: Vec<String>,
        /// Iterated collection expression.
        iterable: Box<PineExpr>,
        /// Loop body.
        body: Vec<PineStmt>,
    },
    /// `while condition` block.
    While {
        /// Loop condition.
        condition: Box<PineExpr>,
        /// Loop body.
        body: Vec<PineStmt>,
    },
    /// `break`: exits the innermost enclosing `for`/`while`. Recorded at
    /// parse; the executor refuses it outside a loop body.
    Break,
    /// Statement-position `switch` (shares [`SwitchStmt`] with expressions).
    Switch(SwitchStmt),
}

/// One `switch` with all its arms; valid as a statement and an expression.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchStmt {
    /// Matched value, or `None` for the bare form whose arms are boolean
    /// conditions (first truthy arm wins).
    pub scrutinee: Option<Box<PineExpr>>,
    /// Arms in source order (defaults last by convention).
    pub arms: Vec<SwitchArm>,
}

/// One `pattern => body` arm; a missing pattern is the default arm.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchArm {
    /// Arm pattern, or `None` for the bare-`=>` default arm.
    pub pattern: Option<PineExpr>,
    /// Arm body (one same-line statement or an indented block).
    pub body: Vec<PineStmt>,
}

/// One Pine expression: the egui set plus tuples and switches.
#[derive(Debug, Clone, PartialEq)]
pub enum PineExpr {
    /// Numeric literal.
    Number(f64),
    /// String literal.
    StringLiteral(String),
    /// Boolean literal.
    Boolean(bool),
    /// Variable or dotted reference (`x`, `timeframe.period`, `size.auto`).
    Variable(String),
    /// Built-in series variable (`open/high/low/close/volume/time`).
    BuiltinVariable(BuiltinVar),
    /// Binary operation.
    Binary {
        /// Left operand.
        left: Box<PineExpr>,
        /// Operator.
        op: BinaryOp,
        /// Right operand.
        right: Box<PineExpr>,
    },
    /// Unary operation.
    Unary {
        /// Operator.
        op: UnaryOp,
        /// Operand.
        expr: Box<PineExpr>,
    },
    /// Plain call (`plot(...)`, `nz(...)`, `int(...)`).
    FunctionCall {
        /// Function name.
        name: String,
        /// Arguments (names preserved).
        args: Vec<CallArg>,
        /// Explicit generic arguments (`map.new<int, int>()`); empty when absent.
        type_args: Vec<PineType>,
    },
    /// Namespaced call (`ta.sma(...)`, `this.register(...)`).
    MethodCall {
        /// Receiver namespace.
        namespace: String,
        /// Method name.
        method: String,
        /// Arguments (names preserved).
        args: Vec<CallArg>,
        /// Explicit generic arguments; empty when absent.
        type_args: Vec<PineType>,
    },
    /// Call on a computed receiver (`Type.new().init(...)`): the receiver
    /// evaluates first, then the method runs against the resulting instance
    /// with no home to write back to.
    InstanceCall {
        /// Receiver expression (evaluates to an instance).
        receiver: Box<PineExpr>,
        /// Method name.
        method: String,
        /// Arguments (names preserved).
        args: Vec<CallArg>,
        /// Explicit generic arguments; empty when absent.
        type_args: Vec<PineType>,
    },
    /// Field read on a computed receiver (`lookup(...).field`). A plain
    /// variable receiver resolves exactly like its glued dotted spelling.
    FieldRead {
        /// Receiver expression.
        receiver: Box<PineExpr>,
        /// Field name.
        field: String,
    },
    /// `condition ? then_expr : else_expr`.
    Ternary {
        /// Condition.
        condition: Box<PineExpr>,
        /// Taken value.
        then_expr: Box<PineExpr>,
        /// Fallback value.
        else_expr: Box<PineExpr>,
    },
    /// `array[index]` with a computed index.
    ArrayAccess {
        /// Accessed value.
        array: Box<PineExpr>,
        /// Index expression.
        index: Box<PineExpr>,
    },
    /// `series[n]` with a literal bar offset.
    SeriesOffset {
        /// Offset series.
        series: Box<PineExpr>,
        /// Bars back.
        offset: i32,
    },
    /// Tuple literal: `[a, b]` values and multi-returns.
    ArrayLiteral(Vec<PineExpr>),
    /// `#rrggbb[aa]` color literal, packed exactly like `color.rgb`.
    ColorLiteral(u32),
    /// Expression-position `switch`.
    Switch(Box<SwitchStmt>),
}

/// One call argument with its optional `name =` prefix preserved.
#[derive(Debug, Clone, PartialEq)]
pub struct CallArg {
    /// Argument name for `name = value`, else positional.
    pub name: Option<String>,
    /// Argument value.
    pub value: PineExpr,
}

/// One declared parameter: `[qualifier] [type] name [= default]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// `simple` (or `const`) qualifier, when present.
    pub qualifier: Option<String>,
    /// Declared type, when present.
    pub type_name: Option<PineType>,
    /// Parameter name.
    pub name: String,
    /// Default value, when present.
    pub default: Option<PineExpr>,
}

/// One item of a comma-separated local declaration: `[type] name [= value]`.
#[derive(Debug, Clone, PartialEq)]
pub struct MultiDecl {
    /// Declared type, when written on the item.
    pub type_name: Option<PineType>,
    /// Variable name.
    pub name: String,
    /// Initializer, when present (absent reads as `na`).
    pub value: Option<PineExpr>,
}

/// Operator of one comma-separated assignment item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    /// `=` assignment.
    Assign,
    /// `:=` reassignment (targets must be declared, `this.*` allowed).
    Reassign,
}

/// One item of a comma-separated assignment: `name op value`.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignItem {
    /// Variable name (possibly a `this.*` path for `:=`).
    pub name: String,
    /// Assignment operator.
    pub op: AssignOp,
    /// Assigned value.
    pub value: PineExpr,
}

/// A Pine type name with optional generic arguments
/// (`float`, `UTIL.Smoothings`, `array<float>`, `map<int, int>`).
#[derive(Debug, Clone, PartialEq)]
pub struct PineType {
    /// Type name (possibly dotted).
    pub name: String,
    /// Generic arguments.
    pub args: Vec<PineType>,
}

/// One `MEMBER = value` enum line.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumMember {
    /// Member name.
    pub name: String,
    /// Member value.
    pub value: PineExpr,
}

/// Persistence qualifier on one `type`-block field line: Pine carries
/// `var`/`varip` into user-type fields, so the declaration records which one
/// (if any) qualified the field. Execution of the qualifier arrives with the
/// headless library work; recording it here keeps parsing faithful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldPersistence {
    /// `var` field: persists across bars of the owning instance.
    Var,
    /// `varip` field: persists intrabar as well.
    Varip,
}

/// One typed field line in a `type` block.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    /// Declared type.
    pub type_name: PineType,
    /// Field name.
    pub name: String,
    /// Default value, when present.
    pub default: Option<PineExpr>,
    /// Persistence qualifier (`var`/`varip`), when present.
    pub persistence: Option<FieldPersistence>,
}

/// Built-in series variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinVar {
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
}

impl fmt::Display for BuiltinVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open => write!(f, "open"),
            Self::High => write!(f, "high"),
            Self::Low => write!(f, "low"),
            Self::Close => write!(f, "close"),
            Self::Volume => write!(f, "volume"),
            Self::Time => write!(f, "time"),
        }
    }
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// `+`.
    Add,
    /// `-`.
    Subtract,
    /// `*`.
    Multiply,
    /// `/`.
    Divide,
    /// `%`.
    Modulo,
    /// `==`.
    Equal,
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
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op = match self {
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
            Self::Divide => "/",
            Self::Modulo => "%",
            Self::Equal => "==",
            Self::NotEqual => "!=",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
            Self::And => "and",
            Self::Or => "or",
        };
        write!(f, "{op}")
    }
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-`.
    Negate,
    /// `not`.
    Not,
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op = match self {
            Self::Negate => "-",
            Self::Not => "not",
        };
        write!(f, "{op}")
    }
}

/// Parse failure with source position.
#[derive(Debug, Clone, PartialEq)]
pub enum PineParseError {
    /// A token of another kind arrived where `expected` was required.
    UnexpectedToken {
        /// What the grammar required.
        expected: String,
        /// What arrived.
        found: TokenKind,
        /// 1-based line.
        line: usize,
        /// 1-based column.
        column: usize,
    },
    /// Input ended mid-construct.
    UnexpectedEof,
    /// Otherwise malformed syntax with a reason.
    InvalidSyntax(String),
}

impl fmt::Display for PineParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedToken {
                expected,
                found,
                line,
                column,
            } => write!(f, "parse error at {line}:{column}: expected {expected}, found {found}"),
            Self::UnexpectedEof => write!(f, "unexpected end of file"),
            Self::InvalidSyntax(message) => write!(f, "invalid syntax: {message}"),
        }
    }
}

impl std::error::Error for PineParseError {}

/// Parse a full source file into a [`PineProgram`].
pub fn parse_program(source: &str) -> Result<PineProgram, PineParseError> {
    PineParser::new(super::lexer::lex(source)).parse()
}

/// Cursor parser behind [`parse_program`].
pub struct PineParser {
    tokens: Vec<PineToken>,
    position: usize,
}

impl PineParser {
    /// Prepare to parse `tokens` (must end in `Eof`, as [`lex`](super::lexer::lex) provides).
    #[must_use]
    pub fn new(tokens: Vec<PineToken>) -> Self {
        Self { tokens, position: 0 }
    }

    /// Parse every top-level statement.
    pub fn parse(&mut self) -> Result<PineProgram, PineParseError> {
        let mut program = PineProgram::new();
        loop {
            self.skip_trivia();
            if self.is_at_end() {
                break;
            }
            let statement = self.parse_statement()?;
            program.statements.push(statement);
            if self.current_kind() == &TokenKind::Newline {
                self.advance();
            }
        }
        Ok(program)
    }

    fn parse_statement(&mut self) -> Result<PineStmt, PineParseError> {
        self.skip_trivia();
        let column = self.current().column;
        match self.current_kind() {
            TokenKind::Var | TokenKind::Varip => self.parse_var_declaration(),
            TokenKind::If => self.parse_if_statement(None),
            TokenKind::For => self.parse_for_statement(),
            TokenKind::While => self.parse_while_statement(),
            TokenKind::Break => {
                self.advance();
                Ok(PineStmt::Break)
            }
            TokenKind::Switch => Ok(PineStmt::Switch(self.parse_switch()?)),
            TokenKind::Import => self.parse_import(),
            TokenKind::Export => {
                self.advance();
                Ok(PineStmt::ExportDecl(Box::new(
                    self.parse_declaration(column)?,
                )))
            }
            TokenKind::Enum => self.parse_enum(column),
            TokenKind::Type => self.parse_type_decl(column),
            TokenKind::Const => self.parse_const_decl(),
            TokenKind::Method => self.parse_method(column),
            TokenKind::LeftBracket => self.parse_bracket_statement(),
            TokenKind::Identifier(_) => self.parse_identifier_statement(),
            _ => Ok(PineStmt::Expression(self.parse_expression()?)),
        }
    }

    /// Parse one indented block: every following statement starting past
    /// `parent_column` belongs to it.
    fn parse_block(&mut self, parent_column: usize) -> Result<Vec<PineStmt>, PineParseError> {
        let mut body = Vec::new();
        while self.next_significant_column() > Some(parent_column) {
            self.skip_trivia();
            if self.is_at_end() {
                break;
            }
            body.push(self.parse_statement()?);
        }
        Ok(body)
    }

    /// Column of the next non-trivia token, if any before `Eof`.
    fn next_significant_column(&self) -> Option<usize> {
        let mut index = self.position;
        while index < self.tokens.len() {
            match &self.tokens[index].kind {
                TokenKind::Newline | TokenKind::Comment(_) => index += 1,
                TokenKind::Eof => return None,
                _ => return Some(self.tokens[index].column),
            }
        }
        None
    }

    fn parse_var_declaration(&mut self) -> Result<PineStmt, PineParseError> {
        let is_varip = self.current_kind() == &TokenKind::Varip;
        self.advance();
        self.skip_trivia();
        // Optional declared type: `var float x = ...` (or `var array<int> x`)
        // shows a type before the name, while `var x = ...` names the
        // variable directly.
        let (name, type_name) = match (self.current_kind(), self.peek_kind()) {
            (TokenKind::Identifier(_), Some(TokenKind::Identifier(_) | TokenKind::Less)) => {
                let parsed = self.parse_type();
                let variable = self.expect_identifier()?;
                (variable, Some(parsed))
            }
            _ => (self.expect_identifier()?, None),
        };
        self.skip_trivia();
        self.expect(&TokenKind::Equal)?;
        let value = Box::new(self.parse_expression()?);
        Ok(PineStmt::VarDeclaration {
            name,
            type_name,
            value,
            is_varip,
        })
    }

    /// Parse `if`. `line_column` carries the statement line's first column
    /// for chained `else if`: the nested `if` starts mid-line (after `else`),
    /// but its body aligns with the `else`, not the inner keyword. Only a
    /// same-line `else if` chains; an `if` on a later line opens a nested
    /// statement inside the `else` block. An `else` binds only at its own
    /// `if`'s column: a dedented `else` after a nested `if` closes the
    /// outer block instead of feeding the inner one an empty branch.
    fn parse_if_statement(&mut self, line_column: Option<usize>) -> Result<PineStmt, PineParseError> {
        let column = line_column.unwrap_or_else(|| self.current().column);
        self.advance();
        let condition = Box::new(self.parse_expression()?);
        let then_branch = self.parse_block(column)?;
        self.skip_trivia();
        let else_branch = if self.current_kind() == &TokenKind::Else
            && self.current().column == column
        {
            let else_column = self.current().column;
            self.advance();
            self.skip_comments();
            if self.current_kind() == &TokenKind::If {
                Some(vec![self.parse_if_statement(Some(else_column))?])
            } else {
                Some(self.parse_block(column)?)
            }
        } else {
            None
        };
        Ok(PineStmt::If {
            condition,
            then_branch,
            else_branch,
        })
    }

    fn parse_for_statement(&mut self) -> Result<PineStmt, PineParseError> {
        let column = self.current().column;
        self.advance();
        self.skip_trivia();
        // Tuple form first: `for [k, v] in pairs`.
        if self.current_kind() == &TokenKind::LeftBracket {
            self.advance();
            self.skip_trivia();
            let mut targets = Vec::new();
            loop {
                self.skip_trivia();
                targets.push(self.expect_identifier()?);
                self.skip_trivia();
                if self.current_kind() == &TokenKind::Comma {
                    self.advance();
                    continue;
                }
                break;
            }
            self.expect(&TokenKind::RightBracket)?;
            self.skip_trivia();
            match self.current_kind() {
                TokenKind::Identifier(id) if id == "in" => self.advance(),
                _ => {
                    return Err(PineParseError::InvalidSyntax(
                        "expected 'in' in for loop".to_owned(),
                    ));
                }
            }
            let iterable = Box::new(self.parse_expression()?);
            let body = self.parse_block(column)?;
            return Ok(PineStmt::ForIn {
                targets,
                iterable,
                body,
            });
        }
        let variable = self.expect_identifier()?;
        self.skip_trivia();
        // Collection form: `for x in arr`.
        if let TokenKind::Identifier(id) = self.current_kind() {
            if id == "in" {
                self.advance();
                let iterable = Box::new(self.parse_expression()?);
                let body = self.parse_block(column)?;
                return Ok(PineStmt::ForIn {
                    targets: vec![variable],
                    iterable,
                    body,
                });
            }
        }
        self.expect(&TokenKind::Equal)?;
        let start = Box::new(self.parse_expression()?);
        self.skip_trivia();
        match self.current_kind() {
            TokenKind::Identifier(id) if id == "to" => self.advance(),
            _ => {
                return Err(PineParseError::InvalidSyntax(
                    "expected 'to' in for loop".to_owned(),
                ));
            }
        }
        let end = Box::new(self.parse_expression()?);
        let body = self.parse_block(column)?;
        Ok(PineStmt::For {
            variable,
            start,
            end,
            body,
        })
    }

    fn parse_while_statement(&mut self) -> Result<PineStmt, PineParseError> {
        let column = self.current().column;
        self.advance();
        let condition = Box::new(self.parse_expression()?);
        let body = self.parse_block(column)?;
        Ok(PineStmt::While { condition, body })
    }

    fn parse_import(&mut self) -> Result<PineStmt, PineParseError> {
        self.advance();
        self.skip_trivia();
        let publisher = self.expect_identifier()?;
        self.skip_trivia();
        self.expect(&TokenKind::Slash)?;
        self.skip_trivia();
        let name = self.expect_identifier()?;
        self.skip_trivia();
        self.expect(&TokenKind::Slash)?;
        self.skip_trivia();
        let version_token = self.current().clone();
        let version = match &version_token.kind {
            TokenKind::Number(n) => {
                self.advance();
                if *n < 0.0 || n.fract() != 0.0 {
                    return Err(PineParseError::InvalidSyntax(format!(
                        "import version must be a non-negative integer, got {n}"
                    )));
                }
                *n as u64
            }
            _ => {
                return Err(PineParseError::UnexpectedToken {
                    expected: "import version".to_owned(),
                    found: version_token.kind.clone(),
                    line: version_token.line,
                    column: version_token.column,
                });
            }
        };
        self.skip_trivia();
        let alias = match self.current_kind() {
            TokenKind::Identifier(marker) if marker == "as" => {
                self.advance();
                self.skip_trivia();
                self.expect_identifier()?
            }
            _ => name.clone(),
        };
        Ok(PineStmt::Import(ImportDecl {
            library: LibraryId {
                publisher,
                name,
                version,
            },
            alias,
        }))
    }

    /// Parse one declaration after `export` (or bare): const, enum, type,
    /// method, or `name(params) =>` function. `column` is the enclosing
    /// statement's first column, which owns the declaration body.
    fn parse_declaration(&mut self, column: usize) -> Result<PineStmt, PineParseError> {
        self.skip_trivia();
        match self.current_kind() {
            TokenKind::Const => self.parse_const_decl(),
            TokenKind::Enum => self.parse_enum(column),
            TokenKind::Type => self.parse_type_decl(column),
            TokenKind::Method => self.parse_method(column),
            TokenKind::Identifier(_) => {
                let name = self.expect_identifier()?;
                self.skip_trivia();
                if self.current_kind() == &TokenKind::LeftParen {
                    self.parse_function(name, column)
                } else {
                    Err(PineParseError::InvalidSyntax(format!(
                        "expected a declaration after the name {name:?}"
                    )))
                }
            }
            _ => {
                let token = self.current().clone();
                Err(PineParseError::UnexpectedToken {
                    expected: "declaration".to_owned(),
                    found: token.kind.clone(),
                    line: token.line,
                    column: token.column,
                })
            }
        }
    }

    fn parse_const_decl(&mut self) -> Result<PineStmt, PineParseError> {
        self.advance();
        self.skip_trivia();
        let type_name = self.parse_type();
        self.skip_trivia();
        let name = self.expect_identifier()?;
        self.skip_trivia();
        self.expect(&TokenKind::Equal)?;
        let value = self.parse_expression()?;
        Ok(PineStmt::ConstDecl {
            type_name,
            name,
            value: Box::new(value),
        })
    }

    fn parse_enum(&mut self, column: usize) -> Result<PineStmt, PineParseError> {
        self.advance();
        self.skip_trivia();
        let name = self.expect_identifier()?;
        let mut members = Vec::new();
        while self.next_significant_column() > Some(column) {
            self.skip_trivia();
            if self.is_at_end() {
                break;
            }
            let member = self.expect_identifier()?;
            self.skip_trivia();
            self.expect(&TokenKind::Equal)?;
            let value = self.parse_expression()?;
            members.push(EnumMember { name: member, value });
        }
        Ok(PineStmt::EnumDecl { name, members })
    }

    fn parse_type_decl(&mut self, column: usize) -> Result<PineStmt, PineParseError> {
        self.advance();
        self.skip_trivia();
        let name = self.expect_identifier()?;
        let mut fields = Vec::new();
        while self.next_significant_column() > Some(column) {
            self.skip_trivia();
            if self.is_at_end() {
                break;
            }
            // Optional persistence qualifier (`var`/`varip`) before the
            // field type, as in `varip float last_price = na`.
            let persistence = match self.current_kind() {
                TokenKind::Var => {
                    self.advance();
                    self.skip_trivia();
                    Some(FieldPersistence::Var)
                }
                TokenKind::Varip => {
                    self.advance();
                    self.skip_trivia();
                    Some(FieldPersistence::Varip)
                }
                _ => None,
            };
            let type_name = self.parse_type();
            self.skip_trivia();
            // A bare `name = default` line declares an untyped field; the
            // parsed type is really the field name. Anything else carries an
            // explicit type before the field name.
            let (type_name, field) = if self.current_kind() == &TokenKind::Equal
                && !type_name.name.is_empty()
                && type_name.args.is_empty()
            {
                (
                    PineType {
                        name: String::new(),
                        args: Vec::new(),
                    },
                    type_name.name,
                )
            } else {
                (type_name, self.expect_identifier()?)
            };
            self.skip_trivia();
            let default = if self.current_kind() == &TokenKind::Equal {
                self.advance();
                Some(self.parse_expression()?)
            } else {
                None
            };
            fields.push(FieldDecl {
                type_name,
                name: field,
                default,
                persistence,
            });
        }
        Ok(PineStmt::TypeDecl { name, fields })
    }

    fn parse_method(&mut self, column: usize) -> Result<PineStmt, PineParseError> {
        self.advance();
        self.skip_trivia();
        let name = self.expect_identifier()?;
        self.skip_trivia();
        self.expect(&TokenKind::LeftParen)?;
        let params = self.parse_params()?;
        self.expect(&TokenKind::RightParen)?;
        self.skip_trivia();
        self.expect(&TokenKind::Arrow)?;
        let body = self.parse_block(column)?;
        Ok(PineStmt::MethodDecl { name, params, body })
    }

    fn parse_function(&mut self, name: String, column: usize) -> Result<PineStmt, PineParseError> {
        self.expect(&TokenKind::LeftParen)?;
        let params = self.parse_params()?;
        self.expect(&TokenKind::RightParen)?;
        self.skip_trivia();
        self.expect(&TokenKind::Arrow)?;
        let body = self.parse_block(column)?;
        Ok(PineStmt::FuncDecl { name, params, body })
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, PineParseError> {
        let mut params = Vec::new();
        loop {
            self.skip_trivia();
            if self.current_kind() == &TokenKind::RightParen {
                break;
            }
            let qualifier = match self.current_kind() {
                TokenKind::Const => {
                    self.advance();
                    self.skip_trivia();
                    Some("const".to_owned())
                }
                TokenKind::Simple => {
                    self.advance();
                    self.skip_trivia();
                    Some("simple".to_owned())
                }
                _ => None,
            };
            let parsed = self.parse_type();
            self.skip_trivia();
            let (type_name, name) = match self.current_kind() {
                TokenKind::Identifier(_) | TokenKind::Type => {
                    let name = self.expect_name()?;
                    (Some(parsed), name)
                }
                _ => {
                    if !parsed.args.is_empty() || parsed.name.contains('.') {
                        let token = self.current().clone();
                        return Err(PineParseError::UnexpectedToken {
                            expected: "parameter name".to_owned(),
                            found: token.kind.clone(),
                            line: token.line,
                            column: token.column,
                        });
                    }
                    if parsed.name.is_empty() {
                        let token = self.current().clone();
                        return Err(PineParseError::UnexpectedToken {
                            expected: "parameter".to_owned(),
                            found: token.kind.clone(),
                            line: token.line,
                            column: token.column,
                        });
                    }
                    (None, parsed.name)
                }
            };
            self.skip_trivia();
            let default = if self.current_kind() == &TokenKind::Equal {
                self.advance();
                Some(self.parse_expression()?)
            } else {
                None
            };
            params.push(Param {
                qualifier,
                type_name,
                name,
                default,
            });
            self.skip_trivia();
            if self.current_kind() == &TokenKind::Comma {
                self.advance();
            } else {
                break;
            }
        }
        Ok(params)
    }

    /// Parse a type name with optional generic arguments.
    fn parse_type(&mut self) -> PineType {
        let name = match self.current_kind() {
            TokenKind::Identifier(id) => {
                let name = id.clone();
                self.advance();
                name
            }
            _ => String::new(),
        };
        let mut args = Vec::new();
        if self.current_kind() == &TokenKind::Less {
            self.advance();
            loop {
                self.skip_trivia();
                args.push(self.parse_type());
                self.skip_trivia();
                if self.current_kind() == &TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            if self.current_kind() == &TokenKind::Greater {
                self.advance();
            }
        }
        PineType { name, args }
    }

    fn parse_identifier_statement(&mut self) -> Result<PineStmt, PineParseError> {
        let checkpoint = self.position;
        let name = self.expect_identifier()?;
        // Newlines are significant here: `foo` at end of line is an
        // expression statement even when the next line starts an assignment.
        // Only comments may intervene before `=`, `:=`, `(`, or a type name.
        self.skip_comments();
        match self.current_kind() {
            TokenKind::Equal | TokenKind::ColonEqual => {
                // Comma-separated assignments share the statement
                // (`o := open, h := high`); a lone pair stays a single node.
                let mut items = Vec::new();
                let (first_name, first_op) = if self.current_kind() == &TokenKind::Equal {
                    self.advance();
                    (name, AssignOp::Assign)
                } else {
                    self.advance();
                    (name, AssignOp::Reassign)
                };
                items.push(AssignItem {
                    name: first_name,
                    op: first_op,
                    value: self.parse_expression()?,
                });
                while self.current_kind() == &TokenKind::Comma {
                    self.advance();
                    self.skip_trivia();
                    let item = self.expect_identifier()?;
                    self.skip_comments();
                    let op = match self.current_kind() {
                        TokenKind::Equal => AssignOp::Assign,
                        TokenKind::ColonEqual => AssignOp::Reassign,
                        _ => {
                            return Err(PineParseError::InvalidSyntax(
                                "expected '=' or ':=' in assignment list".to_owned(),
                            ));
                        }
                    };
                    self.advance();
                    items.push(AssignItem {
                        name: item,
                        op,
                        value: self.parse_expression()?,
                    });
                }
                if items.len() == 1 {
                    let only = items.pop().expect("one assignment parsed");
                    Ok(match only.op {
                        AssignOp::Assign => PineStmt::Assignment {
                            name: only.name,
                            value: Box::new(only.value),
                        },
                        AssignOp::Reassign => PineStmt::Reassign {
                            name: only.name,
                            value: Box::new(only.value),
                        },
                    })
                } else {
                    Ok(PineStmt::MultiAssign { items })
                }
            }
            TokenKind::PlusEqual
            | TokenKind::MinusEqual
            | TokenKind::StarEqual
            | TokenKind::SlashEqual
            | TokenKind::PercentEqual => {
                // Augmented assignment desugars to reassignment over the
                // current binding (`x += v` reads exactly as `x := x + v`),
                // so `:=` target rules and `this.field` paths are reused
                // with no second assignment implementation.
                let op = match self.current_kind() {
                    TokenKind::PlusEqual => BinaryOp::Add,
                    TokenKind::MinusEqual => BinaryOp::Subtract,
                    TokenKind::StarEqual => BinaryOp::Multiply,
                    TokenKind::SlashEqual => BinaryOp::Divide,
                    _ => BinaryOp::Modulo,
                };
                self.advance();
                let rhs = self.parse_expression()?;
                Ok(PineStmt::Reassign {
                    name: name.clone(),
                    value: Box::new(PineExpr::Binary {
                        left: Box::new(PineExpr::Variable(name)),
                        op,
                        right: Box::new(rhs),
                    }),
                })
            }
            TokenKind::Identifier(_) | TokenKind::Less => {
                // Possibly a typed declaration (`int x = ...`,
                // `array<int> keys = ...`): rewind and prove the full
                // `type name =` shape before committing to it.
                self.position = checkpoint;
                if let Some(decl) = self.try_parse_declaration() {
                    Ok(decl)
                } else {
                    self.position = checkpoint;
                    self.parse_call_or_declaration()
                }
            }
            TokenKind::LeftParen => {
                self.position = checkpoint;
                self.parse_call_or_declaration()
            }
            _ => {
                self.position = checkpoint;
                Ok(PineStmt::Expression(self.parse_expression()?))
            }
        }
    }

    /// Attempt `type name = value` with `, name = value` followers; returns
    /// `None` (without consuming) unless the full shape is present. One item
    /// yields [`PineStmt::Declaration`]; two or more yield
    /// [`PineStmt::MultiDeclaration`]. Followers may restate their type
    /// (`float o = na, float h = na`) or share the first (`float o = na,
    /// h = na`), and may omit the initializer (absent reads as `na`).
    fn try_parse_declaration(&mut self) -> Option<PineStmt> {
        let checkpoint = self.position;
        let type_name = self.parse_type();
        if type_name.name.is_empty() {
            self.position = checkpoint;
            return None;
        }
        self.skip_trivia();
        let TokenKind::Identifier(id) = self.current_kind() else {
            self.position = checkpoint;
            return None;
        };
        let name = id.clone();
        self.advance();
        self.skip_trivia();
        if self.current_kind() != &TokenKind::Equal {
            self.position = checkpoint;
            return None;
        }
        self.advance();
        let value = self.parse_expression().ok()?;
        let mut decls = vec![MultiDecl {
            type_name: Some(type_name),
            name,
            value: Some(value),
        }];
        while self.current_kind() == &TokenKind::Comma {
            self.advance();
            self.skip_trivia();
            // Optional restated type: an identifier (or generic opener)
            // followed by another identifier names a type, not the variable.
            let item_type = match (self.current_kind(), self.peek_kind()) {
                (TokenKind::Identifier(_), Some(TokenKind::Identifier(_) | TokenKind::Less)) => {
                    Some(self.parse_type())
                }
                _ => None,
            };
            self.skip_trivia();
            let TokenKind::Identifier(item) = self.current_kind() else {
                self.position = checkpoint;
                return None;
            };
            let item = item.clone();
            self.advance();
            self.skip_trivia();
            let init = if self.current_kind() == &TokenKind::Equal {
                self.advance();
                Some(self.parse_expression().ok()?)
            } else {
                None
            };
            decls.push(MultiDecl {
                type_name: item_type,
                name: item,
                value: init,
            });
        }
        if decls.len() == 1 {
            let only = decls.pop().expect("one declaration parsed");
            return Some(PineStmt::Declaration {
                type_name: only.type_name.expect("first item always typed"),
                name: only.name,
                value: Box::new(only.value.expect("first item always initialized")),
            });
        }
        Some(PineStmt::MultiDeclaration { decls })
    }

    /// Parse `name(...)` (call) or `name(...) =>` (function declaration),
    /// decided by a nesting-aware scan for the `=>` after the parens.
    fn parse_call_or_declaration(&mut self) -> Result<PineStmt, PineParseError> {
        let column = self.current().column;
        let name = self.expect_identifier()?;
        self.skip_comments();
        if self.current_kind() == &TokenKind::LeftParen && self.parens_open_declaration() {
            return self.parse_function(name, column);
        }
        if name == "library" && self.current_kind() == &TokenKind::LeftParen {
            self.advance();
            self.skip_trivia();
            let header = self.current().clone();
            match &header.kind {
                TokenKind::String(library) => {
                    let library = library.clone();
                    self.advance();
                    self.skip_trivia();
                    self.expect(&TokenKind::RightParen)?;
                    return Ok(PineStmt::LibraryDecl { name: library });
                }
                _ => {
                    return Err(PineParseError::UnexpectedToken {
                        expected: "library name".to_owned(),
                        found: header.kind.clone(),
                        line: header.line,
                        column: header.column,
                    });
                }
            }
        }
        if name == "indicator" && self.current_kind() == &TokenKind::LeftParen {
            self.advance();
            let args = self.parse_call_args()?;
            self.expect(&TokenKind::RightParen)?;
            return Ok(PineStmt::IndicatorDecl { args });
        }
        self.position = self.position.saturating_sub(1);
        Ok(PineStmt::Expression(self.parse_expression()?))
    }

    /// Whether the paren group opening here closes right before a `=>`.
    fn parens_open_declaration(&self) -> bool {
        let mut depth = 0;
        let mut index = self.position;
        while index < self.tokens.len() {
            match &self.tokens[index].kind {
                TokenKind::LeftParen => depth += 1,
                TokenKind::RightParen => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(
                            self.tokens.get(index + 1).map(|token| &token.kind),
                            Some(TokenKind::Arrow)
                        );
                    }
                }
                TokenKind::Eof => return false,
                _ => {}
            }
            index += 1;
        }
        false
    }

    /// Parse `[...] = value` destructuring; anything else starting with `[`
    /// falls back to an array-literal expression.
    fn parse_bracket_statement(&mut self) -> Result<PineStmt, PineParseError> {
        let checkpoint = self.position;
        self.advance();
        let mut targets = Vec::new();
        loop {
            self.skip_trivia();
            match self.current_kind() {
                TokenKind::Identifier(name) => {
                    targets.push(name.clone());
                    self.advance();
                }
                _ => break,
            }
            self.skip_trivia();
            if self.current_kind() == &TokenKind::Comma {
                self.advance();
                continue;
            }
            break;
        }
        self.skip_trivia();
        let is_destructure = !targets.is_empty()
            && self.current_kind() == &TokenKind::RightBracket
            && matches!(
                self.peek_kind(),
                Some(TokenKind::Equal | TokenKind::ColonEqual)
            );
        if is_destructure {
            self.advance();
            self.advance();
            return Ok(PineStmt::DestructureAssign {
                targets,
                value: Box::new(self.parse_expression()?),
            });
        }
        self.position = checkpoint;
        Ok(PineStmt::Expression(self.parse_expression()?))
    }

    fn parse_switch(&mut self) -> Result<SwitchStmt, PineParseError> {
        self.advance();
        // A scrutinee rides the `switch` line; a newline opens the bare
        // boolean-arm form instead.
        self.skip_comments();
        let scrutinee = if matches!(
            self.current_kind(),
            TokenKind::Newline | TokenKind::Eof
        ) {
            None
        } else {
            Some(Box::new(self.parse_expression()?))
        };
        // Arms align at the first arm's column; a dedent ends the switch.
        // No constraint against the `switch` keyword column: expression
        // switches (`x = switch y`) indent past the statement, not the key.
        let mut arms = Vec::new();
        let mut arm_column: Option<usize> = None;
        loop {
            self.skip_trivia();
            if self.is_at_end() {
                break;
            }
            let next = self.current().clone();
            if let Some(aligned) = arm_column {
                if next.column < aligned {
                    break;
                }
                if next.column > aligned {
                    let here = self.current().clone();
                    return Err(PineParseError::InvalidSyntax(format!(
                        "unexpected indent in switch arms at {}:{}",
                        here.line, here.column
                    )));
                }
            }
            let at = next.column;
            let pattern = if next.kind == TokenKind::Arrow {
                self.advance();
                None
            } else {
                let pattern = self.parse_expression()?;
                self.skip_trivia();
                self.expect(&TokenKind::Arrow)?;
                Some(pattern)
            };
            // A newline after `=>` opens an indented block; anything else is
            // one same-line statement. The check runs before skipping trivia
            // so a newline genuinely starts a block.
            let mut body = Vec::new();
            if matches!(
                self.current_kind(),
                TokenKind::Newline | TokenKind::Comment(_)
            ) {
                body = self.parse_block(at)?;
                if body.is_empty() {
                    return Err(PineParseError::InvalidSyntax(
                        "switch arm has no body".to_owned(),
                    ));
                }
            } else if !self.is_at_end() {
                body.push(self.parse_statement()?);
            } else {
                return Err(PineParseError::InvalidSyntax(
                    "switch arm has no body".to_owned(),
                ));
            }
            arm_column = Some(at);
            arms.push(SwitchArm { pattern, body });
        }
        if arms.is_empty() {
            return Err(PineParseError::InvalidSyntax(
                "switch has no arms".to_owned(),
            ));
        }
        Ok(SwitchStmt { scrutinee, arms })
    }

    fn parse_expression(&mut self) -> Result<PineExpr, PineParseError> {
        self.parse_ternary()
    }

    fn parse_ternary(&mut self) -> Result<PineExpr, PineParseError> {
        let mut expr = self.parse_logical_or()?;
        self.skip_comments();
        if self.current_kind() == &TokenKind::Question {
            self.advance();
            let then_expr = Box::new(self.parse_expression()?);
            self.skip_comments();
            self.expect(&TokenKind::Colon)?;
            let else_expr = Box::new(self.parse_expression()?);
            expr = PineExpr::Ternary {
                condition: Box::new(expr),
                then_expr,
                else_expr,
            };
        }
        Ok(expr)
    }

    fn parse_logical_or(&mut self) -> Result<PineExpr, PineParseError> {
        let mut left = self.parse_logical_and()?;
        loop {
            self.skip_comments();
            if self.current_kind() != &TokenKind::Or {
                break;
            }
            self.advance();
            let right = self.parse_logical_and()?;
            left = PineExpr::Binary {
                left: Box::new(left),
                op: BinaryOp::Or,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_logical_and(&mut self) -> Result<PineExpr, PineParseError> {
        let mut left = self.parse_equality()?;
        loop {
            self.skip_comments();
            if self.current_kind() != &TokenKind::And {
                break;
            }
            self.advance();
            let right = self.parse_equality()?;
            left = PineExpr::Binary {
                left: Box::new(left),
                op: BinaryOp::And,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<PineExpr, PineParseError> {
        let mut left = self.parse_comparison()?;
        loop {
            self.skip_comments();
            let op = match self.current_kind() {
                TokenKind::EqualEqual => BinaryOp::Equal,
                TokenKind::NotEqual => BinaryOp::NotEqual,
                _ => break,
            };
            self.advance();
            let right = self.parse_comparison()?;
            left = PineExpr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<PineExpr, PineParseError> {
        let mut left = self.parse_addition()?;
        loop {
            self.skip_comments();
            let op = match self.current_kind() {
                TokenKind::Less => BinaryOp::Less,
                TokenKind::LessEqual => BinaryOp::LessEqual,
                TokenKind::Greater => BinaryOp::Greater,
                TokenKind::GreaterEqual => BinaryOp::GreaterEqual,
                _ => break,
            };
            self.advance();
            let right = self.parse_addition()?;
            left = PineExpr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_addition(&mut self) -> Result<PineExpr, PineParseError> {
        let mut left = self.parse_multiplication()?;
        loop {
            self.skip_comments();
            let op = match self.current_kind() {
                TokenKind::Plus => BinaryOp::Add,
                TokenKind::Minus => BinaryOp::Subtract,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplication()?;
            left = PineExpr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_multiplication(&mut self) -> Result<PineExpr, PineParseError> {
        let mut left = self.parse_unary()?;
        loop {
            self.skip_comments();
            let op = match self.current_kind() {
                TokenKind::Star => BinaryOp::Multiply,
                TokenKind::Slash => BinaryOp::Divide,
                TokenKind::Percent => BinaryOp::Modulo,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = PineExpr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<PineExpr, PineParseError> {
        self.skip_trivia();
        match self.current_kind() {
            TokenKind::Minus => {
                self.advance();
                Ok(PineExpr::Unary {
                    op: UnaryOp::Negate,
                    expr: Box::new(self.parse_unary()?),
                })
            }
            TokenKind::Not => {
                self.advance();
                Ok(PineExpr::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(self.parse_unary()?),
                })
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<PineExpr, PineParseError> {
        let mut expr = self.parse_primary()?;
        loop {
            // Postfix brackets bind same-line only: an operand at end of
            // line is complete, and a `[` on the next line starts a new
            // statement (tuple literal), never an index into the operand.
            self.skip_comments();
            match self.current_kind() {
                TokenKind::LeftBracket => {
                    self.advance();
                    self.skip_trivia();
                    let index = self.parse_expression()?;
                    self.skip_trivia();
                    self.expect(&TokenKind::RightBracket)?;
                    if let PineExpr::Number(offset) = index {
                        expr = PineExpr::SeriesOffset {
                            series: Box::new(expr),
                            offset: offset as i32,
                        };
                    } else {
                        expr = PineExpr::ArrayAccess {
                            array: Box::new(expr),
                            index: Box::new(index),
                        };
                    }
                }
                TokenKind::Dot => {
                    self.advance();
                    self.skip_trivia();
                    let field = self.expect_identifier()?;
                    self.skip_trivia();
                    if self.current_kind() == &TokenKind::LeftParen {
                        self.advance();
                        let args = self.parse_call_args()?;
                        self.expect(&TokenKind::RightParen)?;
                        expr = match expr {
                            PineExpr::Variable(namespace) => PineExpr::MethodCall {
                                namespace,
                                method: field,
                                args,
                                type_args: Vec::new(),
                            },
                            receiver => PineExpr::InstanceCall {
                                receiver: Box::new(receiver),
                                method: field,
                                args,
                                type_args: Vec::new(),
                            },
                        };
                    } else if let PineExpr::Variable(name) = expr {
                        // Spaced dotted read resolves like its glued
                        // spelling (`a . b` reads as `a.b`).
                        expr = PineExpr::Variable(format!("{name}.{field}"));
                    } else {
                        expr = PineExpr::FieldRead {
                            receiver: Box::new(expr),
                            field,
                        };
                    }
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<PineExpr, PineParseError> {
        self.skip_trivia();
        match self.current_kind().clone() {
            TokenKind::Number(value) => {
                self.advance();
                Ok(PineExpr::Number(value))
            }
            TokenKind::String(value) => {
                self.advance();
                Ok(PineExpr::StringLiteral(value))
            }
            TokenKind::ColorHex(digits) => {
                self.advance();
                super::builtins_core::color_of_hex(&digits).map(PineExpr::ColorLiteral).ok_or_else(
                    || {
                        let token = self.current().clone();
                        PineParseError::UnexpectedToken {
                            expected: "color literal".to_owned(),
                            found: token.kind.clone(),
                            line: token.line,
                            column: token.column,
                        }
                    },
                )
            }
            TokenKind::True => {
                self.advance();
                Ok(PineExpr::Boolean(true))
            }
            TokenKind::False => {
                self.advance();
                Ok(PineExpr::Boolean(false))
            }
            TokenKind::Open => {
                self.advance();
                Ok(PineExpr::BuiltinVariable(BuiltinVar::Open))
            }
            TokenKind::High => {
                self.advance();
                Ok(PineExpr::BuiltinVariable(BuiltinVar::High))
            }
            TokenKind::Low => {
                self.advance();
                Ok(PineExpr::BuiltinVariable(BuiltinVar::Low))
            }
            TokenKind::Close => {
                self.advance();
                Ok(PineExpr::BuiltinVariable(BuiltinVar::Close))
            }
            TokenKind::Volume => {
                self.advance();
                Ok(PineExpr::BuiltinVariable(BuiltinVar::Volume))
            }
            TokenKind::Time => {
                self.advance();
                // `time` is both the series and Pine's `time(timeframe,
                // session[, timezone])` function: call parens bind same-line.
                self.skip_comments();
                if self.current_kind() == &TokenKind::LeftParen {
                    self.advance();
                    let args = self.parse_call_args()?;
                    self.expect(&TokenKind::RightParen)?;
                    Ok(PineExpr::FunctionCall {
                        name: "time".to_owned(),
                        args,
                        type_args: Vec::new(),
                    })
                } else {
                    Ok(PineExpr::BuiltinVariable(BuiltinVar::Time))
                }
            }
            TokenKind::Switch => Ok(PineExpr::Switch(Box::new(self.parse_switch()?))),
            TokenKind::Type => {
                // Contextual keyword as a variable (`switch type` over a
                // parameter named `type`); see `expect_name`.
                self.advance();
                Ok(PineExpr::Variable("type".to_owned()))
            }
            TokenKind::Identifier(name) => {
                self.advance();
                // Call parens bind same-line (a newline ends the operand).
                self.skip_comments();
                if self.current_kind() == &TokenKind::Less {
                    if let Some(call) = self.try_parse_generic_call(&name) {
                        return Ok(call);
                    }
                }
                if self.current_kind() == &TokenKind::LeftParen {
                    self.advance();
                    let args = self.parse_call_args()?;
                    self.expect(&TokenKind::RightParen)?;
                    if let Some(dot) = name.rfind('.') {
                        Ok(PineExpr::MethodCall {
                            namespace: name[..dot].to_owned(),
                            method: name[dot + 1..].to_owned(),
                            args,
                            type_args: Vec::new(),
                        })
                    } else {
                        Ok(PineExpr::FunctionCall {
                            name,
                            args,
                            type_args: Vec::new(),
                        })
                    }
                } else {
                    Ok(PineExpr::Variable(name))
                }
            }
            TokenKind::LeftParen => {
                self.advance();
                let expr = self.parse_expression()?;
                self.skip_trivia();
                self.expect(&TokenKind::RightParen)?;
                Ok(expr)
            }
            TokenKind::LeftBracket => {
                self.advance();
                let mut items = Vec::new();
                loop {
                    self.skip_trivia();
                    if self.current_kind() == &TokenKind::RightBracket {
                        break;
                    }
                    items.push(self.parse_expression()?);
                    self.skip_trivia();
                    if self.current_kind() == &TokenKind::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.skip_trivia();
                self.expect(&TokenKind::RightBracket)?;
                Ok(PineExpr::ArrayLiteral(items))
            }
            _ => {
                let token = self.current().clone();
                Err(PineParseError::UnexpectedToken {
                    expected: "expression".to_owned(),
                    found: token.kind.clone(),
                    line: token.line,
                    column: token.column,
                })
            }
        }
    }

    /// Attempt `name<T, ...>(args)` (generic call, e.g.
    /// `map.new<int, int>()`); returns `None` without consuming unless the
    /// full shape is present, so `a < b` comparisons never misparse.
    fn try_parse_generic_call(&mut self, name: &str) -> Option<PineExpr> {
        let checkpoint = self.position;
        let call = self.parse_generic_call_tail(name);
        if call.is_none() {
            self.position = checkpoint;
        }
        call
    }

    /// Consume `<T, ...>(args)` after a call name; `None` (consuming) when
    /// the shape is absent — the caller rewinds.
    fn parse_generic_call_tail(&mut self, name: &str) -> Option<PineExpr> {
        self.advance();
        let mut type_args = Vec::new();
        loop {
            self.skip_trivia();
            let arg = self.parse_type();
            if arg.name.is_empty() {
                return None;
            }
            type_args.push(arg);
            self.skip_trivia();
            if self.current_kind() == &TokenKind::Comma {
                self.advance();
            } else {
                break;
            }
        }
        if self.current_kind() != &TokenKind::Greater {
            return None;
        }
        self.advance();
        self.skip_comments();
        if self.current_kind() != &TokenKind::LeftParen {
            return None;
        }
        self.advance();
        let args = self.parse_call_args().ok()?;
        self.skip_trivia();
        if self.current_kind() != &TokenKind::RightParen {
            return None;
        }
        self.advance();
        if let Some(dot) = name.rfind('.') {
            Some(PineExpr::MethodCall {
                namespace: name[..dot].to_owned(),
                method: name[dot + 1..].to_owned(),
                args,
                type_args,
            })
        } else {
            Some(PineExpr::FunctionCall {
                name: name.to_owned(),
                args,
                type_args,
            })
        }
    }

    /// Parse `(`-style arguments with `name =` prefixes preserved.
    fn parse_call_args(&mut self) -> Result<Vec<CallArg>, PineParseError> {
        let mut args = Vec::new();
        loop {
            self.skip_trivia();
            if self.current_kind() == &TokenKind::RightParen {
                break;
            }
            let checkpoint = self.position;
            let name = match self.current_kind() {
                TokenKind::Identifier(id) => {
                    let id = id.clone();
                    self.advance();
                    self.skip_trivia();
                    if self.current_kind() == &TokenKind::Equal {
                        self.advance();
                        Some(id)
                    } else {
                        self.position = checkpoint;
                        None
                    }
                }
                _ => None,
            };
            args.push(CallArg {
                name,
                value: self.parse_expression()?,
            });
            self.skip_trivia();
            if self.current_kind() == &TokenKind::Comma {
                self.advance();
            } else {
                break;
            }
        }
        Ok(args)
    }

    fn skip_trivia(&mut self) {
        while matches!(
            self.current_kind(),
            TokenKind::Newline | TokenKind::Comment(_)
        ) {
            self.advance();
        }
    }

    /// Skip comments but never newlines: for decision points where a line
    /// break ends the construct.
    fn skip_comments(&mut self) {
        while matches!(self.current_kind(), TokenKind::Comment(_)) {
            self.advance();
        }
    }

    fn expect(&mut self, expected: &TokenKind) -> Result<(), PineParseError> {
        let current = self.current().clone();
        if std::mem::discriminant(&current.kind) == std::mem::discriminant(expected) {
            self.advance();
            Ok(())
        } else {
            Err(PineParseError::UnexpectedToken {
                expected: format!("{expected:?}"),
                found: current.kind.clone(),
                line: current.line,
                column: current.column,
            })
        }
    }

    fn expect_identifier(&mut self) -> Result<String, PineParseError> {
        let current = self.current().clone();
        match &current.kind {
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            _ => Err(PineParseError::UnexpectedToken {
                expected: "identifier".to_owned(),
                found: current.kind.clone(),
                line: current.line,
                column: current.column,
            }),
        }
    }

    /// Expect a name where Pine allows the contextual keyword `type`
    /// (`PatternTypes type` parameters and their uses). Every other v6
    /// keyword stays reserved in name position.
    fn expect_name(&mut self) -> Result<String, PineParseError> {
        let current = self.current().clone();
        match &current.kind {
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            TokenKind::Type => {
                self.advance();
                Ok("type".to_owned())
            }
            _ => Err(PineParseError::UnexpectedToken {
                expected: "name".to_owned(),
                found: current.kind.clone(),
                line: current.line,
                column: current.column,
            }),
        }
    }

    fn current(&self) -> &PineToken {
        &self.tokens[self.position]
    }

    fn current_kind(&self) -> &TokenKind {
        &self.current().kind
    }

    fn peek_kind(&self) -> Option<&TokenKind> {
        self.tokens.get(self.position + 1).map(|token| &token.kind)
    }

    fn advance(&mut self) {
        if !self.is_at_end() {
            self.position += 1;
        }
    }

    fn is_at_end(&self) -> bool {
        self.current_kind() == &TokenKind::Eof
    }
}

#[cfg(test)]
#[path = "../../tests/pine_syntax.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/pine_closure_parse.rs"]
mod closure_parse_tests;
