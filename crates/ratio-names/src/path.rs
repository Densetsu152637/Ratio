//! Paths inside a package's source root (chapter 03, "Modules and imports"; chapter 07).
//!
//! An import path is relative to the importing file and must stay inside the root: `..` that
//! leaves it and absolute paths are refused. The path is normalised here, before any registry
//! sees it, and refused if it could mean something else on another platform (a `\`, a `:` for
//! a drive or a stream, a trailing dot or space, a reserved device name such as `con`), so a
//! registry is only ever asked for a plain relative path to a `.ratio` file.

use std::fmt;

/// A normalised path inside a source root: `/`-separated components, none empty, `.` or `..`,
/// the last ending in `.ratio`. Only [`RelPath::parse`] and [`RelPath::join`] make one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

/// Why a path was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    /// `..` leaves the source root.
    Escapes,
    /// The path is malformed; the text says how.
    Invalid(&'static str),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::Escapes => f.write_str("`..` leaves the package's source root"),
            PathError::Invalid(why) => f.write_str(why),
        }
    }
}

/// Windows device names, which name a device whatever the extension (`con.ratio`).
const DEVICES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The longest path accepted, in bytes.
const MAX_PATH_BYTES: usize = 1024;

impl RelPath {
    /// A path relative to the source root.
    ///
    /// # Errors
    ///
    /// The path is malformed or leaves the root.
    pub fn parse(text: &str) -> Result<RelPath, PathError> {
        resolve(&[], text)
    }

    /// `text`, written in the file at this path, resolved against the file's folder.
    ///
    /// # Errors
    ///
    /// The path is malformed or leaves the root.
    pub fn join(&self, text: &str) -> Result<RelPath, PathError> {
        let mut folder: Vec<&str> = self.0.split('/').collect();
        folder.pop();
        resolve(&folder, text)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The file name without `.ratio`.
    #[must_use]
    pub fn stem(&self) -> &str {
        let file = self.0.rsplit('/').next().unwrap_or(&self.0);
        file.strip_suffix(".ratio").unwrap_or(file)
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn resolve(folder: &[&str], text: &str) -> Result<RelPath, PathError> {
    if text.is_empty() {
        return Err(PathError::Invalid("an empty path"));
    }
    if text.len() > MAX_PATH_BYTES {
        return Err(PathError::Invalid("a path longer than 1024 bytes"));
    }
    if text.starts_with('/') {
        return Err(PathError::Invalid("an absolute path"));
    }
    if text.contains('\\') {
        return Err(PathError::Invalid("a `\\` in a path (write `/`)"));
    }
    if text.contains(':') {
        return Err(PathError::Invalid("a `:` in a path (a drive or a stream)"));
    }
    if text.chars().any(char::is_control) {
        return Err(PathError::Invalid("a control character in a path"));
    }
    let mut parts: Vec<&str> = folder.to_vec();
    for part in text.split('/') {
        match part {
            "" => return Err(PathError::Invalid("an empty path component")),
            "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(PathError::Escapes);
                }
            }
            _ if part.ends_with('.') || part.ends_with(' ') => {
                return Err(PathError::Invalid(
                    "a path component ending in a dot or a space",
                ));
            }
            _ => {
                let device = part.split('.').next().unwrap_or(part).trim_end();
                if DEVICES.iter().any(|d| d.eq_ignore_ascii_case(device)) {
                    return Err(PathError::Invalid("a reserved device name in a path"));
                }
                parts.push(part);
            }
        }
    }
    match parts.last() {
        // Case-sensitive on purpose: `a.RATIO` is not a Ratio file on any platform.
        Some(file)
            if std::path::Path::new(file)
                .extension()
                .is_some_and(|e| e == "ratio") =>
        {
            Ok(RelPath(parts.join("/")))
        }
        _ => Err(PathError::Invalid(
            "a path that does not name a `.ratio` file",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: &str, text: &str) -> Result<String, PathError> {
        RelPath::parse(base)
            .unwrap()
            .join(text)
            .map(|p| p.as_str().to_owned())
    }

    #[test]
    fn paths_resolve_against_the_importing_folder() {
        assert_eq!(at("main.ratio", "./lib/a.ratio").unwrap(), "lib/a.ratio");
        assert_eq!(at("lib/a.ratio", "b.ratio").unwrap(), "lib/b.ratio");
        assert_eq!(at("lib/a.ratio", "../c.ratio").unwrap(), "c.ratio");
        assert_eq!(
            at("lib/x/a.ratio", "./.././y/../b.ratio").unwrap(),
            "lib/b.ratio"
        );
    }

    #[test]
    fn escapes_are_refused_even_when_they_come_back() {
        assert_eq!(at("main.ratio", "../main.ratio"), Err(PathError::Escapes));
        assert_eq!(
            at("lib/a.ratio", "../../lib/a.ratio"),
            Err(PathError::Escapes)
        );
    }

    #[test]
    fn ambiguous_and_foreign_paths_are_refused() {
        for text in [
            "",
            "/etc/x.ratio",
            "C:/x.ratio",
            "a\\b.ratio",
            "a//b.ratio",
            "lib/",
            "a.ratio.",
            "a.ratio ",
            "dir./a.ratio",
            "x.ratio:stream",
            "con.ratio",
            "lib/NUL.ratio",
            "Com1.x.ratio",
            ".ratio",
            "a.txt",
            "a\u{0}.ratio",
            ".",
            "..",
        ] {
            assert!(
                matches!(
                    at("main.ratio", text),
                    Err(PathError::Invalid(_) | PathError::Escapes)
                ),
                "{text:?}"
            );
        }
        assert!(at("main.ratio", &"a/".repeat(600)).is_err());
    }

    #[test]
    fn the_stem_is_the_file_name_without_its_extension() {
        assert_eq!(
            RelPath::parse("lib/critics.ratio").unwrap().stem(),
            "critics"
        );
    }
}
