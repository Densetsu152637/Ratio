//! The round trip of the canonical printer (stage 2): `parse → print → parse` gives the same
//! tree (equal span-free dumps), and printing is idempotent (`print(parse(print(x))) ==
//! print(x)`).
//!
//! Three sources of programs feed it: the clean files of `conformance/syntax` (and the ones that
//! are statements, wrapped in a function), a fixed-seed generator that builds trees covering
//! every AST variant (printed, parsed, and compared with the tree it started from), and mutated
//! corpus text that still parses. Unit tests pin each printing rule down by its exact output.

mod support;

use std::collections::BTreeSet;
use std::fs;

use ratio_syntax::ast::{
    Arg, Arm, AssignOp, Attribute, BinaryOp, Block, Catch, ClosureParam, Condition, ConstDecl, Doc,
    Else, EnumDecl, Expr, ExprKind, FieldInit, FieldPattern, FieldType, FnDecl, HarnessDecl, Ident,
    Import, ImportPath, Item, ItemKind, ListItem, ListPattern, Lit, Meta, Module, Param, Pattern,
    PatternKind, PromptBody, PromptDecl, RecordLit, RecordPattern, Section, SectionKind,
    SourceName, StateDecl, Stmt, StmtKind, StrPart, StringLit, TestDecl, Type, TypeDecl, TypeKind,
    UnaryOp, Variant,
};
use ratio_syntax::{Span, dump, parse, print};

/// Checks `parse → print → parse` and the idempotence of printing for `source`. Returns `None`
/// when the source does not parse cleanly (it is not a round-trip case), else the verdict.
fn check(source: &str) -> Option<Result<(), String>> {
    let first = parse(source);
    if !first.is_clean() {
        return None;
    }
    let printed = print(&first.module);
    let second = parse(&printed);
    if !second.is_clean() {
        return Some(Err(format!(
            "the printed text does not parse cleanly: {:?}\n--- printed ---\n{printed}",
            second.diagnostics
        )));
    }
    let (a, b) = (dump(&first.module), dump(&second.module));
    if a != b {
        return Some(Err(format!(
            "the dumps differ\n  first : {a}\n  second: {b}\n--- printed ---\n{printed}"
        )));
    }
    let again = print(&second.module);
    if again != printed {
        return Some(Err(format!(
            "printing is not idempotent\n--- first ---\n{printed}\n--- second ---\n{again}"
        )));
    }
    Some(Ok(()))
}

fn assert_round_trips(source: &str) {
    match check(source) {
        Some(Ok(())) => {}
        Some(Err(why)) => panic!("{source:?}: {why}"),
        None => panic!(
            "{source:?} does not parse cleanly: {:?}",
            parse(source).diagnostics
        ),
    }
}

// ---- the corpus ----

fn corpus_files() -> Vec<(String, String)> {
    let dir = support::conformance_root().join("syntax");
    let mut paths: Vec<_> = fs::read_dir(&dir)
        .expect("conformance/syntax exists")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ratio"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let text = fs::read_to_string(&p).expect("readable");
            (p.display().to_string(), text)
        })
        .collect()
}

/// A corpus text, or the same text as the body of a function (many files are statements for the
/// token reader, which a module cannot hold at its top level).
fn variants(text: &str) -> [String; 2] {
    [
        text.to_owned(),
        format!("fn corpus_wrapper() {{\n{text}\n}}\n"),
    ]
}

#[test]
fn every_clean_conformance_file_round_trips() {
    let mut failures = Vec::new();
    let mut ran = 0;
    for (name, text) in corpus_files() {
        for source in variants(&text) {
            if let Some(verdict) = check(&source) {
                ran += 1;
                if let Err(why) = verdict {
                    failures.push(format!("{name}: {why}"));
                }
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(ran >= 25, "only {ran} corpus files parsed cleanly");
}

#[test]
fn mutated_conformance_files_that_still_parse_round_trip() {
    let mut rng = Rng(0xC0DE_5EED_0BAD_F00D);
    let mut ran = 0;
    for (name, text) in corpus_files() {
        for source in variants(&text) {
            let chars: Vec<char> = source.chars().collect();
            for case in 0..80 {
                let mut mutated = chars.clone();
                for _ in 0..=rng.below(2) {
                    let at = rng.below(mutated.len() + 1);
                    match rng.below(3) {
                        0 => {
                            let end = (at + rng.below(10)).min(mutated.len());
                            mutated.drain(at..end);
                        }
                        1 => {
                            let end = (at + rng.below(10)).min(mutated.len());
                            let chunk: Vec<char> = mutated[at..end].to_vec();
                            let to = rng.below(mutated.len() + 1);
                            for (i, c) in chunk.into_iter().enumerate() {
                                mutated.insert(to + i, c);
                            }
                        }
                        _ => {
                            let c = [' ', '\n', ',', ')', '1', 'x', '"', '}'][rng.below(8)];
                            mutated.insert(at, c);
                        }
                    }
                }
                let source: String = mutated.into_iter().collect();
                if let Some(verdict) = check(&source) {
                    ran += 1;
                    if let Err(why) = verdict {
                        panic!("{name} mutation {case}: {why}\nsource: {source:?}");
                    }
                }
            }
        }
    }
    assert!(ran >= 100, "only {ran} mutations parsed cleanly");
}

// ---- the generator ----

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        usize::try_from(self.next() % u64::try_from(n).unwrap_or(1)).unwrap_or(0)
    }

    fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

const Z: Span = Span {
    start: 0,
    end: 0,
    line: 1,
    col: 1,
    end_line: 1,
    end_col: 1,
};

fn id(name: &str) -> Ident {
    Ident {
        name: name.to_owned(),
        span: Z,
    }
}

fn ex(kind: ExprKind) -> Expr {
    Expr { kind, span: Z }
}

fn ty(kind: TypeKind) -> Type {
    Type { kind, span: Z }
}

fn pt(kind: PatternKind) -> Pattern {
    Pattern { kind, span: Z }
}

fn st(kind: StmtKind) -> Stmt {
    Stmt { kind, span: Z }
}

fn bx(e: Expr) -> Box<Expr> {
    Box::new(e)
}

fn name_expr(name: &str) -> Expr {
    ex(ExprKind::Name(id(name)))
}

/// The binding level of an expression, with the loosest (a jump, or a closure, whose body runs on
/// as far as it can) at 0 and a primary at 12. Chapter 02's table, plus the postfix level 11.
fn level(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Return(_)
        | ExprKind::Break(_)
        | ExprKind::Continue
        | ExprKind::Raise(_)
        | ExprKind::Closure { .. } => 0,
        ExprKind::Binary { op, .. } => op.level(),
        ExprKind::Unary {
            op: UnaryOp::Not, ..
        } => 5,
        ExprKind::Unary {
            op: UnaryOp::Neg, ..
        } => 10,
        ExprKind::Call { .. }
        | ExprKind::Member { .. }
        | ExprKind::Index { .. }
        | ExprKind::WithOverride { .. }
        | ExprKind::Variant { .. } => 11,
        _ => 12,
    }
}

/// `e`, in parentheses when it binds looser than `min`.
fn wrap(e: Expr, min: u8) -> Expr {
    if level(&e) < min {
        ex(ExprKind::Paren(bx(e)))
    } else {
        e
    }
}

const LOWER: &[&str] = &[
    "a", "b", "c", "x", "y", "n", "name", "value", "items", "count", "foo", "bar", "_x", "z9",
];
const UPPER: &[&str] = &["Int", "Str", "Bool", "Note", "Plan", "Pair", "Verdict", "T"];
const VARIANTS: &[&str] = &["Denied", "Failed", "Okay", "Big"];
/// Names that are keywords or contextual words: legal after a `.`.
const MEMBERS: &[&str] = &[
    "loop", "type", "match", "fn", "state", "meta", "in", "is", "len", "get", "map",
];
const SPECS: &[&str] = &[".2", ">6", "<6", "json"];

struct Gen {
    rng: Rng,
    /// Inside the interpolation of a short string: no line breaks.
    flat: bool,
    /// The kind of every string literal made so far, to check the coverage.
    strings: BTreeSet<&'static str>,
}

impl Gen {
    fn lower(&mut self) -> Ident {
        id(self.rng.pick(LOWER))
    }

    fn upper(&mut self) -> Ident {
        id(self.rng.pick(UPPER))
    }

    fn many<T>(&mut self, min: usize, max: usize, mut one: impl FnMut(&mut Self) -> T) -> Vec<T> {
        let n = min + self.rng.below(max - min + 1);
        (0..n).map(|_| one(self)).collect()
    }

    fn opt<T>(&mut self, num: usize, den: usize, one: impl FnOnce(&mut Self) -> T) -> Option<T> {
        if self.rng.chance(num, den) {
            Some(one(self))
        } else {
            None
        }
    }

    // ---- literals ----

    fn lit(&mut self, d: u32) -> Lit {
        match self.rng.below(9) {
            0 => Lit::Int(self.rng.pick(&[0, 1, 7, 31, 42, 1000, 123_456, i64::MAX])),
            1 => Lit::Quantity(self.rng.pick(&[
                0,
                7,
                1000,
                1500,
                1234,
                20_000,
                3_000_000,
                1_000_500,
                1_500_000,
                i64::MAX,
            ])),
            2 => Lit::Float(self.rng.pick(&[
                0.5,
                2.0,
                1e-7,
                1e21,
                123_456.789,
                1.5e300,
                5e-324,
                f64::MAX,
                0.1,
                100.0,
            ])),
            3 => Lit::Duration(self.rng.pick(&[
                0,
                1000,
                500_000,
                1_500_000,
                30_000_000,
                90_000_000,
                300_000_000,
                7_200_000_000,
                9_223_372_036_854_775_000,
            ])),
            4 => Lit::Money(self.rng.pick(&[
                0,
                1,
                500_000,
                1_000_000,
                1_250_000,
                12_000_000,
                1_000_001,
                10_000_000_000,
                i64::MAX,
            ])),
            5 => Lit::Bool(self.rng.chance(1, 2)),
            6 => Lit::None,
            _ => Lit::Str(self.string(d)),
        }
    }

    fn text(&mut self, min: usize, max: usize, alphabet: &[char]) -> String {
        let n = min + self.rng.below(max - min + 1);
        (0..n).map(|_| self.rng.pick(alphabet)).collect()
    }

    fn string(&mut self, d: u32) -> StringLit {
        match self.rng.below(8) {
            0 => self.raw_string(false),
            1 => self.raw_string(true),
            2 | 3 => self.cooked_string(d, true),
            _ => self.cooked_string(d, false),
        }
    }

    fn raw_string(&mut self, long: bool) -> StringLit {
        self.strings.insert(if long { "raw-long" } else { "raw" });
        let text = if long && !self.flat {
            // Lines with their own indentation, the first one without any (the dedent of the
            // text would otherwise remove some).
            let pieces = [
                "a",
                "b c",
                "  x",
                "\t y",
                "{z}",
                "say \"hi\"",
                "",
                "  ",
                "é",
                "\\n",
            ];
            let first = self.rng.pick(&["a", "word", "{x}", "\\"]);
            let mut text = first.to_owned();
            for _ in 0..self.rng.below(4) {
                text.push('\n');
                text.push_str(self.rng.pick(&pieces));
            }
            if self.rng.chance(1, 4) {
                text.push('\n');
            }
            text
        } else {
            // No quote (it would end the string), no line break; a long one in flat context
            // must not start with a space either.
            let alphabet = ['a', 'b', ' ', '{', '}', '\\', 'é', 'x', '/', '.'];
            let mut text = self.text(0, 6, &alphabet);
            if long && text.starts_with(' ') {
                text.insert(0, 'q');
            }
            text
        };
        StringLit {
            long,
            raw: true,
            parts: if text.is_empty() {
                Vec::new()
            } else {
                vec![StrPart::Text(text)]
            },
            span: Z,
        }
    }

    fn cooked_string(&mut self, d: u32, long: bool) -> StringLit {
        self.strings.insert(if long { "long" } else { "short" });
        let alphabet: &[char] = if long {
            &[
                'a', 'b', ' ', ' ', ' ', '\n', '\n', '"', '{', '}', '\\', '\t', '\r', 'é', '😀',
                '\u{1}', '\u{a0}', '/', ':', '0',
            ]
        } else {
            &[
                'a', 'b', ' ', ' ', '"', '{', '}', '\\', '\n', '\t', '\r', 'é', '😀', '\u{1}',
                '\u{a0}', '/', ':', '0',
            ]
        };
        let mut parts = Vec::new();
        for _ in 0..self.rng.below(4) {
            if self.rng.chance(1, 3) && d > 0 {
                let outer = self.flat;
                if !long {
                    self.flat = true;
                }
                let expr = self.simple(d - 1);
                self.flat = outer;
                let spec = self.opt(1, 4, |g| g.rng.pick(SPECS).to_owned());
                parts.push(StrPart::Interp {
                    expr,
                    spec,
                    span: Z,
                });
            } else {
                let text = self.text(1, 6, alphabet);
                if let Some(StrPart::Text(_)) = parts.last() {
                    continue;
                }
                parts.push(StrPart::Text(text));
            }
        }
        StringLit {
            long,
            raw: false,
            parts,
            span: Z,
        }
    }

    fn plain_string(&mut self) -> StringLit {
        let alphabet = ['a', 'b', ' ', '.', '/', '^', '1', 'é'];
        let text = self.text(1, 8, &alphabet);
        StringLit {
            long: false,
            raw: false,
            parts: vec![StrPart::Text(text)],
            span: Z,
        }
    }

    // ---- expressions ----

    fn args(&mut self, d: u32) -> Vec<Arg> {
        let mut args = self.many(0, 2, |g| Arg {
            name: None,
            value: g.expr(d.saturating_sub(1)),
        });
        for _ in 0..self.rng.below(3) {
            let name = self.lower();
            args.push(Arg {
                name: Some(name),
                value: self.expr(d.saturating_sub(1)),
            });
        }
        args
    }

    fn record(&mut self, d: u32) -> RecordLit {
        let fields = self.many(0, 3, |g| match g.rng.below(5) {
            0 => FieldInit::Shorthand(g.lower()),
            1 => FieldInit::Spread(g.simple(d.saturating_sub(1))),
            _ => FieldInit::Field {
                name: g.lower(),
                value: g.expr(d.saturating_sub(1)),
            },
        });
        RecordLit { fields, span: Z }
    }

    fn block(&mut self, d: u32, min: usize, max: usize) -> Block {
        let stmts = self.many(min, max, |g| g.stmt(d.saturating_sub(1)));
        Block { stmts, span: Z }
    }

    /// An expression with no brace construct, closure or jump outside delimiters: legal in the
    /// head of an `if` or `for`, and in an interpolation.
    fn simple(&mut self, d: u32) -> Expr {
        if d == 0 {
            return if self.rng.chance(1, 3) {
                ex(ExprKind::Lit(self.lit(0)))
            } else {
                ex(ExprKind::Name(self.lower()))
            };
        }
        let d1 = d - 1;
        match self.rng.below(14) {
            1 | 2 => ex(ExprKind::Name(self.lower())),
            3 => {
                let inner = self.expr(d1);
                ex(ExprKind::Paren(bx(inner)))
            }
            4 => {
                let callee = self.postfix_base(d1);
                let args = self.args(d1);
                ex(ExprKind::Call {
                    callee: bx(callee),
                    type_args: Vec::new(),
                    args,
                })
            }
            5 => {
                let object = self.postfix_base(d1);
                ex(ExprKind::Member {
                    object: bx(object),
                    name: self.member_name(),
                    optional: self.rng.chance(1, 3),
                })
            }
            6 => {
                let object = self.postfix_base(d1);
                let index = self.simple(d1);
                ex(ExprKind::Index {
                    object: bx(object),
                    index: bx(index),
                })
            }
            7 => {
                let op = self.rng.pick(&[
                    BinaryOp::Add,
                    BinaryOp::Sub,
                    BinaryOp::Mul,
                    BinaryOp::Eq,
                    BinaryOp::Lt,
                    BinaryOp::Coalesce,
                    BinaryOp::And,
                    BinaryOp::Range,
                ]);
                self.binary(op, d1, true)
            }
            8 => {
                let operand = wrap(self.simple(d1), 10);
                ex(ExprKind::Unary {
                    op: UnaryOp::Neg,
                    operand: bx(operand),
                })
            }
            9 => {
                let operand = wrap(self.simple(d1), 5);
                ex(ExprKind::Unary {
                    op: UnaryOp::Not,
                    operand: bx(operand),
                })
            }
            10 => {
                let items = self.many(0, 3, |g| ListItem {
                    spread: g.rng.chance(1, 5),
                    expr: g.expr(d1),
                });
                ex(ExprKind::List(items))
            }
            11 => {
                let items = self.many(1, 3, |g| g.expr(d1));
                ex(ExprKind::Tuple(items))
            }
            12 => {
                let lhs = self.simple(d1);
                let rhs = self.simple(d1);
                Self::binary_of(BinaryOp::Pipe, lhs, rhs)
            }
            _ => ex(ExprKind::Lit(self.lit(d1))),
        }
    }

    fn member_name(&mut self) -> Ident {
        if self.rng.chance(1, 4) {
            id(self.rng.pick(MEMBERS))
        } else {
            self.lower()
        }
    }

    /// What may stand before a postfix operation.
    fn postfix_base(&mut self, d: u32) -> Expr {
        match self.rng.below(if d == 0 { 2 } else { 7 }) {
            0 | 1 => name_expr(self.rng.pick(LOWER)),
            2 => {
                let inner = self.expr(d - 1);
                ex(ExprKind::Paren(bx(inner)))
            }
            3 => {
                let object = self.postfix_base(d - 1);
                ex(ExprKind::Member {
                    object: bx(object),
                    name: self.member_name(),
                    optional: false,
                })
            }
            4 => {
                let callee = self.postfix_base(d - 1);
                let args = self.args(d - 1);
                ex(ExprKind::Call {
                    callee: bx(callee),
                    type_args: Vec::new(),
                    args,
                })
            }
            5 => {
                let items = self.many(0, 2, |g| ListItem {
                    spread: false,
                    expr: g.simple(d - 1),
                });
                ex(ExprKind::List(items))
            }
            _ => {
                let lit = self.cooked_string(0, false);
                ex(ExprKind::Lit(Lit::Str(lit)))
            }
        }
    }

    fn binary(&mut self, op: BinaryOp, d: u32, simple: bool) -> Expr {
        let make = |g: &mut Self| {
            if simple { g.simple(d) } else { g.expr(d) }
        };
        let lhs = make(self);
        let rhs = make(self);
        Self::binary_of(op, lhs, rhs)
    }

    /// `lhs op rhs`, with parentheses where the grammar's levels need them.
    fn binary_of(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
        let (lmin, rmin) = match op {
            BinaryOp::Pipe => (1, 11),
            BinaryOp::Coalesce => (3, 2),
            BinaryOp::Or => (3, 4),
            BinaryOp::And => (4, 5),
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge
            | BinaryOp::In
            | BinaryOp::Is => (7, 7),
            BinaryOp::Range | BinaryOp::RangeInclusive => (8, 8),
            BinaryOp::Add | BinaryOp::Sub => (8, 9),
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => (9, 10),
        };
        ex(ExprKind::Binary {
            op,
            lhs: bx(wrap(lhs, lmin)),
            rhs: bx(wrap(rhs, rmin)),
        })
    }

    fn condition(&mut self, d: u32) -> Condition {
        if self.rng.chance(1, 4) {
            Condition::Let {
                pattern: self.pattern(d.saturating_sub(1)),
                value: self.simple(d.saturating_sub(1)),
            }
        } else {
            Condition::Expr(self.simple(d.saturating_sub(1)))
        }
    }

    fn arms(&mut self, d: u32) -> Vec<Arm> {
        self.many(1, 3, |g| Arm {
            pattern: g.pattern(d.saturating_sub(1)),
            guard: g.opt(1, 4, |g| g.simple(d.saturating_sub(1))),
            body: g.expr(d.saturating_sub(1)),
            span: Z,
        })
    }

    #[allow(clippy::too_many_lines, reason = "one arm per expression form")]
    fn expr(&mut self, d: u32) -> Expr {
        if d == 0 {
            return match self.rng.below(5) {
                0 => ex(ExprKind::Lit(self.lit(0))),
                1 => ex(ExprKind::Continue),
                _ => ex(ExprKind::Name(self.lower())),
            };
        }
        let d1 = d - 1;
        match self.rng.below(40) {
            0..=3 => self.simple(d),
            4 => ex(ExprKind::Lit(self.lit(d1))),
            5 => {
                let inner = self.expr(d1);
                ex(ExprKind::Paren(bx(inner)))
            }
            6 => {
                let record = self.record(d1);
                ex(ExprKind::Record(record))
            }
            7 => {
                let mut path = if self.rng.chance(1, 3) {
                    vec![self.upper()]
                } else {
                    Vec::new()
                };
                path.push(id(self.rng.pick(VARIANTS)));
                let fields = self.record(d1);
                ex(ExprKind::Variant { path, fields })
            }
            8 => {
                let block = self.block(d1, 1, 3);
                ex(ExprKind::Block(block))
            }
            9 => {
                let op = self.rng.pick(&[
                    BinaryOp::Pipe,
                    BinaryOp::Coalesce,
                    BinaryOp::Or,
                    BinaryOp::And,
                    BinaryOp::Eq,
                    BinaryOp::Ne,
                    BinaryOp::Lt,
                    BinaryOp::Le,
                    BinaryOp::Gt,
                    BinaryOp::Ge,
                    BinaryOp::In,
                    BinaryOp::Is,
                    BinaryOp::Range,
                    BinaryOp::RangeInclusive,
                    BinaryOp::Add,
                    BinaryOp::Sub,
                    BinaryOp::Mul,
                    BinaryOp::Div,
                    BinaryOp::Rem,
                ]);
                self.binary(op, d1, false)
            }
            10 => {
                let callee = self.postfix_base(d1);
                let type_args = self.many(0, 2, |g| g.ty(d1.min(1)));
                let args = self.args(d1);
                ex(ExprKind::Call {
                    callee: bx(callee),
                    type_args,
                    args,
                })
            }
            11 => {
                let callee = name_expr(self.rng.pick(LOWER));
                let args = self.args(d1);
                let call = ex(ExprKind::Call {
                    callee: bx(callee),
                    type_args: Vec::new(),
                    args,
                });
                let config = self.record(d1);
                ex(ExprKind::WithOverride {
                    call: bx(call),
                    config,
                })
            }
            12 | 13 => self.if_expr(d1),
            14 | 15 => {
                let scrutinee = self.simple(d1);
                let arms = self.arms(d1);
                ex(ExprKind::Match {
                    scrutinee: bx(scrutinee),
                    arms,
                })
            }
            16 => {
                let pattern = self.pattern(d1);
                let iter = self.simple(d1);
                let body = self.block(d1, 0, 2);
                ex(ExprKind::For {
                    pattern,
                    iter: bx(iter),
                    body,
                })
            }
            17 => {
                let cond = self.condition(d1);
                let body = self.block(d1, 0, 2);
                ex(ExprKind::While {
                    cond: Box::new(cond),
                    body,
                })
            }
            18 => {
                let body = self.block(d1, 0, 2);
                ex(ExprKind::Loop(body))
            }
            19 => {
                let count = self.simple(d1);
                let body = self.block(d1, 0, 2);
                ex(ExprKind::Repeat {
                    count: bx(count),
                    body,
                })
            }
            20 | 21 => {
                let body = self.block(d1, 0, 2);
                let catch = if self.rng.chance(1, 3) {
                    Catch::Bind {
                        name: self.lower(),
                        body: self.block(d1, 0, 2),
                    }
                } else {
                    Catch::Arms(self.arms(d1))
                };
                ex(ExprKind::Try { body, catch })
            }
            22 => {
                let branches = self.many(1, 3, |g| g.expr(d1));
                ex(ExprKind::Par(branches))
            }
            23 => {
                let branches = self.many(1, 3, |g| g.expr(d1));
                ex(ExprKind::Race(branches))
            }
            24 => {
                let duration = ex(ExprKind::Lit(Lit::Duration(self.rng.pick(&[
                    1_000_000,
                    5_000_000,
                    600_000_000,
                ]))));
                let body = self.block(d1, 0, 2);
                let else_ = self.opt(1, 3, |g| g.block(d1, 0, 2));
                ex(ExprKind::WithTimeout {
                    duration: bx(duration),
                    body,
                    else_,
                })
            }
            25 => {
                let config = self.record(d1);
                let body = self.block(d1, 0, 2);
                ex(ExprKind::With { config, body })
            }
            26 => {
                let body = self.block(d1, 0, 2);
                ex(ExprKind::Do(body))
            }
            27 | 28 => {
                // At most one parameter: in a `match` arm `a => |x, y| match n { … }` the token reader
                // takes the comma for the end of the arm and the next `{` for a record (a limit of
                // the reader); the unit tests cover closures with several parameters.
                let params = self.many(0, 1, |g| {
                    let pattern = match g.rng.below(3) {
                        0 => pt(PatternKind::Wildcard),
                        1 => pt(PatternKind::Tuple(vec![
                            pt(PatternKind::Name(g.lower())),
                            pt(PatternKind::Wildcard),
                        ])),
                        _ => pt(PatternKind::Name(g.lower())),
                    };
                    ClosureParam {
                        pattern,
                        ty: g.opt(1, 4, |g| g.ty(1)),
                    }
                });
                let body = self.expr(d1);
                ex(ExprKind::Closure {
                    params,
                    body: bx(body),
                })
            }
            29 => {
                let params = self.params(d1);
                // No result type: in `{ f: fn() -> T { … } }` the token reader takes the body's
                // `{` for a record (a limit of the reader, not of the printer); the unit tests
                // cover function expressions with a result type.
                // A body that is not empty: in `f(a: fn() {})` the reader takes `{}` for a record.
                let body = self.block(d1, 1, 2);
                ex(ExprKind::FnExpr {
                    params,
                    ret: None,
                    body,
                })
            }
            30 | 31 => {
                let body = self.prompt_body(d1);
                ex(ExprKind::Prompt(body))
            }
            32 => {
                let value = self.opt(2, 3, |g| bx(g.expr(d1)));
                ex(ExprKind::Return(value))
            }
            33 => {
                let value = self.opt(1, 2, |g| bx(g.expr(d1)));
                ex(ExprKind::Break(value))
            }
            34 => ex(ExprKind::Continue),
            35 => {
                let value = self.expr(d1);
                ex(ExprKind::Raise(bx(value)))
            }
            36 => {
                let operand = wrap(self.expr(d1), 5);
                ex(ExprKind::Unary {
                    op: UnaryOp::Not,
                    operand: bx(operand),
                })
            }
            37 => {
                let operand = wrap(self.expr(d1), 10);
                ex(ExprKind::Unary {
                    op: UnaryOp::Neg,
                    operand: bx(operand),
                })
            }
            _ => {
                let items = self.many(0, 3, |g| ListItem {
                    spread: g.rng.chance(1, 5),
                    expr: g.expr(d1),
                });
                ex(ExprKind::List(items))
            }
        }
    }

    fn if_expr(&mut self, d: u32) -> Expr {
        let cond = self.condition(d);
        let then = self.block(d, 0, 2);
        let else_ = match self.rng.below(4) {
            0 => {
                let next = self.if_expr(d.saturating_sub(1));
                Some(Box::new(Else::If(next)))
            }
            1 => Some(Box::new(Else::Block(self.block(d, 0, 2)))),
            _ => None,
        };
        ex(ExprKind::If {
            cond: Box::new(cond),
            then,
            else_,
        })
    }

    // ---- statements ----

    fn place(&mut self, d: u32) -> Expr {
        match self.rng.below(if d == 0 { 1 } else { 3 }) {
            0 => name_expr(self.rng.pick(LOWER)),
            1 => {
                let object = self.place(d - 1);
                ex(ExprKind::Member {
                    object: bx(object),
                    name: self.member_name(),
                    optional: false,
                })
            }
            _ => {
                let object = self.place(d - 1);
                let index = self.simple(d - 1);
                ex(ExprKind::Index {
                    object: bx(object),
                    index: bx(index),
                })
            }
        }
    }

    fn stmt(&mut self, d: u32) -> Stmt {
        match self.rng.below(10) {
            0 | 1 => {
                let pattern = self.pattern(d);
                let ty = self.opt(1, 3, |g| g.ty(1));
                let value = self.expr(d);
                st(StmtKind::Let { pattern, ty, value })
            }
            2 => {
                let name = self.lower();
                let ty = self.opt(1, 3, |g| g.ty(1));
                let value = self.expr(d);
                st(StmtKind::Var { name, ty, value })
            }
            3 => {
                let target = self.place(d.min(2));
                let op = self.rng.pick(&[
                    AssignOp::Set,
                    AssignOp::Add,
                    AssignOp::Sub,
                    AssignOp::Mul,
                    AssignOp::Div,
                ]);
                let value = self.expr(d);
                st(StmtKind::Assign { target, op, value })
            }
            4 => {
                let block = self.block(d, 0, 2);
                st(StmtKind::Defer(block))
            }
            _ => st(StmtKind::Expr(self.expr(d))),
        }
    }

    // ---- types ----

    fn ty(&mut self, d: u32) -> Type {
        if d == 0 {
            return ty(TypeKind::Named {
                path: vec![self.upper()],
                args: Vec::new(),
            });
        }
        let d1 = d - 1;
        match self.rng.below(10) {
            0 | 1 => {
                let mut path = vec![self.upper()];
                if self.rng.chance(1, 4) {
                    path.insert(0, id("std"));
                }
                let args = self.many(0, 2, |g| g.ty(d1));
                ty(TypeKind::Named { path, args })
            }
            2 => {
                let inner = self.ty(d1);
                ty(TypeKind::List(Box::new(inner)))
            }
            3 => {
                let fields = self.field_types(d1);
                ty(TypeKind::Record(fields))
            }
            4 => {
                let items = self.many(0, 3, |g| g.ty(d1));
                ty(TypeKind::Tuple(items))
            }
            5 => {
                let params = self.many(0, 2, |g| g.ty(d1));
                let ret = self.ty(d1);
                ty(TypeKind::Fn {
                    params,
                    ret: Box::new(ret),
                })
            }
            6 => {
                let params = self.many(0, 2, |g| g.ty(d1));
                let ret = self.ty(d1);
                ty(TypeKind::Harness {
                    params,
                    ret: Box::new(ret),
                })
            }
            7 => {
                let mut inner = self.ty(d1);
                // `T?` on a function type would read back as part of its result type.
                while matches!(inner.kind, TypeKind::Fn { .. } | TypeKind::Harness { .. }) {
                    inner = self.ty(d1);
                }
                ty(TypeKind::Option(Box::new(inner)))
            }
            _ => ty(TypeKind::Named {
                path: vec![self.upper()],
                args: Vec::new(),
            }),
        }
    }

    /// A declared result type. The token reader takes a `fn` or `harness` type for the head of a
    /// function, and a second head in the result type of a function leaves the first one
    /// unresolved when the body's `{` comes (a limit of the reader, not of the printer).
    fn ret_ty(&mut self, d: u32) -> Type {
        loop {
            let t = self.ty(d);
            if !matches!(t.kind, TypeKind::Fn { .. } | TypeKind::Harness { .. }) {
                return t;
            }
        }
    }

    fn doc(&mut self) -> Doc {
        let text = self
            .rng
            .pick(&[" one", " two words", "", " / slashes", " a: b, c"]);
        Doc {
            text: text.to_owned(),
            span: Z,
        }
    }

    fn attribute(&mut self, d: u32) -> Attribute {
        Attribute {
            name: self.lower(),
            args: self.opt(1, 2, |g| g.args(d.min(1))),
            span: Z,
        }
    }

    fn field_types(&mut self, d: u32) -> Vec<FieldType> {
        self.many(0, 3, |g| FieldType {
            // A documentation line is a line break, which a short string's interpolation cannot
            // hold.
            docs: if g.flat {
                Vec::new()
            } else {
                let n = usize::from(g.rng.chance(1, 4));
                g.many(0, n, Gen::doc)
            },
            attrs: {
                let n = usize::from(g.rng.chance(1, 5));
                g.many(0, n, |g| g.attribute(0))
            },
            name: g.lower(),
            optional: g.rng.chance(1, 4),
            ty: g.ty(d),
            description: g.opt(1, 4, Gen::plain_string),
            span: Z,
        })
    }

    // ---- patterns ----

    fn pattern(&mut self, d: u32) -> Pattern {
        if d > 0 && self.rng.chance(1, 8) {
            let alternatives = self.many(2, 3, |g| g.alt_pattern(d - 1));
            return pt(PatternKind::Or(alternatives));
        }
        self.alt_pattern(d)
    }

    fn record_pattern(&mut self, d: u32) -> RecordPattern {
        let fields = self.many(0, 3, |g| FieldPattern {
            name: g.lower(),
            pattern: g.opt(1, 2, |g| g.pattern(d.saturating_sub(1))),
        });
        let rest = self.rng.chance(1, 3);
        RecordPattern {
            fields,
            rest,
            span: Z,
        }
    }

    fn alt_pattern(&mut self, d: u32) -> Pattern {
        let d1 = d.saturating_sub(1);
        if d == 0 {
            return match self.rng.below(3) {
                0 => pt(PatternKind::Wildcard),
                1 => pt(PatternKind::Lit(Lit::Int(self.rng.pick(&[0, 1, 2])))),
                _ => pt(PatternKind::Name(self.lower())),
            };
        }
        match self.rng.below(11) {
            0 => pt(PatternKind::Wildcard),
            1 => {
                let lit = match self.rng.below(4) {
                    0 => Lit::Bool(true),
                    1 => Lit::None,
                    2 => Lit::Str(self.cooked_string(0, false)),
                    _ => self.lit(0),
                };
                pt(PatternKind::Lit(lit))
            }
            4 => {
                let inner = self.alt_pattern(d1);
                pt(PatternKind::Bind {
                    name: self.lower(),
                    pattern: Box::new(inner),
                })
            }
            5 => pt(PatternKind::Variant {
                path: vec![self.upper(), id(self.rng.pick(VARIANTS))],
                fields: None,
            }),
            6 => {
                let fields = self.record_pattern(d1);
                let path = vec![id(self.rng.pick(VARIANTS))];
                pt(PatternKind::Variant {
                    path,
                    fields: Some(fields),
                })
            }
            7 => {
                let items = self.many(1, 3, |g| g.pattern(d1));
                pt(PatternKind::Tuple(items))
            }
            8 => {
                let items = self.many(0, 3, |g| {
                    if g.rng.chance(1, 4) {
                        ListPattern::Rest(g.opt(1, 2, Gen::lower))
                    } else {
                        ListPattern::Pattern(g.pattern(d1))
                    }
                });
                pt(PatternKind::List(items))
            }
            9 => {
                let record = self.record_pattern(d1);
                pt(PatternKind::Record(record))
            }
            _ => pt(PatternKind::Name(self.lower())),
        }
    }

    // ---- prompts ----

    fn prompt_body(&mut self, d: u32) -> PromptBody {
        let sections = self.many(0, 3, |g| g.section(d.saturating_sub(1)));
        PromptBody { sections, span: Z }
    }

    fn section(&mut self, d: u32) -> Section {
        let kind = match self.rng.below(15) {
            0 => SectionKind::System(self.expr(d)),
            1 => SectionKind::Instruct(self.expr(d)),
            2 => SectionKind::Task(self.expr(d)),
            3 => SectionKind::Data {
                value: self.expr(d),
                label: self.opt(1, 2, |g| g.expr(d)),
            },
            4 => SectionKind::Image {
                value: self.expr(d),
                label: self.opt(1, 2, |g| g.expr(d)),
            },
            5 => SectionKind::Example {
                input: self.simple(d),
                output: self.expr(d),
            },
            6 | 7 => SectionKind::Format(self.ty(2)),
            8 => SectionKind::Steps(self.expr(d)),
            9 => SectionKind::Context(self.expr(d)),
            10 => SectionKind::Note(self.expr(d)),
            11 => SectionKind::Include(self.expr(d)),
            12 | 13 => SectionKind::If {
                cond: self.simple(d),
                then: self.prompt_body(d),
                else_: self.opt(1, 3, |g| g.prompt_body(d)),
            },
            _ => SectionKind::For {
                pattern: self.pattern(d),
                iter: self.simple(d),
                body: self.prompt_body(d),
            },
        };
        Section { kind, span: Z }
    }

    // ---- items ----

    fn params(&mut self, d: u32) -> Vec<Param> {
        self.many(0, 3, |g| Param {
            name: g.lower(),
            ty: g.ty(d.min(2)),
            default: g.opt(1, 4, |g| g.simple(d.min(2))),
            span: Z,
        })
    }

    fn type_params(&mut self) -> Vec<Ident> {
        self.many(0, 2, Gen::upper)
    }

    fn item(&mut self, d: u32) -> Item {
        let docs = self.many(0, 2, Gen::doc);
        let kind = match self.rng.below(11) {
            0 => ItemKind::Import(self.import()),
            1 => ItemKind::Type(TypeDecl {
                name: self.upper(),
                params: self.type_params(),
                ty: self.ty(3),
            }),
            2 => {
                let variants = self.many(1, 3, |g| Variant {
                    name: id(g.rng.pick(VARIANTS)),
                    fields: g.opt(1, 2, |g| g.field_types(1)),
                    span: Z,
                });
                ItemKind::Enum(EnumDecl {
                    name: self.upper(),
                    params: self.type_params(),
                    variants,
                })
            }
            3 => ItemKind::Const(ConstDecl {
                name: id(self.rng.pick(&["LIMIT", "NAME", "C"])),
                ty: self.opt(1, 2, |g| g.ty(2)),
                value: self.expr(d),
            }),
            4 | 5 => ItemKind::Fn(FnDecl {
                name: self.lower(),
                type_params: self.type_params(),
                params: self.params(d),
                ret: self.opt(1, 2, |g| g.ret_ty(2)),
                body: self.block(d, 0, 3),
            }),
            6 => ItemKind::Harness(HarnessDecl {
                name: self.lower(),
                params: self.params(d),
                ret: self.opt(1, 2, |g| g.ret_ty(2)),
                config: self.opt(1, 2, |g| g.record(d.min(2))),
                body: self.block(d, 0, 3),
            }),
            7 | 8 => ItemKind::Prompt(PromptDecl {
                name: self.lower(),
                params: self.params(d),
                body: self.prompt_body(d),
            }),
            9 => ItemKind::State(StateDecl {
                ty: self.ty(2),
                value: self.expr(d),
            }),
            _ => ItemKind::Test(TestDecl {
                name: self.plain_string(),
                body: self.block(d, 0, 3),
            }),
        };
        let test = matches!(kind, ItemKind::Test(_));
        // `state` is a keyword only at the start of an item, so `pub state` is no item.
        let state = matches!(kind, ItemKind::State(_));
        Item {
            docs,
            attrs: if test {
                Vec::new()
            } else {
                self.many(0, 2, |g| g.attribute(1))
            },
            public: !test && !state && self.rng.chance(1, 3),
            kind,
            span: Z,
        }
    }

    fn import(&mut self) -> Import {
        let path = if self.rng.chance(1, 3) {
            ImportPath::File(self.plain_string())
        } else {
            ImportPath::Source(SourceName {
                source: self.opt(2, 3, Gen::lower),
                parts: self.many(1, 2, |g| g.many(1, 2, Gen::lower)),
                version: self.opt(1, 3, Gen::plain_string),
                span: Z,
            })
        };
        Import {
            path,
            alias: self.opt(1, 3, Gen::lower),
            names: self.opt(1, 3, |g| g.many(1, 3, Gen::lower)),
        }
    }

    fn module(&mut self, d: u32) -> Module {
        let meta = self.opt(1, 3, |g| Meta {
            fields: g.record(2).fields,
            span: Z,
        });
        let items = self.many(1, 4, |g| g.item(d));
        Module {
            meta,
            items,
            span: Z,
        }
    }
}

/// The `(tag first-argument)` pairs of a dump, skipping what is inside quoted strings.
fn dump_tags(dump: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = dump.chars().collect();
    let mut tags = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    i += if chars[i] == '\\' { 2 } else { 1 };
                }
                i += 1;
            }
            '(' => {
                let mut j = i + 1;
                let mut tag = String::new();
                while j < chars.len() && !matches!(chars[j], ' ' | ')' | '(' | '[') {
                    tag.push(chars[j]);
                    j += 1;
                }
                let mut first = String::new();
                if chars.get(j) == Some(&' ') {
                    j += 1;
                    while j < chars.len() && !matches!(chars[j], ' ' | ')' | '(' | '[') {
                        first.push(chars[j]);
                        j += 1;
                    }
                }
                tags.push((tag, first));
                i += 1;
            }
            _ => i += 1,
        }
    }
    tags
}

const EVERY_TAG: &[&str] = &[
    "module",
    "meta",
    "item",
    "attr",
    "arg",
    "import",
    "file",
    "source",
    "type",
    "enum",
    "variant",
    "const",
    "fn",
    "harness",
    "prompt",
    "state",
    "test",
    "param",
    "named",
    "list",
    "record-type",
    "tuple",
    "fn-type",
    "harness-type",
    "option",
    "field-type",
    "block",
    "let",
    "var",
    "assign",
    "defer",
    "expr",
    "int",
    "quantity",
    "float",
    "duration",
    "money",
    "bool",
    "none",
    "str",
    "interp",
    "name",
    "paren",
    "record",
    "field",
    "shorthand",
    "spread",
    "neg",
    "not",
    "bin",
    "call",
    "member",
    "opt-member",
    "index",
    "with-override",
    "if",
    "let-cond",
    "else-if",
    "else",
    "match",
    "arm",
    "for",
    "while",
    "loop",
    "repeat",
    "try",
    "catch-arms",
    "catch-bind",
    "par",
    "race",
    "with-timeout",
    "with",
    "do",
    "closure",
    "cparam",
    "fn-expr",
    "prompt-expr",
    "return",
    "break",
    "continue",
    "raise",
    "wild",
    "pat-lit",
    "pat-name",
    "bind",
    "pat-variant",
    "pat-tuple",
    "pat-list",
    "rest",
    "pat-record",
    "record-pat",
    "field-pat",
    "or",
    "prompt-body",
    "system",
    "instruct",
    "task",
    "data",
    "image",
    "example",
    "format",
    "steps",
    "context",
    "note",
    "include",
    "if-section",
    "for-section",
];

const EVERY_BINARY_OP: &[&str] = &[
    "|>", "??", "or", "and", "==", "!=", "<", "<=", ">", ">=", "in", "is", "..", "..=", "+", "-",
    "*", "/", "%",
];

#[test]
fn generated_trees_round_trip_and_cover_every_node() {
    let mut generator = Gen {
        rng: Rng(0x5EED_CAFE_F00D_1234),
        flat: false,
        strings: BTreeSet::new(),
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut ops: BTreeSet<String> = BTreeSet::new();
    let mut assigns: BTreeSet<String> = BTreeSet::new();
    let mut largest = 0;
    for case in 0..1500 {
        let depth = 2 + u32::try_from(case % 3).unwrap_or(0);
        let module = generator.module(depth);
        let text = print(&module);
        let parsed = parse(&text);
        assert!(
            parsed.is_clean(),
            "case {case}: the printed text does not parse cleanly: {:?}\n--- printed ---\n{text}\n\
             --- tree ---\n{}",
            parsed.diagnostics,
            dump(&module)
        );
        let (made, read) = (dump(&module), dump(&parsed.module));
        assert_eq!(
            made, read,
            "case {case}: the printed text reads as another tree\n--- printed ---\n{text}"
        );
        if let Some(Err(why)) = check(&text) {
            panic!("case {case}: {why}");
        }
        largest = largest.max(text.len());
        for (tag, first) in dump_tags(&made) {
            if tag == "bin" {
                ops.insert(first.clone());
            }
            if tag == "assign" {
                assigns.insert(first.clone());
            }
            seen.insert(tag);
        }
    }
    let missing: Vec<&&str> = EVERY_TAG.iter().filter(|t| !seen.contains(**t)).collect();
    assert!(
        missing.is_empty(),
        "node kinds never generated: {missing:?}"
    );
    let missing: Vec<&&str> = EVERY_BINARY_OP
        .iter()
        .filter(|op| !ops.contains(**op))
        .collect();
    assert!(missing.is_empty(), "operators never generated: {missing:?}");
    assert_eq!(
        assigns.len(),
        5,
        "assignment operators generated: {assigns:?}"
    );
    assert_eq!(
        generator.strings.len(),
        4,
        "string forms generated: {:?}",
        generator.strings
    );
    assert!(largest > 500, "the generated programs are too small");
}

// ---- unit tests: one printing rule each, by exact output ----

fn printed(source: &str) -> String {
    let parsed = parse(source);
    assert!(parsed.is_clean(), "{source:?}: {:?}", parsed.diagnostics);
    print(&parsed.module)
}

fn assert_prints(source: &str, expected: &str) {
    let text = printed(source);
    assert_eq!(text, expected, "printing {source:?}");
    // The canonical text is a fixed point.
    assert_eq!(
        printed(&text),
        expected,
        "printing the canonical text again"
    );
}

#[test]
fn layout_and_spacing() {
    assert_prints(
        "meta{name:\"x\"}\nfn f(a:Int,b:Str=\"s\")->Int{let x=a+b*2\nif x>1{return x}else{return -x}}\n",
        "meta { name: \"x\" }\n\nfn f(a: Int, b: Str = \"s\") -> Int {\n    let x = a + b * 2\n    \
         if x > 1 {\n        return x\n    } else {\n        return -x\n    }\n}\n",
    );
    assert_prints("", "");
    assert_prints("fn f() {}", "fn f() {}\n");
    // Ranges have no spaces; other binary operators have one on each side.
    assert_prints(
        "fn f() { let r = 0 ..= n\nlet s = a|>g(1)\nlet t = a??b }",
        "fn f() {\n    let r = 0..=n\n    let s = a |> g(1)\n    let t = a ?? b\n}\n",
    );
    // Items are separated by one blank line; documentation and attributes sit above the item.
    assert_prints(
        "/// one\n@turn\n@allow(read_file)\npub fn a() {}\nconst B=1\n",
        "/// one\n@turn\n@allow(read_file)\npub fn a() {}\n\nconst B = 1\n",
    );
}

#[test]
fn a_single_shorthand_field_keeps_its_comma() {
    let text =
        printed("fn f(x: Int) { let a = { x, }\nlet b = { x, y }\nlet c = { x: 1 }\nlet d = {} }");
    assert!(text.contains("let a = { x, }\n"), "{text}");
    assert!(text.contains("let b = { x, y }\n"), "{text}");
    assert!(text.contains("let c = { x: 1 }\n"), "{text}");
    assert!(text.contains("let d = {}\n"), "{text}");
    // Read back it is still a record, not a block.
    assert!(dump(&parse(&text).module).contains("(record [(shorthand x)])"));
    // An import list takes no trailing comma.
    assert_prints("import \"x\" { c }", "import \"x\" { c }\n");
}

#[test]
fn a_statement_ends_with_a_line_end_or_an_explicit_semicolon() {
    // A `format` section ending in `>` cannot end a statement at a line end (condition 2), so the
    // section before another one gets `;`. The last one needs none before the `}`.
    assert_prints(
        "prompt p(x: Str) {\n  format Map<Str, Int>;\n  task x\n}\n",
        "prompt p(x: Str) {\n    format Map<Str, Int>;\n    task x\n}\n",
    );
    // An item that ends in `>` needs the `;` in front of the line end.
    assert_prints("type M = Map<Str, Int>;\n", "type M = Map<Str, Int>;\n");
    assert_prints(
        "type M = [Int]\nenum E { A }",
        "type M = [Int]\n\nenum E {\n    A,\n}\n",
    );
}

#[test]
fn literals_print_in_canonical_units() {
    assert_prints(
        "fn f() { let a = (0x1F, 1_000, 007)\nlet b = (20k, 1.5M, 1.5k, 3M, 0k)\n\
         let c = (1000ms, 60s, 90s, 7200s, 1500ms, 0ms)\n\
         let d = ($0.5, $12.000, $0.000001, $1.25)\nlet e = (2.0, 1e-3, 1_0.2_5) }",
        "fn f() {\n    let a = (31, 1000, 7)\n    let b = (20k, 1500k, 1.5k, 3M, 0k)\n    \
         let c = (1s, 1m, 90s, 2h, 1500ms, 0s)\n    let d = ($0.50, $12, $0.000001, $1.25)\n    \
         let e = (2.0, 0.001, 10.25)\n}\n",
    );
}

#[test]
fn short_strings_escape_and_print_interpolations_flat() {
    assert_prints(
        "fn f() { let s = \"a\\\"b\\{c\\}\\\\d\\n\\t\\u{1}\\u{a0}é{x:.2}\" }",
        "fn f() {\n    let s = \"a\\\"b\\{c\\}\\\\d\\n\\t\\u{1}\\u{a0}é{x:.2}\"\n}\n",
    );
    // Inside a short string's interpolation everything is on one line.
    assert_prints(
        "fn f() { let s = \"x {if ok { let a = 1; a } else { match v { A => 1, B => 2 } }} y\" }",
        "fn f() {\n    let s = \"x {if ok { let a = 1; a } else { match v { A => 1, B => 2 } }} y\"\n}\n",
    );
    // A long string there becomes a one-line long string, its line feeds written as escapes.
    assert_prints(
        "fn f() { let s = \"{\"\"\"a\\nb\\u{20}\"\"\"}\" }",
        "fn f() {\n    let s = \"{\"\"\"a\\nb \"\"\"}\"\n}\n",
    );
}

#[test]
fn long_strings_print_on_several_lines_with_escapes() {
    assert_prints(
        "fn f() {\n  let s = \"\"\"\n      one \\\"quoted\\\" {x} \\{brace\\}\n        two\\ttab\n      \"\"\"\n}",
        "fn f() {\n    let s = \"\"\"\n    one \\\"quoted\\\" {x} \\{brace\\}\n      two\\ttab\n    \"\"\"\n}\n",
    );
    // A nested one is indented to its statement.
    assert_prints(
        "fn f() { if a {\n let s = \"\"\"\n x\n \"\"\"\n } }",
        "fn f() {\n    if a {\n        let s = \"\"\"\n        x\n        \"\"\"\n    }\n}\n",
    );
    // The first and last lines, blank lines and trailing line feeds survive.
    for text in ["\\n\\nx\\n\\n", "x\\n", "\\n", "  \\n  ", "a\\n\\n  b"] {
        assert_round_trips(&format!("fn f() {{ let s = \"\"\"{text}\"\"\" }}"));
    }
    // A leading space the dedent would remove is kept with `\u{20}`.
    let source = "fn f() { let s = \"\"\"\\u{20}  x\"\"\" }";
    assert!(printed(source).contains("\\u{20}"), "{}", printed(source));
    assert_round_trips(source);
    // The empty long string.
    assert_prints(
        "fn f() { let s = \"\"\"\"\"\" }",
        "fn f() {\n    let s = \"\"\"\n    \"\"\"\n}\n",
    );
}

#[test]
fn raw_long_strings_print_at_column_0() {
    assert_prints(
        "fn f() { if a {\n let s = r\"\"\"\n   x {y} \\n\n  z\n \"\"\"\n } }",
        "fn f() {\n    if a {\n        let s = r\"\"\"\n x {y} \\n\nz\n\"\"\"\n    }\n}\n",
    );
    assert_prints(
        "fn f() { let a = r\"C:\\p{q}\"\nlet b = r\"\"\"\"\"\" }",
        "fn f() {\n    let a = r\"C:\\p{q}\"\n    let b = r\"\"\"\n\"\"\"\n}\n",
    );
}

#[test]
fn a_type_keeps_its_option_layers_and_spacing() {
    assert_prints(
        "type A = Map<K, List<V>>??;\nconst B: Int? = 1\ntype F = fn(A, [B]) -> (C, D)?\ntype R = { a: T, b?: U \"d\" }\n",
        "type A = Map<K, List<V>>??;\n\nconst B: Int? = 1\n\ntype F = fn(A, [B]) -> (C, D)?;\n\n\
         type R = { a: T, b?: U \"d\" }\n",
    );
}

#[test]
fn documented_record_fields_print_one_per_line() {
    assert_prints(
        "type R = {\n  /// the path\n  @key\n  path: Str,\n  n: Int,\n}",
        "type R = {\n    /// the path\n    @key path: Str,\n    n: Int,\n}\n",
    );
}

#[test]
fn match_and_catch_arms_print_one_per_line() {
    assert_prints(
        "fn f() { let x = match v { 1 | 2 => a, [h, ..t] if h > 0 => b, Denied { by, .. } => c, _ => d }\n\
         try { g() } catch { Err { code: 1 } => 1, e => 2 } }",
        "fn f() {\n    let x = match v {\n        1 | 2 => a,\n        [h, ..t] if h > 0 => b,\n        \
         Denied { by, .. } => c,\n        _ => d,\n    }\n    try {\n        g()\n    } catch {\n        \
         Err { code: 1 } => 1,\n        e => 2,\n    }\n}\n",
    );
}

#[test]
fn the_harder_forms_round_trip() {
    // Continuation lines, closures, prompts, `with` in its three forms.
    let programs = [
        "harness h(dir: Str) -> [Str]\n  with { tools: [glob], budget: { tokens: 200k, wall: 10m } }\n{\n  let files = tools.glob(\"*.rs\", path: dir).paths\n    |> list.take(40)\n  par_map(files, |p| { let b = tools.read(p).text\n b }, limit: 4)\n  |> list.sort_by(|n| n.path)\n}\n",
        "fn f() { let a = g(x) with { model: m }\nwith { a: 1 } { x }\nwith timeout 5s { y } else { z }\npar { a(); b() }\nrace { a(); b() } }",
        "prompt p(x: Untrusted<Str>) {\n  instruct \"Do it.\"\n  data x as \"input\"\n  example a -> b\n  if c { note \"n\" } else { steps \"s\" }\n  for i in [1, 2] { include i }\n  format Note\n}\n",
        "enum V<T> { A, B { x: T, y?: Int \"why\" } }\nstate: Int = 0\ntest \"t\" { defer { x = 1 }\nvar y: Int = 2\ny += 3 }\n",
        "import std:debate as d { a, b }\nimport plugin:acme/review @\"^1\"\nimport \"./x.ratio\"\n",
        "fn f() { let g = fn(a: Int, b: Str = \"s\") -> Out { a }\ng(1, fn() -> [Out] { b }) }",
    ];
    for program in programs {
        assert_round_trips(program);
    }
}
