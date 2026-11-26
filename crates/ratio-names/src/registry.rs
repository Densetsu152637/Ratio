//! The registry trait: how the loader reaches packages a host keeps (chapter 10, "The public
//! API"), and an in-memory registry for tests and for the built-in packages.
//!
//! The host decides which sources exist and where their packages are; Ratio decides how names
//! resolve, which imports each source may make, and that paths stay inside a package's root.
//! Loading is checking, and nothing runs at load, so the trait is synchronous.

use std::collections::BTreeMap;

use crate::path::RelPath;
use crate::source::{PackageId, Source};

/// A host's packages. Imperium's daemon registry implements it (chapter 10, stage 18).
///
/// A package's **source root** is the folder its entry file was found in; every path the
/// loader passes is relative to that root and already normalised ([`RelPath`]: no `..`, no
/// absolute path, no `\` or `:`). The loader never asks this trait for `std`: built-in packages
/// come from the library itself.
pub trait Registry {
    /// The entry file of package `name` of `source`, relative to the package's root, or `None`
    /// when the source has no such package (or the source does not exist, such as a plugin that
    /// is not enabled and approved, or an untrusted project).
    ///
    /// # Errors
    ///
    /// The registry could not tell.
    fn find(&self, source: &Source, name: &str) -> Result<Option<RelPath>, RegistryError>;

    /// The bytes of the file at `path` in `package`'s root. The registry must not follow links:
    /// a symbolic link, junction or other reparse point anywhere on the path is
    /// [`RegistryError::Link`]. A file longer than `max_bytes` is [`RegistryError::TooLarge`],
    /// and the registry should not read more of it than that.
    ///
    /// # Errors
    ///
    /// The file is missing, a link, too large, or unreadable.
    fn read(
        &self,
        package: &PackageId,
        path: &RelPath,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RegistryError>;
}

/// Why a registry could not answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// No such file.
    NotFound,
    /// The path is, or passes through, a symbolic link or another kind of link.
    Link,
    /// The file is larger than the limit given.
    TooLarge,
    /// Anything else (an I/O error), with a description for the diagnostic.
    Other(String),
}

#[derive(Clone, Debug, Default)]
struct MemoryPackage {
    entry: Option<RelPath>,
    files: BTreeMap<RelPath, Option<Vec<u8>>>,
}

/// Packages held in memory: the test registry, and the store for the built-in packages.
#[derive(Clone, Debug, Default)]
pub struct MemoryRegistry {
    packages: BTreeMap<PackageId, MemoryPackage>,
}

impl MemoryRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds (or replaces) a file of `package`.
    pub fn add_file(&mut self, package: &PackageId, path: RelPath, bytes: impl Into<Vec<u8>>) {
        let entry = self.packages.entry(package.clone()).or_default();
        entry.files.insert(path, Some(bytes.into()));
    }

    /// Adds a link at `path` in `package`: reading it fails with [`RegistryError::Link`].
    pub fn add_link(&mut self, package: &PackageId, path: RelPath) {
        let entry = self.packages.entry(package.clone()).or_default();
        entry.files.insert(path, None);
    }

    /// Makes `path` the entry file of `package`, so [`Registry::find`] finds the package.
    pub fn set_entry(&mut self, package: &PackageId, path: RelPath) {
        self.packages.entry(package.clone()).or_default().entry = Some(path);
    }
}

impl Registry for MemoryRegistry {
    fn find(&self, source: &Source, name: &str) -> Result<Option<RelPath>, RegistryError> {
        let id = PackageId::new(source.clone(), name);
        Ok(self.packages.get(&id).and_then(|p| p.entry.clone()))
    }

    fn read(
        &self,
        package: &PackageId,
        path: &RelPath,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RegistryError> {
        match self.packages.get(package).and_then(|p| p.files.get(path)) {
            None => Err(RegistryError::NotFound),
            Some(None) => Err(RegistryError::Link),
            Some(Some(bytes)) if bytes.len() > max_bytes => Err(RegistryError::TooLarge),
            Some(Some(bytes)) => Ok(bytes.clone()),
        }
    }
}
