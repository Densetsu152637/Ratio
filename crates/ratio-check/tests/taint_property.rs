//! The stage 5 property (chapter 10, stage 5): generated programs never produce a
//! `TrustedPrompt`, an instruction section or a `task` section from untrusted sources without
//! an audited trust site.
//!
//! A fixed-seed generator (xorshift, as `ratio-syntax`'s round-trip test) builds well-typed
//! functions that bind and combine text from trusted sources (literals, the user's message,
//! scalars computed from anything) and untrusted ones (a web page, a model's verdict, a
//! delegated `Brief`) through interpolation, concatenation, `if`, generic functions,
//! closures, lists, `??`, `var`s assigned in loops and the audited conversions, then put the
//! results in prompt sections and return a `Prompt` or a `TrustedPrompt`. Alongside each
//! expression it computes the **ground truth** by data provenance, independently of the
//! checker: the least trusted source whose text the value may hold (`trust` makes it trusted,
//! `as_task` a `Brief`; scalars are trusted, chapter 03).
//!
//! - **Soundness**: whenever the checker accepts a program, every instruction position holds
//!   trusted text by the ground truth, every `task` section holds at most a `Brief`, and a
//!   `TrustedPrompt` holds neither data nor a `Brief`.
//! - **Precision on the exact fragment**: a program whose ground truth is safe, built only
//!   from constructs whose typing is exact, is accepted. This keeps the generator honest (its
//!   programs are well typed) and the soundness check from being vacuous.
//! - **Coverage**: enough programs are accepted and refused, refusals are taint errors
//!   (`C0035`), and accepted programs use the audited conversions, generics, closures and
//!   joins in sinks.

#![allow(
    clippy::format_push_string,
    reason = "the test programs are built by appending"
)]

use ratio_check::Code;
use ratio_names::{Loader, MemoryRegistry, PackageId, PackageName, QualifiedName, RelPath, Source};

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
}

/// The least trusted source a value's text may come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Taint {
    Clean,
    Brief,
    Untrusted,
}

/// Which constructs a program used, for the coverage assertions.
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "independent flags")]
struct Used {
    trust: bool,
    as_task: bool,
    generic: bool,
    closure: bool,
    join: bool,
    untrusted_source: bool,
}

struct Gen {
    rng: Rng,
    locals: Vec<(String, Taint)>,
    /// Whether every construct used is typed exactly by the checker.
    exact: bool,
    used: Used,
}

impl Gen {
    fn leaf(&mut self) -> (String, Taint) {
        let choice = self.rng.below(9 + usize::from(!self.locals.is_empty()) * 4);
        match choice {
            0 | 1 => (format!("\"t{}\"", self.rng.below(100)), Taint::Clean),
            2 => {
                self.used.untrusted_source = true;
                ("web".to_owned(), Taint::Untrusted)
            }
            3 => ("task.brief".to_owned(), Taint::Brief),
            4 => ("user".to_owned(), Taint::Clean),
            5 => {
                self.used.untrusted_source = true;
                ("v.note".to_owned(), Taint::Untrusted)
            }
            // Scalars are trusted, whatever they are computed from.
            6 => ("\"{n}\"".to_owned(), Taint::Clean),
            7 => ("\"{text.len(web)} chars\"".to_owned(), Taint::Clean),
            8 => ("\"passed: {v.pass}\"".to_owned(), Taint::Clean),
            _ => {
                let i = self.rng.below(self.locals.len());
                self.locals[i].clone()
            }
        }
    }

    /// A text expression and its ground truth.
    #[allow(clippy::too_many_lines, reason = "one arm per construct")]
    fn text(&mut self, depth: usize) -> (String, Taint) {
        if depth == 0 || self.rng.chance(1, 4) {
            return self.leaf();
        }
        let d = depth - 1;
        match self.rng.below(15) {
            0 => {
                let (a, ta) = self.text(d);
                (format!("\"<{{{a}}}>\""), ta)
            }
            1 => {
                let (a, ta) = self.text(d);
                let (b, tb) = self.text(d);
                (format!("\"{{{a}}} and {{{b}}}\""), ta.max(tb))
            }
            2 => {
                let (a, ta) = self.text(d);
                let (b, tb) = self.text(d);
                self.used.join = true;
                (format!("({a} + {b})"), ta.max(tb))
            }
            3 => {
                let (a, ta) = self.text(d);
                let (b, tb) = self.text(d);
                self.used.generic = true;
                self.used.join = true;
                (format!("pick({a}, {b}, flag)"), ta.max(tb))
            }
            4 => {
                let (a, ta) = self.text(d);
                self.used.generic = true;
                (format!("keep({a})"), ta)
            }
            5 => {
                let (a, ta) = self.text(d);
                self.used.generic = true;
                self.used.closure = true;
                (format!("apply(|s| s, {a})"), ta)
            }
            6 => {
                // A closure capturing a value from outside.
                let (a, ta) = self.text(d);
                let (b, tb) = self.text(d);
                self.used.generic = true;
                self.used.closure = true;
                (format!("apply(|s| \"{{s}} {{{b}}}\", {a})"), ta.max(tb))
            }
            7 => {
                let (a, ta) = self.text(d);
                let (b, tb) = self.text(d);
                self.used.join = true;
                (format!("(list.first([{a}]) ?? {b})"), ta.max(tb))
            }
            8 => {
                let (a, ta) = self.text(d);
                let (b, tb) = self.text(d);
                self.used.join = true;
                (format!("if flag {{ {a} }} else {{ {b} }}"), ta.max(tb))
            }
            9 => {
                let (a, ta) = self.text(d);
                // `S` normalises to `Str` or `Untrusted<Str>`: a `Brief` comes back untrusted.
                self.exact &= ta != Taint::Brief;
                (format!("text.upper({a})"), ta)
            }
            10 => {
                let (a, ta) = self.text(d);
                let (b, tb) = self.text(d);
                self.exact &= ta != Taint::Brief && tb != Taint::Brief;
                self.used.closure = true;
                self.used.join = true;
                (
                    format!("text.join(list.map([{a}, {b}], |s| s), \", \")"),
                    ta.max(tb),
                )
            }
            11 => {
                let (a, _) = self.text(d);
                self.used.trust = true;
                (format!("trust({a}, because: \"reviewed\")"), Taint::Clean)
            }
            12 => {
                let (a, _) = self.text(d);
                self.used.as_task = true;
                (
                    format!("as_task({a}, because: \"our planner\")"),
                    Taint::Brief,
                )
            }
            13 => {
                let (a, ta) = self.text(d);
                // `text.show` is untrusted whatever it shows.
                self.exact &= ta == Taint::Untrusted;
                (format!("text.show({a})"), ta)
            }
            _ => {
                let (a, ta) = self.text(d);
                // Through `Json`, which counts as untrusted.
                self.exact &= ta == Taint::Untrusted;
                (format!("json.decode::<Str>(json.parse({a}))"), ta)
            }
        }
    }
}

/// What a section requires of its content.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Need {
    Trusted,
    Task,
    Anything,
}

struct Program {
    source: String,
    /// Whether the ground truth says every sink is safe.
    safe: bool,
    exact: bool,
    trusted_prompt: bool,
    used: Used,
}

fn generate(rng_seed: u64) -> Program {
    let mut g = Gen {
        rng: Rng(rng_seed),
        locals: Vec::new(),
        exact: true,
        used: Used::default(),
    };
    let mut body = String::new();
    for i in 0..g.rng.below(4) {
        let (e, t) = g.text(2);
        body.push_str(&format!("  let a{i} = {e}\n"));
        g.locals.push((format!("a{i}"), t));
    }
    if g.rng.chance(1, 3) {
        // A `var` assigned in a loop: declared `Str`, so only trusted text may be assigned.
        let (a, ta) = g.text(1);
        let (b, tb) = g.text(1);
        let t = ta.max(tb);
        // Assigning anything but trusted text to the `Str` var is refused where it happens.
        g.exact &= t == Taint::Clean;
        body.push_str(&format!(
            "  var acc = \"start\"\n  for x in [{a}, {b}] {{\n    acc = \"{{acc}}{{x}}\"\n  }}\n"
        ));
        g.locals.push(("acc".to_owned(), t));
    }
    let trusted_prompt = g.rng.chance(1, 3);
    let mut safe = true;
    let mut sections = String::new();
    let mut prompt_trusted = true;
    for _ in 0..=g.rng.below(3) {
        let (kind, need) = match g.rng.below(8) {
            0 => ("system", Need::Trusted),
            1 | 2 => ("instruct", Need::Trusted),
            3 => ("note", Need::Trusted),
            4 | 5 => ("task", Need::Task),
            6 => ("data", Need::Anything),
            _ => ("steps", Need::Trusted),
        };
        let (e, t) = g.text(2);
        let line = match kind {
            "data" => format!("    data {e} as \"evidence\"\n"),
            "steps" => format!("    steps [{e}, \"Answer\"]\n"),
            _ => format!("    {kind} {e}\n"),
        };
        sections.push_str(&line);
        safe &= match need {
            Need::Trusted => t == Taint::Clean,
            Need::Task => t != Taint::Untrusted,
            Need::Anything => true,
        };
        prompt_trusted &= need != Need::Anything && t == Taint::Clean;
    }
    if trusted_prompt {
        safe &= prompt_trusted;
    }
    let ret = if trusted_prompt {
        "TrustedPrompt"
    } else {
        "Prompt"
    };
    let source = format!(
        "type Verdict = {{ pass: Bool, note: Str }}\n\
         fn pick<T>(a: T, b: T, c: Bool) -> T {{ if c {{ a }} else {{ b }} }}\n\
         fn keep<T>(x: T) -> T {{ x }}\n\
         fn apply<T, U>(f: fn(T) -> U, x: T) -> U {{ f(x) }}\n\
         fn case(web: Untrusted<Str>, task: Task, user: UserText, v: Untrusted<Verdict>, n: Int, flag: Bool) -> {ret} {{\n\
         {body}  prompt {{\n{sections}  }}\n}}\n"
    );
    Program {
        source,
        safe,
        exact: g.exact,
        trusted_prompt,
        used: g.used,
    }
}

fn check(source: &str) -> (Vec<Code>, Vec<String>) {
    let package = PackageId::new(Source::User, PackageName::parse("app").unwrap());
    let main = RelPath::parse("main.ratio").unwrap();
    let mut registry = MemoryRegistry::new();
    registry.set_entry(&package, main.clone());
    registry.add_file(&package, main, source.as_bytes().to_vec());
    let builtin = MemoryRegistry::new();
    let loaded = Loader::new(&registry, &builtin).load(&QualifiedName::parse("user:app").unwrap());
    assert!(
        loaded.is_clean(),
        "the generator wrote a program that does not parse:\n{source}\n{:?}",
        loaded.modules[0].syntax
    );
    let checked = ratio_check::check(&loaded);
    let errors: Vec<&ratio_check::Diagnostic> = checked
        .diagnostics
        .iter()
        .filter(|d| !d.code.is_warning())
        .collect();
    (
        errors.iter().map(|d| d.code).collect(),
        errors.iter().map(ToString::to_string).collect(),
    )
}

const CASES: u64 = 1000;

#[test]
fn no_untrusted_source_reaches_an_instruction_or_task_without_a_trust_site() {
    let mut accepted = 0;
    let mut refused = 0;
    let mut accepted_with = Used::default();
    let mut refused_unsafe_joins = 0;
    let mut trusted_prompts_accepted = 0;
    for case in 0..CASES {
        let p = generate(0x5EED_0005_u64.wrapping_add(case.wrapping_mul(0x9E37_79B9_7F4A_7C15)));
        let (codes, messages) = check(&p.source);
        if codes.is_empty() {
            accepted += 1;
            assert!(
                p.safe,
                "case {case}: the checker accepted a program that puts untrusted text in an \
                 instruction or task position:\n{}",
                p.source
            );
            accepted_with.trust |= p.used.trust;
            accepted_with.as_task |= p.used.as_task;
            accepted_with.generic |= p.used.generic;
            accepted_with.closure |= p.used.closure;
            accepted_with.join |= p.used.join;
            accepted_with.untrusted_source |= p.used.untrusted_source;
            trusted_prompts_accepted += usize::from(p.trusted_prompt);
        } else {
            refused += 1;
            assert!(
                !(p.safe && p.exact),
                "case {case}: a safe program built from exactly typed constructs was refused \
                 ({messages:?}):\n{}",
                p.source
            );
            assert!(
                codes.iter().all(|c| *c == Code::Untrusted),
                "case {case}: refused for something other than taint ({messages:?}):\n{}",
                p.source
            );
            if !p.safe && p.used.join && p.used.generic {
                refused_unsafe_joins += 1;
            }
        }
    }
    eprintln!(
        "{accepted} accepted, {refused} refused, {trusted_prompts_accepted} trusted prompts accepted, {refused_unsafe_joins} unsafe joins through generics refused"
    );
    // Coverage: both outcomes are common, and accepted programs exercise every construct.
    assert!(accepted > CASES / 10, "only {accepted} accepted");
    assert!(refused > CASES / 10, "only {refused} refused");
    assert!(accepted_with.trust, "no accepted program used `trust`");
    assert!(accepted_with.as_task, "no accepted program used `as_task`");
    assert!(accepted_with.generic && accepted_with.closure && accepted_with.join);
    assert!(
        accepted_with.untrusted_source,
        "no accepted program read an untrusted source"
    );
    assert!(trusted_prompts_accepted > 20, "{trusted_prompts_accepted}");
    assert!(refused_unsafe_joins > 50, "{refused_unsafe_joins}");
}

/// The secrecy half of the property (B2a): a statically secret value never reaches a generic
/// helper that may reach a secrecy sink, by whatever route the helper is called: directly,
/// through a function value (annotated, wrapped by a generic `id`, a record field, returned by
/// a function), through `apply` with a turbofish, a pipe or a closure. The same routes with
/// plain text, or with a helper that keeps its argument local, are accepted.
#[test]
fn no_secret_reaches_a_sink_reaching_helper_through_any_route() {
    const ROUTES: usize = 8;
    let mut rng = Rng(0x5EC2_E7E7_u64);
    let mut refused = [0usize; ROUTES];
    let mut accepted = [0usize; ROUTES];
    for case in 0..400 {
        let route = rng.below(ROUTES);
        let secret = rng.chance(1, 2);
        let sinking = rng.chance(2, 3);
        let (arg, ty) = if secret {
            ("tok()", "Secret<UserText>")
        } else {
            ("\"plain\"", "Str")
        };
        let h = if sinking { "send" } else { "quiet" };
        let call = match route {
            0 => format!("  {h}({arg})\n"),
            1 => format!("  let f: fn({ty}) -> () = {h}\n  f({arg})\n"),
            2 => format!("  apply::<{ty}, ()>({h}, {arg})\n"),
            3 => format!("  let f = id::<fn({ty}) -> ()>({h})\n  f({arg})\n"),
            4 => format!("  let r: {{ g: fn({ty}) -> () }} = {{ g: {h} }}\n  r.g({arg})\n"),
            5 => format!("  {arg} |> {h}()\n"),
            6 => format!("  let c = |v: {ty}| {h}(v)\n  c({arg})\n"),
            _ => format!("  let f = get_{h}_{}()\n  f({arg})\n", u8::from(secret)),
        };
        let source = format!(
            "fn tok() -> Secret<UserText> {{\n  let a = session.ask([{{ question: \"p\", options: [], recommended: \"\", secret: true }}])\n  a[0]\n}}\n\
             fn send<T>(x: T) -> () {{\n  let _ = tools.web_fetch(\"{{x}}\")\n}}\n\
             fn quiet<T>(x: T) -> () {{\n  let _ = x\n}}\n\
             fn id<T>(x: T) -> T {{\n  x\n}}\n\
             fn apply<A, B>(f: fn(A) -> B, a: A) -> B {{\n  f(a)\n}}\n\
             fn get_{h}_{s}() -> fn({ty}) -> () {{\n  {h}\n}}\n\
             fn case() -> () {{\n{call}}}\n",
            s = u8::from(secret)
        );
        let (codes, messages) = check(&source);
        let unsafe_flow = secret && sinking;
        if codes.is_empty() {
            assert!(
                !unsafe_flow,
                "case {case}: a secret reached a sink-reaching helper by route {route}:\n{source}"
            );
            accepted[route] += 1;
        } else {
            assert!(
                unsafe_flow,
                "case {case}: a safe flow by route {route} was refused ({messages:?}):\n{source}"
            );
            assert!(
                codes.iter().all(|c| *c == Code::Secret),
                "case {case}: refused for something other than secrecy ({messages:?}):\n{source}"
            );
            refused[route] += 1;
        }
    }
    for route in 0..ROUTES {
        assert!(
            refused[route] > 5 && accepted[route] > 5,
            "route {route}: {} refused, {} accepted",
            refused[route],
            accepted[route]
        );
    }
}
