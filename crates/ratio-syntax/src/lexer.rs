//! The token reader.
//!
//! One pass over the source with an explicit mode stack: code mode reads tokens, a quote pushes a
//! string mode, a `{` in string text pushes an interpolation (code mode again, with a brace depth
//! of zero) and its closing `}` pops back to the string. Semicolons are inserted at line ends by
//! the rules of chapter 02. Reading never panics and never recurses; whatever the input, it
//! returns a token stream that ends in `Eof` and keeps strings and interpolations balanced.
//!
//! Semicolon insertion depends on what the innermost `{` opened, which the grammar decides with
//! the parser's help (a block or a record). The reader approximates the parser with the keyword
//! that led to the `{` and rule 1 of chapter 02's disambiguation rules (after `with` a record then
//! a block; after a type name a variant or record literal; elsewhere a record starts with `}`,
//! `..`, or a name followed by `:` or `,`); each token's [`BraceKind`] records the decision.

use crate::diag::{Code, Diagnostic};
use crate::number;
use crate::token::{BraceKind, Contextual, Keyword, Span, Token, TokenKind};

/// The largest interpolation nesting a configuration may allow (chapter 07).
pub const MAX_INTERPOLATION_DEPTH: usize = 8;
/// The largest bracket, block and interpolation nesting a configuration may allow (chapter 07).
pub const MAX_NESTING: usize = 256;
/// The largest source a configuration may allow, in bytes (chapter 07: 256 KiB per file).
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
/// The most diagnostics a configuration may ask the reader to report before it stops.
pub const MAX_DIAGNOSTICS: usize = 100;

/// Open frames, pending hints and the like are bounded so that hostile input costs linear time.
const HINT_CAP: usize = 256;
/// How far down the frame stack an unmatched closing delimiter looks for its opener.
const CLOSE_SEARCH_DEPTH: usize = 64;

/// The limits that apply while reading tokens (chapter 07's table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexOptions {
    /// Package source per file: 256 KiB, also the maximum [`MAX_SOURCE_BYTES`] (an authored
    /// harness: 64 KiB, see [`LexOptions::authored`]).
    pub max_source_bytes: usize,
    /// Interpolation nesting inside strings: default 4, at most [`MAX_INTERPOLATION_DEPTH`].
    pub max_interpolation_depth: usize,
    /// Brackets, blocks and interpolations: default 128, at most [`MAX_NESTING`].
    pub max_nesting: usize,
    /// Diagnostics reported before the reader stops: default 100, also the maximum
    /// [`MAX_DIAGNOSTICS`]. When one more error is found, the reader adds a final
    /// [`Code::TooManyErrors`] and stops, so a read reports at most this many plus that one.
    pub max_diagnostics: usize,
}

impl Default for LexOptions {
    fn default() -> Self {
        LexOptions {
            max_source_bytes: MAX_SOURCE_BYTES,
            max_interpolation_depth: 4,
            max_nesting: 128,
            max_diagnostics: MAX_DIAGNOSTICS,
        }
    }
}

impl LexOptions {
    /// The limits for an authored harness (source of at most 64 KiB).
    #[must_use]
    pub fn authored() -> Self {
        LexOptions {
            max_source_bytes: 64 * 1024,
            ..LexOptions::default()
        }
    }
}

/// The result of reading a source: tokens (always ending in `Eof`) and diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Lexed {
    /// Whether the source was read without a diagnostic.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Reads `source` with the default limits.
#[must_use]
pub fn lex(source: &str) -> Lexed {
    lex_with(source, &LexOptions::default())
}

/// Reads `source` with the given limits (raised no further than the maximums of chapter 07).
#[must_use]
pub fn lex_with(source: &str, options: &LexOptions) -> Lexed {
    let mut options = options.clone();
    options.max_interpolation_depth = options.max_interpolation_depth.min(MAX_INTERPOLATION_DEPTH);
    options.max_nesting = options.max_nesting.min(MAX_NESTING);
    options.max_source_bytes = options.max_source_bytes.min(MAX_SOURCE_BYTES);
    options.max_diagnostics = options.max_diagnostics.min(MAX_DIAGNOSTICS);
    Lexer::new(source, options).run()
}

#[derive(Clone, Copy)]
struct Mark {
    off: usize,
    line: u32,
    col: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Delim {
    Paren,
    Bracket,
    Brace(BraceKind),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Paren,
    Bracket,
    Brace,
}

impl Want {
    fn close_char(self) -> char {
        match self {
            Want::Paren => ')',
            Want::Bracket => ']',
            Want::Brace => '}',
        }
    }
}

fn delim_open_char(delim: Delim) -> char {
    match delim {
        Delim::Paren => '(',
        Delim::Bracket => '[',
        Delim::Brace(_) => '{',
    }
}

enum Frame {
    Delim { delim: Delim, open: Mark },
    Interp { open: Mark },
    Str(StrFrame),
}

struct StrFrame {
    long: bool,
    raw: bool,
    start: Mark,
    items: Vec<Item>,
    texts: Vec<Text>,
    raw_chars: Vec<(char, bool)>,
}

/// A run of string text; `chars` pairs each character with whether an escape produced it (an
/// escaped character is never indentation and never a line break).
struct Text {
    token: usize,
    chars: Vec<(char, bool)>,
}

enum Item {
    Text(usize),
    Interp,
}

/// What the next `{` at a nesting depth opens, decided by the keyword before it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PendKind {
    /// A block (`if`, `else`, `loop`, `par`, ...).
    Block,
    Match,
    Enum,
    /// The head of a `fn`, `harness` or `prompt` declaration, up to the `{` of its body.
    Head {
        prompt: bool,
    },
    /// A prompt section's `{` (`if`, `else`, `for` sections; a `prompt { }` expression).
    PromptBlock,
    Catch,
    Record,
    /// `with { config } { body }`: a record, then a block.
    WithRecord,
}

#[derive(Clone, Copy)]
struct Pending {
    kind: PendKind,
    depth: usize,
}

/// Where a pattern ends: the `=` or `:` of a `let`, the `in` of a `for`, the `=>` of an arm.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PatEnd {
    Let,
    In,
    Arrow,
}

#[derive(Clone, Copy)]
struct Pat {
    end: PatEnd,
    depth: usize,
}

/// Where the reader is in an attribute that starts a line (`@name` or `@name(…)`). While one is
/// open, a line end never inserts a semicolon: the attribute belongs to the item below it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Attr {
    /// After the `@`.
    At,
    /// After the name, or after the closing `)` of the arguments.
    Named,
    /// Inside the arguments; the frame stack's depth outside their `(`.
    Args(usize),
}

/// What the reader knew about the previous token when a word was read.
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about the previous token, not a state machine"
)]
struct WordCtx {
    prev_rparen: bool,
    prev_type_position: bool,
    prev_colon: bool,
    stmt_start: bool,
    after_prompt_body: bool,
}

enum Word {
    Ident,
    Keyword(Keyword),
    Contextual(Contextual),
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent reader flags, not a state machine"
)]
struct Lexer<'a> {
    src: &'a str,
    opts: LexOptions,
    pos: usize,
    line: u32,
    col: u32,
    out: Vec<Token>,
    diags: Vec<Diagnostic>,
    stack: Vec<Frame>,
    pendings: Vec<Pending>,
    pats: Vec<Pat>,
    last_sig: Option<usize>,
    closed_prompt_body: bool,
    type_decl: bool,
    /// Inside a prompt body's `format` section, up to its statement end.
    format_section: bool,
    attr: Option<Attr>,
    /// The frame depths of the `<` read since the last statement end that no `>` has closed yet
    /// (most recent last). A `,` while one is open at the current depth is in type position.
    /// A `<` is recorded whether it opens a generic argument list or is a comparison (the reader
    /// cannot tell), so an unmatched comparison `<` can leave an entry until the statement ends.
    angles: Vec<usize>,
    /// The frame depth of the `|…|` parameter list of a closure being read: its commas do not
    /// start a new match arm.
    closure_params: Option<usize>,
    /// The index of the token that is a `>` which closed a recorded `<` (for the `>` statement
    /// end rule: only such a `>` can end a type declaration or a `format` line).
    closed_angle: Option<usize>,
    halted: bool,
    interp_depth: usize,
    nesting: usize,
    short_open: usize,
    /// The end of the blank and comment-only region the last continuation check scanned, and its
    /// answer (so a run of blank lines is scanned once).
    cont_cache: Option<(usize, bool)>,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Skips whitespace and `//` comments (a doc comment is skipped too).
///
/// This lookahead uses `trim_start`, which skips all Unicode whitespace (a no-break space, a lone
/// carriage return), while code mode accepts only spaces, tabs and line ends. That is harmless:
/// any other whitespace character is reported as an error when the reader reaches it, so the
/// lookahead can differ from the reader only on a source that is refused anyway, where the guess
/// it informs (a brace kind, a contextual keyword) no longer matters.
fn skip_trivia(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if s.starts_with("//") {
            s = match s.find('\n') {
                Some(i) => s.get(i..).unwrap_or(""),
                None => "",
            };
        } else {
            return s;
        }
    }
}

fn starts_with_word(s: &str, word: &str) -> bool {
    s.strip_prefix(word)
        .is_some_and(|rest| !rest.chars().next().is_some_and(is_ident_continue))
}

/// `name :` (a parameter) as opposed to a type.
fn named_param(s: &str) -> bool {
    if !s.starts_with(is_ident_start) {
        return false;
    }
    let len = s.chars().take_while(|c| is_ident_continue(*c)).count();
    let after = skip_trivia(s.get(len..).unwrap_or(""));
    after.starts_with(':') && !after.starts_with("::")
}

/// After `fn()` that follows a `:`: whether it is a function expression (`a: fn() {}`,
/// `a: fn() -> T { x }`) rather than a function type (`a: fn() -> T`). It is an expression when a
/// `{` comes next, or after the result type. Looks ahead a bounded distance, on the same line.
fn fn_expr_ahead(after_paren: &str) -> bool {
    let s = skip_trivia(after_paren);
    if s.starts_with('{') {
        return true;
    }
    let Some(ty) = s.strip_prefix("->") else {
        return false;
    };
    let bytes = ty.as_bytes();
    let mut depth = 0usize;
    // The last byte that was not a space; `~` stands for an arrow.
    let mut prev = b'~';
    let mut i = 0;
    while i < bytes.len() && i < 512 {
        let c = bytes[i];
        match c {
            b'-' if bytes.get(i + 1) == Some(&b'>') => {
                i += 1;
                prev = b'~';
            }
            b'{' if depth == 0 && (prev.is_ascii_alphanumeric() || b"_>])?}".contains(&prev)) => {
                return true;
            }
            b'(' | b'[' | b'<' | b'{' => {
                depth += 1;
                prev = c;
            }
            b')' | b']' | b'}' | b'>' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
                prev = c;
            }
            b',' | b'=' | b';' | b'\n' | b'/' if depth == 0 => return false,
            b' ' | b'\t' | b'\r' => {}
            _ => prev = c,
        }
        i += 1;
    }
    false
}

/// Rule 1 of the disambiguation rules: a record starts with `}`, `..`, or a name followed by `:`
/// or `,` (a field name may carry a `?` in a record type). `@` starts an attribute on a field.
fn looks_like_record(after_brace: &str) -> bool {
    let s = skip_trivia(after_brace);
    if s.starts_with('}') || s.starts_with("..") || s.starts_with('@') {
        return true;
    }
    if !s.starts_with(is_ident_start) {
        return false;
    }
    let len = s.chars().take_while(|c| is_ident_continue(*c)).count();
    let after = skip_trivia(s.get(len..).unwrap_or(""));
    let after = after.strip_prefix('?').unwrap_or(after);
    after.starts_with(',') || (after.starts_with(':') && !after.starts_with("::"))
}

/// A type name: an identifier spelled `UpperCamel` (the last part of `a.B` too).
fn is_type_name(kind: &TokenKind) -> bool {
    matches!(kind, TokenKind::Ident(name) if name.starts_with(|c: char| c.is_ascii_uppercase()))
}

/// Whether a token at the end of a line can end a statement (condition 2).
fn ends_statement(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Ident(_)
            | TokenKind::Int(_)
            | TokenKind::Quantity(_)
            | TokenKind::Float(_)
            | TokenKind::Duration(_)
            | TokenKind::Money(_)
            | TokenKind::StringEnd
            | TokenKind::RawString { .. }
            | TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::RBrace
            | TokenKind::Question
            | TokenKind::Keyword(
                Keyword::Break
                    | Keyword::Continue
                    | Keyword::Return
                    | Keyword::None
                    | Keyword::True
                    | Keyword::False
            )
    )
}

/// Operators and separators (not the delimiters, which change the frame stack).
fn operator(rest: &str) -> Option<(usize, TokenKind)> {
    use TokenKind as T;
    let mut bytes = rest.bytes();
    let a = bytes.next()?;
    let b = bytes.next();
    let c = bytes.next();
    Some(match (a, b, c) {
        (b'.', Some(b'.'), Some(b'=')) => (3, T::DotDotEq),
        (b'.', Some(b'.'), _) => (2, T::DotDot),
        (b'.', _, _) => (1, T::Dot),
        (b':', Some(b':'), _) => (2, T::ColonColon),
        (b':', _, _) => (1, T::Colon),
        (b'?', Some(b'.'), _) => (2, T::QuestionDot),
        (b'?', Some(b'?'), _) => (2, T::QuestionQuestion),
        (b'?', _, _) => (1, T::Question),
        (b'-', Some(b'>'), _) => (2, T::Arrow),
        (b'-', Some(b'='), _) => (2, T::MinusEq),
        (b'-', _, _) => (1, T::Minus),
        (b'=', Some(b'='), _) => (2, T::EqEq),
        (b'=', Some(b'>'), _) => (2, T::FatArrow),
        (b'=', _, _) => (1, T::Eq),
        (b'!', Some(b'='), _) => (2, T::NotEq),
        (b'<', Some(b'='), _) => (2, T::Le),
        (b'<', _, _) => (1, T::Lt),
        (b'>', Some(b'='), _) => (2, T::Ge),
        (b'>', _, _) => (1, T::Gt),
        (b'+', Some(b'='), _) => (2, T::PlusEq),
        (b'+', _, _) => (1, T::Plus),
        (b'*', Some(b'='), _) => (2, T::StarEq),
        (b'*', _, _) => (1, T::Star),
        (b'/', Some(b'='), _) => (2, T::SlashEq),
        (b'/', _, _) => (1, T::Slash),
        (b'%', _, _) => (1, T::Percent),
        (b'|', Some(b'|'), _) => (2, T::PipePipe),
        (b'|', Some(b'>'), _) => (2, T::PipeGt),
        (b'|', _, _) => (1, T::Pipe),
        (b',', _, _) => (1, T::Comma),
        (b';', _, _) => (1, T::Semi { inserted: false }),
        (b'@', _, _) => (1, T::At),
        _ => return None,
    })
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, opts: LexOptions) -> Self {
        Lexer {
            src,
            opts,
            pos: 0,
            line: 1,
            col: 1,
            out: Vec::new(),
            diags: Vec::new(),
            stack: Vec::new(),
            pendings: Vec::new(),
            pats: Vec::new(),
            last_sig: None,
            closed_prompt_body: false,
            type_decl: false,
            closure_params: None,
            format_section: false,
            closed_angle: None,
            attr: None,
            angles: Vec::new(),
            halted: false,
            interp_depth: 0,
            nesting: 0,
            short_open: 0,
            cont_cache: None,
        }
    }

    fn run(mut self) -> Lexed {
        if self.src.len() > self.opts.max_source_bytes {
            let span = self.span_from(self.mark());
            let message = format!(
                "the source is {} bytes; a file may hold at most {}",
                self.src.len(),
                self.opts.max_source_bytes
            );
            self.report(Code::SourceTooLarge, message, span);
            let eof = self.span_from(self.mark());
            self.out.push(Token {
                kind: TokenKind::Eof,
                span: eof,
            });
            return self.into_lexed();
        }
        if self.rest().starts_with('\u{feff}') {
            self.pos += '\u{feff}'.len_utf8();
        }
        while !self.halted {
            let in_string = matches!(self.stack.last(), Some(Frame::Str(_)));
            if in_string {
                self.string_step();
            } else if !self.code_step() {
                break;
            }
        }
        self.finish();
        self.into_lexed()
    }

    fn into_lexed(self) -> Lexed {
        Lexed {
            tokens: self.out,
            diagnostics: self.diags,
        }
    }

    // ---- cursor ----

    fn rest(&self) -> &'a str {
        self.src.get(self.pos..).unwrap_or("")
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek2(&self) -> Option<char> {
        self.rest().chars().nth(1)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line = self.line.saturating_add(1);
            self.col = 1;
        } else {
            self.col = self.col.saturating_add(1);
        }
        Some(c)
    }

    fn mark(&self) -> Mark {
        Mark {
            off: self.pos,
            line: self.line,
            col: self.col,
        }
    }

    fn span_from(&self, m: Mark) -> Span {
        Span {
            start: m.off,
            end: self.pos,
            line: m.line,
            col: m.col,
            end_line: self.line,
            end_col: self.col,
        }
    }

    /// A zero-width span at an opening mark, for diagnostics about unclosed things.
    fn point_span(m: Mark) -> Span {
        Span {
            start: m.off,
            end: m.off,
            line: m.line,
            col: m.col,
            end_line: m.line,
            end_col: m.col,
        }
    }

    fn line_break_here(&self) -> bool {
        match self.peek() {
            Some('\n') => true,
            Some('\r') => self.peek2() == Some('\n'),
            _ => false,
        }
    }

    // ---- output ----

    fn report(&mut self, code: Code, message: impl Into<String>, span: Span) {
        if self.halted {
            return;
        }
        if self.diags.len() >= self.opts.max_diagnostics {
            // One error more than the cap: say so where it was found, and stop.
            self.halted = true;
            self.diags.push(Diagnostic {
                code: Code::TooManyErrors,
                message: "too many errors; the reader stops here".to_owned(),
                span,
            });
            return;
        }
        self.diags.push(Diagnostic {
            code,
            message: message.into(),
            span,
        });
        if code == Code::NestingTooDeep {
            // Past the nesting limit nothing more is worth reading: every further token would be
            // deeper still, and the parser refuses the file anyway.
            self.halted = true;
        }
    }

    fn push_tok(&mut self, kind: TokenKind, m: Mark) -> usize {
        let span = self.span_from(m);
        self.push_tok_span(kind, span)
    }

    fn push_tok_span(&mut self, kind: TokenKind, span: Span) -> usize {
        let is_doc = matches!(kind, TokenKind::DocComment(_));
        if !is_doc {
            self.track_attr(&kind);
        }
        self.out.push(Token { kind, span });
        let index = self.out.len() - 1;
        if !is_doc {
            self.last_sig = Some(index);
            self.closed_prompt_body = false;
        }
        index
    }

    /// Follows an attribute at the start of a line through its name and arguments; any other
    /// token ends it. Called before `kind` becomes the previous token.
    fn track_attr(&mut self, kind: &TokenKind) {
        let depth = self.stack.len();
        self.attr = match (self.attr, kind) {
            (Some(Attr::At), TokenKind::Ident(_) | TokenKind::Keyword(_)) => Some(Attr::Named),
            (Some(Attr::Named), TokenKind::LParen) => Some(Attr::Args(depth)),
            (Some(Attr::Args(outer)), TokenKind::RParen) if depth == outer => Some(Attr::Named),
            (Some(Attr::Args(outer)), _) => Some(Attr::Args(outer)),
            (_, TokenKind::At) if self.at_stmt_start() => Some(Attr::At),
            _ => None,
        };
    }

    fn prev_kind(&self) -> Option<&TokenKind> {
        self.last_sig.and_then(|i| self.out.get(i)).map(|t| &t.kind)
    }

    // ---- frames ----

    fn push_delim(&mut self, delim: Delim, open: Mark) {
        self.stack.push(Frame::Delim { delim, open });
        self.nesting += 1;
        self.check_nesting(open);
    }

    fn check_nesting(&mut self, at: Mark) {
        if self.nesting > self.opts.max_nesting {
            let span = Self::point_span(at);
            let message = format!(
                "brackets, blocks and interpolations nest deeper than {}",
                self.opts.max_nesting
            );
            self.report(Code::NestingTooDeep, message, span);
        }
    }

    fn pop_frame(&mut self) -> Option<Frame> {
        let frame = self.stack.pop()?;
        match &frame {
            Frame::Delim { .. } => self.nesting = self.nesting.saturating_sub(1),
            Frame::Interp { .. } => {
                self.nesting = self.nesting.saturating_sub(1);
                self.interp_depth = self.interp_depth.saturating_sub(1);
            }
            Frame::Str(_) => {}
        }
        let len = self.stack.len();
        if self.closure_params.is_some_and(|d| d > len) {
            self.closure_params = None;
        }
        while self.pendings.last().is_some_and(|p| p.depth > len) {
            self.pendings.pop();
        }
        while self.pats.last().is_some_and(|p| p.depth > len) {
            self.pats.pop();
        }
        while self.angles.last().is_some_and(|d| *d > len) {
            self.angles.pop();
        }
        Some(frame)
    }

    fn add_pending(&mut self, kind: PendKind) {
        let depth = self.stack.len();
        let duplicate = kind == PendKind::Block
            && self
                .pendings
                .last()
                .is_some_and(|p| p.depth == depth && p.kind == PendKind::Block);
        if duplicate || self.pendings.len() >= HINT_CAP {
            return;
        }
        self.pendings.push(Pending { kind, depth });
    }

    fn add_pat(&mut self, end: PatEnd) {
        if self.pats.len() < HINT_CAP {
            let depth = self.stack.len();
            self.pats.push(Pat { end, depth });
        }
    }

    fn clear_pats(&mut self, end: PatEnd) {
        let depth = self.stack.len();
        self.pats.retain(|p| p.depth != depth || p.end != end);
    }

    fn head_pending_here(&self) -> bool {
        let depth = self.stack.len();
        self.pendings
            .iter()
            .rev()
            .take_while(|p| p.depth == depth)
            .any(|p| matches!(p.kind, PendKind::Head { .. }))
    }

    fn in_prompt_body(&self) -> bool {
        matches!(
            self.stack.last(),
            Some(Frame::Delim {
                delim: Delim::Brace(BraceKind::PromptBody),
                ..
            })
        )
    }

    /// Whether the next token starts a statement or item: after a `;`, a `{`, the start of the
    /// file, or a complete attribute (`@turn` then `pub harness …`).
    fn at_stmt_start(&self) -> bool {
        self.attr == Some(Attr::Named)
            || matches!(
                self.prev_kind(),
                None | Some(TokenKind::Semi { .. } | TokenKind::LBrace(_))
            )
    }

    // ---- code mode ----

    /// Reads one token or one piece of trivia. Returns `false` at the end of the source.
    fn code_step(&mut self) -> bool {
        let Some(c) = self.peek() else {
            return false;
        };
        match c {
            ' ' | '\t' => {
                self.bump();
            }
            '\n' => self.newline_in_code(1),
            '\r' => {
                if self.peek2() == Some('\n') {
                    self.newline_in_code(2);
                } else {
                    let m = self.mark();
                    self.bump();
                    let span = self.span_from(m);
                    self.report(
                        Code::BareCarriageReturn,
                        "a carriage return must be followed by a line feed",
                        span,
                    );
                }
            }
            '/' if self.rest().starts_with("//") => self.line_comment(),
            '"' => self.open_string(false),
            'r' if self.peek2() == Some('"') => self.open_string(true),
            '0'..='9' | '$' => self.number(),
            c if is_ident_start(c) => self.word(),
            c => self.punct(c),
        }
        true
    }

    fn line_comment(&mut self) {
        let m = self.mark();
        // `////…` is a documentation comment too: it starts with `///`.
        let doc = self.rest().starts_with("///");
        while !self.line_break_here() && self.peek().is_some() {
            let at = self.mark();
            if self.bump() == Some('\r') {
                let span = self.span_from(at);
                self.report(
                    Code::BareCarriageReturn,
                    "a carriage return must be followed by a line feed",
                    span,
                );
            }
        }
        if doc {
            let text = self.src.get(m.off + 3..self.pos).unwrap_or("").to_owned();
            self.push_tok(TokenKind::DocComment(text), m);
        }
    }

    /// A line end in code mode: insert a semicolon when the rules say so. `len` is the length
    /// of the line terminator in characters.
    fn newline_in_code(&mut self, len: usize) {
        if self.short_open > 0 {
            let m = self.mark();
            for _ in 0..len {
                self.bump();
            }
            let span = self.span_from(m);
            self.report(
                Code::LineBreakInInterpolation,
                "an interpolation in a short string cannot contain a line break",
                span,
            );
            return;
        }
        if self.should_insert() {
            let m = self.mark();
            self.push_tok(TokenKind::Semi { inserted: true }, m);
            self.on_semi();
        }
        for _ in 0..len {
            self.bump();
        }
    }

    fn on_semi(&mut self) {
        let depth = self.stack.len();
        self.type_decl = false;
        self.format_section = false;
        while self.angles.last().is_some_and(|d| *d >= depth) {
            self.angles.pop();
        }
        while self.pendings.last().is_some_and(|p| p.depth >= depth) {
            self.pendings.pop();
        }
        while self.pats.last().is_some_and(|p| p.depth >= depth) {
            self.pats.pop();
        }
    }

    /// Conditions 1 and 2 of the insertion rule, then condition 3 by looking ahead.
    fn should_insert(&mut self) -> bool {
        let innermost_is_block = matches!(
            self.stack.last(),
            None | Some(Frame::Delim {
                delim: Delim::Brace(BraceKind::Block | BraceKind::PromptBody),
                ..
            })
        );
        if !innermost_is_block || self.head_pending_here() || self.attr.is_some() {
            return false;
        }
        // In a type declaration or a `format` section a closing `>` or a `??` ends the type.
        let closes_type = (self.type_decl || self.format_section)
            && ((self.last_sig.is_some() && self.last_sig == self.closed_angle)
                || matches!(self.prev_kind(), Some(TokenKind::QuestionQuestion)));
        let ends = closes_type || self.prev_kind().is_some_and(ends_statement);
        ends && !self.continuation_follows()
    }

    /// Condition 3: the next non-blank line starts with `.`, `?.`, `|>`, `??`, `and`, `or`,
    /// `else` or `catch`. Comment-only lines count as blank; a doc comment does not.
    fn continuation_follows(&mut self) -> bool {
        if let Some((until, answer)) = self.cont_cache
            && self.pos < until
        {
            return answer;
        }
        let rest = self.rest();
        let bytes = rest.as_bytes();
        let mut i = 0;
        loop {
            match bytes.get(i) {
                Some(b' ' | b'\t' | b'\r' | b'\n') => i += 1,
                Some(b'/')
                    if bytes.get(i + 1) == Some(&b'/') && bytes.get(i + 2) != Some(&b'/') =>
                {
                    while bytes.get(i).is_some_and(|c| *c != b'\n') {
                        i += 1;
                    }
                }
                _ => break,
            }
        }
        let tail = rest.get(i..).unwrap_or("");
        let answer = tail.starts_with('.')
            || tail.starts_with("?.")
            || tail.starts_with("??")
            || tail.starts_with("|>")
            || ["and", "or", "else", "catch"]
                .iter()
                .any(|word| starts_with_word(tail, word));
        self.cont_cache = Some((self.pos + i, answer));
        answer
    }

    fn number(&mut self) {
        let m = self.mark();
        let (end, result) = number::scan(self.src, self.pos);
        while self.pos < end && self.bump().is_some() {}
        match result {
            Ok(kind) => {
                self.push_tok(kind, m);
            }
            Err(e) => {
                let span = self.span_from(m);
                self.report(e.code, e.message, span);
            }
        }
    }

    fn word(&mut self) {
        let m = self.mark();
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        let src = self.src;
        let text = src.get(m.off..self.pos).unwrap_or("");
        let member = matches!(
            self.prev_kind(),
            Some(TokenKind::Dot | TokenKind::QuestionDot | TokenKind::ColonColon)
        );
        let ctx = WordCtx {
            prev_rparen: matches!(self.prev_kind(), Some(TokenKind::RParen)),
            prev_type_position: match self.prev_kind() {
                Some(TokenKind::Colon | TokenKind::Arrow | TokenKind::Lt) => true,
                // `Map<Str, fn() -> T>`: a `,` inside an open `<…>` separates type arguments.
                Some(TokenKind::Comma) => self.angles.last() == Some(&self.stack.len()),
                _ => false,
            },
            prev_colon: matches!(self.prev_kind(), Some(TokenKind::Colon)),
            stmt_start: self.at_stmt_start(),
            after_prompt_body: self.closed_prompt_body,
        };
        let word = if member {
            Word::Ident
        } else if let Some(keyword) = Keyword::from_word(text) {
            Word::Keyword(keyword)
        } else if let Some(contextual) = self.contextual(text, ctx.stmt_start) {
            Word::Contextual(contextual)
        } else {
            Word::Ident
        };
        let kind = match &word {
            Word::Ident => TokenKind::Ident(text.to_owned()),
            Word::Keyword(k) => TokenKind::Keyword(*k),
            Word::Contextual(c) => TokenKind::Contextual(*c),
        };
        self.push_tok(kind, m);
        match word {
            Word::Keyword(keyword) => self.keyword_effects(keyword, ctx),
            Word::Contextual(Contextual::Par | Contextual::Race) => {
                self.add_pending(PendKind::Block);
            }
            Word::Contextual(Contextual::Meta) => self.add_pending(PendKind::Record),
            Word::Contextual(Contextual::Format) => self.format_section = true,
            Word::Contextual(_) | Word::Ident => {}
        }
    }

    fn contextual(&self, text: &str, stmt_start: bool) -> Option<Contextual> {
        let next = skip_trivia(self.rest());
        match text {
            "meta" => {
                (self.last_sig.is_none() && next.starts_with('{')).then_some(Contextual::Meta)
            }
            "state" => (self.stack.is_empty()
                && stmt_start
                && next.starts_with(':')
                && !next.starts_with("::"))
            .then_some(Contextual::State),
            "par" => next.starts_with('{').then_some(Contextual::Par),
            "race" => next.starts_with('{').then_some(Contextual::Race),
            "timeout" => matches!(self.prev_kind(), Some(TokenKind::Keyword(Keyword::With)))
                .then_some(Contextual::Timeout),
            other => {
                if stmt_start && self.in_prompt_body() {
                    Contextual::prompt_section(other)
                } else {
                    None
                }
            }
        }
    }

    /// What a keyword tells the reader about the next `{` and the patterns around it.
    fn keyword_effects(&mut self, keyword: Keyword, ctx: WordCtx) {
        let next = skip_trivia(self.rest());
        let prompt_section = ctx.stmt_start && self.in_prompt_body();
        match keyword {
            Keyword::If | Keyword::While => {
                let depth = self.stack.len();
                let in_guard = keyword == Keyword::If
                    && self
                        .pats
                        .last()
                        .is_some_and(|p| p.depth == depth && p.end == PatEnd::Arrow);
                if in_guard {
                    self.clear_pats(PatEnd::Arrow);
                } else if keyword == Keyword::If && prompt_section {
                    self.add_pending(PendKind::PromptBlock);
                } else {
                    self.add_pending(PendKind::Block);
                }
            }
            Keyword::For => {
                self.add_pat(PatEnd::In);
                if prompt_section {
                    self.add_pending(PendKind::PromptBlock);
                } else {
                    self.add_pending(PendKind::Block);
                }
            }
            Keyword::Else => {
                if ctx.after_prompt_body && self.in_prompt_body() {
                    self.add_pending(PendKind::PromptBlock);
                } else {
                    self.add_pending(PendKind::Block);
                }
            }
            Keyword::Loop
            | Keyword::Do
            | Keyword::Try
            | Keyword::Defer
            | Keyword::Repeat
            | Keyword::Test => self.add_pending(PendKind::Block),
            Keyword::Match => self.add_pending(PendKind::Match),
            Keyword::Enum => self.add_pending(PendKind::Enum),
            Keyword::Catch => self.add_pending(PendKind::Catch),
            Keyword::Import => self.add_pending(PendKind::Record),
            Keyword::With => {
                // After a `)` it is a call override; in a declaration head it is the harness's
                // configuration. Either way no block follows it.
                let standalone = !(ctx.prev_rparen || self.head_pending_here());
                if standalone {
                    let kind = if starts_with_word(next, "timeout") {
                        PendKind::Block
                    } else {
                        PendKind::WithRecord
                    };
                    self.add_pending(kind);
                }
            }
            Keyword::Fn => {
                let head = if next.starts_with(is_ident_start) {
                    true
                } else if let Some(inner) = next.strip_prefix('(') {
                    let inner = skip_trivia(inner);
                    if let Some(after) = inner.strip_prefix(')') {
                        if self.type_decl || self.format_section {
                            false
                        } else if ctx.prev_colon {
                            // After a `:` it is a field type or a field value (`a: fn() {}`).
                            fn_expr_ahead(after)
                        } else {
                            !ctx.prev_type_position
                        }
                    } else {
                        named_param(inner)
                    }
                } else {
                    false
                };
                if head {
                    self.add_pending(PendKind::Head { prompt: false });
                }
            }
            Keyword::Harness => {
                if next.starts_with(is_ident_start) {
                    self.add_pending(PendKind::Head { prompt: false });
                }
            }
            Keyword::Prompt => {
                if next.starts_with(is_ident_start) {
                    self.add_pending(PendKind::Head { prompt: true });
                } else if next.starts_with('{') {
                    self.add_pending(PendKind::PromptBlock);
                }
            }
            Keyword::Let => self.add_pat(PatEnd::Let),
            Keyword::In => self.clear_pats(PatEnd::In),
            Keyword::Type => self.type_decl = true,
            _ => {}
        }
    }

    /// Decides what the `{` just read opens. The `{` itself is consumed, `self.pos` is after it.
    fn classify_open_brace(&mut self) -> BraceKind {
        let depth = self.stack.len();
        if matches!(
            self.stack.last(),
            Some(Frame::Delim {
                delim: Delim::Brace(BraceKind::EnumBody),
                ..
            })
        ) {
            return BraceKind::Record;
        }
        if self.pats.last().is_some_and(|p| p.depth == depth) {
            return BraceKind::Record;
        }
        let pending = self
            .pendings
            .iter()
            .rposition(|p| p.depth == depth)
            .and_then(|i| self.pendings.get(i).map(|p| (i, p.kind)));
        if let Some((index, kind)) = pending {
            match kind {
                PendKind::Block => {
                    let _ = self.pendings.remove(index);
                    return BraceKind::Block;
                }
                PendKind::Match => {
                    let _ = self.pendings.remove(index);
                    return BraceKind::MatchBody;
                }
                PendKind::Enum => {
                    let _ = self.pendings.remove(index);
                    return BraceKind::EnumBody;
                }
                PendKind::PromptBlock => {
                    let _ = self.pendings.remove(index);
                    return BraceKind::PromptBody;
                }
                PendKind::Record => {
                    let _ = self.pendings.remove(index);
                    return BraceKind::Record;
                }
                PendKind::Catch => {
                    let _ = self.pendings.remove(index);
                    let direct =
                        matches!(self.prev_kind(), Some(TokenKind::Keyword(Keyword::Catch)));
                    return if direct {
                        BraceKind::MatchBody
                    } else {
                        BraceKind::Block
                    };
                }
                PendKind::WithRecord => {
                    if let Some(p) = self.pendings.get_mut(index) {
                        p.kind = PendKind::Block;
                    }
                    return BraceKind::Record;
                }
                PendKind::Head { prompt } => {
                    let record_type = matches!(
                        self.prev_kind(),
                        Some(
                            TokenKind::Arrow
                                | TokenKind::Lt
                                | TokenKind::Comma
                                | TokenKind::Keyword(Keyword::With)
                        )
                    );
                    if record_type {
                        return BraceKind::Record;
                    }
                    let _ = self.pendings.remove(index);
                    return if prompt {
                        BraceKind::PromptBody
                    } else {
                        BraceKind::Block
                    };
                }
            }
        }
        // A `{` straight after a type name, with no head keyword waiting for a block, is a
        // variant or record literal (`Denied { by }`, `Ok { value: v }`): rule 1 of chapter 02.
        if self.prev_kind().is_some_and(is_type_name) || looks_like_record(self.rest()) {
            BraceKind::Record
        } else {
            BraceKind::Block
        }
    }

    fn punct(&mut self, c: char) {
        let m = self.mark();
        match c {
            '(' => {
                self.bump();
                self.push_tok(TokenKind::LParen, m);
                self.push_delim(Delim::Paren, m);
            }
            '[' => {
                self.bump();
                self.push_tok(TokenKind::LBracket, m);
                self.push_delim(Delim::Bracket, m);
            }
            '{' => {
                self.bump();
                let kind = self.classify_open_brace();
                self.push_tok(TokenKind::LBrace(kind), m);
                self.push_delim(Delim::Brace(kind), m);
                if kind == BraceKind::MatchBody {
                    self.add_pat(PatEnd::Arrow);
                }
            }
            ')' => {
                self.bump();
                self.close(Want::Paren, TokenKind::RParen, m);
            }
            ']' => {
                self.bump();
                self.close(Want::Bracket, TokenKind::RBracket, m);
            }
            '}' => {
                self.bump();
                self.close(Want::Brace, TokenKind::RBrace, m);
            }
            ':' if matches!(self.stack.last(), Some(Frame::Interp { .. }))
                && !self.rest().starts_with("::") =>
            {
                self.format_spec(m);
            }
            _ => {
                if let Some((len, kind)) = operator(self.rest()) {
                    for _ in 0..len {
                        self.bump();
                    }
                    self.push_tok(kind.clone(), m);
                    self.after_operator(&kind);
                } else {
                    self.bump();
                    let span = self.span_from(m);
                    let message = match c {
                        '!' => "`!` is not an operator; write `not`, or `!=`".to_owned(),
                        '&' => "`&` is not an operator; write `and`".to_owned(),
                        other => format!("unexpected character `{}`", other.escape_debug()),
                    };
                    self.report(Code::UnexpectedCharacter, message, span);
                }
            }
        }
    }

    fn after_operator(&mut self, kind: &TokenKind) {
        match kind {
            TokenKind::Semi { .. } => self.on_semi(),
            TokenKind::Eq | TokenKind::Colon => self.clear_pats(PatEnd::Let),
            TokenKind::FatArrow => self.clear_pats(PatEnd::Arrow),
            TokenKind::Lt => {
                if self.angles.len() < HINT_CAP {
                    self.angles.push(self.stack.len());
                }
            }
            TokenKind::Gt => {
                if self.angles.last() == Some(&self.stack.len()) {
                    self.angles.pop();
                    self.closed_angle = self.last_sig;
                }
            }
            TokenKind::Pipe => {
                let depth = self.stack.len();
                if self.closure_params == Some(depth) {
                    self.closure_params = None;
                } else {
                    // A `|` that follows no operand opens a closure's parameters.
                    let before = self
                        .out
                        .iter()
                        .rev()
                        .filter(|t| !matches!(t.kind, TokenKind::DocComment(_)))
                        .nth(1);
                    if !before.is_some_and(|t| ends_statement(&t.kind)) {
                        self.closure_params = Some(depth);
                    }
                }
            }
            TokenKind::Comma
                if self.closure_params != Some(self.stack.len())
                    && matches!(
                        self.stack.last(),
                        Some(Frame::Delim {
                            delim: Delim::Brace(BraceKind::MatchBody),
                            ..
                        })
                    ) =>
            {
                self.clear_pats(PatEnd::Arrow);
                self.add_pat(PatEnd::Arrow);
            }
            _ => {}
        }
    }

    /// The `:` of an interpolation and the format spec after it, up to the closing `}`.
    fn format_spec(&mut self, m: Mark) {
        self.bump();
        let text_start = self.pos;
        loop {
            match self.peek() {
                None | Some('}') => break,
                _ if self.line_break_here() => {
                    let span = Self::point_span(self.mark());
                    self.report(
                        Code::InvalidFormatSpec,
                        "a format spec cannot contain a line break",
                        span,
                    );
                    break;
                }
                Some('{') => {
                    let at = self.mark();
                    self.bump();
                    let span = self.span_from(at);
                    self.report(
                        Code::InvalidFormatSpec,
                        "a format spec cannot contain `{`",
                        span,
                    );
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
        let text = self.src.get(text_start..self.pos).unwrap_or("").to_owned();
        self.push_tok(TokenKind::FormatSpec(text), m);
    }

    // ---- closing delimiters ----

    fn find_close_target(&self, want: Want) -> Option<usize> {
        for (index, frame) in self.stack.iter().enumerate().rev().take(CLOSE_SEARCH_DEPTH) {
            match frame {
                Frame::Str(_) => return None,
                Frame::Interp { .. } => return (want == Want::Brace).then_some(index),
                Frame::Delim { delim, .. } => {
                    let hit = matches!(
                        (delim, want),
                        (Delim::Paren, Want::Paren)
                            | (Delim::Bracket, Want::Bracket)
                            | (Delim::Brace(_), Want::Brace)
                    );
                    if hit {
                        return Some(index);
                    }
                }
            }
        }
        None
    }

    fn close(&mut self, want: Want, kind: TokenKind, m: Mark) {
        let Some(target) = self.find_close_target(want) else {
            let span = self.span_from(m);
            self.report(
                Code::UnbalancedDelimiter,
                format!("`{}` closes nothing", want.close_char()),
                span,
            );
            self.push_tok(kind, m);
            return;
        };
        while self.stack.len() > target + 1 {
            if let Some(Frame::Delim { delim, open }) = self.pop_frame() {
                let span = Self::point_span(open);
                self.report(
                    Code::UnbalancedDelimiter,
                    format!("`{}` is never closed", delim_open_char(delim)),
                    span,
                );
            }
        }
        match self.pop_frame() {
            Some(Frame::Interp { .. }) => {
                self.push_tok(TokenKind::InterpEnd, m);
            }
            Some(Frame::Delim {
                delim: Delim::Brace(BraceKind::PromptBody),
                ..
            }) => {
                self.push_tok(kind, m);
                self.closed_prompt_body = true;
            }
            _ => {
                self.push_tok(kind, m);
            }
        }
    }

    // ---- strings ----

    fn open_string(&mut self, raw: bool) {
        let m = self.mark();
        if raw {
            self.bump();
        }
        let long = self.rest().starts_with("\"\"\"");
        let quotes = if long { 3 } else { 1 };
        for _ in 0..quotes {
            self.bump();
        }
        if !raw {
            self.push_tok(TokenKind::StringStart { long }, m);
            if !long {
                self.short_open += 1;
            }
        }
        self.stack.push(Frame::Str(StrFrame {
            long,
            raw,
            start: m,
            items: Vec::new(),
            texts: Vec::new(),
            raw_chars: Vec::new(),
        }));
    }

    /// Reads string text until the string closes or an interpolation opens.
    fn string_step(&mut self) {
        let Some(Frame::Str(mut sf)) = self.stack.pop() else {
            return;
        };
        let mut buf: Vec<(char, bool)> = Vec::new();
        let tm = self.mark();
        let string_start = sf.start;
        loop {
            let Some(c) = self.peek() else {
                self.flush_text(&mut sf, &mut buf, tm);
                self.finish_string(sf, None);
                self.unterminated_string_error(string_start);
                return;
            };
            match c {
                '"' if sf.long => {
                    if self.rest().starts_with("\"\"\"") {
                        self.flush_text(&mut sf, &mut buf, tm);
                        let close = self.mark();
                        for _ in 0..3 {
                            self.bump();
                        }
                        self.finish_string(sf, Some(close));
                        return;
                    }
                    self.bump();
                    buf.push(('"', false));
                }
                '"' => {
                    self.flush_text(&mut sf, &mut buf, tm);
                    let close = self.mark();
                    self.bump();
                    self.finish_string(sf, Some(close));
                    return;
                }
                '\\' if !sf.raw => self.escape(&mut buf),
                '{' if !sf.raw => {
                    self.flush_text(&mut sf, &mut buf, tm);
                    let open = self.mark();
                    self.bump();
                    let span = self.span_from(open);
                    self.push_tok_span(TokenKind::InterpStart, span);
                    sf.items.push(Item::Interp);
                    self.stack.push(Frame::Str(sf));
                    self.stack.push(Frame::Interp { open });
                    self.interp_depth += 1;
                    self.nesting += 1;
                    if self.interp_depth > self.opts.max_interpolation_depth {
                        let message = format!(
                            "interpolations nest deeper than {}",
                            self.opts.max_interpolation_depth
                        );
                        self.report(Code::InterpolationTooDeep, message, span);
                    }
                    self.check_nesting(open);
                    return;
                }
                '}' if !sf.raw => {
                    let at = self.mark();
                    self.bump();
                    let span = self.span_from(at);
                    self.report(
                        Code::LoneCloseBrace,
                        "a lone `}` in a string; write `\\}`",
                        span,
                    );
                    buf.push(('}', false));
                }
                '\n' | '\r' if self.line_break_here() => {
                    if sf.long {
                        let two = self.rest().starts_with("\r\n");
                        self.bump();
                        if two {
                            self.bump();
                        }
                        buf.push(('\n', false));
                    } else {
                        self.flush_text(&mut sf, &mut buf, tm);
                        self.finish_string(sf, None);
                        self.unterminated_string_error(string_start);
                        return;
                    }
                }
                '\r' => {
                    let at = self.mark();
                    self.bump();
                    let span = self.span_from(at);
                    self.report(
                        Code::BareCarriageReturn,
                        "a carriage return must be followed by a line feed",
                        span,
                    );
                }
                other => {
                    self.bump();
                    buf.push((other, false));
                }
            }
        }
    }

    fn unterminated_string_error(&mut self, start: Mark) {
        let span = Self::point_span(start);
        self.report(
            Code::UnterminatedString,
            "the string is not closed before the end of the line or of the source",
            span,
        );
    }

    /// Ends a run of text: emits its token (the cooked text is filled in when the string ends).
    fn flush_text(&mut self, sf: &mut StrFrame, buf: &mut Vec<(char, bool)>, from: Mark) {
        if sf.raw {
            sf.raw_chars.append(buf);
            return;
        }
        if self.pos > from.off {
            let token = self.push_tok(TokenKind::StringText(String::new()), from);
            sf.texts.push(Text {
                token,
                chars: std::mem::take(buf),
            });
            sf.items.push(Item::Text(sf.texts.len() - 1));
        }
        buf.clear();
    }

    fn escape(&mut self, buf: &mut Vec<(char, bool)>) {
        let at = self.mark();
        self.bump();
        let Some(c) = self.peek() else {
            let span = self.span_from(at);
            self.report(Code::InvalidEscape, "a `\\` at the end of the source", span);
            return;
        };
        let simple = match c {
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            '\\' | '"' | '{' | '}' => Some(c),
            _ => None,
        };
        if let Some(decoded) = simple {
            self.bump();
            buf.push((decoded, true));
        } else if c == 'u' {
            self.bump();
            self.unicode_escape(at, buf);
        } else if self.line_break_here() || c == '\r' {
            let span = self.span_from(at);
            self.report(
                Code::InvalidEscape,
                "a `\\` at the end of a line (there are no line continuations)",
                span,
            );
        } else {
            self.bump();
            let span = self.span_from(at);
            let message = format!("unknown escape `\\{}`", c.escape_debug());
            self.report(Code::InvalidEscape, message, span);
        }
    }

    /// `\u{hex}` after the `\u`.
    fn unicode_escape(&mut self, at: Mark, buf: &mut Vec<(char, bool)>) {
        if self.peek() != Some('{') {
            let span = self.span_from(at);
            self.report(
                Code::InvalidEscape,
                "a unicode escape is written `\\u{41}`",
                span,
            );
            return;
        }
        self.bump();
        let mut value: u32 = 0;
        let mut count = 0usize;
        let mut too_big = false;
        while let Some(digit) = self.peek().and_then(|c| c.to_digit(16)) {
            self.bump();
            count += 1;
            value = match value.checked_mul(16).and_then(|v| v.checked_add(digit)) {
                Some(v) if v <= 0x0010_FFFF => v,
                _ => {
                    too_big = true;
                    0
                }
            };
        }
        let closed = self.peek() == Some('}');
        if closed {
            self.bump();
        }
        let decoded = if count == 0 || too_big || !closed {
            None
        } else {
            char::from_u32(value)
        };
        if let Some(c) = decoded {
            buf.push((c, true));
        } else {
            let span = self.span_from(at);
            self.report(
                Code::InvalidEscape,
                "a unicode escape needs hex digits naming a scalar value, as in `\\u{41}`",
                span,
            );
        }
    }

    /// Closes a string: cooks its text, then emits the closing token. `close` is `None` when the
    /// string ends without its quote (an unterminated string).
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the frame is popped off the stack and ends here"
    )]
    fn finish_string(&mut self, sf: StrFrame, close: Option<Mark>) {
        if !sf.long && !sf.raw {
            self.short_open = self.short_open.saturating_sub(1);
        }
        if sf.raw {
            let cells: Vec<Cell> = sf
                .raw_chars
                .iter()
                .map(|(c, escaped)| Cell::Ch(0, *c, *escaped))
                .collect();
            let kept = if sf.long { dedent_cells(cells) } else { cells };
            let text: String = kept
                .iter()
                .filter_map(|cell| match cell {
                    Cell::Ch(_, c, _) => Some(*c),
                    Cell::Interp => None,
                })
                .collect();
            self.push_tok(
                TokenKind::RawString {
                    text,
                    long: sf.long,
                },
                sf.start,
            );
            return;
        }
        let cooked = cook(&sf);
        for (text, cooked) in sf.texts.iter().zip(cooked) {
            if let Some(token) = self.out.get_mut(text.token) {
                token.kind = TokenKind::StringText(cooked);
            }
        }
        let at = close.unwrap_or_else(|| self.mark());
        self.push_tok(TokenKind::StringEnd, at);
    }

    // ---- the end ----

    fn finish(&mut self) {
        if self.stack.is_empty() && self.should_insert_at_end() {
            let m = self.mark();
            self.push_tok(TokenKind::Semi { inserted: true }, m);
        }
        while let Some(frame) = self.pop_frame() {
            match frame {
                Frame::Delim { delim, open } => {
                    let span = Self::point_span(open);
                    self.report(
                        Code::UnbalancedDelimiter,
                        format!("`{}` is never closed", delim_open_char(delim)),
                        span,
                    );
                }
                Frame::Interp { open } => {
                    let span = Self::point_span(open);
                    self.report(
                        Code::UnterminatedInterpolation,
                        "the interpolation is not closed before the end of the source",
                        span,
                    );
                    let m = self.mark();
                    self.push_tok(TokenKind::InterpEnd, m);
                }
                Frame::Str(sf) => {
                    let span = Self::point_span(sf.start);
                    self.report(
                        Code::UnterminatedString,
                        "the string is not closed before the end of the source",
                        span,
                    );
                    self.finish_string(sf, None);
                }
            }
        }
        let eof = self.span_from(self.mark());
        self.out.push(Token {
            kind: TokenKind::Eof,
            span: eof,
        });
    }

    /// The end of the source ends the last line.
    fn should_insert_at_end(&self) -> bool {
        !self.head_pending_here()
            && self.attr.is_none()
            && self.prev_kind().is_some_and(ends_statement)
    }
}

// ---- cooking string text ----

#[derive(Clone, Copy)]
enum Cell {
    /// A character of the text piece with this index, and whether an escape produced it.
    Ch(usize, char, bool),
    Interp,
}

fn is_indent(cell: &Cell) -> bool {
    matches!(cell, Cell::Ch(_, ' ' | '\t', false))
}

fn is_line_break(cell: &Cell) -> bool {
    matches!(cell, Cell::Ch(_, '\n', false))
}

fn is_blank(line: &[Cell]) -> bool {
    line.iter().all(is_indent)
}

fn indent_of(line: &[Cell]) -> usize {
    line.iter().take_while(|cell| is_indent(cell)).count()
}

/// The text of every run of a string, with escapes already decoded; a long string is dedented.
fn cook(sf: &StrFrame) -> Vec<String> {
    if !sf.long {
        return sf
            .texts
            .iter()
            .map(|text| text.chars.iter().map(|(c, _)| *c).collect())
            .collect();
    }
    let mut cells = Vec::new();
    for item in &sf.items {
        match item {
            Item::Text(index) => {
                if let Some(text) = sf.texts.get(*index) {
                    for (c, escaped) in &text.chars {
                        cells.push(Cell::Ch(*index, *c, *escaped));
                    }
                }
            }
            Item::Interp => cells.push(Cell::Interp),
        }
    }
    let mut cooked = vec![String::new(); sf.texts.len()];
    for cell in dedent_cells(cells) {
        match cell {
            Cell::Ch(index, c, _) => {
                if let Some(text) = cooked.get_mut(index) {
                    text.push(c);
                }
            }
            Cell::Interp => {}
        }
    }
    cooked
}

/// Long-string dedent: the first and last lines are removed when blank, then the common
/// indentation of the other non-blank lines (an interpolation makes its line non-blank) is
/// stripped. Only literal spaces and tabs count as indentation, and only literal line feeds
/// split lines.
fn dedent_cells(cells: Vec<Cell>) -> Vec<Cell> {
    let mut lines: Vec<Vec<Cell>> = vec![Vec::new()];
    let mut breaks: Vec<Cell> = Vec::new();
    for cell in cells {
        if is_line_break(&cell) {
            breaks.push(cell);
            lines.push(Vec::new());
        } else if let Some(last) = lines.last_mut() {
            last.push(cell);
        }
    }
    let original = lines.len();
    if original >= 2 && lines.first().is_some_and(|line| is_blank(line)) {
        let _ = lines.remove(0);
        let _ = breaks.remove(0);
    }
    if original >= 2 && lines.last().is_some_and(|line| is_blank(line)) {
        let _ = lines.pop();
        let _ = breaks.pop();
    }
    let common = lines
        .iter()
        .filter(|line| !is_blank(line))
        .map(|line| indent_of(line))
        .min()
        .unwrap_or(0);
    for line in &mut lines {
        let strip = indent_of(line).min(common);
        *line = line.split_off(strip);
    }
    let mut out = Vec::new();
    let mut breaks = breaks.into_iter();
    for line in lines {
        out.extend(line);
        if let Some(line_break) = breaks.next() {
            out.push(line_break);
        }
    }
    out
}
