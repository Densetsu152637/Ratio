//! The canonical serialisation of a package's IR, its node numbering, and the IR hash.
//!
//! The form is UTF-8 text: S-expressions with single spaces, one item per line. It names every
//! cross-reference by what it refers to, never by a load-order index: items as
//! `(item "<package>" "<path>" "<name>")`, built-ins by name, and types and enums by a number
//! local to the package, given in order of first reference and defined in a trailing
//! `defs` section (so a type shared by many nodes is written once and a recursive enum ends).
//! Integers are decimal, floats the 16 hexadecimal digits of their IEEE 754 bits, strings
//! quoted with `\\`, `\"`, `\n`, `\r`, `\t` and `\u{…}` for other control characters. Modules
//! are written in order of their paths and items in source order; source positions are left
//! out. Nothing in the form depends on the platform, pointer width or hash-map order.
//!
//! Writing the form also numbers the package's expressions in pre-order ([`crate::Expr::id`]):
//! with the package hash, a node's number is its site id.
//!
//! The hash is `sha256` over the form, which starts with `ratio-ir <IR_VERSION>` and
//! `stdlib <STDLIB_VERSION>` lines. The built-in prompt texts a package uses will be added to
//! the form by stage 12; there are none before it.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::ir::{
    ArgFrom, Arm, AssignOp, Callee, Catch, Expr, ExprKind, Func, ItemKind, ListItem, Lit, Pattern,
    PlaceStep, Program, PromptPart, RecordPart, RecordPat, Stmt, Type, UnOp,
};
use crate::{IR_VERSION, STDLIB_VERSION};

/// A string literal of the form.
#[must_use]
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Numbers every package's expressions and computes the packages' hashes, following the tree
/// at most `max_depth` deep (`None` when it is deeper).
pub(crate) fn finish(program: &mut Program, max_depth: usize) -> Option<()> {
    for p in 0..program.packages.len() {
        let text = write_package(program, p, max_depth)?;
        program.packages[p].hash = ratio_check::sha256(text.as_bytes());
    }
    Some(())
}

/// The canonical form of package `package` (an index into [`Program::packages`]); `None`
/// when the program is nested deeper than the writer follows on the stack it has (only when no
/// large stack can be started).
///
/// # Panics
///
/// If `package` is out of range.
#[must_use]
pub fn canonical_text(program: &Program, package: usize) -> Option<String> {
    // The writer (and the copy it numbers) recurses over the tree, whose depth the parser
    // bounds; it runs on a large stack as lowering does, with an explicit bound.
    crate::stack::run(|max_depth| {
        let mut copy = program.clone();
        write_package(&mut copy, package, max_depth)
    })
    .flatten()
}

/// The names the writer needs from modules other than the one being written.
struct Names {
    modules: Vec<(String, String, Vec<Option<String>>)>,
}

fn write_package(program: &mut Program, package: usize, max_depth: usize) -> Option<String> {
    let names = Names {
        modules: program
            .modules
            .iter()
            .map(|m| {
                (
                    m.package.clone(),
                    m.path.clone(),
                    m.items
                        .iter()
                        .map(|i| i.as_ref().map(|i| i.name.clone()))
                        .collect(),
                )
            })
            .collect(),
    };
    let mut order: Vec<u32> = program.packages[package].modules.clone();
    order.sort_by(|a, b| {
        names.modules[*a as usize]
            .1
            .cmp(&names.modules[*b as usize].1)
    });
    let enums = std::mem::take(&mut program.enums);
    let types = std::mem::take(&mut program.types);
    let mut w = Writer {
        out: String::new(),
        next: 0,
        names: &names,
        enums: &enums,
        types: &types,
        type_local: HashMap::new(),
        type_queue: Vec::new(),
        enum_local: HashMap::new(),
        enum_queue: Vec::new(),
        depth: 0,
        max_depth,
        too_deep: false,
    };
    let _ = writeln!(w.out, "ratio-ir {IR_VERSION}");
    let _ = writeln!(w.out, "stdlib {STDLIB_VERSION}");
    let _ = writeln!(w.out, "package {}", quote(&program.packages[package].name));
    for m in order {
        let module = &mut program.modules[m as usize];
        let _ = writeln!(w.out, "module {}", quote(&module.path));
        for item in module.items.iter_mut().flatten() {
            w.out.push_str("item ");
            w.out.push_str(&quote(&item.name));
            if item.public {
                w.out.push_str(" pub");
            }
            w.out.push_str(" (attrs");
            for a in &item.attrs {
                w.out.push(' ');
                w.out.push_str(&quote(a));
            }
            w.out.push(')');
            match &mut item.kind {
                ItemKind::Fn(f) => {
                    w.out.push_str(" fn ");
                    w.func(f);
                }
                ItemKind::Harness { func, config } => {
                    w.out.push_str(" harness ");
                    w.func(func);
                    if let Some(c) = config {
                        w.out.push_str(" (config ");
                        w.expr(c);
                        w.out.push(')');
                    }
                }
                ItemKind::Prompt(f) => {
                    w.out.push_str(" prompt ");
                    w.func(f);
                }
                ItemKind::Const { value, slots } => {
                    let _ = write!(w.out, " const (slots {slots}) ");
                    w.expr(value);
                }
                ItemKind::State { value, slots } => {
                    let _ = write!(w.out, " state (slots {slots}) ");
                    w.expr(value);
                }
            }
            w.out.push('\n');
        }
    }
    w.defs();
    let (out, too_deep) = (w.out, w.too_deep);
    program.enums = enums;
    program.types = types;
    (!too_deep).then_some(out)
}

struct Writer<'a> {
    out: String,
    next: u32,
    names: &'a Names,
    enums: &'a [crate::ir::EnumDef],
    types: &'a [Type],
    type_local: HashMap<u32, u32>,
    type_queue: Vec<u32>,
    enum_local: HashMap<u32, u32>,
    enum_queue: Vec<u32>,
    depth: usize,
    max_depth: usize,
    too_deep: bool,
}

impl Writer<'_> {
    /// Enters one level of the tree; `false` (and the form refused) beyond the bound.
    fn enter(&mut self) -> bool {
        self.depth += 1;
        if self.depth > self.max_depth {
            self.too_deep = true;
        }
        !self.too_deep
    }

    fn expr(&mut self, e: &mut Expr) {
        if self.enter() {
            self.expr_inner(e);
        }
        self.depth -= 1;
    }

    fn pattern(&mut self, p: &Pattern) {
        if self.enter() {
            self.pattern_inner(p);
        }
        self.depth -= 1;
    }

    fn prompt(&mut self, parts: &mut [PromptPart]) {
        if self.enter() {
            self.prompt_inner(parts);
        }
        self.depth -= 1;
    }

    fn ty(&mut self, global: u32) {
        let next = u32::try_from(self.type_local.len()).unwrap_or(u32::MAX);
        let local = *self.type_local.entry(global).or_insert_with(|| {
            self.type_queue.push(global);
            next
        });
        let _ = write!(self.out, "t{local}");
    }

    fn en(&mut self, global: u32) {
        let next = u32::try_from(self.enum_local.len()).unwrap_or(u32::MAX);
        let local = *self.enum_local.entry(global).or_insert_with(|| {
            self.enum_queue.push(global);
            next
        });
        let _ = write!(self.out, "e{local}");
    }

    /// The `defs` section: every type and enum referenced, in order of first reference.
    fn defs(&mut self) {
        self.out.push_str("defs\n");
        let (mut ti, mut ei) = (0, 0);
        loop {
            if ti < self.type_queue.len() {
                let g = self.type_queue[ti];
                let _ = write!(self.out, "type t{ti} ");
                self.type_def(g);
                self.out.push('\n');
                ti += 1;
            } else if ei < self.enum_queue.len() {
                let g = self.enum_queue[ei];
                let def = &self.enums[g as usize];
                let _ = write!(
                    self.out,
                    "enum e{ei} {} {}",
                    quote(&def.origin),
                    quote(&def.name)
                );
                self.out.push_str(" (params");
                for p in &def.params {
                    self.out.push(' ');
                    self.out.push_str(&quote(p));
                }
                self.out.push(')');
                for v in &def.variants {
                    self.out.push_str(" (variant ");
                    self.out.push_str(&quote(&v.name));
                    if let Some(p) = v.payload {
                        self.out.push(' ');
                        self.ty(p);
                    }
                    self.out.push(')');
                }
                self.out.push('\n');
                ei += 1;
            } else {
                break;
            }
        }
    }

    fn type_def(&mut self, g: u32) {
        let t = self.types[g as usize].clone();
        let simple = |s: &str, out: &mut String| out.push_str(s);
        match t {
            Type::Error => simple("error", &mut self.out),
            Type::Never => simple("never", &mut self.out),
            Type::Unit => simple("unit", &mut self.out),
            Type::Bool => simple("bool", &mut self.out),
            Type::Int => simple("int", &mut self.out),
            Type::Float => simple("float", &mut self.out),
            Type::Duration => simple("duration", &mut self.out),
            Type::Money => simple("money", &mut self.out),
            Type::Str => simple("str", &mut self.out),
            Type::UserText => simple("user-text", &mut self.out),
            Type::Brief => simple("brief", &mut self.out),
            Type::UntrustedStr => simple("untrusted-str", &mut self.out),
            Type::Json => simple("json", &mut self.out),
            Type::Image => simple("image", &mut self.out),
            Type::Prompt => simple("prompt", &mut self.out),
            Type::TrustedPrompt => simple("trusted-prompt", &mut self.out),
            Type::Transcript => simple("transcript", &mut self.out),
            Type::Range => simple("range", &mut self.out),
            Type::ToolName => simple("tool-name", &mut self.out),
            Type::ToolSet => simple("tool-set", &mut self.out),
            Type::Handle(h) => {
                self.out.push_str("(handle ");
                self.out.push_str(&quote(&h));
                self.out.push(')');
            }
            Type::Task(x) => self.wrap("task", &[x]),
            Type::Option(x) => self.wrap("option", &[x]),
            Type::List(x) => self.wrap("list", &[x]),
            Type::Map(k, v) => self.wrap("map", &[k, v]),
            Type::Tuple(xs) => self.wrap("tuple", &xs),
            Type::Untrusted(x) => self.wrap("untrusted", &[x]),
            Type::Secret(x) => self.wrap("secret", &[x]),
            Type::Record(fields) => {
                self.out.push_str("(record");
                for f in fields {
                    self.out.push_str(" (field ");
                    self.out.push_str(&quote(&f.name));
                    self.out.push(' ');
                    self.ty(f.ty);
                    if f.optional {
                        self.out.push_str(" opt");
                    }
                    if !f.meta.is_empty() {
                        self.out.push_str(" (meta ");
                        self.out.push_str(&quote(&f.meta));
                        self.out.push(')');
                    }
                    self.out.push(')');
                }
                self.out.push(')');
            }
            Type::Enum(e, args) => {
                self.out.push_str("(enum ");
                self.en(e);
                for a in args {
                    self.out.push(' ');
                    self.ty(a);
                }
                self.out.push(')');
            }
            Type::Fn(ps, r) | Type::Harness(ps, r) => {
                let tag = if matches!(self.types[g as usize], Type::Fn(..)) {
                    "(fn ("
                } else {
                    "(harness ("
                };
                self.out.push_str(tag);
                for (i, p) in ps.iter().enumerate() {
                    if i > 0 {
                        self.out.push(' ');
                    }
                    self.ty(*p);
                }
                self.out.push_str(") ");
                self.ty(r);
                self.out.push(')');
            }
            Type::Param(n) => {
                self.out.push_str("(param ");
                self.out.push_str(&quote(&n));
                self.out.push(')');
            }
        }
    }

    fn wrap(&mut self, tag: &str, xs: &[u32]) {
        self.out.push('(');
        self.out.push_str(tag);
        for x in xs {
            self.out.push(' ');
            self.ty(*x);
        }
        self.out.push(')');
    }

    fn func(&mut self, f: &mut Func) {
        let _ = write!(self.out, "(func (slots {}) (captures", f.slots);
        for (outer, own) in &f.captures {
            let _ = write!(self.out, " ({outer} {own})");
        }
        self.out.push_str(") (params");
        for p in &mut f.params {
            self.out.push_str(" (param ");
            self.pattern(&p.pattern);
            if let Some(d) = &mut p.default {
                self.out.push_str(" (default ");
                self.expr(d);
                self.out.push(')');
            }
            self.out.push(')');
        }
        self.out.push_str(") ");
        self.expr(&mut f.body);
        self.out.push(')');
    }

    fn lit(&mut self, l: &Lit) {
        match l {
            Lit::Unit => self.out.push_str("(unit)"),
            Lit::None => self.out.push_str("(none)"),
            Lit::Bool(b) => {
                let _ = write!(self.out, "(bool {b})");
            }
            Lit::Int(v) => {
                let _ = write!(self.out, "(int {v})");
            }
            Lit::Float(v) => {
                let _ = write!(self.out, "(float {:016x})", v.to_bits());
            }
            Lit::Duration(v) => {
                let _ = write!(self.out, "(duration {v})");
            }
            Lit::Money(v) => {
                let _ = write!(self.out, "(money {v})");
            }
            Lit::Str(s) => {
                self.out.push_str("(str ");
                self.out.push_str(&quote(s));
                self.out.push(')');
            }
        }
    }

    fn exprs(&mut self, xs: &mut [Expr]) {
        for x in xs {
            self.out.push(' ');
            self.expr(x);
        }
    }

    fn opt(&mut self, x: Option<&mut Box<Expr>>) {
        match x {
            Some(x) => {
                self.out.push(' ');
                self.expr(x);
            }
            None => self.out.push_str(" -"),
        }
    }

    fn args(&mut self, args: &mut [Expr], order: &[ArgFrom], type_args: &[u32]) {
        self.out.push_str(" (args");
        self.exprs(args);
        self.out.push_str(") (order");
        for o in order {
            match o {
                ArgFrom::Written(i) => {
                    let _ = write!(self.out, " {i}");
                }
                ArgFrom::Default => self.out.push_str(" default"),
            }
        }
        self.out.push_str(") (types");
        for t in type_args {
            self.out.push(' ');
            self.ty(*t);
        }
        self.out.push(')');
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of expression")]
    fn expr_inner(&mut self, e: &mut Expr) {
        e.id = self.next;
        self.next += 1;
        match &mut e.kind {
            ExprKind::Lit(l) => self.lit(l),
            ExprKind::Local(s) => {
                let _ = write!(self.out, "(local {s})");
            }
            ExprKind::Item(id) => {
                let (pkg, path, items) = &self.names.modules[id.module as usize];
                let name = items
                    .get(id.item as usize)
                    .and_then(Clone::clone)
                    .unwrap_or_else(|| format!("#{}", id.item));
                let _ = write!(
                    self.out,
                    "(item {} {} {})",
                    quote(pkg),
                    quote(path),
                    quote(&name)
                );
            }
            ExprKind::Builtin(n) => {
                let _ = write!(self.out, "(builtin {})", quote(n));
            }
            ExprKind::Concat(xs) => {
                self.out.push_str("(concat");
                self.exprs(xs);
                self.out.push(')');
            }
            ExprKind::Show { value, spec } => {
                self.out.push_str("(show ");
                self.expr(value);
                if let Some(s) = spec {
                    self.out.push(' ');
                    self.out.push_str(&quote(s));
                }
                self.out.push(')');
            }
            ExprKind::Tuple(xs) => {
                self.out.push_str("(tuple");
                self.exprs(xs);
                self.out.push(')');
            }
            ExprKind::List(items) => {
                self.out.push_str("(list");
                for item in items {
                    match item {
                        ListItem::One(x) => {
                            self.out.push(' ');
                            self.expr(x);
                        }
                        ListItem::Spread(x) => {
                            self.out.push_str(" (spread ");
                            self.expr(x);
                            self.out.push(')');
                        }
                    }
                }
                self.out.push(')');
            }
            ExprKind::Record { parts, ty } => {
                self.out.push_str("(record ");
                self.ty(*ty);
                self.record_parts(parts);
                self.out.push(')');
            }
            ExprKind::Map(entries) => {
                self.out.push_str("(map");
                for (k, v) in entries {
                    self.out.push_str(" (");
                    self.out.push_str(&quote(k));
                    self.out.push(' ');
                    self.expr(v);
                    self.out.push(')');
                }
                self.out.push(')');
            }
            ExprKind::Variant {
                enum_,
                index,
                payload,
            } => {
                self.out.push_str("(variant ");
                self.en(*enum_);
                let _ = write!(self.out, " {index}");
                self.opt(payload.as_mut());
                self.out.push(')');
            }
            ExprKind::Block(stmts) => {
                self.out.push_str("(block");
                for s in stmts {
                    self.out.push(' ');
                    self.stmt(s);
                }
                self.out.push(')');
            }
            ExprKind::Unary(op, x) => {
                let tag = match op {
                    UnOp::Neg => "(neg ",
                    UnOp::Not => "(not ",
                };
                self.out.push_str(tag);
                self.expr(x);
                self.out.push(')');
            }
            ExprKind::Binary(op, a, b) => {
                let _ = write!(self.out, "(bin {} ", quote(op.name()));
                self.expr(a);
                self.out.push(' ');
                self.expr(b);
                self.out.push(')');
            }
            ExprKind::And(a, b) => self.pair("and", a, b),
            ExprKind::Or(a, b) => self.pair("or", a, b),
            ExprKind::Coalesce(a, b) => self.pair("coalesce", a, b),
            ExprKind::Is {
                value,
                enum_,
                index,
            } => {
                self.out.push_str("(is ");
                self.expr(value);
                self.out.push(' ');
                self.en(*enum_);
                let _ = write!(self.out, " {index})");
            }
            ExprKind::Call {
                callee,
                args,
                order,
                type_args,
            } => {
                self.out.push_str("(call ");
                match callee {
                    Callee::Item(id) => {
                        let (pkg, path, items) = &self.names.modules[id.module as usize];
                        let name = items
                            .get(id.item as usize)
                            .and_then(Clone::clone)
                            .unwrap_or_else(|| format!("#{}", id.item));
                        let _ = write!(
                            self.out,
                            "(item {} {} {})",
                            quote(pkg),
                            quote(path),
                            quote(&name)
                        );
                    }
                    Callee::Builtin(n) => {
                        let _ = write!(self.out, "(builtin {})", quote(n));
                    }
                    Callee::Value(f) => {
                        self.out.push_str("(value ");
                        self.expr(f);
                        self.out.push(')');
                    }
                }
                self.args(args, order, type_args);
                self.out.push(')');
            }
            ExprKind::Effect {
                builtin,
                args,
                order,
                type_args,
            } => {
                let _ = write!(self.out, "(effect {}", quote(builtin));
                self.args(args, order, type_args);
                self.out.push(')');
            }
            ExprKind::Field { object, name } => {
                self.out.push_str("(field ");
                self.expr(object);
                self.out.push(' ');
                self.out.push_str(&quote(name));
                self.out.push(')');
            }
            ExprKind::OptChain { object, slot, then } => {
                self.out.push_str("(opt ");
                self.expr(object);
                let _ = write!(self.out, " {slot} ");
                self.expr(then);
                self.out.push(')');
            }
            ExprKind::Index { object, index } => self.pair("index", object, index),
            ExprKind::If { cond, then, else_ } => {
                self.out.push_str("(if ");
                self.expr(cond);
                self.out.push(' ');
                self.expr(then);
                self.opt(else_.as_mut());
                self.out.push(')');
            }
            ExprKind::IfLet {
                pattern,
                value,
                unwrap,
                then,
                else_,
            } => {
                self.out.push_str("(if-let ");
                self.pattern(pattern);
                self.out.push_str(if *unwrap { " unwrap " } else { " " });
                self.expr(value);
                self.out.push(' ');
                self.expr(then);
                self.opt(else_.as_mut());
                self.out.push(')');
            }
            ExprKind::Match { scrutinee, arms } => {
                self.out.push_str("(match ");
                self.expr(scrutinee);
                self.arms(arms);
                self.out.push(')');
            }
            ExprKind::For {
                pattern,
                iter,
                body,
            } => {
                self.out.push_str("(for ");
                self.pattern(pattern);
                self.out.push(' ');
                self.expr(iter);
                self.out.push(' ');
                self.expr(body);
                self.out.push(')');
            }
            ExprKind::While { cond, body } => self.pair("while", cond, body),
            ExprKind::WhileLet {
                pattern,
                value,
                unwrap,
                body,
            } => {
                self.out.push_str("(while-let ");
                self.pattern(pattern);
                self.out.push_str(if *unwrap { " unwrap " } else { " " });
                self.expr(value);
                self.out.push(' ');
                self.expr(body);
                self.out.push(')');
            }
            ExprKind::Loop(b) => {
                self.out.push_str("(loop ");
                self.expr(b);
                self.out.push(')');
            }
            ExprKind::Repeat { count, body } => self.pair("repeat", count, body),
            ExprKind::Break(x) => {
                self.out.push_str("(break");
                self.opt(x.as_mut());
                self.out.push(')');
            }
            ExprKind::Continue => self.out.push_str("(continue)"),
            ExprKind::Return(x) => {
                self.out.push_str("(return");
                self.opt(x.as_mut());
                self.out.push(')');
            }
            ExprKind::Raise(x) => {
                self.out.push_str("(raise ");
                self.expr(x);
                self.out.push(')');
            }
            ExprKind::Try { body, catch } => {
                self.out.push_str("(try ");
                self.expr(body);
                match catch {
                    Catch::Arms(arms) => {
                        self.out.push_str(" (arms");
                        self.arms(arms);
                        self.out.push(')');
                    }
                    Catch::Bind(slot, b) => {
                        let _ = write!(self.out, " (bind {slot} ");
                        self.expr(b);
                        self.out.push(')');
                    }
                }
                self.out.push(')');
            }
            ExprKind::Closure(f) => {
                self.out.push_str("(closure ");
                self.func(f);
                self.out.push(')');
            }
            ExprKind::Prompt(parts) => {
                self.out.push_str("(prompt");
                self.prompt(parts);
                self.out.push(')');
            }
            ExprKind::Par(xs) => {
                self.out.push_str("(par");
                self.exprs(xs);
                self.out.push(')');
            }
            ExprKind::Race(xs) => {
                self.out.push_str("(race");
                self.exprs(xs);
                self.out.push(')');
            }
            ExprKind::WithTimeout {
                duration,
                body,
                else_,
            } => {
                self.out.push_str("(timeout ");
                self.expr(duration);
                self.out.push(' ');
                self.expr(body);
                self.opt(else_.as_mut());
                self.out.push(')');
            }
            ExprKind::With { config, body } => self.pair("with", config, body),
            ExprKind::Override { call, config } => self.pair("override", call, config),
            ExprKind::ToolName(n) => {
                let _ = write!(self.out, "(tool {})", quote(n));
            }
            ExprKind::ToolSet(x) => {
                self.out.push_str("(tool-set ");
                self.expr(x);
                self.out.push(')');
            }
        }
    }

    fn pair(&mut self, tag: &str, a: &mut Expr, b: &mut Expr) {
        self.out.push('(');
        self.out.push_str(tag);
        self.out.push(' ');
        self.expr(a);
        self.out.push(' ');
        self.expr(b);
        self.out.push(')');
    }

    fn record_parts(&mut self, parts: &mut [RecordPart]) {
        for p in parts {
            match p {
                RecordPart::Field(n, x) => {
                    self.out.push_str(" (field ");
                    self.out.push_str(&quote(n));
                    self.out.push(' ');
                    self.expr(x);
                    self.out.push(')');
                }
                RecordPart::Spread(x) => {
                    self.out.push_str(" (spread ");
                    self.expr(x);
                    self.out.push(')');
                }
            }
        }
    }

    fn arms(&mut self, arms: &mut [Arm]) {
        for a in arms {
            self.out.push_str(" (arm ");
            self.pattern(&a.pattern);
            match &mut a.guard {
                Some(g) => {
                    self.out.push_str(" (guard ");
                    self.expr(g);
                    self.out.push(')');
                }
                None => self.out.push_str(" -"),
            }
            self.out.push(' ');
            self.expr(&mut a.body);
            self.out.push(')');
        }
    }

    fn prompt_inner(&mut self, parts: &mut [PromptPart]) {
        for p in parts {
            match p {
                PromptPart::Section(kind, xs) => {
                    let _ = write!(self.out, " ({}", kind.name());
                    self.exprs(xs);
                    self.out.push(')');
                }
                PromptPart::Format(t) => {
                    let _ = write!(self.out, " (format {})", quote(t));
                }
                PromptPart::If { cond, then, else_ } => {
                    self.out.push_str(" (if ");
                    self.expr(cond);
                    self.out.push_str(" (then");
                    self.prompt(then);
                    self.out.push_str(") (else");
                    self.prompt(else_);
                    self.out.push_str("))");
                }
                PromptPart::For {
                    pattern,
                    iter,
                    body,
                } => {
                    self.out.push_str(" (for ");
                    self.pattern(pattern);
                    self.out.push(' ');
                    self.expr(iter);
                    self.out.push_str(" (body");
                    self.prompt(body);
                    self.out.push_str("))");
                }
            }
        }
    }

    fn stmt(&mut self, s: &mut Stmt) {
        match s {
            Stmt::Let { pattern, value } => {
                self.out.push_str("(let ");
                self.pattern(pattern);
                self.out.push(' ');
                self.expr(value);
                self.out.push(')');
            }
            Stmt::Var { slot, value } => {
                let _ = write!(self.out, "(var {slot} ");
                self.expr(value);
                self.out.push(')');
            }
            Stmt::Assign {
                slot,
                path,
                op,
                value,
                ..
            } => {
                let op = match op {
                    AssignOp::Set => "=",
                    AssignOp::Add => "+=",
                    AssignOp::Sub => "-=",
                    AssignOp::Mul => "*=",
                    AssignOp::Div => "/=",
                };
                let _ = write!(self.out, "(assign {slot} (path");
                for step in path {
                    match step {
                        PlaceStep::Field(n) => {
                            self.out.push_str(" (field ");
                            self.out.push_str(&quote(n));
                            self.out.push(')');
                        }
                        PlaceStep::Index(x) => {
                            self.out.push_str(" (index ");
                            self.expr(x);
                            self.out.push(')');
                        }
                    }
                }
                let _ = write!(self.out, ") {} ", quote(op));
                self.expr(value);
                self.out.push(')');
            }
            Stmt::Defer(b) => {
                self.out.push_str("(defer ");
                self.expr(b);
                self.out.push(')');
            }
            Stmt::Expr(x) => {
                self.out.push_str("(expr ");
                self.expr(x);
                self.out.push(')');
            }
        }
    }

    fn pattern_inner(&mut self, p: &Pattern) {
        match p {
            Pattern::Wild => self.out.push('_'),
            Pattern::Bind { slot, sub } => {
                let _ = write!(self.out, "(bind {slot}");
                if let Some(q) = sub {
                    self.out.push(' ');
                    self.pattern(q);
                }
                self.out.push(')');
            }
            Pattern::Lit(l) => {
                self.out.push_str("(lit ");
                self.lit(l);
                self.out.push(')');
            }
            Pattern::Variant {
                enum_,
                index,
                fields,
            } => {
                self.out.push_str("(variant ");
                self.en(*enum_);
                let _ = write!(self.out, " {index}");
                if let Some(rp) = fields {
                    self.out.push(' ');
                    self.record_pat(rp);
                }
                self.out.push(')');
            }
            Pattern::Tuple(ps) => {
                self.out.push_str("(tuple");
                for q in ps {
                    self.out.push(' ');
                    self.pattern(q);
                }
                self.out.push(')');
            }
            Pattern::List {
                before,
                rest,
                after,
            } => {
                self.out.push_str("(list");
                for q in before {
                    self.out.push(' ');
                    self.pattern(q);
                }
                match rest {
                    Some(Some(s)) => {
                        let _ = write!(self.out, " (rest {s})");
                    }
                    Some(None) => self.out.push_str(" (rest)"),
                    None => {}
                }
                for q in after {
                    self.out.push(' ');
                    self.pattern(q);
                }
                self.out.push(')');
            }
            Pattern::Record(rp) => self.record_pat(rp),
            Pattern::Or(ps) => {
                self.out.push_str("(or");
                for q in ps {
                    self.out.push(' ');
                    self.pattern(q);
                }
                self.out.push(')');
            }
        }
    }

    fn record_pat(&mut self, rp: &RecordPat) {
        self.out.push_str("(record");
        for (n, q) in &rp.fields {
            self.out.push_str(" (");
            self.out.push_str(&quote(n));
            self.out.push(' ');
            self.pattern(q);
            self.out.push(')');
        }
        if rp.rest {
            self.out.push_str(" ..");
        }
        self.out.push(')');
    }
}
