//! Sources, their trust order, and source-qualified names (chapter 03, "Modules and imports";
//! chapter 07, "Who writes Ratio"; chapter 08, "Where harnesses come from").

use std::fmt;

use ratio_syntax::ast::SourceName;

use crate::version::VersionReq;

/// An authored area (chapter 07, "Authored harnesses").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Area {
    /// `authored:session/<name>`: the session's area.
    Session,
    /// `authored:env/<name>`: the environment's, once the user persists a package.
    Env,
}

impl Area {
    /// The area as written in a name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Area::Session => "session",
            Area::Env => "env",
        }
    }
}

/// Where a package comes from. The trust order is the order of the variants: `Std` is the most
/// trusted, `Authored` the least ([`Source::trust_rank`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    /// Built in: shipped with the `ratio` library.
    Std,
    /// The user's `<config>/harnesses/`.
    User,
    /// An approved plugin's `harnesses/`, named by the plugin.
    Plugin(String),
    /// A trusted project's `.imperium/harnesses/`.
    Project,
    /// Written by a model through `write_harness`, in an area.
    Authored(Area),
}

impl Source {
    /// The position in the trust order: 0 for `std`, 4 for `authored`. Lower is more trusted.
    #[must_use]
    pub const fn trust_rank(&self) -> u8 {
        match self {
            Source::Std => 0,
            Source::User => 1,
            Source::Plugin(_) => 2,
            Source::Project => 3,
            Source::Authored(_) => 4,
        }
    }

    /// Whether a package of this source may import a registered package of `target` (chapter 03,
    /// "Trust follows source"). Every package may import `std`; a `std` package nothing else; a
    /// user package user, plugin and project packages; a plugin package only its own plugin's; a
    /// project package only its project's; an authored package only its own area's. Files of a
    /// package's own source root are always importable and do not go through this rule.
    #[must_use]
    pub fn may_import(&self, target: &Source) -> bool {
        match (self, target) {
            (_, Source::Std)
            | (Source::User, Source::User | Source::Plugin(_) | Source::Project)
            | (Source::Project, Source::Project) => true,
            (Source::Plugin(a), Source::Plugin(b)) => a == b,
            (Source::Authored(a), Source::Authored(b)) => a == b,
            _ => false,
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Std => f.write_str("std"),
            Source::User => f.write_str("user"),
            Source::Plugin(plugin) => write!(f, "plugin:{plugin}"),
            Source::Project => f.write_str("project"),
            Source::Authored(area) => write!(f, "authored:{}", area.as_str()),
        }
    }
}

/// A registered package: its source and its name (`critics.strict`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackageId {
    pub source: Source,
    pub name: String,
}

impl PackageId {
    #[must_use]
    pub fn new(source: Source, name: impl Into<String>) -> Self {
        PackageId {
            source,
            name: name.into(),
        }
    }
}

impl fmt::Display for PackageId {
    /// The fully qualified name: `std:debate`, `plugin:acme/review`, `authored:session/vote`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            Source::Plugin(_) | Source::Authored(_) => write!(f, "{}/{}", self.source, self.name),
            source => write!(f, "{source}:{}", self.name),
        }
    }
}

/// Where a [`QualifiedName`] is looked up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// A bare name (`debate`): `std`, then `user`. A package of a lower source is reached only by
    /// its qualified name, so it can neither shadow a higher one nor be picked up unasked.
    Bare,
    /// `authored:<name>`: the session's area, then the environment's.
    Authored,
    /// One source (`std:debate`, `plugin:acme/review`, `authored:env/vote`).
    In(Source),
}

/// A package name as an import or a host writes it: `std:debate`, `user:critics.strict`,
/// `plugin:acme/review@^1`, `authored:session/vote`, or a bare `debate`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualifiedName {
    pub lookup: Lookup,
    /// The package name: dot-separated words.
    pub name: String,
    /// The semver range after `@`, if any.
    pub range: Option<VersionReq>,
}

impl QualifiedName {
    /// The sources to look in, most trusted first; the first that has the name wins.
    #[must_use]
    pub fn candidates(&self) -> Vec<Source> {
        match &self.lookup {
            Lookup::Bare => vec![Source::Std, Source::User],
            Lookup::Authored => vec![Source::Authored(Area::Session), Source::Authored(Area::Env)],
            Lookup::In(source) => vec![source.clone()],
        }
    }

    /// Reads a name written as text (a setting, `meta.harness`): `source:part/part@range`.
    ///
    /// # Errors
    ///
    /// A message saying what is wrong with the name.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (body, range) = match text.split_once('@') {
            Some((body, range)) => (body, Some(range)),
            None => (text, None),
        };
        let (source, path) = match body.split_once(':') {
            Some((source, path)) => (Some(source), path),
            None => (None, body),
        };
        let parts: Vec<&str> = path.split('/').collect();
        for part in &parts {
            if part.is_empty() || !part.split('.').all(is_word) {
                return Err(format!("`{text}` is not a package name"));
            }
        }
        build(source, &parts, range)
    }

    /// The name an import's `source_name` spells.
    ///
    /// # Errors
    ///
    /// A message saying what is wrong with the name.
    pub fn from_ast(name: &SourceName) -> Result<Self, String> {
        let parts: Vec<String> = name
            .parts
            .iter()
            .map(|part| {
                part.iter()
                    .map(|ident| ident.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".")
            })
            .collect();
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        let range = match &name.version {
            None => None,
            Some(lit) => Some(
                crate::plain_text(lit)
                    .ok_or("a version range must be a plain string, without interpolation")?,
            ),
        };
        build(
            name.source.as_ref().map(|s| s.name.as_str()),
            &parts,
            range.as_deref(),
        )
    }
}

impl fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.lookup {
            Lookup::Bare => f.write_str(&self.name)?,
            Lookup::Authored => write!(f, "authored:{}", self.name)?,
            Lookup::In(source) => write!(f, "{}", PackageId::new(source.clone(), &*self.name))?,
        }
        match &self.range {
            Some(range) => write!(f, "@{range}"),
            None => Ok(()),
        }
    }
}

/// A word of a package name: a letter or `_`, then letters, digits, `_` or `-` (the built-in
/// packages' names have hyphens: `std:consul-escalation`).
fn is_word(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

fn build(
    source: Option<&str>,
    parts: &[&str],
    range: Option<&str>,
) -> Result<QualifiedName, String> {
    let written = match source {
        Some(source) => format!("{source}:{}", parts.join("/")),
        None => parts.join("/"),
    };
    let range = match range {
        None => None,
        Some(text) => Some(VersionReq::parse(text).ok_or_else(|| {
            format!("`{text}` is not a version range (`^1.2`, `~1.2.3`, `=1.2.3`)")
        })?),
    };
    let one = |lookup: Lookup| match parts {
        [name] => Ok(QualifiedName {
            lookup,
            name: (*name).to_owned(),
            range,
        }),
        _ => Err(format!("`{written}` has a `/` its source does not take")),
    };
    match source {
        None => one(Lookup::Bare),
        Some("std") => one(Lookup::In(Source::Std)),
        Some("user") => one(Lookup::In(Source::User)),
        Some("project") => one(Lookup::In(Source::Project)),
        Some("plugin") => match parts {
            [plugin, name] if !plugin.contains('.') => Ok(QualifiedName {
                lookup: Lookup::In(Source::Plugin((*plugin).to_owned())),
                name: (*name).to_owned(),
                range,
            }),
            _ => Err(format!(
                "`{written}` needs the form `plugin:<plugin>/<name>`"
            )),
        },
        Some("authored") => match parts {
            [_] => one(Lookup::Authored),
            [area, name] => {
                let area = match *area {
                    "session" => Area::Session,
                    "env" => Area::Env,
                    _ => {
                        return Err(format!(
                            "`{written}`: the authored areas are `session` and `env`"
                        ));
                    }
                };
                Ok(QualifiedName {
                    lookup: Lookup::In(Source::Authored(area)),
                    name: (*name).to_owned(),
                    range,
                })
            }
            _ => Err(format!(
                "`{written}` needs the form `authored:[<area>/]<name>`"
            )),
        },
        Some(other) => Err(format!(
            "`{other}` is not a source (`std`, `user`, `plugin`, `project` or `authored`)"
        )),
    }
}
