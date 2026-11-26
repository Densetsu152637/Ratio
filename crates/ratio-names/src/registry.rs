//! The registry trait: how the loader reaches packages a host keeps (chapter 10, "The public
//! API"), and an in-memory registry for tests and for the built-in packages.
//!
//! The host decides which sources exist and where their packages are; Ratio decides how names
//! resolve, which imports each source may make, and that paths stay inside a package's root.
//! Loading is checking, and nothing runs at load, so the trait is synchronous.

use std::collections::BTreeMap;

use crate::path::RelPath;
use crate::source::{PackageId, PackageName, Source};

/// A host's packages. Imperium's daemon registry implements it (chapter 10, stage 18).
///
/// A package's **source root** is the folder its entry file was found in. Every path the loader
/// passes is relative to that package root and already normalised ([`RelPath`]: ASCII letters,
/// digits, `_`, `-`, `.` and `/`; no `..`, no absolute path, no device name), and every name is
/// a checked [`PackageName`]. The loader never asks this trait for `std`: built-in packages come
/// from the library itself.
///
/// **What the host must guarantee.** The loader cannot see the file system, so these are the
/// implementation's duties, and a registry that does less lets a package read outside its root:
///
/// - Open each path from an already-open handle on the package root, one component at a time,
///   never following a link: a symbolic link, junction, mount point or other reparse point on
///   any component, the package root and the source folders above it included, is
///   [`RegistryError::Link`]. Check the file's attributes on the opened handle, not on the path
///   (no check-then-open race).
/// - Refuse hard links: a file with more than one link (`nlink > 1`) is [`RegistryError::Link`].
/// - Match every component's name case-exactly against the name on disk, even on a
///   case-insensitive file system, and treat a name that only matches case-insensitively as
///   [`RegistryError::NotFound`].
/// - Check the file's size on the opened handle before reading, and answer
///   [`RegistryError::TooLarge`] without reading when it is over `max_bytes`; never read more
///   than `max_bytes`. The loader checks the length again and refuses a longer answer.
///
/// **Pinning is the host's.** The loader hands back every file's text
/// ([`LoadedModule::source`](crate::LoadedModule::source)); the daemon pins a package by hashing
/// those texts (the closure hash of chapter 07), and chooses which authored version `find`
/// returns.
pub trait Registry {
    /// The entry file of package `name` of `source`, relative to the package's root, or `None`
    /// when the source has no such package (or the source does not exist, such as a plugin that
    /// is not enabled and approved, or an untrusted project).
    ///
    /// # Errors
    ///
    /// The registry could not tell.
    fn find(&self, source: &Source, name: &PackageName) -> Result<Option<RelPath>, RegistryError>;

    /// The bytes of the file at `path` in `package`'s root, opened as the trait's documentation
    /// requires. A file longer than `max_bytes` is [`RegistryError::TooLarge`].
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

/// Why a registry could not answer. It carries no text: a host's error messages (which may name
/// paths outside the package) never reach diagnostics or anything journalled; the host logs its
/// own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// No such file.
    NotFound,
    /// The path is, or passes through, a link (symbolic, hard, junction or reparse point).
    Link,
    /// The file is larger than the limit given.
    TooLarge,
    /// Anything else (an I/O error).
    Other,
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
    fn find(&self, source: &Source, name: &PackageName) -> Result<Option<RelPath>, RegistryError> {
        let id = PackageId::new(source.clone(), name.clone());
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
