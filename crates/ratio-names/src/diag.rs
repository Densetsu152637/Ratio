//! Diagnostics of loading and name resolution: the `N` series (chapter 10, proposed).

use std::fmt;

use ratio_syntax::Span;

use crate::loader::ModuleId;

macro_rules! codes {
    ($($(#[$doc:meta])* $name:ident => $text:literal,)*) => {
        /// The diagnostic codes of loading and name resolution (`N`).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Code { $($(#[$doc])* $name,)* }

        impl Code {
            /// Every code, in numeric order.
            pub const ALL: &'static [Code] = &[$(Code::$name,)*];

            /// The code as written in diagnostics and conformance files (`N0001`).
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Code::$name => $text,)* }
            }
        }
    };
}

codes! {
    /// An import path that is not a plain relative path to a `.ratio` file.
    InvalidPath => "N0001",
    /// An import path whose `..` leaves the package's source root.
    PathEscapes => "N0002",
    /// An import path that is, or passes through, a symbolic link or another link.
    Link => "N0003",
    /// No file at an import path.
    FileNotFound => "N0004",
    /// No package by that name in the sources looked in.
    PackageNotFound => "N0005",
    /// A malformed source-qualified name or version range.
    InvalidName => "N0006",
    /// An import the trust rules refuse (a package importing a source it may not).
    ImportRefused => "N0007",
    /// A package whose `meta` version is missing or outside the import's range.
    VersionMismatch => "N0008",
    /// An import cycle.
    ImportCycle => "N0009",
    /// A file or package over its size limit.
    TooLarge => "N0010",
    /// More modules or packages, or deeper imports, than the loader's limits.
    LimitExceeded => "N0011",
    /// A second definition of a name in a module.
    DuplicateName => "N0012",
    /// An import list naming something the imported module does not define.
    UnknownImportedName => "N0013",
    /// An import list naming an item that is not `pub`.
    PrivateImportedName => "N0014",
    /// Two paths of a package that differ only in letter case.
    CaseConflict => "N0015",
    /// A file the registry could not read, or that is not UTF-8.
    Unreadable => "N0016",
    /// An imported module with syntax errors.
    SyntaxErrors => "N0017",
    /// An import that gives no name to bind (the file name is not an identifier); `as` is needed.
    NoBindingName => "N0018",
    /// `pub` on an import (imports are not re-exported).
    PubImport => "N0019",
    /// A warning: a name that shadows a standard-library module.
    ShadowsStandardModule => "N0020",
    /// A warning: a name that shadows `trust`, `as_task` or `declassify`, so a call that reads
    /// as an audited conversion is not one.
    ShadowsTrustFunction => "N0021",
}

impl Code {
    /// Whether the code is a warning; every other code is an error.
    #[must_use]
    pub const fn is_warning(self) -> bool {
        matches!(
            self,
            Code::ShadowsStandardModule | Code::ShadowsTrustFunction
        )
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

/// A problem found while loading or resolving names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub message: String,
    /// The module and span it was found at; `None` when the entry package itself could not be
    /// found or read.
    pub at: Option<(ModuleId, Span)>,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = if self.code.is_warning() {
            "warning"
        } else {
            "error"
        };
        if let Some((_, span)) = self.at {
            write!(f, "{}:{}: ", span.line, span.col)?;
        }
        write!(f, "{level}[{}]: {}", self.code, self.message)
    }
}
