//! Package versions (`meta { version: "1.4.0" }`) and the ranges an import pins (`@"^1.2"`).

use std::fmt;

/// A version: three numbers, without pre-release or build parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Reads `MAJOR.MINOR.PATCH` (no leading zeros, no other parts).
    #[must_use]
    pub fn parse(text: &str) -> Option<Version> {
        match numbers(text)?.as_slice() {
            &[major, minor, patch] => Some(Version {
                major,
                minor,
                patch,
            }),
            _ => None,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// How a range compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `^1.2`: compatible versions (the same major, or the same minor below 1.0).
    Caret,
    /// `~1.2`: the same minor (the same major when only the major is given).
    Tilde,
    /// `=1.2.3`: exactly this version.
    Exact,
}

/// A version range: an operator and one to three numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VersionReq {
    pub op: Op,
    pub major: u64,
    pub minor: Option<u64>,
    pub patch: Option<u64>,
}

impl VersionReq {
    /// Reads `^M[.m[.p]]`, `~M[.m[.p]]` or `=M.m.p`.
    #[must_use]
    pub fn parse(text: &str) -> Option<VersionReq> {
        let (op, rest) = match text.as_bytes().first()? {
            b'^' => (Op::Caret, &text[1..]),
            b'~' => (Op::Tilde, &text[1..]),
            b'=' => (Op::Exact, &text[1..]),
            _ => return None,
        };
        let (major, minor, patch) = match *numbers(rest)?.as_slice() {
            [major] => (major, None, None),
            [major, minor] => (major, Some(minor), None),
            [major, minor, patch] => (major, Some(minor), Some(patch)),
            _ => return None,
        };
        if op == Op::Exact && patch.is_none() {
            return None;
        }
        Some(VersionReq {
            op,
            major,
            minor,
            patch,
        })
    }

    /// Whether `version` is in the range.
    #[must_use]
    pub fn matches(&self, version: &Version) -> bool {
        let floor = Version {
            major: self.major,
            minor: self.minor.unwrap_or(0),
            patch: self.patch.unwrap_or(0),
        };
        if *version < floor {
            return false;
        }
        match (self.op, self.minor, self.patch) {
            (Op::Exact, ..) => *version == floor,
            (Op::Tilde, None, _) => version.major == self.major,
            (Op::Tilde, Some(minor), _) => version.major == self.major && version.minor == minor,
            (Op::Caret, minor, patch) => {
                if self.major > 0 || minor.is_none() {
                    version.major == self.major
                } else if minor != Some(0) || patch.is_none() {
                    version.major == 0 && Some(version.minor) == minor
                } else {
                    *version == floor
                }
            }
        }
    }
}

impl fmt::Display for VersionReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op = match self.op {
            Op::Caret => '^',
            Op::Tilde => '~',
            Op::Exact => '=',
        };
        write!(f, "{op}{}", self.major)?;
        if let Some(minor) = self.minor {
            write!(f, ".{minor}")?;
        }
        if let Some(patch) = self.patch {
            write!(f, ".{patch}")?;
        }
        Ok(())
    }
}

/// Dot-separated decimal numbers without leading zeros.
fn numbers(text: &str) -> Option<Vec<u64>> {
    text.split('.')
        .map(|part| {
            let digits = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
            let leading_zero = part.len() > 1 && part.starts_with('0');
            if digits && !leading_zero {
                part.parse().ok()
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(range: &str, version: &str) -> bool {
        VersionReq::parse(range)
            .unwrap()
            .matches(&Version::parse(version).unwrap())
    }

    #[test]
    fn caret_ranges_follow_semver_compatibility() {
        assert!(ok("^1.2", "1.9.0") && !ok("^1.2", "1.1.9") && !ok("^1.2", "2.0.0"));
        assert!(ok("^1", "1.0.0") && !ok("^1", "0.9.9"));
        assert!(ok("^0.2.3", "0.2.9") && !ok("^0.2.3", "0.3.0"));
        assert!(ok("^0.0.3", "0.0.3") && !ok("^0.0.3", "0.0.4"));
        assert!(ok("^0.0", "0.0.7") && !ok("^0.0", "0.1.0"));
        assert!(ok("^0", "0.9.0") && !ok("^0", "1.0.0"));
    }

    #[test]
    fn tilde_and_exact_ranges() {
        assert!(ok("~1.2", "1.2.7") && !ok("~1.2", "1.3.0"));
        assert!(ok("~1", "1.7.0") && !ok("~1", "2.0.0"));
        assert!(ok("=1.2.3", "1.2.3") && !ok("=1.2.3", "1.2.4"));
    }

    #[test]
    fn malformed_versions_and_ranges_are_refused() {
        for range in [
            "",
            "1.2",
            "^",
            "^1.",
            "^01",
            "=1.2",
            "^1.2.3.4",
            ">1",
            "^1.2-beta",
            "^ 1",
        ] {
            assert_eq!(VersionReq::parse(range), None, "{range}");
        }
        for version in ["1.2", "1.2.3.4", "01.2.3", "1.2.3-rc1", "", "a.b.c"] {
            assert_eq!(Version::parse(version), None, "{version}");
        }
        assert_eq!(VersionReq::parse("^99999999999999999999"), None);
    }
}
