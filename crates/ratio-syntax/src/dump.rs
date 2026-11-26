//! The span-free tree dump: a deterministic S-expression of a [`Module`].
//!
//! The conformance runner compares it with a file's `tree:` expectation, and the round-trip test
//! compares the dumps of two parses. It leaves out every span, so two trees read from different
//! text (a source and its canonical print) dump alike exactly when they are the same tree.
//!
//! # Notation
//!
//! The dump is one line, tokens separated by single spaces. Every node is one parenthesised
//! form `(tag child …)`; the tag names the node and the children follow in a fixed order.
//!
//! - A **sequence** is written `[a b c]` (empty: `[]`), so a node's arity never varies.
//! - An **absent option** is `-`; a present one is written as the value itself.
//! - A **name** is written bare (`x`, `Verdict`); a **string** (text, doc text, a format spec) is
//!   written in double quotes with `\\`, `\"`, `\n`, `\r`, `\t` and `\u{hex}` escapes.
//! - Numbers are written as the tree holds them: `(int 42)`, `(quantity 20000)`,
//!   `(duration 500000)` (microseconds), `(money 500000)` (millionths of a dollar), and floats
//!   in Rust's shortest round-trip spelling (`(float 0.5)`, `(float 1e-7)`).
//! - Operators are bare symbols (`|>`, `==`, `+=`).
//!
//! A string literal is `(str KIND [PART …])` with KIND one of `short`, `long`, `raw`, `raw-long`;
//! a part is a quoted text or `(interp EXPR SPEC)`.
//!
//! ```text
//! module   = (module META [ITEM …])      META = - | (meta [FIELD …])
//! item     = (item [DOC …] [ATTR …] PUB KIND)   DOC = "text"   PUB = pub | -
//! attr     = (attr NAME ARGS)            ARGS = - | [(arg NAME EXPR) …]   NAME = - when positional
//! kind     = (import PATH ALIAS NAMES) | (type NAME [P …] TYPE) | (enum NAME [P …] [VARIANT …])
//!          | (const NAME TYPE EXPR) | (fn NAME [P …] [PARAM …] RET BLOCK)
//!          | (harness NAME [PARAM …] RET CONFIG BLOCK) | (prompt NAME [PARAM …] PROMPT-BODY)
//!          | (state TYPE EXPR) | (test STR BLOCK)
//! path     = (file STR) | (source SOURCE [[NAME …] …] VERSION)
//! variant  = (variant NAME FIELDS)       FIELDS = - | [FIELD-TYPE …]
//! param    = (param NAME TYPE DEFAULT)
//! type     = (type-error) | (named [NAME …] [TYPE …]) | (list TYPE) | (record-type [FIELD-TYPE …])
//!          | (tuple [TYPE …]) | (fn-type [TYPE …] TYPE) | (harness-type [TYPE …] TYPE)
//!          | (option TYPE)
//! field-type = (field-type [DOC …] [ATTR …] NAME OPTIONAL TYPE DESCRIPTION)   OPTIONAL = ? | -
//! block    = (block [STMT …])
//! stmt     = (let PATTERN TYPE EXPR) | (var NAME TYPE EXPR) | (assign OP TARGET EXPR)
//!          | (defer BLOCK) | (expr EXPR)
//! expr     = (error) | (int N) | (quantity N) | (float F) | (duration N) | (money N)
//!          | (bool true) | (none) | STR | (name NAME) | (paren EXPR) | (tuple [EXPR …])
//!          | (list [LIST-ITEM …]) | RECORD | (variant [NAME …] RECORD) | BLOCK
//!          | (neg EXPR) | (not EXPR) | (bin OP EXPR EXPR)
//!          | (call EXPR [TYPE …] [(arg NAME EXPR) …])
//!          | (member EXPR NAME) | (opt-member EXPR NAME) | (index EXPR EXPR)
//!          | (with-override EXPR RECORD)
//!          | (if COND BLOCK ELSE) | (match EXPR [ARM …]) | (for PATTERN EXPR BLOCK)
//!          | (while COND BLOCK) | (loop BLOCK) | (repeat EXPR BLOCK)
//!          | (try BLOCK CATCH) | (par [EXPR …]) | (race [EXPR …])
//!          | (with-timeout EXPR BLOCK ELSE-BLOCK) | (with RECORD BLOCK) | (do BLOCK)
//!          | (closure [(cparam PATTERN TYPE) …] EXPR) | (fn-expr [PARAM …] RET BLOCK)
//!          | (prompt-expr PROMPT-BODY) | (return EXPR) | (break EXPR) | (continue) | (raise EXPR)
//! list-item = EXPR | (spread EXPR)
//! record   = (record [FIELD …])    FIELD = (field NAME EXPR) | (shorthand NAME) | (spread EXPR)
//! cond     = EXPR | (let-cond PATTERN EXPR)
//! else     = - | (else-if EXPR) | (else BLOCK)
//! arm      = (arm PATTERN GUARD EXPR)
//! catch    = (catch-arms [ARM …]) | (catch-bind NAME BLOCK)
//! pattern  = (pattern-error) | (wild) | (pat-lit LIT) | (pat-name NAME) | (bind NAME PATTERN)
//!          | (pat-variant [NAME …] RECORD-PATTERN) | (pat-tuple [PATTERN …])
//!          | (pat-list [PATTERN | (rest NAME) …]) | (pat-record RECORD-PATTERN)
//!          | (or [PATTERN …])
//! record-pattern = (record-pat [(field-pat NAME PATTERN) …] REST)   REST = .. | -
//! prompt-body = (prompt-body [SECTION …])
//! section  = (system E) | (instruct E) | (task E) | (data E LABEL) | (image E LABEL)
//!          | (example IN OUT) | (format TYPE) | (steps E) | (context E) | (note E) | (include E)
//!          | (if-section E PROMPT-BODY ELSE) | (for-section PATTERN E PROMPT-BODY)
//! ```
//!
//! In the table `LIT` is an expression literal form, and a literal in a pattern is
//! `(pat-lit LIT)`. The dump is stable: a change to it is a change to the conformance suite.

use std::fmt::Write as _;

use crate::stack::{self, FALLBACK_DEPTH};

use crate::ast::{
    Arg, Arm, Attribute, Block, Catch, Condition, Doc, Else, EnumDecl, Expr, ExprKind, FieldInit,
    FieldPattern, FieldType, FnDecl, HarnessDecl, Ident, Import, ImportPath, Item, ItemKind,
    ListItem, ListPattern, Lit, Module, Param, Pattern, PatternKind, PromptBody, PromptDecl,
    RecordLit, RecordPattern, Section, SectionKind, SourceName, StateDecl, Stmt, StmtKind, StrPart,
    StringLit, TestDecl, Type, TypeDecl, TypeKind, UnaryOp, Variant,
};

/// The span-free S-expression of `module` (the notation is described in the module documentation).
#[must_use]
pub fn dump(module: &Module) -> String {
    stack::run(|big| dump_on(module, big)).unwrap_or_else(|| "(internal-error)".to_owned())
}

/// [`dump`] on the current thread; `big` says it has the big stack (see [`crate::stack`]).
pub(crate) fn dump_on(module: &Module, big: bool) -> String {
    let mut d = Dump {
        out: String::new(),
        depth: 0,
        max_depth: if big { usize::MAX } else { FALLBACK_DEPTH },
    };
    d.module(module);
    d.out
}

struct Dump {
    out: String,
    /// The nesting of expressions, types and patterns being dumped, and the most allowed.
    depth: usize,
    max_depth: usize,
}

impl Dump {
    /// A space, unless the output is at its start or just after an opening delimiter.
    fn sep(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with(['(', '[']) {
            self.out.push(' ');
        }
    }

    fn atom(&mut self, text: &str) {
        self.sep();
        self.out.push_str(text);
    }

    fn name(&mut self, ident: &Ident) {
        if ident.name.is_empty() {
            self.string("");
        } else {
            self.atom(&ident.name);
        }
    }

    fn string(&mut self, text: &str) {
        self.sep();
        self.out.push('"');
        for c in text.chars() {
            match c {
                '\\' => self.out.push_str("\\\\"),
                '"' => self.out.push_str("\\\""),
                '\n' => self.out.push_str("\\n"),
                '\r' => self.out.push_str("\\r"),
                '\t' => self.out.push_str("\\t"),
                c if c.is_control() => {
                    let _ = write!(self.out, "\\u{{{:x}}}", u32::from(c));
                }
                c => self.out.push(c),
            }
        }
        self.out.push('"');
    }

    fn none(&mut self) {
        self.atom("-");
    }

    fn node(&mut self, tag: &str, body: impl FnOnce(&mut Self)) {
        self.sep();
        self.out.push('(');
        self.out.push_str(tag);
        body(self);
        self.out.push(')');
    }

    fn seq<T>(&mut self, items: &[T], mut one: impl FnMut(&mut Self, &T)) {
        self.sep();
        self.out.push('[');
        for item in items {
            one(self, item);
        }
        self.out.push(']');
    }

    fn opt<T>(&mut self, value: Option<&T>, one: impl FnOnce(&mut Self, &T)) {
        match value {
            Some(value) => one(self, value),
            None => self.none(),
        }
    }

    fn names(&mut self, names: &[Ident]) {
        self.seq(names, Self::name);
    }

    // ---- module and items ----

    /// Enters one level of nesting; past the limit writes `(too-deep)` and says no.
    fn enter(&mut self) -> bool {
        if self.depth >= self.max_depth {
            self.sep();
            self.out.push_str("(too-deep)");
            return false;
        }
        self.depth += 1;
        true
    }

    fn module(&mut self, module: &Module) {
        self.node("module", |d| {
            d.opt(module.meta.as_ref(), |d, meta| {
                d.node("meta", |d| d.seq(&meta.fields, Self::field_init));
            });
            d.seq(&module.items, Self::item);
        });
    }

    fn docs(&mut self, docs: &[Doc]) {
        self.seq(docs, |d, doc| d.string(&doc.text));
    }

    fn attrs(&mut self, attrs: &[Attribute]) {
        self.seq(attrs, Self::attribute);
    }

    fn attribute(&mut self, attr: &Attribute) {
        self.node("attr", |d| {
            d.name(&attr.name);
            d.opt(attr.args.as_ref(), |d, args| d.seq(args, Self::arg));
        });
    }

    fn arg(&mut self, arg: &Arg) {
        self.node("arg", |d| {
            d.opt(arg.name.as_ref(), Self::name_ref);
            d.expr(&arg.value);
        });
    }

    fn name_ref(&mut self, ident: &Ident) {
        self.name(ident);
    }

    fn item(&mut self, item: &Item) {
        self.node("item", |d| {
            d.docs(&item.docs);
            d.attrs(&item.attrs);
            d.atom(if item.public { "pub" } else { "-" });
            d.item_kind(&item.kind);
        });
    }

    fn item_kind(&mut self, kind: &ItemKind) {
        match kind {
            ItemKind::Import(import) => self.import(import),
            ItemKind::Type(decl) => self.type_decl(decl),
            ItemKind::Enum(decl) => self.enum_decl(decl),
            ItemKind::Const(decl) => self.node("const", |d| {
                d.name(&decl.name);
                d.opt(decl.ty.as_ref(), Self::ty);
                d.expr(&decl.value);
            }),
            ItemKind::Fn(decl) => self.fn_decl(decl),
            ItemKind::Harness(decl) => self.harness_decl(decl),
            ItemKind::Prompt(decl) => self.prompt_decl(decl),
            ItemKind::State(decl) => self.state_decl(decl),
            ItemKind::Test(decl) => self.test_decl(decl),
        }
    }

    fn import(&mut self, import: &Import) {
        self.node("import", |d| {
            match &import.path {
                ImportPath::File(lit) => d.node("file", |d| d.string_lit(lit)),
                ImportPath::Source(source) => d.source_name(source),
            }
            d.opt(import.alias.as_ref(), Self::name_ref);
            d.opt(import.names.as_ref(), |d, names| d.names(names));
        });
    }

    fn source_name(&mut self, source: &SourceName) {
        self.node("source", |d| {
            d.opt(source.source.as_ref(), Self::name_ref);
            d.seq(&source.parts, |d, part| d.names(part));
            d.opt(source.version.as_ref(), Self::string_lit_ref);
        });
    }

    fn string_lit_ref(&mut self, lit: &StringLit) {
        self.string_lit(lit);
    }

    fn type_decl(&mut self, decl: &TypeDecl) {
        self.node("type", |d| {
            d.name(&decl.name);
            d.names(&decl.params);
            d.ty(&decl.ty);
        });
    }

    fn enum_decl(&mut self, decl: &EnumDecl) {
        self.node("enum", |d| {
            d.name(&decl.name);
            d.names(&decl.params);
            d.seq(&decl.variants, Self::variant);
        });
    }

    fn variant(&mut self, variant: &Variant) {
        self.node("variant", |d| {
            d.name(&variant.name);
            d.opt(variant.fields.as_ref(), |d, fields| {
                d.seq(fields, Self::field_type);
            });
        });
    }

    fn fn_decl(&mut self, decl: &FnDecl) {
        self.node("fn", |d| {
            d.name(&decl.name);
            d.names(&decl.type_params);
            d.seq(&decl.params, Self::param);
            d.opt(decl.ret.as_ref(), Self::ty);
            d.block(&decl.body);
        });
    }

    fn harness_decl(&mut self, decl: &HarnessDecl) {
        self.node("harness", |d| {
            d.name(&decl.name);
            d.seq(&decl.params, Self::param);
            d.opt(decl.ret.as_ref(), Self::ty);
            d.opt(decl.config.as_ref(), Self::record);
            d.block(&decl.body);
        });
    }

    fn prompt_decl(&mut self, decl: &PromptDecl) {
        self.node("prompt", |d| {
            d.name(&decl.name);
            d.seq(&decl.params, Self::param);
            d.prompt_body(&decl.body);
        });
    }

    fn state_decl(&mut self, decl: &StateDecl) {
        self.node("state", |d| {
            d.ty(&decl.ty);
            d.expr(&decl.value);
        });
    }

    fn test_decl(&mut self, decl: &TestDecl) {
        self.node("test", |d| {
            d.string_lit(&decl.name);
            d.block(&decl.body);
        });
    }

    fn param(&mut self, param: &Param) {
        self.node("param", |d| {
            d.name(&param.name);
            d.ty(&param.ty);
            d.opt(param.default.as_ref(), Self::expr_ref);
        });
    }

    fn expr_ref(&mut self, expr: &Expr) {
        self.expr(expr);
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
            TypeKind::Error => self.node("type-error", |_| {}),
            TypeKind::Named { path, args } => self.node("named", |d| {
                d.names(path);
                d.seq(args, Self::ty);
            }),
            TypeKind::List(inner) => self.node("list", |d| d.ty(inner)),
            TypeKind::Record(fields) => {
                self.node("record-type", |d| d.seq(fields, Self::field_type));
            }
            TypeKind::Tuple(items) => self.node("tuple", |d| d.seq(items, Self::ty)),
            TypeKind::Fn { params, ret } => self.node("fn-type", |d| {
                d.seq(params, Self::ty);
                d.ty(ret);
            }),
            TypeKind::Harness { params, ret } => self.node("harness-type", |d| {
                d.seq(params, Self::ty);
                d.ty(ret);
            }),
            TypeKind::Option(inner) => self.node("option", |d| d.ty(inner)),
        }
    }

    fn field_type(&mut self, field: &FieldType) {
        self.node("field-type", |d| {
            d.docs(&field.docs);
            d.attrs(&field.attrs);
            d.name(&field.name);
            d.atom(if field.optional { "?" } else { "-" });
            d.ty(&field.ty);
            d.opt(field.description.as_ref(), Self::string_lit_ref);
        });
    }

    // ---- blocks and statements ----

    fn block(&mut self, block: &Block) {
        self.node("block", |d| d.seq(&block.stmts, Self::stmt));
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let { pattern, ty, value } => self.node("let", |d| {
                d.pattern(pattern);
                d.opt(ty.as_ref(), Self::ty);
                d.expr(value);
            }),
            StmtKind::Var { name, ty, value } => self.node("var", |d| {
                d.name(name);
                d.opt(ty.as_ref(), Self::ty);
                d.expr(value);
            }),
            StmtKind::Assign { target, op, value } => self.node("assign", |d| {
                d.atom(op.as_str());
                d.expr(target);
                d.expr(value);
            }),
            StmtKind::Defer(block) => self.node("defer", |d| {
                if d.enter() {
                    d.block(block);
                    d.depth -= 1;
                }
            }),
            StmtKind::Expr(expr) => self.node("expr", |d| d.expr(expr)),
        }
    }

    // ---- expressions ----

    fn lit(&mut self, lit: &Lit) {
        match lit {
            Lit::Int(v) => self.node("int", |d| d.atom(&v.to_string())),
            Lit::Quantity(v) => self.node("quantity", |d| d.atom(&v.to_string())),
            Lit::Float(v) => self.node("float", |d| d.atom(&format!("{v:?}"))),
            Lit::Duration(v) => self.node("duration", |d| d.atom(&v.to_string())),
            Lit::Money(v) => self.node("money", |d| d.atom(&v.to_string())),
            Lit::Bool(v) => self.node("bool", |d| d.atom(if *v { "true" } else { "false" })),
            Lit::None => self.node("none", |_| {}),
            Lit::Str(lit) => self.string_lit(lit),
        }
    }

    fn string_lit(&mut self, lit: &StringLit) {
        let kind = match (lit.raw, lit.long) {
            (false, false) => "short",
            (false, true) => "long",
            (true, false) => "raw",
            (true, true) => "raw-long",
        };
        self.node("str", |d| {
            d.atom(kind);
            d.seq(&lit.parts, |d, part| match part {
                StrPart::Text(text) => d.string(text),
                StrPart::Interp { expr, spec, .. } => d.node("interp", |d| {
                    d.expr(expr);
                    d.opt(spec.as_ref(), |d, spec| d.string(spec));
                }),
            });
        });
    }

    fn record(&mut self, record: &RecordLit) {
        self.node("record", |d| d.seq(&record.fields, Self::field_init));
    }

    fn field_init(&mut self, field: &FieldInit) {
        match field {
            FieldInit::Field { name, value } => self.node("field", |d| {
                d.name(name);
                d.expr(value);
            }),
            FieldInit::Shorthand(name) => self.node("shorthand", |d| d.name(name)),
            FieldInit::Spread(expr) => self.node("spread", |d| d.expr(expr)),
        }
    }

    fn list_item(&mut self, item: &ListItem) {
        if item.spread {
            self.node("spread", |d| d.expr(&item.expr));
        } else {
            self.expr(&item.expr);
        }
    }

    fn condition(&mut self, cond: &Condition) {
        match cond {
            Condition::Expr(expr) => self.expr(expr),
            Condition::Let { pattern, value } => self.node("let-cond", |d| {
                d.pattern(pattern);
                d.expr(value);
            }),
        }
    }

    fn arm(&mut self, arm: &Arm) {
        self.node("arm", |d| {
            d.pattern(&arm.pattern);
            d.opt(arm.guard.as_ref(), Self::expr_ref);
            d.expr(&arm.body);
        });
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
            ExprKind::Error => self.node("error", |_| {}),
            ExprKind::Lit(lit) => self.lit(lit),
            ExprKind::Name(name) => self.node("name", |d| d.name(name)),
            ExprKind::Paren(inner) => self.node("paren", |d| d.expr(inner)),
            ExprKind::Tuple(items) => self.node("tuple", |d| d.seq(items, Self::expr_ref)),
            ExprKind::List(items) => self.node("list", |d| d.seq(items, Self::list_item)),
            ExprKind::Record(record) => self.record(record),
            ExprKind::Variant { path, fields } => self.node("variant", |d| {
                d.names(path);
                d.record(fields);
            }),
            ExprKind::Block(block) => self.block(block),
            ExprKind::Unary { op, operand } => {
                let tag = match op {
                    UnaryOp::Neg => "neg",
                    UnaryOp::Not => "not",
                };
                self.node(tag, |d| d.expr(operand));
            }
            ExprKind::Binary { op, lhs, rhs } => self.node("bin", |d| {
                d.atom(op.as_str());
                d.expr(lhs);
                d.expr(rhs);
            }),
            ExprKind::Call {
                callee,
                type_args,
                args,
            } => self.node("call", |d| {
                d.expr(callee);
                d.seq(type_args, Self::ty);
                d.seq(args, Self::arg);
            }),
            ExprKind::Member {
                object,
                name,
                optional,
            } => self.node(if *optional { "opt-member" } else { "member" }, |d| {
                d.expr(object);
                d.name(name);
            }),
            ExprKind::Index { object, index } => self.node("index", |d| {
                d.expr(object);
                d.expr(index);
            }),
            ExprKind::WithOverride { call, config } => self.node("with-override", |d| {
                d.expr(call);
                d.record(config);
            }),
            ExprKind::If { cond, then, else_ } => self.node("if", |d| {
                d.condition(cond);
                d.block(then);
                d.opt(else_.as_deref(), |d, else_| match else_ {
                    Else::If(expr) => d.node("else-if", |d| d.expr(expr)),
                    Else::Block(block) => d.node("else", |d| d.block(block)),
                });
            }),
            ExprKind::Match { scrutinee, arms } => self.node("match", |d| {
                d.expr(scrutinee);
                d.seq(arms, Self::arm);
            }),
            ExprKind::For {
                pattern,
                iter,
                body,
            } => self.node("for", |d| {
                d.pattern(pattern);
                d.expr(iter);
                d.block(body);
            }),
            ExprKind::While { cond, body } => self.node("while", |d| {
                d.condition(cond);
                d.block(body);
            }),
            ExprKind::Loop(body) => self.node("loop", |d| d.block(body)),
            ExprKind::Repeat { count, body } => self.node("repeat", |d| {
                d.expr(count);
                d.block(body);
            }),
            ExprKind::Try { body, catch } => self.node("try", |d| {
                d.block(body);
                match catch {
                    Catch::Arms(arms) => d.node("catch-arms", |d| d.seq(arms, Self::arm)),
                    Catch::Bind { name, body } => d.node("catch-bind", |d| {
                        d.name(name);
                        d.block(body);
                    }),
                }
            }),
            ExprKind::Par(branches) => self.node("par", |d| d.seq(branches, Self::expr_ref)),
            ExprKind::Race(branches) => self.node("race", |d| d.seq(branches, Self::expr_ref)),
            ExprKind::WithTimeout {
                duration,
                body,
                else_,
            } => self.node("with-timeout", |d| {
                d.expr(duration);
                d.block(body);
                d.opt(else_.as_ref(), Self::block);
            }),
            ExprKind::With { config, body } => self.node("with", |d| {
                d.record(config);
                d.block(body);
            }),
            ExprKind::Do(body) => self.node("do", |d| d.block(body)),
            ExprKind::Closure { params, body } => self.node("closure", |d| {
                d.seq(params, |d, param| {
                    d.node("cparam", |d| {
                        d.pattern(&param.pattern);
                        d.opt(param.ty.as_ref(), Self::ty);
                    });
                });
                d.expr(body);
            }),
            ExprKind::FnExpr { params, ret, body } => self.node("fn-expr", |d| {
                d.seq(params, Self::param);
                d.opt(ret.as_ref(), Self::ty);
                d.block(body);
            }),
            ExprKind::Prompt(body) => self.node("prompt-expr", |d| d.prompt_body(body)),
            ExprKind::Return(value) => self.node("return", |d| {
                d.opt(value.as_deref(), Self::expr_ref);
            }),
            ExprKind::Break(value) => self.node("break", |d| {
                d.opt(value.as_deref(), Self::expr_ref);
            }),
            ExprKind::Continue => self.node("continue", |_| {}),
            ExprKind::Raise(value) => self.node("raise", |d| d.expr(value)),
        }
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
            PatternKind::Error => self.node("pattern-error", |_| {}),
            PatternKind::Wildcard => self.node("wild", |_| {}),
            PatternKind::Lit(lit) => self.node("pat-lit", |d| d.lit(lit)),
            PatternKind::Name(name) => self.node("pat-name", |d| d.name(name)),
            PatternKind::Bind { name, pattern } => self.node("bind", |d| {
                d.name(name);
                d.pattern(pattern);
            }),
            PatternKind::Variant { path, fields } => self.node("pat-variant", |d| {
                d.names(path);
                d.opt(fields.as_ref(), Self::record_pattern);
            }),
            PatternKind::Tuple(items) => {
                self.node("pat-tuple", |d| d.seq(items, Self::pattern));
            }
            PatternKind::List(items) => self.node("pat-list", |d| {
                d.seq(items, |d, item| match item {
                    ListPattern::Pattern(pattern) => d.pattern(pattern),
                    ListPattern::Rest(name) => d.node("rest", |d| {
                        d.opt(name.as_ref(), Self::name_ref);
                    }),
                });
            }),
            PatternKind::Record(record) => {
                self.node("pat-record", |d| d.record_pattern(record));
            }
            PatternKind::Or(alternatives) => {
                self.node("or", |d| d.seq(alternatives, Self::pattern));
            }
        }
    }

    fn record_pattern(&mut self, record: &RecordPattern) {
        self.node("record-pat", |d| {
            d.seq(&record.fields, |d, field: &FieldPattern| {
                d.node("field-pat", |d| {
                    d.name(&field.name);
                    d.opt(field.pattern.as_ref(), Self::pattern_ref);
                });
            });
            d.atom(if record.rest { ".." } else { "-" });
        });
    }

    fn pattern_ref(&mut self, pattern: &Pattern) {
        self.pattern(pattern);
    }

    // ---- prompts ----

    fn prompt_body(&mut self, body: &PromptBody) {
        if self.enter() {
            self.prompt_body_inner(body);
            self.depth -= 1;
        }
    }

    fn prompt_body_inner(&mut self, body: &PromptBody) {
        self.node("prompt-body", |d| d.seq(&body.sections, Self::section));
    }

    fn section(&mut self, section: &Section) {
        match &section.kind {
            SectionKind::System(e) => self.node("system", |d| d.expr(e)),
            SectionKind::Instruct(e) => self.node("instruct", |d| d.expr(e)),
            SectionKind::Task(e) => self.node("task", |d| d.expr(e)),
            SectionKind::Data { value, label } => self.node("data", |d| {
                d.expr(value);
                d.opt(label.as_ref(), Self::expr_ref);
            }),
            SectionKind::Image { value, label } => self.node("image", |d| {
                d.expr(value);
                d.opt(label.as_ref(), Self::expr_ref);
            }),
            SectionKind::Example { input, output } => self.node("example", |d| {
                d.expr(input);
                d.expr(output);
            }),
            SectionKind::Format(ty) => self.node("format", |d| d.ty(ty)),
            SectionKind::Steps(e) => self.node("steps", |d| d.expr(e)),
            SectionKind::Context(e) => self.node("context", |d| d.expr(e)),
            SectionKind::Note(e) => self.node("note", |d| d.expr(e)),
            SectionKind::Include(e) => self.node("include", |d| d.expr(e)),
            SectionKind::If { cond, then, else_ } => self.node("if-section", |d| {
                d.expr(cond);
                d.prompt_body(then);
                d.opt(else_.as_ref(), Self::prompt_body);
            }),
            SectionKind::For {
                pattern,
                iter,
                body,
            } => self.node("for-section", |d| {
                d.pattern(pattern);
                d.expr(iter);
                d.prompt_body(body);
            }),
        }
    }
}
