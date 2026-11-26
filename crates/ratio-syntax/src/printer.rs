//! The canonical printer: a [`Module`] back to source text.
//!
//! The text it writes reads back as the same tree: for every tree the parser builds without
//! errors whose printed text is within the source limit (`MAX_SOURCE_BYTES`),
//! `parse(print(tree))` has the same [span-free dump](crate::dump) as the tree, and printing that
//! again gives the same text (printing is idempotent). The limit is a real condition: printing
//! indents by depth, so a deep tree can print to more bytes than the source it came from (255
//! nested `if a {` print to about 264,000 bytes) and the text is then refused with `L0013`.
//!
//! The printer runs on a thread with a large stack; if none can be started it runs on the
//! caller's stack and writes `<too deep>` for anything nested deeper than 32 levels. The printer keeps what the
//! tree keeps (parentheses, `T??` as two options, long and raw strings) and drops what it does
//! not: comments other than `///` documentation, blank lines, and the original spacing.
//!
//! # The canonical form
//!
//! - **Layout.** Four spaces per level; `\n` line ends; one `\n` ends the text. Items are
//!   separated by one blank line, and the `meta` header is followed by one. Documentation
//!   (`/// text`, one line each) comes first, then attributes (one per line), then the item.
//! - **Blocks** (`{ … }` of a function, `if`, `for`, a closure body and so on, `par`/`race`
//!   branches, prompt bodies) hold one statement per line; an empty block is `{}`. `else`,
//!   `catch` and `with` follow the closing `}` on its line.
//! - **Statements end** with a line end, which the token reader turns into a `;`. A statement
//!   whose last token cannot end a statement under insertion condition 2 of chapter 02 (a type
//!   that ends in `>` or in `??`) is followed by an explicit `;`, and so is one that holds a
//!   `fn` or `harness` type outside brackets and no `{` after it, because the reader takes that
//!   type for the head of a function and waits for its `{` (`format fn() -> Int;`). The last
//!   statement of a block needs none: the `}` ends it.
//! - **Everything else is on one line**: lists, calls, records, record types, tuples, parameter
//!   lists, `match` heads. The one exception is a `match` or `catch` arm list, an enum body, and
//!   a record type with documentation on its fields: one entry per line, each ending in `,`.
//! - **Spacing.** One space around binary operators and `=`, `=>`, `->`, `:` is followed by a
//!   space, `,` too; `..` and `..=` have no spaces (`0..n`); no space inside `()` and `[]`;
//!   `{ a, b }` has one inside braces, `{}` has none.
//! - **Records.** A record with a single shorthand field prints as `{ x, }`, so the token reader
//!   does not take it for a block (rule 1 of chapter 02). `import` name lists take no trailing
//!   comma: `{ a, b }`.
//! - **Literals** print in canonical units: integers in decimal (`31`, not `0x1F`); a quantity
//!   as `M` when it is a whole number of millions, else `k` (with a fraction when needed:
//!   `1.5k`); a duration in the largest of `h`, `m`, `s`, `ms` that holds it whole; money as
//!   `$12` or with at least two decimals (`$0.50`); floats in the shortest spelling that reads
//!   back to the same value (always with a fraction or an exponent).
//! - **Strings.** A short string escapes `\`, `"`, `{`, `}`, line feed, carriage return and tab
//!   (`\n`, `\r`, `\t`), and writes any other control or non-ASCII whitespace character as
//!   `\u{…}`. A *long* string prints on several lines, the `"""` of the opening alone on its
//!   line and the closing `"""` alone on a line at the statement's indentation; the text is
//!   indented to that level, `"`, `{`, `}`, `\` and tab are escaped, and a leading space that
//!   the dedent would otherwise remove is written `\u{20}`. A *raw* string prints as read
//!   (`r"…"`); a raw long string prints at column 0 (it has no escapes to keep the indentation
//!   apart from the text).
//! - **Interpolations** inside a short string print on one line ("flat": blocks as
//!   `{ a; b }`, arm lists as `{ p => v, q => w }`), because a short string's interpolation
//!   cannot hold a line break. A long string nested there prints as a one-line long string
//!   (`"""text\nmore"""`). An interpolation in a long string prints like any other code.
//!
//! # What the printer assumes
//!
//! It prints the tree it is given: it adds no parentheses, because the tree keeps the ones the
//! source had. A tree built by hand must hold [`ExprKind::Paren`] wherever the grammar needs
//! parentheses (an operand of lower precedence, a record or variant literal in the head of an
//! `if`, `while`, `for`, `match`, `repeat` or `with timeout`, a record or `match` in an
//! interpolation). It prints `Error` nodes as `<error>` for debugging; that text is not source.

use std::fmt::Write as _;

use crate::stack::{self, FALLBACK_DEPTH};

use crate::ast::{
    Arg, Arm, Attribute, BinaryOp, Block, Catch, ClosureParam, Condition, Doc, Else, EnumDecl,
    Expr, ExprKind, FieldInit, FieldPattern, FieldType, Ident, Import, ImportPath, Item, ItemKind,
    ListItem, ListPattern, Lit, Module, Param, Pattern, PatternKind, PromptBody, RecordLit,
    RecordPattern, Section, SectionKind, SourceName, Stmt, StmtKind, StrPart, StringLit, Type,
    TypeKind, UnaryOp, Variant,
};

/// The canonical source text of `module` (the notation is described in the module documentation).
#[must_use]
pub fn print(module: &Module) -> String {
    stack::run(|big| {
        let mut p = Printer {
            out: String::new(),
            indent: 0,
            flat: 0,
            bol: true,
            type_depth: 0,
            open_head: false,
            depth: 0,
            max_depth: if big { usize::MAX } else { FALLBACK_DEPTH },
        };
        p.module(module);
        p.out
    })
    .unwrap_or_else(|| "<internal error>".to_owned())
}

struct Printer {
    out: String,
    /// The current indentation, in levels of four spaces.
    indent: usize,
    /// Greater than 0 inside a short string's interpolation: no line breaks.
    flat: usize,
    /// Whether the next write starts a line (and so is preceded by the indentation).
    bol: bool,
    /// Inside the brackets of a type being printed (see `open_head`).
    type_depth: usize,
    /// Whether the statement being printed holds a `fn` or `harness` type outside brackets and
    /// no `{` after it. The token reader takes such a type for the head of a function and puts
    /// off ending the statement until a `{` or an explicit `;`.
    open_head: bool,
    /// The nesting of expressions, types and patterns being printed, and the most allowed.
    depth: usize,
    max_depth: usize,
}

/// Whether the printed text so far ends in a token that can end a statement at a line end
/// (insertion condition 2 of chapter 02): an identifier, number, string, `)`, `]`, `}` or `?`.
/// Not a `??` (so not `T??` as a type) and not a `>`.
fn ends_statement(out: &str) -> bool {
    let Some(last) = out.chars().last() else {
        return false;
    };
    if last == '?' {
        // The reader takes `??` greedily, left to right: an even run ends in the token `??`.
        return out.chars().rev().take_while(|c| *c == '?').count() % 2 == 1;
    }
    last.is_ascii_alphanumeric() || matches!(last, '_' | ')' | ']' | '}' | '"')
}

/// Writes `text` as the body of a string (short or long) with the escapes of chapter 02.
fn push_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || (c.is_whitespace() && c != ' ') => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
}

/// A piece of one line of a long string.
enum Seg<'a> {
    Text(String),
    Interp(&'a StrPart),
}

impl Seg<'_> {
    fn is_empty(&self) -> bool {
        matches!(self, Seg::Text(t) if t.is_empty())
    }
}

/// Splits the parts of a long string at the line feeds of its text.
fn long_lines(parts: &[StrPart]) -> Vec<Vec<Seg<'_>>> {
    let mut lines: Vec<Vec<Seg<'_>>> = vec![Vec::new()];
    for part in parts {
        match part {
            StrPart::Text(text) => {
                for (i, piece) in text.split('\n').enumerate() {
                    if i > 0 {
                        lines.push(Vec::new());
                    }
                    if piece.is_empty() {
                        continue;
                    }
                    if let Some(line) = lines.last_mut() {
                        match line.last_mut() {
                            Some(Seg::Text(previous)) => previous.push_str(piece),
                            _ => line.push(Seg::Text(piece.to_owned())),
                        }
                    }
                }
            }
            StrPart::Interp { .. } => {
                if let Some(line) = lines.last_mut() {
                    line.push(Seg::Interp(part));
                }
            }
        }
    }
    lines
}

/// Whether the dedent counts a line as blank (only spaces, no interpolation), and how many
/// spaces start it before the first other character or interpolation.
fn line_shape(line: &[Seg<'_>]) -> (bool, usize) {
    let mut blank = true;
    let mut indent = 0;
    let mut counting = true;
    for seg in line {
        match seg {
            Seg::Interp(_) => {
                blank = false;
                counting = false;
            }
            Seg::Text(text) => {
                for c in text.chars() {
                    if c == ' ' {
                        if counting {
                            indent += 1;
                        }
                    } else {
                        blank = false;
                        counting = false;
                    }
                }
            }
        }
    }
    (blank, indent)
}

/// Writes the escaped text of one line of a long string; `escape_first_space` writes the leading
/// space as `\u{20}` so the dedent keeps it.
fn push_long_text(out: &mut String, text: &str, escape_first_space: bool) {
    let mut rest = text;
    if escape_first_space && let Some(after) = text.strip_prefix(' ') {
        out.push_str("\\u{20}");
        rest = after;
    }
    push_escaped(out, rest);
}

impl Printer {
    // ---- output ----

    fn w(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.bol {
            for _ in 0..self.indent {
                self.out.push_str("    ");
            }
            self.bol = false;
        }
        self.out.push_str(text);
    }

    /// Writes text that starts a brace group (`{`, `{}`, `{ `). It ends a pending `fn` head,
    /// unless the token reader takes this `{` for a record type or the configuration record of
    /// a head (after `->`, `<`, `,` or `with`): it leaves the head pending.
    fn brace(&mut self, text: &str) {
        let record_in_head = ["-> ", "<", ", ", "with "]
            .iter()
            .any(|before| self.out.ends_with(before));
        if !record_in_head {
            self.open_head = false;
        }
        self.w(text);
    }

    fn nl(&mut self) {
        self.out.push('\n');
        self.bol = true;
    }

    fn is_flat(&self) -> bool {
        self.flat > 0
    }

    fn comma_sep<T>(&mut self, items: &[T], mut one: impl FnMut(&mut Self, &T)) {
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            one(self, item);
        }
    }

    /// `{` entries `}`, one entry per line separated by statement ends (blocks, prompt bodies,
    /// `par` and `race`); one line with `;` in a flat context.
    fn stmt_seq<T>(&mut self, items: &[T], mut one: impl FnMut(&mut Self, &T)) {
        if items.is_empty() {
            self.brace("{}");
            return;
        }
        self.brace("{");
        if self.is_flat() {
            self.w(" ");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    self.w("; ");
                }
                self.open_head = false;
                one(self, item);
            }
            self.w(" }");
            self.open_head = false;
            return;
        }
        self.indent += 1;
        for (i, item) in items.iter().enumerate() {
            self.nl();
            self.open_head = false;
            one(self, item);
            if i + 1 < items.len() && (self.open_head || !ends_statement(&self.out)) {
                self.w(";");
            }
        }
        self.open_head = false;
        self.indent -= 1;
        self.nl();
        self.w("}");
    }

    /// `{` entries `}`, comma-separated: one per line with a trailing comma, or on one line in a
    /// flat context.
    fn comma_block<T>(&mut self, items: &[T], mut one: impl FnMut(&mut Self, &T)) {
        if items.is_empty() {
            self.brace("{}");
            return;
        }
        self.brace("{");
        if self.is_flat() {
            self.w(" ");
            self.comma_sep(items, one);
            self.w(" }");
            return;
        }
        self.indent += 1;
        for item in items {
            self.nl();
            one(self, item);
            self.w(",");
        }
        self.indent -= 1;
        self.nl();
        self.w("}");
    }

    // ---- module and items ----

    /// Enters one level of nesting; past the limit writes `<too deep>` and says no.
    fn enter(&mut self) -> bool {
        if self.depth >= self.max_depth {
            self.w("<too deep>");
            return false;
        }
        self.depth += 1;
        true
    }

    fn module(&mut self, module: &Module) {
        let mut first = true;
        if let Some(meta) = &module.meta {
            self.w("meta ");
            self.record_fields(&meta.fields);
            self.nl();
            first = false;
        }
        for item in &module.items {
            if !first {
                self.nl();
            }
            first = false;
            self.item(item);
        }
    }

    fn docs(&mut self, docs: &[Doc]) {
        for doc in docs {
            self.w("///");
            self.w(&doc.text);
            self.nl();
        }
    }

    fn attribute(&mut self, attr: &Attribute) {
        self.w("@");
        self.ident(&attr.name);
        if let Some(args) = &attr.args {
            self.args(args);
        }
    }

    fn item(&mut self, item: &Item) {
        self.open_head = false;
        self.docs(&item.docs);
        for attr in &item.attrs {
            self.attribute(attr);
            self.nl();
        }
        if item.public {
            self.w("pub ");
        }
        match &item.kind {
            ItemKind::Import(import) => self.import(import),
            ItemKind::Type(decl) => {
                self.w("type ");
                self.ident(&decl.name);
                self.type_params(&decl.params);
                self.w(" = ");
                self.ty(&decl.ty);
            }
            ItemKind::Enum(decl) => self.enum_decl(decl),
            ItemKind::Const(decl) => {
                self.w("const ");
                self.ident(&decl.name);
                if let Some(ty) = &decl.ty {
                    self.w(": ");
                    self.ty(ty);
                }
                self.w(" = ");
                self.expr(&decl.value);
            }
            ItemKind::Fn(decl) => {
                self.w("fn ");
                self.ident(&decl.name);
                self.type_params(&decl.type_params);
                self.params(&decl.params);
                self.ret_type(decl.ret.as_ref());
                self.w(" ");
                self.block(&decl.body);
            }
            ItemKind::Harness(decl) => {
                self.w("harness ");
                self.ident(&decl.name);
                self.params(&decl.params);
                self.ret_type(decl.ret.as_ref());
                if let Some(config) = &decl.config {
                    self.w(" with ");
                    self.record(config);
                }
                self.w(" ");
                self.block(&decl.body);
            }
            ItemKind::Prompt(decl) => {
                self.w("prompt ");
                self.ident(&decl.name);
                self.params(&decl.params);
                self.w(" ");
                self.prompt_body(&decl.body);
            }
            ItemKind::State(decl) => {
                self.w("state: ");
                self.ty(&decl.ty);
                self.w(" = ");
                self.expr(&decl.value);
            }
            ItemKind::Test(decl) => {
                self.w("test ");
                self.string(&decl.name);
                self.w(" ");
                self.block(&decl.body);
            }
        }
        if self.open_head || !ends_statement(&self.out) {
            self.w(";");
        }
        self.open_head = false;
        self.nl();
    }

    fn import(&mut self, import: &Import) {
        self.w("import ");
        match &import.path {
            ImportPath::File(lit) => self.string(lit),
            ImportPath::Source(source) => self.source_name(source),
        }
        if let Some(alias) = &import.alias {
            self.w(" as ");
            self.ident(alias);
        }
        if let Some(names) = &import.names {
            self.w(" { ");
            self.comma_sep(names, Self::ident);
            self.w(" }");
        }
    }

    fn source_name(&mut self, source: &SourceName) {
        if let Some(name) = &source.source {
            self.ident(name);
            self.w(":");
        }
        for (i, part) in source.parts.iter().enumerate() {
            if i > 0 {
                self.w("/");
            }
            self.path(part);
        }
        if let Some(version) = &source.version {
            self.w(" @");
            self.string(version);
        }
    }

    fn enum_decl(&mut self, decl: &EnumDecl) {
        self.w("enum ");
        self.ident(&decl.name);
        self.type_params(&decl.params);
        self.w(" ");
        self.comma_block(&decl.variants, Self::variant);
    }

    fn variant(&mut self, variant: &Variant) {
        self.ident(&variant.name);
        if let Some(fields) = &variant.fields {
            self.w(" ");
            self.record_type(fields);
        }
    }

    fn type_params(&mut self, params: &[Ident]) {
        if params.is_empty() {
            return;
        }
        self.w("<");
        self.comma_sep(params, Self::ident);
        self.w(">");
    }

    fn params(&mut self, params: &[Param]) {
        self.w("(");
        self.comma_sep(params, |p, param| {
            p.ident(&param.name);
            p.w(": ");
            p.ty(&param.ty);
            if let Some(default) = &param.default {
                p.w(" = ");
                p.expr(default);
            }
        });
        self.w(")");
    }

    fn ret_type(&mut self, ret: Option<&Type>) {
        if let Some(ty) = ret {
            self.w(" -> ");
            self.ty(ty);
        }
    }

    // ---- names ----

    fn ident(&mut self, ident: &Ident) {
        self.w(&ident.name);
    }

    /// `a.b.c`
    fn path(&mut self, path: &[Ident]) {
        for (i, ident) in path.iter().enumerate() {
            if i > 0 {
                self.w(".");
            }
            self.ident(ident);
        }
    }

    // ---- types ----

    fn ty(&mut self, ty: &Type) {
        if self.enter() {
            self.ty_inner(ty);
            self.depth -= 1;
        }
    }

    fn ty_inner(&mut self, ty: &Type) {
        match &ty.kind {
            TypeKind::Error => self.w("<error>"),
            TypeKind::Named { path, args } => {
                self.path(path);
                if !args.is_empty() {
                    self.w("<");
                    self.comma_sep(args, Self::ty);
                    self.w(">");
                }
            }
            TypeKind::List(inner) => {
                self.w("[");
                self.type_depth += 1;
                self.ty(inner);
                self.type_depth -= 1;
                self.w("]");
            }
            TypeKind::Record(fields) => self.record_type(fields),
            TypeKind::Tuple(items) => {
                self.w("(");
                self.type_depth += 1;
                self.comma_sep(items, Self::ty);
                self.type_depth -= 1;
                self.w(")");
            }
            TypeKind::Fn { params, ret } => self.fn_type("fn", params, ret),
            TypeKind::Harness { params, ret } => self.fn_type("harness", params, ret),
            TypeKind::Option(inner) => {
                self.ty(inner);
                self.w("?");
            }
        }
    }

    fn fn_type(&mut self, word: &str, params: &[Type], ret: &Type) {
        self.w(word);
        if self.type_depth == 0 {
            self.open_head = true;
        }
        self.w("(");
        self.type_depth += 1;
        self.comma_sep(params, Self::ty);
        self.type_depth -= 1;
        self.w(") -> ");
        self.ty(ret);
    }

    fn record_type(&mut self, fields: &[FieldType]) {
        let documented = fields.iter().any(|f| !f.docs.is_empty());
        if documented {
            self.comma_block(fields, Self::field_type);
            return;
        }
        if fields.is_empty() {
            self.brace("{}");
            return;
        }
        self.brace("{ ");
        self.comma_sep(fields, Self::field_type);
        self.w(" }");
    }

    fn field_type(&mut self, field: &FieldType) {
        self.docs(&field.docs);
        for attr in &field.attrs {
            self.attribute(attr);
            self.w(" ");
        }
        self.ident(&field.name);
        if field.optional {
            self.w("?");
        }
        self.w(": ");
        self.ty(&field.ty);
        if let Some(description) = &field.description {
            self.w(" ");
            self.string(description);
        }
    }

    // ---- blocks and statements ----

    fn block(&mut self, block: &Block) {
        self.stmt_seq(&block.stmts, Self::stmt);
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let { pattern, ty, value } => {
                self.w("let ");
                self.pattern(pattern);
                if let Some(ty) = ty {
                    self.w(": ");
                    self.ty(ty);
                }
                self.w(" = ");
                self.expr(value);
            }
            StmtKind::Var { name, ty, value } => {
                self.w("var ");
                self.ident(name);
                if let Some(ty) = ty {
                    self.w(": ");
                    self.ty(ty);
                }
                self.w(" = ");
                self.expr(value);
            }
            StmtKind::Assign { target, op, value } => {
                self.expr(target);
                self.w(" ");
                self.w(op.as_str());
                self.w(" ");
                self.expr(value);
            }
            StmtKind::Defer(block) => {
                self.w("defer ");
                self.block(block);
            }
            StmtKind::Expr(expr) => self.expr(expr),
        }
    }

    // ---- expressions ----

    fn args(&mut self, args: &[Arg]) {
        self.w("(");
        self.comma_sep(args, |p, arg| {
            if let Some(name) = &arg.name {
                p.ident(name);
                p.w(": ");
            }
            p.expr(&arg.value);
        });
        self.w(")");
    }

    fn record(&mut self, record: &RecordLit) {
        self.record_fields(&record.fields);
    }

    /// `{ a: 1, b, ..c }`. A lone shorthand field gets a trailing comma (see the module docs).
    fn record_fields(&mut self, fields: &[FieldInit]) {
        if fields.is_empty() {
            self.brace("{}");
            return;
        }
        self.brace("{ ");
        self.comma_sep(fields, |p, field| match field {
            FieldInit::Field { name, value } => {
                p.ident(name);
                p.w(": ");
                p.expr(value);
            }
            FieldInit::Shorthand(name) => p.ident(name),
            FieldInit::Spread(expr) => {
                p.w("..");
                p.expr(expr);
            }
        });
        if matches!(fields, [FieldInit::Shorthand(_)]) {
            self.w(",");
        }
        self.w(" }");
    }

    fn list_item(&mut self, item: &ListItem) {
        if item.spread {
            self.w("..");
        }
        self.expr(&item.expr);
    }

    fn condition(&mut self, cond: &Condition) {
        match cond {
            Condition::Expr(expr) => self.expr(expr),
            Condition::Let { pattern, value } => {
                self.w("let ");
                self.pattern(pattern);
                self.w(" = ");
                self.expr(value);
            }
        }
    }

    fn arm(&mut self, arm: &Arm) {
        self.pattern(&arm.pattern);
        if let Some(guard) = &arm.guard {
            self.w(" if ");
            self.expr(guard);
        }
        self.w(" => ");
        self.expr(&arm.body);
    }

    fn arms(&mut self, arms: &[Arm]) {
        self.comma_block(arms, Self::arm);
    }

    fn expr(&mut self, expr: &Expr) {
        if self.enter() {
            self.expr_inner(expr);
            self.depth -= 1;
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per expression form, in the order of the AST"
    )]
    fn expr_inner(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Error => self.w("<error>"),
            ExprKind::Lit(lit) => self.lit(lit),
            ExprKind::Name(name) => self.ident(name),
            ExprKind::Paren(inner) => {
                self.w("(");
                self.expr(inner);
                self.w(")");
            }
            ExprKind::Tuple(items) => {
                self.w("(");
                self.comma_sep(items, Self::expr);
                if items.len() == 1 {
                    self.w(",");
                }
                self.w(")");
            }
            ExprKind::List(items) => {
                self.w("[");
                self.comma_sep(items, Self::list_item);
                self.w("]");
            }
            ExprKind::Record(record) => self.record(record),
            ExprKind::Variant { path, fields } => {
                self.path(path);
                self.w(" ");
                self.record(fields);
            }
            ExprKind::Block(block) => self.block(block),
            ExprKind::Unary { op, operand } => {
                self.w(match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "not ",
                });
                self.expr(operand);
            }
            ExprKind::Binary { op, lhs, rhs } => {
                self.expr(lhs);
                if matches!(op, BinaryOp::Range | BinaryOp::RangeInclusive) {
                    self.w(op.as_str());
                } else {
                    self.w(" ");
                    self.w(op.as_str());
                    self.w(" ");
                }
                self.expr(rhs);
            }
            ExprKind::Call {
                callee,
                type_args,
                args,
            } => {
                self.expr(callee);
                if !type_args.is_empty() {
                    self.w("::<");
                    self.comma_sep(type_args, Self::ty);
                    self.w(">");
                }
                self.args(args);
            }
            ExprKind::Member {
                object,
                name,
                optional,
            } => {
                self.expr(object);
                self.w(if *optional { "?." } else { "." });
                self.ident(name);
            }
            ExprKind::Index { object, index } => {
                self.expr(object);
                self.w("[");
                self.expr(index);
                self.w("]");
            }
            ExprKind::WithOverride { call, config } => {
                self.expr(call);
                self.w(" with ");
                self.record(config);
            }
            ExprKind::If { cond, then, else_ } => {
                self.w("if ");
                self.condition(cond);
                self.w(" ");
                self.block(then);
                match else_.as_deref() {
                    Some(Else::If(next)) => {
                        self.w(" else ");
                        self.expr(next);
                    }
                    Some(Else::Block(block)) => {
                        self.w(" else ");
                        self.block(block);
                    }
                    None => {}
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.w("match ");
                self.expr(scrutinee);
                self.w(" ");
                self.arms(arms);
            }
            ExprKind::For {
                pattern,
                iter,
                body,
            } => {
                self.w("for ");
                self.pattern(pattern);
                self.w(" in ");
                self.expr(iter);
                self.w(" ");
                self.block(body);
            }
            ExprKind::While { cond, body } => {
                self.w("while ");
                self.condition(cond);
                self.w(" ");
                self.block(body);
            }
            ExprKind::Loop(body) => {
                self.w("loop ");
                self.block(body);
            }
            ExprKind::Repeat { count, body } => {
                self.w("repeat ");
                self.expr(count);
                self.w(" ");
                self.block(body);
            }
            ExprKind::Try { body, catch } => {
                self.w("try ");
                self.block(body);
                self.w(" catch ");
                match catch {
                    Catch::Arms(arms) => self.arms(arms),
                    Catch::Bind { name, body } => {
                        self.ident(name);
                        self.w(" ");
                        self.block(body);
                    }
                }
            }
            ExprKind::Par(branches) => {
                self.w("par ");
                self.stmt_seq(branches, Self::expr);
            }
            ExprKind::Race(branches) => {
                self.w("race ");
                self.stmt_seq(branches, Self::expr);
            }
            ExprKind::WithTimeout {
                duration,
                body,
                else_,
            } => {
                self.w("with timeout ");
                self.expr(duration);
                self.w(" ");
                self.block(body);
                if let Some(else_) = else_ {
                    self.w(" else ");
                    self.block(else_);
                }
            }
            ExprKind::With { config, body } => {
                self.w("with ");
                self.record(config);
                self.w(" ");
                self.block(body);
            }
            ExprKind::Do(body) => {
                self.w("do ");
                self.block(body);
            }
            ExprKind::Closure { params, body } => {
                self.closure_params(params);
                self.w(" ");
                self.expr(body);
            }
            ExprKind::FnExpr { params, ret, body } => {
                self.w("fn");
                self.params(params);
                self.ret_type(ret.as_ref());
                self.w(" ");
                self.block(body);
            }
            ExprKind::Prompt(body) => {
                self.w("prompt ");
                self.prompt_body(body);
            }
            ExprKind::Return(value) => self.jump("return", value.as_deref()),
            ExprKind::Break(value) => self.jump("break", value.as_deref()),
            ExprKind::Continue => self.w("continue"),
            ExprKind::Raise(value) => {
                self.w("raise ");
                self.expr(value);
            }
        }
    }

    fn jump(&mut self, word: &str, value: Option<&Expr>) {
        self.w(word);
        if let Some(value) = value {
            self.w(" ");
            self.expr(value);
        }
    }

    fn closure_params(&mut self, params: &[ClosureParam]) {
        if params.is_empty() {
            self.w("||");
            return;
        }
        self.w("|");
        self.comma_sep(params, |p, param| {
            p.pattern(&param.pattern);
            if let Some(ty) = &param.ty {
                p.w(": ");
                p.ty(ty);
            }
        });
        self.w("|");
    }

    // ---- literals ----

    fn lit(&mut self, lit: &Lit) {
        match lit {
            Lit::Int(v) => self.w(&v.to_string()),
            Lit::Quantity(v) => self.w(&quantity(*v)),
            Lit::Float(v) => self.w(&format!("{v:?}")),
            Lit::Duration(v) => self.w(&duration(*v)),
            Lit::Money(v) => self.w(&money(*v)),
            Lit::Bool(true) => self.w("true"),
            Lit::Bool(false) => self.w("false"),
            Lit::None => self.w("none"),
            Lit::Str(lit) => self.string(lit),
        }
    }

    fn string(&mut self, lit: &StringLit) {
        match (lit.raw, lit.long) {
            (true, false) => {
                self.w("r\"");
                self.raw_text(lit);
                self.w("\"");
            }
            (true, true) => self.raw_long(lit),
            (false, false) => {
                self.w("\"");
                self.string_parts(&lit.parts, true);
                self.w("\"");
            }
            (false, true) if self.is_flat() => {
                self.w("\"\"\"");
                self.flat_long(lit);
                self.w("\"\"\"");
            }
            (false, true) => self.long(lit),
        }
    }

    fn raw_text(&mut self, lit: &StringLit) {
        for part in &lit.parts {
            if let StrPart::Text(text) = part {
                self.w(text);
            }
        }
    }

    /// The parts of a short string: escaped text, and interpolations printed flat.
    fn string_parts(&mut self, parts: &[StrPart], flat: bool) {
        for part in parts {
            match part {
                StrPart::Text(text) => {
                    let mut escaped = String::new();
                    push_escaped(&mut escaped, text);
                    self.w(&escaped);
                }
                StrPart::Interp { .. } => self.interp(part, flat),
            }
        }
    }

    fn interp(&mut self, part: &StrPart, flat: bool) {
        let StrPart::Interp { expr, spec, .. } = part else {
            return;
        };
        self.w("{");
        if flat {
            self.flat += 1;
        }
        self.expr(expr);
        if flat {
            self.flat -= 1;
        }
        if let Some(spec) = spec {
            self.w(":");
            self.w(spec);
        }
        self.w("}");
    }

    /// The body of a long string printed on one line, inside a short string's interpolation.
    fn flat_long(&mut self, lit: &StringLit) {
        let lines = long_lines(&lit.parts);
        // The dedent strips the indentation of the one line unless the line is blank.
        let mut first_line: Vec<Seg<'_>> = Vec::new();
        for (i, line) in lines.into_iter().enumerate() {
            if i > 0 {
                first_line.push(Seg::Text("\n".to_owned()));
            }
            first_line.extend(line);
        }
        let (blank, indent) = line_shape(&first_line);
        let mut escape = !blank && indent > 0;
        for seg in &first_line {
            match seg {
                Seg::Text(text) => {
                    let mut escaped = String::new();
                    push_long_text(&mut escaped, text, escape);
                    self.w(&escaped);
                    escape = false;
                }
                Seg::Interp(part) => {
                    escape = false;
                    self.interp(part, true);
                }
            }
        }
    }

    /// A long string on several lines.
    fn long(&mut self, lit: &StringLit) {
        let lines = long_lines(&lit.parts);
        let shapes: Vec<(bool, usize)> = lines.iter().map(|l| line_shape(l)).collect();
        let has_unindented = shapes.iter().any(|(blank, indent)| !blank && *indent == 0);
        let base = if has_unindented { self.indent * 4 } else { 0 };
        // With no line at indentation 0 the dedent would strip some of the text: write the first
        // space of the first line that has text as an escape, which counts as no indentation.
        let escaped_line = if has_unindented {
            None
        } else {
            shapes.iter().position(|(blank, _)| !blank)
        };
        self.w("\"\"\"");
        let empty = lit.parts.is_empty();
        if !empty {
            for (i, line) in lines.iter().enumerate() {
                self.out.push('\n');
                self.bol = false;
                if line.iter().all(Seg::is_empty) {
                    continue;
                }
                for _ in 0..base {
                    self.out.push(' ');
                }
                let mut escape = escaped_line == Some(i);
                for seg in line {
                    match seg {
                        Seg::Text(text) => {
                            let mut escaped = String::new();
                            push_long_text(&mut escaped, text, escape);
                            self.out.push_str(&escaped);
                            escape = false;
                        }
                        Seg::Interp(part) => {
                            escape = false;
                            self.interp(part, false);
                        }
                    }
                }
            }
        }
        // The closing line is blank before its `"""`, so any indentation does: the statement's.
        self.out.push('\n');
        for _ in 0..self.indent * 4 {
            self.out.push(' ');
        }
        self.out.push_str("\"\"\"");
        self.bol = false;
    }

    /// A raw long string: one line in a flat context, otherwise at column 0.
    fn raw_long(&mut self, lit: &StringLit) {
        self.w("r\"\"\"");
        if self.is_flat() {
            self.raw_text(lit);
            self.w("\"\"\"");
            return;
        }
        if !lit.parts.is_empty() {
            self.out.push('\n');
            self.bol = false;
            self.raw_text(lit);
        }
        self.out.push('\n');
        self.out.push_str("\"\"\"");
        self.bol = false;
    }

    // ---- patterns ----

    fn pattern(&mut self, pattern: &Pattern) {
        if self.enter() {
            self.pattern_inner(pattern);
            self.depth -= 1;
        }
    }

    fn pattern_inner(&mut self, pattern: &Pattern) {
        match &pattern.kind {
            PatternKind::Error => self.w("<error>"),
            PatternKind::Wildcard => self.w("_"),
            PatternKind::Lit(lit) => self.lit(lit),
            PatternKind::Name(name) => self.ident(name),
            PatternKind::Bind { name, pattern } => {
                self.ident(name);
                self.w(" @ ");
                self.pattern(pattern);
            }
            PatternKind::Variant { path, fields } => {
                self.path(path);
                if let Some(fields) = fields {
                    self.w(" ");
                    self.record_pattern(fields);
                }
            }
            PatternKind::Tuple(items) => {
                self.w("(");
                self.comma_sep(items, Self::pattern);
                if items.len() == 1 {
                    self.w(",");
                }
                self.w(")");
            }
            PatternKind::List(items) => {
                self.w("[");
                self.comma_sep(items, |p, item| match item {
                    ListPattern::Pattern(pattern) => p.pattern(pattern),
                    ListPattern::Rest(name) => {
                        p.w("..");
                        if let Some(name) = name {
                            p.ident(name);
                        }
                    }
                });
                self.w("]");
            }
            PatternKind::Record(record) => self.record_pattern(record),
            PatternKind::Or(alternatives) => {
                for (i, alternative) in alternatives.iter().enumerate() {
                    if i > 0 {
                        self.w(" | ");
                    }
                    self.pattern(alternative);
                }
            }
        }
    }

    fn record_pattern(&mut self, record: &RecordPattern) {
        if record.fields.is_empty() && !record.rest {
            self.brace("{}");
            return;
        }
        self.brace("{ ");
        self.comma_sep(&record.fields, |p, field: &FieldPattern| {
            p.ident(&field.name);
            if let Some(pattern) = &field.pattern {
                p.w(": ");
                p.pattern(pattern);
            }
        });
        if record.rest {
            if !record.fields.is_empty() {
                self.w(", ");
            }
            self.w("..");
        }
        self.w(" }");
    }

    // ---- prompts ----

    fn prompt_body(&mut self, body: &PromptBody) {
        self.stmt_seq(&body.sections, Self::section);
    }

    fn section(&mut self, section: &Section) {
        match &section.kind {
            SectionKind::System(e) => self.keyword_expr("system", e),
            SectionKind::Instruct(e) => self.keyword_expr("instruct", e),
            SectionKind::Task(e) => self.keyword_expr("task", e),
            SectionKind::Steps(e) => self.keyword_expr("steps", e),
            SectionKind::Context(e) => self.keyword_expr("context", e),
            SectionKind::Note(e) => self.keyword_expr("note", e),
            SectionKind::Include(e) => self.keyword_expr("include", e),
            SectionKind::Data { value, label } => self.labelled("data", value, label.as_ref()),
            SectionKind::Image { value, label } => self.labelled("image", value, label.as_ref()),
            SectionKind::Example { input, output } => {
                self.w("example ");
                self.expr(input);
                self.w(" -> ");
                self.expr(output);
            }
            SectionKind::Format(ty) => {
                self.w("format ");
                self.ty(ty);
            }
            SectionKind::If { cond, then, else_ } => {
                self.w("if ");
                self.expr(cond);
                self.w(" ");
                self.prompt_body(then);
                if let Some(else_) = else_ {
                    self.w(" else ");
                    self.prompt_body(else_);
                }
            }
            SectionKind::For {
                pattern,
                iter,
                body,
            } => {
                self.w("for ");
                self.pattern(pattern);
                self.w(" in ");
                self.expr(iter);
                self.w(" ");
                self.prompt_body(body);
            }
        }
    }

    fn keyword_expr(&mut self, word: &str, expr: &Expr) {
        self.w(word);
        self.w(" ");
        self.expr(expr);
    }

    fn labelled(&mut self, word: &str, value: &Expr, label: Option<&Expr>) {
        self.keyword_expr(word, value);
        if let Some(label) = label {
            self.w(" as ");
            self.expr(label);
        }
    }
}

// ---- canonical literal units ----

/// `20k`, `3M`, `1.5k`: `M` for a whole number of millions, else `k`, with the fraction that
/// the value needs (at most three digits).
fn quantity(value: i64) -> String {
    if value == 0 {
        return "0k".to_owned();
    }
    if value % 1_000_000 == 0 {
        return format!("{}M", value / 1_000_000);
    }
    if value % 1000 == 0 {
        return format!("{}k", value / 1000);
    }
    let whole = value / 1000;
    let fraction = format!("{:03}", (value % 1000).abs());
    format!("{whole}.{}k", fraction.trim_end_matches('0'))
}

/// `2h`, `5m`, `30s`, `500ms`: the largest unit that holds the value whole. A value that is not
/// a whole number of milliseconds has no spelling; it prints truncated.
fn duration(micros: i64) -> String {
    if micros == 0 {
        return "0s".to_owned();
    }
    for (unit, scale) in [
        ("h", 3_600_000_000),
        ("m", 60_000_000),
        ("s", 1_000_000),
        ("ms", 1000),
    ] {
        if micros % scale == 0 {
            return format!("{}{unit}", micros / scale);
        }
    }
    format!("{}ms", micros / 1000)
}

/// `$12`, `$0.50`, `$0.000001`: whole dollars bare, otherwise at least two decimals.
fn money(micros: i64) -> String {
    let dollars = micros / 1_000_000;
    let fraction = micros % 1_000_000;
    if fraction == 0 {
        return format!("${dollars}");
    }
    let digits = format!("{fraction:06}");
    let trimmed = digits.trim_end_matches('0');
    format!("${dollars}.{trimmed:0<2}")
}
