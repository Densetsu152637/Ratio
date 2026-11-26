//! A [`Registry`] over folders on disk: the packages named on the command line.
//!
//! It keeps the duties the trait's documentation gives a host (`ratio-names`, `Registry`),
//! with what the standard library offers and no platform calls of its own:
//!
//! - **No links.** Each path is walked from the package root one component at a time, and a
//!   component that is a symbolic link, junction or other reparse point (seen on the entry
//!   itself, without following it) is [`RegistryError::Link`]; so is the package root.
//! - **Hard links.** On Unix a file with more than one link is refused. On Windows the standard
//!   library has no stable way to count links, so hard links there are not detected (a stated
//!   residual).
//! - **No check-then-open race on the file.** On Windows the file is opened without following
//!   a reparse point and its attributes are read from the handle; on Unix the opened handle's
//!   device and inode must be the ones the walk saw. A directory swapped for a link between the
//!   walk and the open is not detected (the standard library has no `openat`).
//! - **Case-exact names.** Each component must appear with exactly that spelling in its
//!   folder's listing, even on a case-insensitive file system; a name that matches only
//!   ignoring case is [`RegistryError::NotFound`].
//! - **Size first.** The size is read from the opened handle and a file over `max_bytes` is
//!   [`RegistryError::TooLarge`] without being read; never more than `max_bytes` + 1 bytes are
//!   read, so a file that grows is refused too.
//!
//! Errors carry no text: the loader turns them into its own diagnostics.

use std::collections::BTreeMap;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use ratio::{PackageId, PackageName, Registry, RegistryError, RelPath, Source};

/// A package on disk: the folder its entry file is in, and the entry's name there.
#[derive(Clone, Debug)]
struct DiskPackage {
    root: PathBuf,
    entry: RelPath,
}

/// The `user:` packages named on the command line.
#[derive(Clone, Debug, Default)]
pub struct FsRegistry {
    packages: BTreeMap<PackageName, DiskPackage>,
}

impl FsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `user:<name>` with its root folder and entry file.
    pub fn add(&mut self, name: PackageName, root: PathBuf, entry: RelPath) {
        self.packages.insert(name, DiskPackage { root, entry });
    }

    /// Whether `name` is taken.
    pub fn has(&self, name: &PackageName) -> bool {
        self.packages.contains_key(name)
    }
}

impl Registry for FsRegistry {
    fn find(&self, source: &Source, name: &PackageName) -> Result<Option<RelPath>, RegistryError> {
        if *source != Source::User {
            return Ok(None);
        }
        Ok(self.packages.get(name).map(|p| p.entry.clone()))
    }

    fn read(
        &self,
        package: &PackageId,
        path: &RelPath,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RegistryError> {
        if package.source != Source::User {
            return Err(RegistryError::NotFound);
        }
        let disk = self
            .packages
            .get(&package.name)
            .ok_or(RegistryError::NotFound)?;
        read_under(&disk.root, path, max_bytes)
    }
}

/// Reads `path` under `root` with the duties above.
pub fn read_under(root: &Path, path: &RelPath, max_bytes: usize) -> Result<Vec<u8>, RegistryError> {
    check_root(root)?;
    let components: Vec<&str> = path.as_str().split('/').collect();
    let mut here = root.to_path_buf();
    let mut seen: Option<Metadata> = None;
    for (i, name) in components.iter().enumerate() {
        let last = i + 1 == components.len();
        let (next, meta) = exact_child(&here, name)?;
        if last {
            if !meta.is_file() {
                return Err(RegistryError::NotFound);
            }
        } else if !meta.is_dir() {
            return Err(RegistryError::NotFound);
        }
        here = next;
        seen = Some(meta);
    }
    let seen = seen.ok_or(RegistryError::NotFound)?;
    let mut file = open_no_follow(&here)?;
    let opened = file.metadata().map_err(|_| RegistryError::Other)?;
    if !opened.is_file() || is_link(&opened) {
        return Err(RegistryError::Link);
    }
    if !same_file(&seen, &opened) {
        return Err(RegistryError::Link);
    }
    let len = usize::try_from(opened.len()).unwrap_or(usize::MAX);
    if len > max_bytes {
        return Err(RegistryError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(len);
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    (&mut file)
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|_| RegistryError::Other)?;
    if bytes.len() > max_bytes {
        return Err(RegistryError::TooLarge);
    }
    Ok(bytes)
}

/// The package root must be a folder, and not itself a link.
pub fn check_root(root: &Path) -> Result<(), RegistryError> {
    let meta = fs::symlink_metadata(root).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => RegistryError::NotFound,
        _ => RegistryError::Other,
    })?;
    if is_link(&meta) {
        return Err(RegistryError::Link);
    }
    if !meta.is_dir() {
        return Err(RegistryError::NotFound);
    }
    Ok(())
}

/// The entry of folder `dir` spelt exactly `name`, and its metadata, not following it.
fn exact_child(dir: &Path, name: &str) -> Result<(PathBuf, Metadata), RegistryError> {
    let listing = fs::read_dir(dir).map_err(|_| RegistryError::Other)?;
    for entry in listing {
        let entry = entry.map_err(|_| RegistryError::Other)?;
        if entry.file_name().to_str() != Some(name) {
            continue;
        }
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|_| RegistryError::Other)?;
        if is_link(&meta) || entry.file_type().is_ok_and(|t| t.is_symlink()) {
            return Err(RegistryError::Link);
        }
        return Ok((path, meta));
    }
    Err(RegistryError::NotFound)
}

#[cfg(windows)]
fn is_link(meta: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    /// `FILE_ATTRIBUTE_REPARSE_POINT`: symbolic links, junctions, mount points and every other
    /// reparse point.
    const REPARSE_POINT: u32 = 0x400;
    meta.file_type().is_symlink() || meta.file_attributes() & REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link(meta: &Metadata) -> bool {
    meta.file_type().is_symlink()
}

#[cfg(windows)]
fn open_no_follow(path: &Path) -> Result<File, RegistryError> {
    use std::os::windows::fs::OpenOptionsExt;
    /// `FILE_FLAG_OPEN_REPARSE_POINT`: open a reparse point itself, never its target.
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .custom_flags(OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| RegistryError::Other)
}

#[cfg(not(windows))]
fn open_no_follow(path: &Path) -> Result<File, RegistryError> {
    // Without a portable `O_NOFOLLOW`, the open may follow a link swapped in after the walk;
    // `same_file` then refuses it, since the handle is not the file the walk saw.
    OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|_| RegistryError::Other)
}

/// Whether the opened handle is the file the walk saw, with no other link to it.
#[cfg(unix)]
fn same_file(seen: &Metadata, opened: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    seen.dev() == opened.dev() && seen.ino() == opened.ino() && opened.nlink() <= 1
}

/// Windows: the open did not follow a reparse point, and the handle's attributes were checked;
/// the standard library cannot compare file ids or count hard links there.
#[cfg(not(unix))]
fn same_file(_seen: &Metadata, _opened: &Metadata) -> bool {
    true
}
