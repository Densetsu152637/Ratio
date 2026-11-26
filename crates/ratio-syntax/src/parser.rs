//! The parser: chapter 02's grammar over the token reader's stream.
//!
//! Recursive descent with one token of lookahead (two for a named argument or record field), no
//! backtracking, so parsing is linear in the number of tokens. Operator chains (`a + b + c`,
//! `x.f().g`, `T??`) are read in loops, and recursion happens only where the tree nests; every
//! level of nesting is counted against the limit of chapter 07 (`LexOptions::max_nesting`), so the
//! parser's stack and the depth of the tree it builds are bounded whatever the input. Past the
//! limit it reports `P0002` and stops.
//!
//! What a `{` opens is decided once, by the token reader, with rule 1 of chapter 02's
//! disambiguation rules; the reader records it on the token ([`BraceKind`]) and the parser takes
//! that decision, so semicolon insertion and parsing never disagree. Where the grammar needs
//! another kind (a block after `if x`, a record after `with`), the parser reports `P0003`.
//!
//! Errors never panic. The parser reports one diagnostic per problem, puts an `Error` node where it
//! could not read an expression, type or pattern, and resynchronises at the next `;` (a statement
//! or item), `,` (a list) or closing delimiter.

use crate::ast::{
    Arg, Arm, AssignOp, Attribute, BinaryOp, Block, Catch, ClosureParam, Condition, ConstDecl,
    Doc, Else, EnumDecl, Expr, ExprKind, FieldInit, FieldPattern, FieldType, FnDecl, HarnessDecl,
    Ident, Import, ImportPath, Item, ItemKind, ListItem, ListPattern, Lit, Meta, Module, Param,
    Pattern, PatternKind, PromptBody, PromptDecl, RecordLit, RecordPattern, Section, SectionKind,
    SourceName, StateDecl, Stmt, StmtKind, StrPart, StringLit, TestDecl, Type, TypeDecl, TypeKind,
    UnaryOp, Variant,
};
use crate::diag::{Code, Diagnostic};
use crate::lexer::{LexOptions, MAX_DIAGNOSTICS, MAX_NESTING, lex_with};
use crate::token::{BraceKind, Contextual, Keyword, Span, Token, TokenKind};

/// The result of parsing a source: the module (always present, with `Error` nodes where parsing
/// failed) and every diagnostic of the token reader and the parser, in source order.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub module: Module,
    pub diagnostics: Vec<Diagnostic>,
}

impl Parsed {
    /// Whether the source was read and parsed without a diagnostic.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Parses `source` with the default limits.
#[must_use]
pub fn parse(source: &str) -> Parsed {
    parse_with(source, &LexOptions::default())
}

/// Parses `source` with the given limits (raised no further than the maximums of chapter 07).
///
/// The token reader runs first. When it stops early (a source over the size limit, brackets
/// nested past the limit, too many errors) the module is left empty and only its diagnostics are
/// returned; otherwise the parser reads whatever tokens there are and recovers from errors.
#[must_use]
pub fn parse_with(source: &str, options: &LexOptions) -> Parsed {
    let lexed = lex_with(source, options);
    let max_depth = options.max_nesting.min(MAX_NESTING);
    let max_diagnostics = options.max_diagnostics.min(MAX_DIAGNOSTICS);
    let whole = whole_span(source, lexed.tokens.last().map(|t| t.span));
    let stopped = lexed.diagnostics.iter().any(|d| {
        matches!(
            d.code,
            Code::SourceTooLarge | Code::NestingTooDeep | Code::TooManyErrors
        )
    });
    if stopped {
        return Parsed {
            module: Module {
                meta: None,
                items: Vec::new(),
                span: whole,
            },
            diagnostics: lexed.diagnostics,
        };
    }
    let budget = max_diagnostics.saturating_sub(lexed.diagnostics.len());
    let mut parser = Parser::new(lexed.tokens, max_depth, budget);
    let mut module = parser.module();
    module.span = whole;
    let mut diagnostics = lexed.diagnostics;
    diagnostics.extend(parser.diags);
    // Source order; a "too many errors" marker stays last, where reporting stopped.
    diagnostics.sort_by_key(|d| (d.code == Code::ParseTooManyErrors, d.span.start));
    Parsed {
        module,
        diagnostics,
    }
}

fn whole_span(source: &str, eof: Option<Span>) -> Span {
    let end = eof.unwrap_or(Span {
        start: source.len(),
        end: source.len(),
        line: 1,
        col: 1,
        end_line: 1,
        end_col: 1,
    });
    Span {
        start: 0,
        end: end.end,
        line: 1,
        col: 1,
        end_line: end.end_line,
        end_col: end.end_col,
    }
}

/// A span from the start of `a` to the end of `b` (just `a` if `b` ends before `a` starts).
fn join(a: Span, b: Span) -> Span {
    if b.end < a.start {
        return a;
    }
    Span {
        start: a.start,
        end: b.end,
        line: a.line,
        col: a.col,
        end_line: b.end_line,
        end_col: b.end_col,
    }
}

fn point(at: Span) -> Span {
    Span {
        start: at.start,
        end: at.start,
        line: at.line,
        col: at.col,
        end_line: at.line,
        end_col: at.col,
    }
}

fn describe(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Ident(name) => format!("`{name}`"),
        TokenKind::Keyword(k) => format!("`{}`", k.as_str()),
        TokenKind::Contextual(c) => format!("`{}`", c.as_str()),
        TokenKind::Int(_)
        | TokenKind::Quantity(_)
        | TokenKind::Float(_)
        | TokenKind::Duration(_)
        | TokenKind::Money(_) => "a number".to_owned(),
        TokenKind::StringStart { .. } | TokenKind::RawString { .. } => "a string".to_owned(),
        TokenKind::StringText(_) | TokenKind::StringEnd => "the end of the string".to_owned(),
        TokenKind::InterpStart => "an interpolation".to_owned(),
        TokenKind::FormatSpec(_) => "a format spec".to_owned(),
        TokenKind::InterpEnd => "the end of the interpolation".to_owned(),
        TokenKind::DocComment(_) => "a documentation comment".to_owned(),
        TokenKind::LBrace(_) => "`{`".to_owned(),
        TokenKind::Semi { inserted: true } => "the end of the line".to_owned(),
        TokenKind::Eof => "the end of the file".to_owned(),
        other => format!("`{}`", other.punctuation().unwrap_or("?")),
    }
}

fn brace_name(kind: BraceKind) -> &'static str {
    match kind {
        BraceKind::Block => "a block",
        BraceKind::Record => "a record",
        BraceKind::PromptBody => "a prompt body",
        BraceKind::EnumBody => "an enum body",
        BraceKind::MatchBody => "a list of arms",
    }
}

fn is_upper_camel(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
}

/// `a`, `a.b`, `a.B` as a list of names, if the expression is only that.
fn as_path(mut expr: &Expr) -> Option<Vec<Ident>> {
    let mut names = Vec::new();
    loop {
        match &expr.kind {
            ExprKind::Name(name) => {
                names.push(name.clone());
                names.reverse();
                return Some(names);
            }
            ExprKind::Member {
                object,
                name,
                optional: false,
            } => {
                names.push(name.clone());
                expr = object;
            }
            _ => return None,
        }
    }
}

/// A place: a name followed by `.name` and `[index]` steps.
fn is_place(mut expr: &Expr) -> bool {
    loop {
        match &expr.kind {
            ExprKind::Name(_) => return true,
            ExprKind::Member {
                object,
                optional: false,
                ..
            }
            | ExprKind::Index { object, .. } => expr = object,
            _ => return false,
        }
    }
}

fn compare_op(kind: &TokenKind) -> Option<BinaryOp> {
    Some(match kind {
        TokenKind::EqEq => BinaryOp::Eq,
        TokenKind::NotEq => BinaryOp::Ne,
        TokenKind::Lt => BinaryOp::Lt,
        TokenKind::Le => BinaryOp::Le,
        TokenKind::Gt => BinaryOp::Gt,
        TokenKind::Ge => BinaryOp::Ge,
        TokenKind::Keyword(Keyword::In) => BinaryOp::In,
        TokenKind::Keyword(Keyword::Is) => BinaryOp::Is,
        _ => return None,
    })
}

fn range_op(kind: &TokenKind) -> Option<BinaryOp> {
    match kind {
        TokenKind::DotDot => Some(BinaryOp::Range),
        TokenKind::DotDotEq => Some(BinaryOp::RangeInclusive),
        _ => None,
    }
}

fn add_op(kind: &TokenKind) -> Option<BinaryOp> {
    match kind {
        TokenKind::Plus => Some(BinaryOp::Add),
        TokenKind::Minus => Some(BinaryOp::Sub),
        _ => None,
    }
}

fn mul_op(kind: &TokenKind) -> Option<BinaryOp> {
    match kind {
        TokenKind::Star => Some(BinaryOp::Mul),
        TokenKind::Slash => Some(BinaryOp::Div),
        TokenKind::Percent => Some(BinaryOp::Rem),
        _ => None,
    }
}

fn or_op(kind: &TokenKind) -> Option<BinaryOp> {
    matches!(kind, TokenKind::Keyword(Keyword::Or)).then_some(BinaryOp::Or)
}

fn and_op(kind: &TokenKind) -> Option<BinaryOp> {
    matches!(kind, TokenKind::Keyword(Keyword::And)).then_some(BinaryOp::And)
}

fn assign_op(kind: &TokenKind) -> Option<AssignOp> {
    Some(match kind {
        TokenKind::Eq => AssignOp::Set,
        TokenKind::PlusEq => AssignOp::Add,
        TokenKind::MinusEq => AssignOp::Sub,
        TokenKind::StarEq => AssignOp::Mul,
        TokenKind::SlashEq => AssignOp::Div,
        _ => return None,
    })
}

/// Whether a token can begin an expression (decides whether `return` and `break` have a value).
fn starts_expr(kind: &TokenKind) -> bool {
    match kind {
        TokenKind::Ident(_)
        | TokenKind::Contextual(_)
        | TokenKind::Int(_)
        | TokenKind::Quantity(_)
        | TokenKind::Float(_)
        | TokenKind::Duration(_)
        | TokenKind::Money(_)
        | TokenKind::StringStart { .. }
        | TokenKind::RawString { .. }
        | TokenKind::LParen
        | TokenKind::LBracket
        | TokenKind::LBrace(_)
        | TokenKind::Minus
        | TokenKind::Pipe
        | TokenKind::PipePipe => true,
        TokenKind::Keyword(k) => matches!(
            k,
            Keyword::Not
                | Keyword::If
                | Keyword::Match
                | Keyword::For
                | Keyword::While
                | Keyword::Loop
                | Keyword::Repeat
                | Keyword::Try
                | Keyword::With
                | Keyword::Do
                | Keyword::Fn
                | Keyword::Prompt
                | Keyword::True
                | Keyword::False
                | Keyword::None
                | Keyword::Return
                | Keyword::Break
                | Keyword::Continue
                | Keyword::Raise
        ),
        _ => false,
    }
}

fn is_opener(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::LParen
            | TokenKind::LBracket
            | TokenKind::LBrace(_)
            | TokenKind::StringStart { .. }
            | TokenKind::InterpStart
    )
}

fn is_closer(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::RBrace
            | TokenKind::StringEnd
            | TokenKind::InterpEnd
    )
}

const ITEM_WORDS: &str =
    "an item (`fn`, `harness`, `prompt`, `type`, `enum`, `const`, `import`, `state` or `test`)";
const SECTION_WORDS: &str = "a prompt section (`system`, `instruct`, `task`, `data`, `image`, \
     `example`, `format`, `steps`, `context`, `note`, `include`, `if` or `for`)";

struct Parser {
    /// The tokens without documentation comments; the last is `Eof`.
    toks: Vec<Token>,
    /// The documentation comments written directly before each token of `toks`.
    docs: Vec<Vec<Doc>>,
    pos: usize,
    /// The span of the last token consumed.
    prev: Span,
    diags: Vec<Diagnostic>,
    budget: usize,
    depth: usize,
    max_depth: usize,
    /// Set by a nesting error or too many errors: the parser then sees only `Eof`.
    halted: bool,
    /// The token index of the last error, so one bad token gives one diagnostic.
    last_error: Option<usize>,
    eof: TokenKind,
}

impl Parser {
    fn new(tokens: Vec<Token>, max_depth: usize, budget: usize) -> Self {
        let mut toks = Vec::with_capacity(tokens.len());
        let mut docs: Vec<Vec<Doc>> = Vec::with_capacity(tokens.len());
        let mut pending: Vec<Doc> = Vec::new();
        for token in tokens {
            if let TokenKind::DocComment(text) = token.kind {
                pending.push(Doc {
                    text,
                    span: token.span,
                });
            } else {
                toks.push(token);
                docs.push(std::mem::take(&mut pending));
            }
        }
        let start = toks.first().map_or(
            Span {
                start: 0,
                end: 0,
                line: 1,
                col: 1,
                end_line: 1,
                end_col: 1,
            },
            |t| point(t.span),
        );
        Parser {
            toks,
            docs,
            pos: 0,
            prev: start,
            diags: Vec::new(),
            budget,
            depth: 0,
            max_depth,
            halted: false,
            last_error: None,
            eof: TokenKind::Eof,
        }
    }

    // ---- the cursor ----

    fn kind(&self) -> &TokenKind {
        self.nth(0)
    }

    fn nth(&self, n: usize) -> &TokenKind {
        if self.halted {
            return &self.eof;
        }
        let index = (self.pos + n).min(self.toks.len().saturating_sub(1));
        self.toks.get(index).map_or(&self.eof, |t| &t.kind)
    }

    fn nth_span(&self, n: usize) -> Span {
        let index = (self.pos + n).min(self.toks.len().saturating_sub(1));
        match self.toks.get(index) {
            Some(t) if !self.halted => t.span,
            _ => point(self.prev),
        }
    }

    fn span(&self) -> Span {
        self.nth_span(0)
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.kind() == kind
    }

    fn at_kw(&self, keyword: Keyword) -> bool {
        matches!(self.kind(), TokenKind::Keyword(k) if *k == keyword)
    }

    fn at_eof(&self) -> bool {
        matches!(self.kind(), TokenKind::Eof)
    }

    fn at_semi(&self) -> bool {
        matches!(self.kind(), TokenKind::Semi { .. })
    }

    /// The end of the enclosing string, interpolation or file: no recovery consumes these.
    fn at_hard_stop(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Eof | TokenKind::InterpEnd | TokenKind::StringEnd
        )
    }

    fn bump(&mut self) {
        if self.halted || self.at_eof() {
            return;
        }
        if let Some(t) = self.toks.get(self.pos) {
            self.prev = t.span;
        }
        self.pos += 1;
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, keyword: Keyword) -> bool {
        if self.at_kw(keyword) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_semi(&mut self) -> bool {
        if self.at_semi() {
            self.bump();
            true
        } else {
            false
        }
    }

    /// The span from `start` to the end of the last token consumed.
    fn finish(&self, start: Span) -> Span {
        if self.prev.end < start.start || self.prev.start < start.start {
            point(start)
        } else {
            join(start, self.prev)
        }
    }

    fn take_docs(&mut self) -> Vec<Doc> {
        if self.halted {
            return Vec::new();
        }
        self.docs
            .get_mut(self.pos)
            .map(std::mem::take)
            .unwrap_or_default()
    }

    // ---- diagnostics and limits ----

    fn report(&mut self, code: Code, message: impl Into<String>, span: Span) {
        if self.halted {
            return;
        }
        if self.budget == 0 {
            self.diags.push(Diagnostic {
                code: Code::ParseTooManyErrors,
                message: "too many errors; the parser stops here".to_owned(),
                span,
            });
            self.halted = true;
            return;
        }
        self.budget -= 1;
        self.diags.push(Diagnostic {
            code,
            message: message.into(),
            span,
        });
    }

    /// Reports at the current token, once per token.
    fn error_here(&mut self, code: Code, message: impl Into<String>) {
        if self.halted || self.last_error == Some(self.pos) {
            return;
        }
        self.last_error = Some(self.pos);
        let span = self.span();
        self.report(code, message, span);
    }

    fn expected(&mut self, what: &str) {
        let found = describe(self.kind());
        self.error_here(Code::UnexpectedToken, format!("expected {what}, found {found}"));
    }

    fn expect(&mut self, kind: &TokenKind, what: &str) -> bool {
        if self.eat(kind) {
            true
        } else {
            self.expected(what);
            false
        }
    }

    /// One level deeper. Every call is paired with `leave`, even when it returns `false`.
    fn enter(&mut self) -> bool {
        self.depth += 1;
        if self.depth > self.max_depth {
            if !self.halted {
                let span = self.span();
                let message = format!(
                    "expressions, types and patterns nest deeper than {}",
                    self.max_depth
                );
                self.report(Code::ParseNestingTooDeep, message, span);
                self.halted = true;
            }
            return false;
        }
        true
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn leave_n(&mut self, n: usize) {
        self.depth = self.depth.saturating_sub(n);
    }

    // ---- recovery ----

    /// Skips tokens until `stop` holds at nesting depth 0, or a closing delimiter or the end of
    /// the enclosing string is reached at depth 0. Nothing it stops at is consumed.
    fn skip_until(&mut self, stop: impl Fn(&TokenKind) -> bool) {
        let mut depth = 0usize;
        loop {
            let kind = self.kind();
            if matches!(kind, TokenKind::Eof) {
                return;
            }
            if depth == 0 && (stop(kind) || is_closer(kind)) {
                return;
            }
            if is_opener(kind) {
                depth += 1;
            } else if is_closer(kind) {
                depth -= 1;
            }
            self.bump();
        }
    }

    /// After a bad statement: on to the next `;` (consumed) or the `}` that closes the block.
    fn recover_stmt(&mut self) {
        loop {
            self.skip_until(|k| matches!(k, TokenKind::Semi { .. }));
            if self.eat_semi() || self.at(&TokenKind::RBrace) || self.at_hard_stop() {
                return;
            }
            // A stray `)` or `]`.
            self.bump();
        }
    }

    /// After a bad item: on to the next `;` (consumed).
    fn recover_item(&mut self) {
        loop {
            self.skip_until(|k| matches!(k, TokenKind::Semi { .. }));
            if self.eat_semi() || self.at_eof() {
                return;
            }
            self.bump();
        }
    }

    /// Expects `closer`; when it is missing, reports and skips to it.
    fn close(&mut self, closer: &TokenKind, what: &str) {
        if self.eat(closer) {
            return;
        }
        self.expected(what);
        self.skip_until(|k| k == closer);
        self.eat(closer);
    }

    /// Elements separated by commas, up to `close` (not consumed).
    fn comma_list<T>(
        &mut self,
        close: &TokenKind,
        trailing: bool,
        what: &str,
        mut element: impl FnMut(&mut Self) -> Option<T>,
    ) -> Vec<T> {
        let mut out = Vec::new();
        loop {
            if self.at(close) || self.at_hard_stop() || is_closer(self.kind()) {
                break;
            }
            if let Some(value) = element(self) {
                out.push(value);
            }
            if self.at(&TokenKind::Comma) {
                let comma = self.span();
                self.bump();
                if !trailing && self.at(close) {
                    self.report(
                        Code::UnexpectedToken,
                        format!("{what} take no trailing `,`"),
                        comma,
                    );
                }
                continue;
            }
            if self.at(close) {
                break;
            }
            let closer = close.punctuation().unwrap_or("?");
            self.expected(&format!("`,` or `{closer}`"));
            self.skip_until(|k| k == close || matches!(k, TokenKind::Comma));
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        out
    }

    // ---- names ----

    fn ident_opt(&mut self) -> Option<Ident> {
        let name = match self.kind() {
            TokenKind::Ident(name) => name.clone(),
            TokenKind::Contextual(c) => c.as_str().to_owned(),
            _ => return None,
        };
        let span = self.span();
        self.bump();
        Some(Ident { name, span })
    }

    fn ident(&mut self, what: &str) -> Ident {
        if let Some(ident) = self.ident_opt() {
            return ident;
        }
        self.expected(what);
        Ident {
            name: String::new(),
            span: point(self.span()),
        }
    }

    fn at_ident(&self) -> bool {
        matches!(self.kind(), TokenKind::Ident(_) | TokenKind::Contextual(_))
    }

    /// `a.b.c`, starting with `first` if it was already read.
    fn name_path(&mut self, first: Option<Ident>, what: &str) -> Vec<Ident> {
        let mut path = vec![first.unwrap_or_else(|| self.ident(what))];
        while self.at(&TokenKind::Dot) && matches!(self.nth(1), TokenKind::Ident(_)) {
            self.bump();
            path.push(self.ident("a name"));
        }
        path
    }

    // ---- braces ----

    /// Consumes a `{` that must open `want`. A `{` on the line after a head that ended with an
    /// inserted `;` is reported once (`P0008`) and read anyway.
    fn open_brace(&mut self, want: BraceKind, what: &str) -> bool {
        if matches!(self.kind(), TokenKind::Semi { inserted: true })
            && matches!(self.nth(1), TokenKind::LBrace(_))
        {
            self.bump();
            self.error_here(
                Code::HeadSplitAcrossLines,
                format!(
                    "{what} must start on the line of its head: the line end before this `{{` \
                     ended the statement"
                ),
            );
            self.bump();
            return true;
        }
        match self.kind() {
            TokenKind::LBrace(kind) => {
                let kind = *kind;
                if kind != want {
                    self.error_here(
                        Code::WrongBrace,
                        format!(
                            "expected {what}, but this `{{` starts {} here (rule 1 of chapter 02)",
                            brace_name(kind)
                        ),
                    );
                }
                self.bump();
                true
            }
            _ => {
                self.expected(&format!("`{{` starting {what}"));
                false
            }
        }
    }

    // ---- items ----

    fn module(&mut self) -> Module {
        let start = self.span();
        let meta = if matches!(self.kind(), TokenKind::Contextual(Contextual::Meta)) {
            let meta_start = self.span();
            self.bump();
            let record = self.record_lit();
            let span = self.finish(meta_start);
            self.end_item();
            Some(Meta {
                fields: record.fields,
                span,
            })
        } else {
            None
        };
        let mut items = Vec::new();
        while !self.at_eof() {
            let before = self.pos;
            if let Some(item) = self.item() {
                items.push(item);
            }
            self.end_item();
            if self.pos == before {
                self.bump();
            }
        }
        Module {
            meta,
            items,
            span: self.finish(start),
        }
    }

    fn end_item(&mut self) {
        if self.eat_semi() {
            return;
        }
        self.expected("`;` or a line end after the item");
        self.recover_item();
    }

    fn item(&mut self) -> Option<Item> {
        let start = self.span();
        let mut docs = self.take_docs();
        let mut attrs = Vec::new();
        while self.at(&TokenKind::At) {
            attrs.push(self.attribute());
            docs.extend(self.take_docs());
        }
        let public = self.eat_kw(Keyword::Pub);
        if public {
            docs.extend(self.take_docs());
        }
        let kind = match self.kind() {
            TokenKind::Keyword(Keyword::Import) => ItemKind::Import(self.import()),
            TokenKind::Keyword(Keyword::Type) => ItemKind::Type(self.type_decl()),
            TokenKind::Keyword(Keyword::Enum) => ItemKind::Enum(self.enum_decl()),
            TokenKind::Keyword(Keyword::Const) => ItemKind::Const(self.const_decl()),
            TokenKind::Keyword(Keyword::Fn) => ItemKind::Fn(self.fn_decl()),
            TokenKind::Keyword(Keyword::Harness) => ItemKind::Harness(self.harness_decl()),
            TokenKind::Keyword(Keyword::Prompt) => ItemKind::Prompt(self.prompt_decl()),
            TokenKind::Contextual(Contextual::State) => ItemKind::State(self.state_decl()),
            TokenKind::Keyword(Keyword::Test) => {
                if public || !attrs.is_empty() {
                    self.report(
                        Code::MisplacedModifier,
                        "a test takes no attributes and no `pub`",
                        start,
                    );
                }
                ItemKind::Test(self.test_decl())
            }
            _ => {
                self.expected(ITEM_WORDS);
                return None;
            }
        };
        Some(Item {
            docs,
            attrs,
            public,
            kind,
            span: self.finish(start),
        })
    }

    fn attribute(&mut self) -> Attribute {
        let start = self.span();
        self.bump();
        let name = self.ident("an attribute name");
        let args = if self.at(&TokenKind::LParen) {
            Some(self.call_args())
        } else {
            None
        };
        Attribute {
            name,
            args,
            span: self.finish(start),
        }
    }

    fn import(&mut self) -> Import {
        self.bump();
        let path = if matches!(self.kind(), TokenKind::StringStart { .. }) {
            ImportPath::File(self.string_lit())
        } else {
            ImportPath::Source(self.source_name())
        };
        let alias = if self.eat_kw(Keyword::As) {
            Some(self.ident("a name after `as`"))
        } else {
            None
        };
        let names = if matches!(self.kind(), TokenKind::LBrace(_)) {
            self.open_brace(BraceKind::Record, "the imported names");
            let names = self.comma_list(&TokenKind::RBrace, false, "import lists", |p| {
                if let Some(name) = p.ident_opt() {
                    Some(name)
                } else {
                    p.expected("a name to import");
                    None
                }
            });
            if names.is_empty() {
                self.expected("a name to import");
            }
            self.close(&TokenKind::RBrace, "`}`");
            Some(names)
        } else {
            None
        };
        Import { path, alias, names }
    }

    fn source_name(&mut self) -> SourceName {
        let start = self.span();
        let first = self.ident("an import path");
        let (source, first) = if self.eat(&TokenKind::Colon) {
            (Some(first), None)
        } else {
            (None, Some(first))
        };
        let mut parts = vec![self.name_path(first, "a package name")];
        while self.eat(&TokenKind::Slash) {
            parts.push(self.name_path(None, "a package name"));
        }
        let version = if self.eat(&TokenKind::At) {
            if matches!(self.kind(), TokenKind::StringStart { .. }) {
                Some(self.string_lit())
            } else {
                self.expected("a version string (`@\"^1\"`)");
                None
            }
        } else {
            None
        };
        SourceName {
            source,
            parts,
            version,
            span: self.finish(start),
        }
    }

    fn type_params(&mut self) -> Vec<Ident> {
        if !self.at(&TokenKind::Lt) {
            return Vec::new();
        }
        self.bump();
        let params = self.comma_list(&TokenKind::Gt, false, "type parameters", |p| {
            if let Some(name) = p.ident_opt() {
                Some(name)
            } else {
                p.expected("a type parameter");
                None
            }
        });
        if params.is_empty() {
            self.expected("a type parameter");
        }
        self.close(&TokenKind::Gt, "`>`");
        params
    }

    fn type_decl(&mut self) -> TypeDecl {
        self.bump();
        let name = self.ident("a type name");
        let params = self.type_params();
        self.expect(&TokenKind::Eq, "`=`");
        let ty = self.ty();
        TypeDecl { name, params, ty }
    }

    fn enum_decl(&mut self) -> EnumDecl {
        self.bump();
        let name = self.ident("an enum name");
        let params = self.type_params();
        let mut variants = Vec::new();
        if self.open_brace(BraceKind::EnumBody, "the variants of an enum") {
            variants = self.comma_list(&TokenKind::RBrace, true, "enum bodies", |p| {
                let start = p.span();
                let Some(name) = p.ident_opt() else {
                    p.expected("a variant name");
                    return None;
                };
                let fields = if matches!(p.kind(), TokenKind::LBrace(_)) {
                    Some(p.record_type_fields())
                } else {
                    None
                };
                Some(Variant {
                    name,
                    fields,
                    span: p.finish(start),
                })
            });
            if variants.is_empty() {
                self.expected("a variant (an enum has at least one)");
            }
            self.close(&TokenKind::RBrace, "`}`");
        }
        EnumDecl {
            name,
            params,
            variants,
        }
    }

    fn const_decl(&mut self) -> ConstDecl {
        self.bump();
        let name = self.ident("a constant name");
        let ty = if self.eat(&TokenKind::Colon) {
            Some(self.ty())
        } else {
            None
        };
        self.expect(&TokenKind::Eq, "`=`");
        let value = self.expr();
        ConstDecl { name, ty, value }
    }

    fn fn_decl(&mut self) -> FnDecl {
        self.bump();
        let name = self.ident("a function name");
        let type_params = self.type_params();
        let params = self.params();
        let ret = self.ret_type();
        let body = self.block();
        FnDecl {
            name,
            type_params,
            params,
            ret,
            body,
        }
    }

    fn harness_decl(&mut self) -> HarnessDecl {
        self.bump();
        let name = self.ident("a harness name");
        let params = self.params();
        let ret = self.ret_type();
        let config = if self.eat_kw(Keyword::With) {
            Some(self.record_lit())
        } else {
            None
        };
        let body = self.block();
        HarnessDecl {
            name,
            params,
            ret,
            config,
            body,
        }
    }

    fn prompt_decl(&mut self) -> PromptDecl {
        self.bump();
        let name = self.ident("a prompt name");
        let params = self.params();
        let body = self.prompt_body();
        PromptDecl { name, params, body }
    }

    fn state_decl(&mut self) -> StateDecl {
        self.bump();
        self.expect(&TokenKind::Colon, "`:`");
        let ty = self.ty();
        self.expect(&TokenKind::Eq, "`=`");
        let value = self.expr();
        StateDecl { ty, value }
    }

    fn test_decl(&mut self) -> TestDecl {
        self.bump();
        let name = if matches!(self.kind(), TokenKind::StringStart { .. }) {
            self.string_lit()
        } else {
            self.expected("the test's name, a string");
            StringLit {
                long: false,
                raw: false,
                parts: Vec::new(),
                span: point(self.span()),
            }
        };
        let body = self.block();
        TestDecl { name, body }
    }

    fn ret_type(&mut self) -> Option<Type> {
        if self.eat(&TokenKind::Arrow) {
            Some(self.ty())
        } else {
            None
        }
    }

    fn params(&mut self) -> Vec<Param> {
        if !self.expect(&TokenKind::LParen, "`(`") {
            return Vec::new();
        }
        let params = self.comma_list(&TokenKind::RParen, true, "parameter lists", |p| {
            let start = p.span();
            let Some(name) = p.ident_opt() else {
                p.expected("a parameter name");
                return None;
            };
            p.expect(&TokenKind::Colon, "`:` and the parameter's type");
            let ty = p.ty();
            let default = if p.eat(&TokenKind::Eq) {
                Some(p.expr())
            } else {
                None
            };
            Some(Param {
                name,
                ty,
                default,
                span: p.finish(start),
            })
        });
        self.close(&TokenKind::RParen, "`,` or `)`");
        params
    }

    // ---- types ----

    fn ty(&mut self) -> Type {
        let start = self.span();
        if !self.enter() {
            self.leave();
            return Type {
                kind: TypeKind::Error,
                span: point(start),
            };
        }
        let mut ty = self.base_type();
        let mut chain = 0;
        loop {
            let layers = match self.kind() {
                TokenKind::Question => 1,
                TokenKind::QuestionQuestion => 2,
                _ => break,
            };
            let mut entered = true;
            for _ in 0..layers {
                chain += 1;
                entered = self.enter();
                if !entered {
                    break;
                }
            }
            if !entered {
                break;
            }
            self.bump();
            for _ in 0..layers {
                ty = Type {
                    kind: TypeKind::Option(Box::new(ty)),
                    span: self.finish(start),
                };
            }
        }
        self.leave_n(chain);
        self.leave();
        ty
    }

    fn type_list(&mut self, close: &TokenKind, what: &str) -> Vec<Type> {
        let types = self.comma_list(close, false, what, |p| Some(p.ty()));
        let closer = close.punctuation().unwrap_or("?");
        self.close(close, &format!("`,` or `{closer}`"));
        types
    }

    fn base_type(&mut self) -> Type {
        let start = self.span();
        let kind = match self.kind() {
            TokenKind::Ident(_) | TokenKind::Contextual(_) => {
                let path = self.name_path(None, "a type");
                let args = if self.eat(&TokenKind::Lt) {
                    let args = self.type_list(&TokenKind::Gt, "type arguments");
                    if args.is_empty() {
                        self.report(
                            Code::UnexpectedToken,
                            "type arguments need at least one type",
                            self.prev,
                        );
                    }
                    args
                } else {
                    Vec::new()
                };
                TypeKind::Named { path, args }
            }
            TokenKind::LBracket => {
                self.bump();
                let inner = self.ty();
                self.close(&TokenKind::RBracket, "`]`");
                TypeKind::List(Box::new(inner))
            }
            TokenKind::LBrace(_) => TypeKind::Record(self.record_type_fields()),
            TokenKind::LParen => {
                self.bump();
                TypeKind::Tuple(self.type_list(&TokenKind::RParen, "tuple types"))
            }
            TokenKind::Keyword(keyword @ (Keyword::Fn | Keyword::Harness)) => {
                let harness = *keyword == Keyword::Harness;
                self.bump();
                let params = if self.expect(&TokenKind::LParen, "`(`") {
                    self.type_list(&TokenKind::RParen, "parameter types")
                } else {
                    Vec::new()
                };
                self.expect(&TokenKind::Arrow, "`->` and a result type");
                let ret = Box::new(self.ty());
                if harness {
                    TypeKind::Harness { params, ret }
                } else {
                    TypeKind::Fn { params, ret }
                }
            }
            _ => {
                self.expected("a type");
                TypeKind::Error
            }
        };
        Type {
            kind,
            span: self.finish(start),
        }
    }

    fn record_type_fields(&mut self) -> Vec<FieldType> {
        if !self.open_brace(BraceKind::Record, "a record type") {
            return Vec::new();
        }
        let fields = self.comma_list(&TokenKind::RBrace, true, "record types", |p| {
            let start = p.span();
            let mut docs = p.take_docs();
            let mut attrs = Vec::new();
            while p.at(&TokenKind::At) {
                attrs.push(p.attribute());
                docs.extend(p.take_docs());
            }
            let Some(name) = p.ident_opt() else {
                p.expected("a field name");
                return None;
            };
            let optional = p.eat(&TokenKind::Question);
            p.expect(&TokenKind::Colon, "`:` and the field's type");
            let ty = p.ty();
            let description = if matches!(p.kind(), TokenKind::StringStart { .. }) {
                Some(p.string_lit())
            } else {
                None
            };
            Some(FieldType {
                docs,
                attrs,
                name,
                optional,
                ty,
                description,
                span: p.finish(start),
            })
        });
        self.close(&TokenKind::RBrace, "`,` or `}`");
        fields
    }

    // ---- blocks and statements ----

    fn block(&mut self) -> Block {
        let start = self.span();
        if !self.open_brace(BraceKind::Block, "a block") {
            return Block {
                stmts: Vec::new(),
                span: point(start),
            };
        }
        let stmts = self.block_body();
        self.close(&TokenKind::RBrace, "`;`, a line end or `}`");
        Block {
            stmts,
            span: self.finish(start),
        }
    }

    /// Statements up to the `}` (not consumed).
    fn block_body(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        loop {
            if self.at(&TokenKind::RBrace) || self.at_hard_stop() {
                break;
            }
            if let Some(stmt) = self.statement() {
                stmts.push(stmt);
            }
            if self.eat_semi() {
                continue;
            }
            if self.at(&TokenKind::RBrace) || self.at_hard_stop() {
                break;
            }
            self.expected("`;`, a line end or `}`");
            self.recover_stmt();
        }
        stmts
    }

    fn statement(&mut self) -> Option<Stmt> {
        if self.at(&TokenKind::At) {
            self.error_here(
                Code::MisplacedModifier,
                "attributes apply to items and record-type fields, not to statements",
            );
            while self.at(&TokenKind::At) {
                let _ = self.attribute();
            }
        }
        if self.at_semi() {
            self.expected("a statement");
            return None;
        }
        let start = self.span();
        let kind = match self.kind() {
            TokenKind::Keyword(Keyword::Let) => {
                self.bump();
                let pattern = self.pattern();
                let ty = if self.eat(&TokenKind::Colon) {
                    Some(self.ty())
                } else {
                    None
                };
                self.expect(&TokenKind::Eq, "`=`");
                let value = self.expr();
                StmtKind::Let { pattern, ty, value }
            }
            TokenKind::Keyword(Keyword::Var) => {
                self.bump();
                let name = self.ident("a variable name");
                let ty = if self.eat(&TokenKind::Colon) {
                    Some(self.ty())
                } else {
                    None
                };
                self.expect(&TokenKind::Eq, "`=`");
                let value = self.expr();
                StmtKind::Var { name, ty, value }
            }
            TokenKind::Keyword(Keyword::Defer) => {
                self.bump();
                StmtKind::Defer(self.block())
            }
            _ => {
                let target = self.expr();
                if let Some(op) = assign_op(self.kind()) {
                    if !is_place(&target) && !matches!(target.kind, ExprKind::Error) {
                        self.report(
                            Code::NotAPlace,
                            "only a name, a field (`x.a`) or an element (`x[i]`) can be assigned",
                            target.span,
                        );
                    }
                    self.bump();
                    let value = self.expr();
                    StmtKind::Assign { target, op, value }
                } else {
                    StmtKind::Expr(target)
                }
            }
        };
        Some(Stmt {
            kind,
            span: self.finish(start),
        })
    }

    // ---- expressions ----

    fn error_expr(&self) -> Expr {
        Expr {
            kind: ExprKind::Error,
            span: point(self.span()),
        }
    }

    fn expr(&mut self) -> Expr {
        if !self.enter() {
            self.leave();
            return self.error_expr();
        }
        let expr = match self.kind() {
            TokenKind::Keyword(
                Keyword::Return | Keyword::Break | Keyword::Continue | Keyword::Raise,
            ) => self.jump(),
            _ => self.pipe(),
        };
        self.leave();
        expr
    }

    fn jump(&mut self) -> Expr {
        let start = self.span();
        let keyword = match self.kind() {
            TokenKind::Keyword(k) => *k,
            _ => return self.error_expr(),
        };
        self.bump();
        let kind = match keyword {
            Keyword::Continue => ExprKind::Continue,
            Keyword::Raise => ExprKind::Raise(Box::new(self.expr())),
            Keyword::Break | Keyword::Return => {
                let value = if starts_expr(self.kind()) {
                    Some(Box::new(self.expr()))
                } else {
                    None
                };
                if keyword == Keyword::Return {
                    ExprKind::Return(value)
                } else {
                    ExprKind::Break(value)
                }
            }
            _ => ExprKind::Error,
        };
        Expr {
            kind,
            span: self.finish(start),
        }
    }

    fn binary(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
        let span = join(lhs.span, rhs.span);
        Expr {
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            span,
        }
    }

    /// A left-associative chain: `next { op next }`.
    fn left_chain(
        &mut self,
        next: fn(&mut Self) -> Expr,
        op_of: fn(&TokenKind) -> Option<BinaryOp>,
    ) -> Expr {
        let mut lhs = next(self);
        let mut chain = 0;
        while let Some(op) = op_of(self.kind()) {
            chain += 1;
            if !self.enter() {
                break;
            }
            self.bump();
            let rhs = next(self);
            lhs = Self::binary(op, lhs, rhs);
        }
        self.leave_n(chain);
        lhs
    }

    /// `coalesce { "|>" postfix }`.
    fn pipe(&mut self) -> Expr {
        let mut lhs = self.coalesce();
        let mut chain = 0;
        while self.at(&TokenKind::PipeGt) {
            chain += 1;
            if !self.enter() {
                break;
            }
            self.bump();
            let rhs = self.postfix_expr();
            lhs = Self::binary(BinaryOp::Pipe, lhs, rhs);
        }
        self.leave_n(chain);
        lhs
    }

    /// `or [ "??" coalesce ]`, right-associative, read as a loop.
    fn coalesce(&mut self) -> Expr {
        let mut operands = vec![self.or_expr()];
        let mut chain = 0;
        while self.at(&TokenKind::QuestionQuestion) {
            chain += 1;
            if !self.enter() {
                break;
            }
            self.bump();
            operands.push(self.or_expr());
        }
        self.leave_n(chain);
        let mut acc = operands.pop().unwrap_or_else(|| self.error_expr());
        while let Some(lhs) = operands.pop() {
            acc = Self::binary(BinaryOp::Coalesce, lhs, acc);
        }
        acc
    }

    fn or_expr(&mut self) -> Expr {
        self.left_chain(Self::and_expr, or_op)
    }

    fn and_expr(&mut self) -> Expr {
        self.left_chain(Self::not_expr, and_op)
    }

    fn not_expr(&mut self) -> Expr {
        let mut nots = Vec::new();
        while self.at_kw(Keyword::Not) {
            nots.push(self.span());
            if !self.enter() {
                break;
            }
            self.bump();
        }
        let mut expr = self.compare();
        let count = nots.len();
        while let Some(start) = nots.pop() {
            let span = join(start, expr.span);
            expr = Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(expr),
                },
                span,
            };
        }
        self.leave_n(count);
        expr
    }

    /// A non-associative level: `next [ op next ]`; a second operator is `P0005`.
    fn non_assoc(
        &mut self,
        next: fn(&mut Self) -> Expr,
        op_of: fn(&TokenKind) -> Option<BinaryOp>,
        what: &str,
    ) -> Expr {
        let mut lhs = next(self);
        let mut chain = 0;
        while let Some(op) = op_of(self.kind()) {
            if chain > 0 {
                self.error_here(Code::NonAssociative, what);
            }
            chain += 1;
            if !self.enter() {
                break;
            }
            self.bump();
            let rhs = next(self);
            lhs = Self::binary(op, lhs, rhs);
        }
        self.leave_n(chain);
        lhs
    }

    fn compare(&mut self) -> Expr {
        self.non_assoc(
            Self::range,
            compare_op,
            "comparisons do not chain: write `a < b and b < c`, or add parentheses",
        )
    }

    fn range(&mut self) -> Expr {
        self.non_assoc(
            Self::add,
            range_op,
            "ranges do not chain; add parentheses",
        )
    }

    fn add(&mut self) -> Expr {
        self.left_chain(Self::mul, add_op)
    }

    fn mul(&mut self) -> Expr {
        self.left_chain(Self::unary, mul_op)
    }

    fn unary(&mut self) -> Expr {
        let mut minuses = Vec::new();
        while self.at(&TokenKind::Minus) {
            minuses.push(self.span());
            if !self.enter() {
                break;
            }
            self.bump();
        }
        let mut expr = self.postfix_expr();
        let count = minuses.len();
        while let Some(start) = minuses.pop() {
            let span = join(start, expr.span);
            expr = Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Neg,
                    operand: Box::new(expr),
                },
                span,
            };
        }
        self.leave_n(count);
        expr
    }

    /// Whether the token after the expression so far continues it as a postfix operation.
    fn postfix_follows(&self, expr: &Expr) -> bool {
        match self.kind() {
            TokenKind::ColonColon
            | TokenKind::LParen
            | TokenKind::Dot
            | TokenKind::QuestionDot
            | TokenKind::LBracket => true,
            // A call override starts on the line of the call (rule 7).
            TokenKind::Keyword(Keyword::With) => {
                matches!(self.nth(1), TokenKind::LBrace(_)) && self.span().line == self.prev.end_line
            }
            // A variant literal: a type name, then a `{` the reader read as a record (rule 1).
            TokenKind::LBrace(BraceKind::Record) => as_path(expr)
                .and_then(|path| path.last().map(|name| is_upper_camel(&name.name)))
                .unwrap_or(false),
            _ => false,
        }
    }

    fn postfix_expr(&mut self) -> Expr {
        let mut expr = self.primary();
        let mut chain = 0;
        while self.postfix_follows(&expr) {
            chain += 1;
            if !self.enter() {
                break;
            }
            expr = self.postfix(expr);
        }
        self.leave_n(chain);
        expr
    }

    fn postfix(&mut self, expr: Expr) -> Expr {
        let start = expr.span;
        let kind = match self.kind() {
            TokenKind::ColonColon => {
                self.bump();
                let type_args = if self.expect(&TokenKind::Lt, "`<` and type arguments") {
                    let args = self.type_list(&TokenKind::Gt, "type arguments");
                    if args.is_empty() {
                        self.report(
                            Code::UnexpectedToken,
                            "type arguments need at least one type",
                            self.prev,
                        );
                    }
                    args
                } else {
                    Vec::new()
                };
                let args = if self.at(&TokenKind::LParen) {
                    self.call_args()
                } else {
                    self.expected("`(` and the arguments (`f::<T>(x)`)");
                    Vec::new()
                };
                ExprKind::Call {
                    callee: Box::new(expr),
                    type_args,
                    args,
                }
            }
            TokenKind::LParen => {
                let args = self.call_args();
                ExprKind::Call {
                    callee: Box::new(expr),
                    type_args: Vec::new(),
                    args,
                }
            }
            TokenKind::Dot | TokenKind::QuestionDot => {
                let optional = self.at(&TokenKind::QuestionDot);
                self.bump();
                let name = self.ident("a field or method name");
                ExprKind::Member {
                    object: Box::new(expr),
                    name,
                    optional,
                }
            }
            TokenKind::LBracket => {
                self.bump();
                let index = self.expr();
                self.close(&TokenKind::RBracket, "`]`");
                ExprKind::Index {
                    object: Box::new(expr),
                    index: Box::new(index),
                }
            }
            TokenKind::Keyword(Keyword::With) => {
                self.bump();
                let config = self.record_lit();
                ExprKind::WithOverride {
                    call: Box::new(expr),
                    config,
                }
            }
            TokenKind::LBrace(_) => {
                let path = as_path(&expr).unwrap_or_default();
                let fields = self.record_lit();
                ExprKind::Variant { path, fields }
            }
            _ => return expr,
        };
        Expr {
            kind,
            span: self.finish(start),
        }
    }

    fn call_args(&mut self) -> Vec<Arg> {
        self.bump();
        let mut named_seen = false;
        let args = self.comma_list(&TokenKind::RParen, true, "argument lists", |p| {
            let named = p.at_ident()
                && matches!(p.nth(1), TokenKind::Colon)
                && !matches!(p.nth(2), TokenKind::Colon);
            if named {
                let name = p.ident("a parameter name");
                p.bump();
                named_seen = true;
                return Some(Arg {
                    name: Some(name),
                    value: p.expr(),
                });
            }
            let value = p.expr();
            if named_seen {
                p.report(
                    Code::PositionalAfterNamed,
                    "positional arguments come before named ones (rule 9 of chapter 02); name \
                     this one or move it before them",
                    value.span,
                );
            }
            Some(Arg { name: None, value })
        });
        self.close(&TokenKind::RParen, "`,` or `)`");
        args
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per primary form of the grammar, in its order"
    )]
    fn primary(&mut self) -> Expr {
        let start = self.span();
        let kind = match self.kind() {
            TokenKind::Int(_)
            | TokenKind::Quantity(_)
            | TokenKind::Float(_)
            | TokenKind::Duration(_)
            | TokenKind::Money(_)
            | TokenKind::StringStart { .. }
            | TokenKind::RawString { .. }
            | TokenKind::Keyword(Keyword::True | Keyword::False | Keyword::None) => {
                match self.literal() {
                    Some(lit) => ExprKind::Lit(lit),
                    None => ExprKind::Error,
                }
            }
            TokenKind::Contextual(Contextual::Par | Contextual::Race)
                if matches!(self.nth(1), TokenKind::LBrace(_)) =>
            {
                let race = matches!(self.kind(), TokenKind::Contextual(Contextual::Race));
                self.bump();
                let branches = self.branches(if race { "`race`" } else { "`par`" });
                if race {
                    ExprKind::Race(branches)
                } else {
                    ExprKind::Par(branches)
                }
            }
            TokenKind::Ident(_) | TokenKind::Contextual(_) => {
                ExprKind::Name(self.ident("a name"))
            }
            TokenKind::LParen => self.paren_or_tuple(),
            TokenKind::LBracket => {
                self.bump();
                let items = self.comma_list(&TokenKind::RBracket, true, "lists", |p| {
                    let spread = p.eat(&TokenKind::DotDot);
                    Some(ListItem {
                        spread,
                        expr: p.expr(),
                    })
                });
                self.close(&TokenKind::RBracket, "`,` or `]`");
                ExprKind::List(items)
            }
            TokenKind::LBrace(BraceKind::Record) => ExprKind::Record(self.record_lit()),
            TokenKind::LBrace(_) => ExprKind::Block(self.block()),
            TokenKind::Keyword(Keyword::If) => return self.if_expr(),
            TokenKind::Keyword(Keyword::Match) => self.match_expr(),
            TokenKind::Keyword(Keyword::For) => {
                self.bump();
                let pattern = self.pattern();
                self.expect(&TokenKind::Keyword(Keyword::In), "`in`");
                let iter = Box::new(self.expr());
                let body = self.block();
                ExprKind::For {
                    pattern,
                    iter,
                    body,
                }
            }
            TokenKind::Keyword(Keyword::While) => {
                self.bump();
                let cond = Box::new(self.condition());
                let body = self.block();
                ExprKind::While { cond, body }
            }
            TokenKind::Keyword(Keyword::Loop) => {
                self.bump();
                ExprKind::Loop(self.block())
            }
            TokenKind::Keyword(Keyword::Repeat) => {
                self.bump();
                let count = Box::new(self.expr());
                let body = self.block();
                ExprKind::Repeat { count, body }
            }
            TokenKind::Keyword(Keyword::Try) => self.try_expr(),
            TokenKind::Keyword(Keyword::With) => self.with_expr(),
            TokenKind::Keyword(Keyword::Do) => {
                self.bump();
                ExprKind::Do(self.block())
            }
            TokenKind::Keyword(Keyword::Fn) => {
                self.bump();
                let params = self.params();
                let ret = self.ret_type();
                let body = self.block();
                ExprKind::FnExpr { params, ret, body }
            }
            TokenKind::Keyword(Keyword::Prompt) => {
                self.bump();
                ExprKind::Prompt(self.prompt_body())
            }
            TokenKind::Pipe | TokenKind::PipePipe => self.closure(),
            TokenKind::Keyword(
                Keyword::Return | Keyword::Break | Keyword::Continue | Keyword::Raise,
            ) => {
                let word = describe(self.kind());
                self.error_here(
                    Code::UnexpectedToken,
                    format!(
                        "{word} starts a statement, an argument, an element, an arm or a closure \
                         body, not an operand; put it in parentheses"
                    ),
                );
                return self.jump();
            }
            _ => {
                self.expected("an expression");
                return self.error_expr();
            }
        };
        Expr {
            kind,
            span: self.finish(start),
        }
    }

    fn literal(&mut self) -> Option<Lit> {
        let lit = match self.kind() {
            TokenKind::Int(v) => Lit::Int(*v),
            TokenKind::Quantity(v) => Lit::Quantity(*v),
            TokenKind::Float(v) => Lit::Float(*v),
            TokenKind::Duration(v) => Lit::Duration(*v),
            TokenKind::Money(v) => Lit::Money(*v),
            TokenKind::Keyword(Keyword::True) => Lit::Bool(true),
            TokenKind::Keyword(Keyword::False) => Lit::Bool(false),
            TokenKind::Keyword(Keyword::None) => Lit::None,
            TokenKind::StringStart { .. } | TokenKind::RawString { .. } => {
                return Some(Lit::Str(self.string_lit()));
            }
            _ => return None,
        };
        self.bump();
        Some(lit)
    }

    fn string_lit(&mut self) -> StringLit {
        let start = self.span();
        match self.kind() {
            TokenKind::RawString { text, long } => {
                let (text, long) = (text.clone(), *long);
                self.bump();
                let parts = if text.is_empty() {
                    Vec::new()
                } else {
                    vec![StrPart::Text(text)]
                };
                StringLit {
                    long,
                    raw: true,
                    parts,
                    span: self.finish(start),
                }
            }
            TokenKind::StringStart { long } => {
                let long = *long;
                self.bump();
                let mut parts = Vec::new();
                loop {
                    match self.kind() {
                        TokenKind::StringText(text) => {
                            if !text.is_empty() {
                                let text = text.clone();
                                if let Some(StrPart::Text(previous)) = parts.last_mut() {
                                    previous.push_str(&text);
                                } else {
                                    parts.push(StrPart::Text(text));
                                }
                            }
                            self.bump();
                        }
                        TokenKind::InterpStart => parts.push(self.interpolation()),
                        TokenKind::StringEnd => {
                            self.bump();
                            break;
                        }
                        _ => break,
                    }
                }
                StringLit {
                    long,
                    raw: false,
                    parts,
                    span: self.finish(start),
                }
            }
            _ => {
                self.expected("a string");
                StringLit {
                    long: false,
                    raw: false,
                    parts: Vec::new(),
                    span: point(start),
                }
            }
        }
    }

    fn interpolation(&mut self) -> StrPart {
        let start = self.span();
        self.bump();
        let expr = self.expr();
        let spec = if let TokenKind::FormatSpec(spec) = self.kind() {
            let spec = spec.clone();
            self.bump();
            Some(spec)
        } else {
            None
        };
        if !self.at(&TokenKind::InterpEnd) {
            self.expected("`}` ending the interpolation");
            self.skip_until(|k| matches!(k, TokenKind::InterpEnd));
        }
        self.eat(&TokenKind::InterpEnd);
        StrPart::Interp {
            expr,
            spec,
            span: self.finish(start),
        }
    }

    fn paren_or_tuple(&mut self) -> ExprKind {
        self.bump();
        if self.at(&TokenKind::RParen) {
            self.error_here(
                Code::UnexpectedToken,
                "`()` is not an expression; a tuple needs a `,`, as in `(x,)`",
            );
            self.bump();
            return ExprKind::Error;
        }
        let first = self.expr();
        if !self.eat(&TokenKind::Comma) {
            self.close(&TokenKind::RParen, "`,` or `)`");
            return ExprKind::Paren(Box::new(first));
        }
        let mut items = vec![first];
        if !self.at(&TokenKind::RParen) {
            loop {
                items.push(self.expr());
                if !self.at(&TokenKind::Comma) {
                    break;
                }
                let comma = self.span();
                self.bump();
                if self.at(&TokenKind::RParen) {
                    self.report(
                        Code::UnexpectedToken,
                        "tuples of two or more take no trailing `,`",
                        comma,
                    );
                    break;
                }
            }
        }
        self.close(&TokenKind::RParen, "`,` or `)`");
        ExprKind::Tuple(items)
    }

    fn record_lit(&mut self) -> RecordLit {
        let start = self.span();
        if !self.open_brace(BraceKind::Record, "a record") {
            return RecordLit {
                fields: Vec::new(),
                span: point(start),
            };
        }
        let fields = self.comma_list(&TokenKind::RBrace, true, "records", |p| {
            if p.eat(&TokenKind::DotDot) {
                return Some(FieldInit::Spread(p.expr()));
            }
            let Some(name) = p.ident_opt() else {
                p.expected("a field (`name: value`, `name` or `..base`)");
                return None;
            };
            if p.eat(&TokenKind::Colon) {
                Some(FieldInit::Field {
                    name,
                    value: p.expr(),
                })
            } else {
                Some(FieldInit::Shorthand(name))
            }
        });
        self.close(&TokenKind::RBrace, "`,` or `}`");
        RecordLit {
            fields,
            span: self.finish(start),
        }
    }

    fn condition(&mut self) -> Condition {
        if self.eat_kw(Keyword::Let) {
            let pattern = self.pattern();
            self.expect(&TokenKind::Eq, "`=`");
            let value = self.expr();
            Condition::Let { pattern, value }
        } else {
            Condition::Expr(self.expr())
        }
    }

    fn if_expr(&mut self) -> Expr {
        let start = self.span();
        self.bump();
        let cond = Box::new(self.condition());
        let then = self.block();
        let else_ = if self.eat_kw(Keyword::Else) {
            if self.at_kw(Keyword::If) {
                let nested = if self.enter() {
                    self.if_expr()
                } else {
                    self.error_expr()
                };
                self.leave();
                Some(Box::new(Else::If(nested)))
            } else {
                Some(Box::new(Else::Block(self.block())))
            }
        } else {
            None
        };
        Expr {
            kind: ExprKind::If { cond, then, else_ },
            span: self.finish(start),
        }
    }

    fn arms(&mut self, what: &str) -> Vec<Arm> {
        if !self.open_brace(BraceKind::MatchBody, what) {
            return Vec::new();
        }
        let arms = self.comma_list(&TokenKind::RBrace, true, "arm lists", |p| {
            let start = p.span();
            let pattern = p.pattern();
            let guard = if p.eat_kw(Keyword::If) {
                Some(p.expr())
            } else {
                None
            };
            p.expect(&TokenKind::FatArrow, "`=>`");
            let body = p.expr();
            Some(Arm {
                pattern,
                guard,
                body,
                span: p.finish(start),
            })
        });
        if arms.is_empty() {
            self.expected("an arm (`pattern => value`)");
        }
        self.close(&TokenKind::RBrace, "`,` or `}`");
        arms
    }

    fn match_expr(&mut self) -> ExprKind {
        self.bump();
        let scrutinee = Box::new(self.expr());
        let arms = self.arms("the arms of a `match`");
        ExprKind::Match { scrutinee, arms }
    }

    fn try_expr(&mut self) -> ExprKind {
        self.bump();
        let body = self.block();
        let catch = if self.eat_kw(Keyword::Catch) {
            if self.at_ident() {
                let name = self.ident("a name");
                let body = self.block();
                Catch::Bind { name, body }
            } else {
                Catch::Arms(self.arms("the arms of a `catch`"))
            }
        } else {
            self.expected("`catch`");
            Catch::Arms(Vec::new())
        };
        ExprKind::Try { body, catch }
    }

    fn with_expr(&mut self) -> ExprKind {
        self.bump();
        let timeout = match self.kind() {
            TokenKind::Contextual(Contextual::Timeout) => true,
            TokenKind::Ident(name) => name == "timeout",
            _ => false,
        };
        if timeout {
            self.bump();
            let duration = Box::new(self.expr());
            let body = self.block();
            let else_ = if self.eat_kw(Keyword::Else) {
                Some(self.block())
            } else {
                None
            };
            ExprKind::WithTimeout {
                duration,
                body,
                else_,
            }
        } else {
            let config = self.record_lit();
            let body = self.block();
            ExprKind::With { config, body }
        }
    }

    /// The `{ e; e }` of `par` and `race`.
    fn branches(&mut self, what: &str) -> Vec<Expr> {
        let mut branches = Vec::new();
        if !self.open_brace(BraceKind::Block, &format!("the branches of {what}")) {
            return branches;
        }
        loop {
            if self.at(&TokenKind::RBrace) || self.at_hard_stop() {
                break;
            }
            if self.at_semi() {
                self.expected("an expression");
                self.recover_stmt();
                continue;
            }
            branches.push(self.expr());
            if self.eat_semi() {
                continue;
            }
            if self.at(&TokenKind::RBrace) || self.at_hard_stop() {
                break;
            }
            self.expected("`;`, a line end or `}`");
            self.recover_stmt();
        }
        if branches.is_empty() {
            self.expected("a branch");
        }
        self.close(&TokenKind::RBrace, "`}`");
        branches
    }

    fn closure(&mut self) -> ExprKind {
        let params = if self.eat(&TokenKind::PipePipe) {
            Vec::new()
        } else {
            self.bump();
            let params = self.comma_list(&TokenKind::Pipe, false, "closure parameters", |p| {
                let pattern = p.alt_pattern();
                let ty = if p.eat(&TokenKind::Colon) {
                    Some(p.ty())
                } else {
                    None
                };
                Some(ClosureParam { pattern, ty })
            });
            self.close(&TokenKind::Pipe, "`,` or `|`");
            params
        };
        let body = Box::new(self.expr());
        ExprKind::Closure { params, body }
    }

    // ---- patterns ----

    fn error_pattern(&self) -> Pattern {
        Pattern {
            kind: PatternKind::Error,
            span: point(self.span()),
        }
    }

    fn pattern(&mut self) -> Pattern {
        if !self.enter() {
            self.leave();
            return self.error_pattern();
        }
        let first = self.alt_pattern();
        if !self.at(&TokenKind::Pipe) {
            self.leave();
            return first;
        }
        let start = first.span;
        let mut alternatives = vec![first];
        while self.eat(&TokenKind::Pipe) {
            alternatives.push(self.alt_pattern());
        }
        self.leave();
        Pattern {
            kind: PatternKind::Or(alternatives),
            span: self.finish(start),
        }
    }

    fn alt_pattern(&mut self) -> Pattern {
        if !self.enter() {
            self.leave();
            return self.error_pattern();
        }
        let start = self.span();
        let kind = match self.kind() {
            TokenKind::Ident(name) if name == "_" => {
                self.bump();
                PatternKind::Wildcard
            }
            TokenKind::Int(_)
            | TokenKind::Quantity(_)
            | TokenKind::Float(_)
            | TokenKind::Duration(_)
            | TokenKind::Money(_)
            | TokenKind::StringStart { .. }
            | TokenKind::RawString { .. }
            | TokenKind::Keyword(Keyword::True | Keyword::False | Keyword::None) => {
                self.literal().map_or(PatternKind::Error, PatternKind::Lit)
            }
            TokenKind::Ident(_) | TokenKind::Contextual(_) => {
                let mut path = self.name_path(None, "a name");
                if path.len() == 1 && self.eat(&TokenKind::At) {
                    let name = path.pop().unwrap_or_else(|| Ident {
                        name: String::new(),
                        span: point(start),
                    });
                    let pattern = Box::new(self.alt_pattern());
                    PatternKind::Bind { name, pattern }
                } else if matches!(self.kind(), TokenKind::LBrace(_)) {
                    let fields = Some(self.record_pattern());
                    PatternKind::Variant { path, fields }
                } else if path.len() == 1 {
                    match path.pop() {
                        Some(name) => PatternKind::Name(name),
                        None => PatternKind::Error,
                    }
                } else {
                    PatternKind::Variant { path, fields: None }
                }
            }
            TokenKind::LParen => self.tuple_pattern(),
            TokenKind::LBracket => {
                self.bump();
                let items = self.comma_list(&TokenKind::RBracket, false, "list patterns", |p| {
                    if p.eat(&TokenKind::DotDot) {
                        let rest = if matches!(p.kind(), TokenKind::Ident(_)) {
                            p.ident_opt()
                        } else {
                            None
                        };
                        Some(ListPattern::Rest(rest))
                    } else {
                        Some(ListPattern::Pattern(p.pattern()))
                    }
                });
                self.close(&TokenKind::RBracket, "`,` or `]`");
                PatternKind::List(items)
            }
            TokenKind::LBrace(_) => PatternKind::Record(self.record_pattern()),
            _ => {
                self.expected("a pattern");
                self.leave();
                return self.error_pattern();
            }
        };
        self.leave();
        Pattern {
            kind,
            span: self.finish(start),
        }
    }

    fn tuple_pattern(&mut self) -> PatternKind {
        self.bump();
        let first = self.pattern();
        if !self.at(&TokenKind::Comma) {
            self.expected("`,` (a tuple pattern has a comma; patterns take no parentheses)");
            self.close(&TokenKind::RParen, "`)`");
            return PatternKind::Tuple(vec![first]);
        }
        self.bump();
        let mut items = vec![first];
        if !self.at(&TokenKind::RParen) {
            loop {
                items.push(self.pattern());
                if !self.at(&TokenKind::Comma) {
                    break;
                }
                let comma = self.span();
                self.bump();
                if self.at(&TokenKind::RParen) {
                    self.report(
                        Code::UnexpectedToken,
                        "tuple patterns of two or more take no trailing `,`",
                        comma,
                    );
                    break;
                }
            }
        }
        self.close(&TokenKind::RParen, "`,` or `)`");
        PatternKind::Tuple(items)
    }

    /// `{ a, b: p, .. }`. Any brace kind is accepted: a pattern's `{` is never a block, and the
    /// token reader cannot always tell a closure parameter's pattern from a block.
    fn record_pattern(&mut self) -> RecordPattern {
        let start = self.span();
        self.bump();
        let mut fields = Vec::new();
        let mut rest = false;
        loop {
            if self.at(&TokenKind::RBrace) || self.at_hard_stop() || is_closer(self.kind()) {
                break;
            }
            if self.eat(&TokenKind::DotDot) {
                rest = true;
                self.eat(&TokenKind::Comma);
                break;
            }
            if let Some(name) = self.ident_opt() {
                let pattern = if self.eat(&TokenKind::Colon) {
                    Some(self.pattern())
                } else {
                    None
                };
                fields.push(FieldPattern { name, pattern });
                if self.eat(&TokenKind::Comma) {
                    continue;
                }
                if self.at(&TokenKind::RBrace) {
                    break;
                }
                self.expected("`,` or `}`");
            } else {
                self.expected("a field name or `..`");
            }
            self.skip_until(|k| matches!(k, TokenKind::Comma));
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.close(&TokenKind::RBrace, "`}`");
        RecordPattern {
            fields,
            rest,
            span: self.finish(start),
        }
    }

    // ---- prompts ----

    fn prompt_body(&mut self) -> PromptBody {
        let start = self.span();
        if !self.open_brace(BraceKind::PromptBody, "a prompt body") {
            return PromptBody {
                sections: Vec::new(),
                span: point(start),
            };
        }
        let mut sections = Vec::new();
        loop {
            if self.at(&TokenKind::RBrace) || self.at_hard_stop() {
                break;
            }
            if let Some(section) = self.section() {
                sections.push(section);
            }
            if self.eat_semi() {
                continue;
            }
            if self.at(&TokenKind::RBrace) || self.at_hard_stop() {
                break;
            }
            self.expected("`;`, a line end or `}`");
            self.recover_stmt();
        }
        self.close(&TokenKind::RBrace, "`}`");
        PromptBody {
            sections,
            span: self.finish(start),
        }
    }

    fn section(&mut self) -> Option<Section> {
        let start = self.span();
        let word = match self.kind() {
            TokenKind::Contextual(c) => Contextual::prompt_section(c.as_str()),
            TokenKind::Ident(name) => Contextual::prompt_section(name),
            _ => None,
        };
        let kind = if let Some(word) = word {
            self.bump();
            match word {
                Contextual::System => SectionKind::System(self.expr()),
                Contextual::Instruct => SectionKind::Instruct(self.expr()),
                Contextual::Task => SectionKind::Task(self.expr()),
                Contextual::Steps => SectionKind::Steps(self.expr()),
                Contextual::Context => SectionKind::Context(self.expr()),
                Contextual::Note => SectionKind::Note(self.expr()),
                Contextual::Include => SectionKind::Include(self.expr()),
                Contextual::Format => SectionKind::Format(self.ty()),
                Contextual::Data | Contextual::Image => {
                    let value = self.expr();
                    let label = if self.eat_kw(Keyword::As) {
                        Some(self.expr())
                    } else {
                        None
                    };
                    if word == Contextual::Data {
                        SectionKind::Data { value, label }
                    } else {
                        SectionKind::Image { value, label }
                    }
                }
                Contextual::Example => {
                    let input = self.expr();
                    self.expect(&TokenKind::Arrow, "`->` and the example's output");
                    let output = self.expr();
                    SectionKind::Example { input, output }
                }
                Contextual::Meta
                | Contextual::State
                | Contextual::Par
                | Contextual::Race
                | Contextual::Timeout => return None,
            }
        } else if self.eat_kw(Keyword::If) {
            let cond = self.expr();
            let then = self.prompt_body();
            let else_ = if self.eat_kw(Keyword::Else) {
                Some(self.prompt_body())
            } else {
                None
            };
            SectionKind::If { cond, then, else_ }
        } else if self.eat_kw(Keyword::For) {
            let pattern = self.pattern();
            self.expect(&TokenKind::Keyword(Keyword::In), "`in`");
            let iter = self.expr();
            let body = self.prompt_body();
            SectionKind::For {
                pattern,
                iter,
                body,
            }
        } else {
            self.expected(SECTION_WORDS);
            return None;
        };
        Some(Section {
            kind,
            span: self.finish(start),
        })
    }
}
