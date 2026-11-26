//! Ratio's modules and names: stage 3 of the implementation plan (chapter 10).
//!
//! A host keeps packages in **sources** (`std`, `user`, `plugin`, `project`, `authored`, in
//! decreasing order of trust) and serves them through the [`Registry`] trait. The [`Loader`]
//! finds a package by its [`QualifiedName`], reads and parses its files, follows its imports
//! (by path inside the package's source root, or by name under the trust rules of chapter 03),
//! and resolves each module's top-level names ([`Scope`]) in front of the [`prelude`].
//!
//! - **Paths** are normalised before a registry sees them ([`RelPath`]): `..` that leaves the
//!   root, absolute paths and platform-dependent spellings are refused, and a registry reports a
//!   link as [`RegistryError::Link`], which the loader refuses.
//! - **Bare names** (`import debate`) reach `std`, then `user`: a package of a lower source never
//!   shadows a higher one, and a plugin's, project's or model-authored package is reached only
//!   by its qualified name, never picked up unasked.
//! - **Trust follows source**: [`Source::may_import`] decides which registered packages a
//!   package may import.
//! - `std` packages come only from the built-in registry the loader is given, never from the
//!   host's.

mod diag;
mod loader;
mod path;
pub mod prelude;
mod registry;
mod scope;
mod source;
mod version;

pub use diag::{Code, Diagnostic};
pub use loader::{Limits, Loaded, LoadedModule, Loader, ModuleId};
pub use path::{PathError, RelPath};
pub use registry::{MemoryRegistry, Registry, RegistryError};
pub use scope::{Binding, Resolution, Scope};
pub use source::{Area, Lookup, PackageId, PackageName, PluginName, QualifiedName, Source};
pub use version::{Op, Version, VersionReq};

use ratio_syntax::ast::{StrPart, StringLit};

/// The text of a string literal without interpolations, or `None` if it has any.
fn plain_text(lit: &StringLit) -> Option<String> {
    let mut text = String::new();
    for part in &lit.parts {
        match part {
            StrPart::Text(part) => text.push_str(part),
            StrPart::Interp { .. } => return None,
        }
    }
    Some(text)
}
