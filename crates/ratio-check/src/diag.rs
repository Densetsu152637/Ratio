//! Diagnostics of the type checker: the `C` series (chapter 10, proposed).

use std::fmt;

use ratio_names::ModuleId;
use ratio_syntax::Span;

macro_rules! codes {
    ($($(#[$doc:meta])* $name:ident => $text:literal,)*) => {
        /// The diagnostic codes of the type checker (`C`).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Code { $($(#[$doc])* $name,)* }

        impl Code {
            /// Every code, in numeric order.
            pub const ALL: &'static [Code] = &[$(Code::$name,)*];

            /// The code as written in diagnostics and conformance files (`C0001`).
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Code::$name => $text,)* }
            }
        }
    };
}

codes! {
    /// A name that is no local, item, import or prelude name (or a built-in the checker has
    /// no signature for yet).
    UnknownName => "C0001",
    /// A type name that resolves to no type.
    UnknownType => "C0002",
    /// Type arguments that do not fit a type: the wrong number, or some on a type that takes
    /// none.
    TypeArity => "C0003",
    /// A value whose type is not a subtype of the type expected where it stands.
    Mismatch => "C0004",
    /// No such field, module member or method.
    NoMember => "C0005",
    /// A record literal or pattern whose fields do not match the record type (missing, extra or
    /// repeated fields).
    RecordFields => "C0006",
    /// An unknown variant, or a variant literal whose fields do not match its payload.
    UnknownVariant => "C0007",
    /// A bare variant name that several enums in scope define.
    AmbiguousVariant => "C0008",
    /// A `match` that is not exhaustive.
    NonExhaustive => "C0009",
    /// A refutable pattern where an irrefutable one is needed (`let`, `for`, parameters).
    Refutable => "C0010",
    /// A pattern that cannot match a value of the type it is matched against.
    PatternMismatch => "C0011",
    /// Arguments that do not fit the parameters: too many or too few, an unknown or repeated
    /// name.
    Arguments => "C0012",
    /// A call of something that is not a function or harness.
    NotCallable => "C0013",
    /// A type parameter or closure parameter whose type cannot be inferred.
    CannotInfer => "C0014",
    /// Type arguments (a turbofish) that do not fit the function's type parameters, or that do
    /// not satisfy a parameter's bound.
    TypeArguments => "C0015",
    /// A call that makes effects in a constant expression (`const`, `state`, harness
    /// configuration).
    EffectInConstant => "C0016",
    /// A constant whose value depends on itself.
    ConstantCycle => "C0017",
    /// A type alias that refers to itself.
    RecursiveAlias => "C0018",
    /// An assignment to something that is not a `var`, or to a `var` a closure captured.
    NotAssignable => "C0019",
    /// Branches or elements with no common supertype.
    NoCommonType => "C0020",
    /// `return`, `break` or `continue` outside a function or loop, or `break` with a value
    /// outside `loop`.
    MisplacedJump => "C0021",
    /// A type deeper than the limit, aliases or constants nested deeper than the limit, or a
    /// check that ran out of its work budget.
    TooComplex => "C0022",
    /// A `match` too large to check for exhaustiveness within the bound.
    MatchTooComplex => "C0023",
    /// A harness without a return type.
    HarnessReturn => "C0024",
    /// A call override (`f(x) with { … }`) on a call whose callee is not a harness.
    OverrideNotHarness => "C0025",
    /// An operator applied to operands it does not take.
    Operator => "C0026",
    /// A map whose key type is not `Str`, `Untrusted<Str>`, `Int`, `Bool` or a unit-only enum.
    MapKey => "C0027",
    /// A tuple with more than eight elements.
    TupleArity => "C0028",
    /// A member of another module that is not `pub`.
    Private => "C0029",
    /// A name declared twice: a variant, field, parameter or type parameter, or a name bound
    /// twice in one pattern.
    Duplicate => "C0030",
    /// A warning: a `let` that shadows an earlier binding (lint `shadowed_binding`).
    ShadowedBinding => "C0031",
    /// A warning: a local binding that shadows a standard-library module.
    ShadowsModule => "C0032",
}

impl Code {
    /// Whether the code is a warning; every other code is an error.
    #[must_use]
    pub const fn is_warning(self) -> bool {
        matches!(self, Code::ShadowedBinding | Code::ShadowsModule)
    }

    /// The code spelled `text`, if there is one.
    #[must_use]
    pub fn from_code(text: &str) -> Option<Code> {
        Code::ALL.iter().copied().find(|code| code.as_str() == text)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A problem the checker found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub message: String,
    /// The module and the span it was found at.
    pub module: ModuleId,
    pub span: Span,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = if self.code.is_warning() {
            "warning"
        } else {
            "error"
        };
        write!(
            f,
            "{}:{}: {level}[{}]: {}",
            self.span.line, self.span.col, self.code, self.message
        )
    }
}
