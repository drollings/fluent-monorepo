//! Pine program display: canonical source rendering for one parsed program.
//!
//! [`display_program`] turns a [`PineProgram`] back into source text that
//! re-parses to a structurally equal program. It normalizes layout (comments
//! and blank lines vanish, blocks re-indent to four spaces, composite
//! expressions gain clarifying parentheses), so round-trip means structural
//! equality of the two programs, never byte equality of text.
//!
//! One rule carries most of the weight: a `switch` inside an expression is
//! always wrapped in parentheses whose closing paren sits alone on its own
//! line at the enclosing indent. The dedent ends the switch exactly where
//! the outer construct (call argument, array item, ternary branch, comma
//! list, condition) resumes, so single-line joining stays sound everywhere.
//!
//! One documented normalization: `#rrggbbaa` literals scale their alpha byte
//! into the `0..=100` transparency lane, which cannot invert exactly, so
//! display emits the opaque `#rrggbb` channels. Six-digit forms (the only
//! ones in the shipped files) round-trip exactly.

use super::syntax::{
    AssignItem, AssignOp, CallArg, EnumMember, FieldDecl, FieldPersistence, MultiDecl, Param,
    PineExpr, PineProgram, PineStmt, PineType, SwitchArm, SwitchStmt, UnaryOp,
};

/// Render `program` as canonical Pine source: one top-level statement per
/// line, four-space indents, trailing newline when non-empty.
#[must_use]
/// Render `program` as canonical Pine source: one top-level statement per
/// line, four-space indents, trailing newline when non-empty.
pub fn display_program(program: &PineProgram) -> String {
    let mut renderer = Renderer::new();
    renderer.program(program);
    renderer.finish()
}

struct Renderer {
    out: String,
}

impl Renderer {
    fn new() -> Self {
        Self { out: String::new() }
    }

    fn finish(self) -> String {
        self.out
    }

    fn indent(&mut self, level: usize) {
        for _ in 0..level {
            self.out.push_str("    ");
        }
    }

    fn line(&mut self, level: usize, text: &str) {
        self.indent(level);
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn program(&mut self, program: &PineProgram) {
        for stmt in &program.statements {
            self.stmt(0, stmt);
        }
    }

    fn body(&mut self, level: usize, stmts: &[PineStmt]) {
        for stmt in stmts {
            self.stmt(level, stmt);
        }
    }

    /// A statement that owns no block reads as one line (headers, assignments,
    /// declarations, simple expressions). Anything else takes the indented
    /// block form, so a same-line compound can never swallow its sisters.
    fn is_simple(stmt: &PineStmt) -> bool {
        matches!(
            stmt,
            PineStmt::Import(_)
                | PineStmt::LibraryDecl { .. }
                | PineStmt::IndicatorDecl { .. }
                | PineStmt::ConstDecl { .. }
                | PineStmt::VarDeclaration { .. }
                | PineStmt::Declaration { .. }
                | PineStmt::MultiDeclaration { .. }
                | PineStmt::Assignment { .. }
                | PineStmt::MultiAssign { .. }
                | PineStmt::Reassign { .. }
                | PineStmt::DestructureAssign { .. }
                | PineStmt::Expression(_)
        )
    }

    #[allow(clippy::too_many_lines)]
    fn stmt(&mut self, level: usize, stmt: &PineStmt) {
        match stmt {
            PineStmt::Import(import) => {
                self.line(
                    level,
                    &format!(
                        "import {}/{}/{} as {}",
                        import.library.publisher,
                        import.library.name,
                        import.library.version,
                        import.alias
                    ),
                );
            }
            PineStmt::LibraryDecl { name } => {
                self.line(level, &format!("library('{}')", escape_single(name)));
            }
            PineStmt::IndicatorDecl { args } => {
                self.line(level, &format!("indicator{}", self.call_args(level, args)));
            }
            PineStmt::ExportDecl(inner) => match inner.as_ref() {
                PineStmt::FuncDecl { name, params, body } => {
                    self.line(
                        level,
                        &format!("export {} =>", self.func_head(name, params)),
                    );
                    self.body(level + 1, body);
                }
                PineStmt::MethodDecl { name, params, body } => {
                    self.line(
                        level,
                        &format!("export {} =>", self.method_head(name, params)),
                    );
                    self.body(level + 1, body);
                }
                PineStmt::EnumDecl { name, members } => {
                    self.line(level, &format!("export enum {name}"));
                    self.enum_members(level + 1, members);
                }
                PineStmt::TypeDecl { name, fields } => {
                    self.line(level, &format!("export type {name}"));
                    self.type_fields(level + 1, fields);
                }
                _ => {
                    let mut inner_text = Renderer::new();
                    inner_text.stmt(0, inner);
                    let rendered = inner_text.finish();
                    self.line(level, &format!("export {}", rendered.trim_end()));
                }
            },
            PineStmt::FuncDecl { name, params, body } => {
                self.line(level, &format!("{} =>", self.func_head(name, params)));
                self.body(level + 1, body);
            }
            PineStmt::MethodDecl { name, params, body } => {
                self.line(level, &format!("{} =>", self.method_head(name, params)));
                self.body(level + 1, body);
            }
            PineStmt::ConstDecl {
                type_name,
                name,
                value,
            } => {
                self.line(
                    level,
                    &format!(
                        "const {} {name} = {}",
                        display_type(type_name),
                        self.expr(level, value)
                    ),
                );
            }
            PineStmt::EnumDecl { name, members } => {
                self.line(level, &format!("enum {name}"));
                self.enum_members(level + 1, members);
            }
            PineStmt::TypeDecl { name, fields } => {
                self.line(level, &format!("type {name}"));
                self.type_fields(level + 1, fields);
            }
            PineStmt::VarDeclaration {
                name,
                type_name,
                value,
                is_varip,
                ..
            } => {
                let keyword = if *is_varip { "varip" } else { "var" };
                let typed = type_name.as_ref().map_or_else(String::new, |type_name| {
                    format!("{} ", display_type(type_name))
                });
                self.line(
                    level,
                    &format!("{keyword} {typed}{name} = {}", self.expr(level, value)),
                );
            }
            PineStmt::Declaration {
                type_name,
                name,
                value,
            } => {
                self.line(
                    level,
                    &format!(
                        "{} {name} = {}",
                        display_type(type_name),
                        self.expr(level, value)
                    ),
                );
            }
            PineStmt::MultiDeclaration { decls } => {
                let rendered: Vec<String> = decls
                    .iter()
                    .map(|decl| self.multi_decl(level, decl))
                    .collect();
                self.line(level, &rendered.join(", "));
            }
            PineStmt::Assignment { name, value } => {
                self.line(level, &format!("{name} = {}", self.expr(level, value)));
            }
            PineStmt::MultiAssign { items } => {
                let rendered: Vec<String> = items
                    .iter()
                    .map(|item| self.assign_item(level, item))
                    .collect();
                self.line(level, &rendered.join(", "));
            }
            PineStmt::Reassign { name, value } => {
                self.line(level, &format!("{name} := {}", self.expr(level, value)));
            }
            PineStmt::DestructureAssign { targets, value } => {
                self.line(
                    level,
                    &format!("[{}] = {}", targets.join(", "), self.expr(level, value)),
                );
            }
            PineStmt::Expression(expr) => {
                self.line(level, &self.expr(level, expr));
            }
            PineStmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.if_head(level, "if", condition);
                self.body(level + 1, then_branch);
                self.if_else(level, else_branch.as_ref());
            }
            PineStmt::For {
                variable,
                start,
                end,
                step,
                body,
            } => {
                let header = match step {
                    Some(step) => format!(
                        "for {variable} = {} to {} by {}",
                        self.expr(level, start),
                        self.expr(level, end),
                        self.expr(level, step)
                    ),
                    None => format!(
                        "for {variable} = {} to {}",
                        self.expr(level, start),
                        self.expr(level, end)
                    ),
                };
                self.line(level, &header);
                self.body(level + 1, body);
            }
            PineStmt::ForIn {
                targets,
                iterable,
                body,
            } => {
                let bound = if targets.len() == 1 {
                    targets[0].clone()
                } else {
                    format!("[{}]", targets.join(", "))
                };
                self.line(
                    level,
                    &format!("for {bound} in {}", self.expr(level, iterable)),
                );
                self.body(level + 1, body);
            }
            PineStmt::While { condition, body } => {
                self.line(level, &format!("while {}", self.expr(level, condition)));
                self.body(level + 1, body);
            }
            PineStmt::Break => {
                self.line(level, "break");
            }
            PineStmt::Switch(switch) => {
                self.switch(level, switch);
            }
        }
    }

    fn if_head(&mut self, level: usize, keyword: &str, condition: &PineExpr) {
        self.line(level, &format!("{keyword} {}", self.expr(level, condition)));
    }

    /// A lone nested `if` re-chains onto the `else` line; both shapes re-parse
    /// to the same node, so the chain survives the trip either way.
    fn if_else(&mut self, level: usize, else_branch: Option<&Vec<PineStmt>>) {
        if let Some(else_stmts) = else_branch {
            if let [PineStmt::If {
                condition,
                then_branch,
                else_branch,
            }] = else_stmts.as_slice()
            {
                self.if_head(level, "else if", condition);
                self.body(level + 1, then_branch);
                self.if_else(level, else_branch.as_ref());
            } else {
                self.line(level, "else");
                self.body(level + 1, else_stmts);
            }
        }
    }

    fn switch(&mut self, level: usize, switch: &SwitchStmt) {
        match &switch.scrutinee {
            Some(scrutinee) => {
                self.line(level, &format!("switch {}", self.expr(level, scrutinee)));
            }
            None => self.line(level, "switch"),
        }
        for arm in &switch.arms {
            self.arm(level + 1, arm);
        }
    }

    fn arm(&mut self, level: usize, arm: &SwitchArm) {
        // No space before `=>` on the default arm: the arrow's own column
        // is the arm's alignment column.
        let head = arm.pattern.as_ref().map_or_else(
            || "=>".to_owned(),
            |pattern| format!("{} =>", self.expr(level, pattern)),
        );
        if arm.body.len() == 1 && Self::is_simple(&arm.body[0]) {
            let mut one_line = Renderer::new();
            one_line.stmt(0, &arm.body[0]);
            let rendered = one_line.finish();
            self.line(level, &format!("{head} {}", rendered.trim_end()));
        } else {
            self.line(level, &head);
            self.body(level + 1, &arm.body);
        }
    }

    fn func_head(&self, name: &str, params: &[Param]) -> String {
        format!("{name}({})", self.params(params))
    }

    fn method_head(&self, name: &str, params: &[Param]) -> String {
        format!("method {name}({})", self.params(params))
    }

    fn params(&self, params: &[Param]) -> String {
        params
            .iter()
            .map(|param| {
                let qualifier = param
                    .qualifier
                    .as_ref()
                    .map_or_else(String::new, |qualifier| format!("{qualifier} "));
                let typed = param
                    .type_name
                    .as_ref()
                    .map_or_else(String::new, |type_name| {
                        format!("{} ", display_type(type_name))
                    });
                let default = param.default.as_ref().map_or_else(String::new, |default| {
                    format!(" = {}", self.expr(0, default))
                });
                format!("{qualifier}{typed}{}{default}", param.name)
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn enum_members(&mut self, level: usize, members: &[EnumMember]) {
        for member in members {
            self.line(
                level,
                &format!("{} = {}", member.name, self.expr(level, &member.value)),
            );
        }
    }

    fn type_fields(&mut self, level: usize, fields: &[FieldDecl]) {
        for field in fields {
            let persistence = field
                .persistence
                .map_or("", |persistence| match persistence {
                    FieldPersistence::Var => "var ",
                    FieldPersistence::Varip => "varip ",
                });
            let type_name = display_type(&field.type_name);
            let head = if type_name.is_empty() {
                format!("{persistence}{}", field.name)
            } else {
                format!("{persistence}{type_name} {}", field.name)
            };
            match &field.default {
                Some(default) => {
                    self.line(level, &format!("{head} = {}", self.expr(level, default)));
                }
                None => self.line(level, &head),
            }
        }
    }

    fn multi_decl(&self, level: usize, decl: &MultiDecl) -> String {
        let typed = decl
            .type_name
            .as_ref()
            .map_or_else(String::new, |type_name| {
                format!("{} ", display_type(type_name))
            });
        match &decl.value {
            Some(value) => format!("{typed}{} = {}", decl.name, self.expr(level, value)),
            None => format!("{typed}{}", decl.name),
        }
    }

    fn assign_item(&self, level: usize, item: &AssignItem) -> String {
        let op = match item.op {
            AssignOp::Assign => "=",
            AssignOp::Reassign => ":=",
        };
        format!("{} {op} {}", item.name, self.expr(level, &item.value))
    }

    fn call_args(&self, level: usize, args: &[CallArg]) -> String {
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| match &arg.name {
                Some(name) => format!("{name} = {}", self.expr(level, &arg.value)),
                None => self.expr(level, &arg.value),
            })
            .collect();
        format!("({})", rendered.join(", "))
    }

    fn expr(&self, level: usize, expr: &PineExpr) -> String {
        match expr {
            PineExpr::Number(value) => format!("{value}"),
            PineExpr::StringLiteral(value) => format!("\"{}\"", escape_double(value)),
            PineExpr::Boolean(value) => value.to_string(),
            PineExpr::Variable(name) => name.clone(),
            PineExpr::BuiltinVariable(variable) => variable.to_string(),
            PineExpr::Binary { left, op, right } => {
                format!(
                    "({} {op} {})",
                    self.expr(level, left),
                    self.expr(level, right)
                )
            }
            PineExpr::Unary { op, expr } => {
                let mark = match op {
                    UnaryOp::Negate => "-",
                    UnaryOp::Not => "not ",
                };
                format!("({mark}{})", self.expr(level, expr))
            }
            PineExpr::FunctionCall {
                name,
                args,
                type_args,
                ..
            } => {
                format!(
                    "{name}{}{}",
                    display_type_args(type_args),
                    self.call_args(level, args)
                )
            }
            PineExpr::MethodCall {
                namespace,
                method,
                args,
                type_args,
                ..
            } => {
                format!(
                    "{namespace}.{method}{}{}",
                    display_type_args(type_args),
                    self.call_args(level, args)
                )
            }
            PineExpr::InstanceCall {
                receiver,
                method,
                args,
                type_args,
                ..
            } => {
                format!(
                    "{}.{method}{}{}",
                    self.expr(level, receiver),
                    display_type_args(type_args),
                    self.call_args(level, args)
                )
            }
            PineExpr::FieldRead { receiver, field } => {
                format!("{}.{field}", self.expr(level, receiver))
            }
            PineExpr::Ternary {
                condition,
                then_expr,
                else_expr,
            } => format!(
                "({} ? {} : {})",
                self.expr(level, condition),
                self.expr(level, then_expr),
                self.expr(level, else_expr)
            ),
            PineExpr::ArrayAccess { array, index } => {
                format!("{}[{}]", self.expr(level, array), self.expr(level, index))
            }
            PineExpr::SeriesOffset { series, offset } => {
                format!("{}[{offset}]", self.expr(level, series))
            }
            PineExpr::ArrayLiteral(items) => {
                let rendered: Vec<String> =
                    items.iter().map(|item| self.expr(level, item)).collect();
                format!("[{}]", rendered.join(", "))
            }
            PineExpr::ColorLiteral(packed) => {
                // The transparency lane cannot invert exactly, so only the
                // opaque channels survive the trip (documented above).
                format!(
                    "#{:02x}{:02x}{:02x}",
                    (packed >> 24) & 0xFF,
                    (packed >> 16) & 0xFF,
                    (packed >> 8) & 0xFF
                )
            }
            PineExpr::Switch(switch) => self.parenthesized_switch(level, switch),
        }
    }

    /// A switch inside an expression: the `switch` line opens after the
    /// current position, the arms indent one past the enclosing level, and
    /// the closing paren sits alone at the enclosing indent so the first
    /// dedent ends the switch exactly where the outer construct resumes.
    fn parenthesized_switch(&self, level: usize, switch: &SwitchStmt) -> String {
        let mut blocked = match &switch.scrutinee {
            Some(scrutinee) => format!("(switch {}", self.expr(level + 1, scrutinee)),
            None => "(switch".to_owned(),
        };
        blocked.push('\n');
        let mut nested = Renderer::new();
        for arm in &switch.arms {
            nested.arm(level + 1, arm);
        }
        blocked.push_str(&nested.finish());
        for _ in 0..level {
            blocked.push_str("    ");
        }
        blocked.push(')');
        blocked
    }
}

fn display_type(type_name: &PineType) -> String {
    if type_name.args.is_empty() {
        type_name.name.clone()
    } else {
        let args: Vec<String> = type_name.args.iter().map(display_type).collect();
        format!("{}<{}>", type_name.name, args.join(", "))
    }
}

fn display_type_args(type_args: &[PineType]) -> String {
    if type_args.is_empty() {
        String::new()
    } else {
        let args: Vec<String> = type_args.iter().map(display_type).collect();
        format!("<{}>", args.join(", "))
    }
}

fn escape_double(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(c),
        }
    }
    escaped
}

fn escape_single(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '\'' => escaped.push_str("\\'"),
            _ => escaped.push(c),
        }
    }
    escaped
}