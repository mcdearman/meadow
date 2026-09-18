//! Versions, and what a dependency will take.
//!
//! A package's releases are its git tags: `v1.2.0`, or `1.2.0` for anyone who
//! leaves the `v` off. A manifest names one of them and means "that, or any
//! later release that does not break it":
//!
//! ```toml
//! json = { git = "https://github.com/someone/meadow-json", version = "1.2.0" }
//! ```
//!
//! which is `>=1.2.0, <2.0.0`. Below 1.0 the minor is the breaking digit, as
//! Cargo reads it: `0.3.1` means `>=0.3.1, <0.4.0`, because a package finding
//! its shape changes it under the minor.
//!
//! A pre-release (`1.0.0-rc1`) is never chosen for a requirement that does not
//! ask for one: it sorts below the release it precedes, and wanting one is
//! saying so.

use std::cmp::Ordering;
use std::fmt;

/// A release: `major.minor.patch`, and a pre-release tail if it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// `rc1` of `1.0.0-rc1`. Empty for a release.
    pub pre: String,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
            pre: String::new(),
        }
    }

    /// `1.2.3`, `v1.2.3` or `1.2.3-rc1` as a version, or `None` for anything
    /// else -- which is how a tag that is not a release is passed over.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.strip_prefix('v').unwrap_or(text);
        // Build metadata (`+deadbeef`) names the same release, so it is read
        // and dropped rather than refused.
        let text = text.split('+').next().unwrap_or(text);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) if !pre.is_empty() => (core, pre.to_string()),
            Some(_) => return None,
            None => (text, String::new()),
        };
        let mut parts = core.split('.');
        let mut number = || -> Option<u64> {
            let p = parts.next()?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            // A leading zero means it is not the number it looks like.
            if p.len() > 1 && p.starts_with('0') {
                return None;
            }
            p.parse().ok()
        };
        let (major, minor, patch) = (number()?, number()?, number()?);
        if parts.next().is_some() {
            return None;
        }
        if !pre
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// The digit that breaks compatibility: the major, or the minor below 1.0.
    ///
    /// Two releases with the same one can stand in for each other; two with
    /// different ones are different packages as far as a build is concerned.
    pub fn breaking(&self) -> (u64, u64) {
        if self.major > 0 {
            (self.major, 0)
        } else {
            (0, self.minor)
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Version) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                // A release outranks the pre-releases leading to it.
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => compare_pre(&self.pre, &other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Pre-release tails, compared field by field: numbers as numbers, and a
/// number below anything spelled out, as semver says.
fn compare_pre(a: &str, b: &str) -> Ordering {
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            // Fewer fields, all equal so far, is the earlier one.
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let numeric = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
                let ordering = match (numeric(x), numeric(y)) {
                    (true, true) => x
                        .parse::<u64>()
                        .unwrap_or(0)
                        .cmp(&y.parse::<u64>().unwrap_or(0)),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => x.cmp(y),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

/// What a dependency will take: a release, and anything after it that does not
/// break it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Req {
    pub least: Version,
}

impl Req {
    /// `version = "1.2.0"` as a requirement.
    pub fn parse(text: &str) -> Option<Req> {
        // `^1.2.0` is what this means, so it is accepted spelled out.
        let text = text.trim().strip_prefix('^').unwrap_or(text.trim());
        Version::parse(text).map(|least| Req { least })
    }

    /// Whether `v` is this release or a later compatible one.
    pub fn allows(&self, v: &Version) -> bool {
        // Asking for a release is not asking for the pre-releases of the one
        // after it, which would be newer but not yet what they precede.
        if v.is_prerelease() && !self.least.is_prerelease() {
            return false;
        }
        if v.is_prerelease() && (v.major, v.minor, v.patch) != self.least_numbers() {
            return false;
        }
        v >= &self.least && v.breaking() == self.least.breaking()
    }

    fn least_numbers(&self) -> (u64, u64, u64) {
        (self.least.major, self.least.minor, self.least.patch)
    }

    /// The newest of `versions` this will take.
    pub fn best<'a>(&self, versions: &'a [Version]) -> Option<&'a Version> {
        versions.iter().filter(|v| self.allows(v)).max()
    }

    /// Whether the two could be met by one release: `^1.2` and `^1.4` can,
    /// `^1.2` and `^2.0` cannot.
    pub fn compatible_with(&self, other: &Req) -> bool {
        self.least.breaking() == other.least.breaking()
    }

    /// The stricter of two compatible requirements.
    pub fn strictest(&self, other: &Req) -> Req {
        if other.least > self.least {
            other.clone()
        } else {
            self.clone()
        }
    }
}

impl fmt::Display for Req {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.least)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).expect("a version")
    }

    #[test]
    fn versions_parse_with_or_without_the_v() {
        assert_eq!(v("1.2.3"), Version::new(1, 2, 3));
        assert_eq!(v("v1.2.3"), Version::new(1, 2, 3));
        assert_eq!(v("v1.2.3-rc1").pre, "rc1");
        assert_eq!(v("1.2.3+build7"), Version::new(1, 2, 3));
    }

    #[test]
    fn what_is_not_a_release_is_not_a_version() {
        for text in [
            "",
            "v",
            "1",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "v01.2.3",
            "1.2.-3",
            "release-1",
            "1.2.3-",
        ] {
            assert!(Version::parse(text).is_none(), "{text} parsed");
        }
    }

    #[test]
    fn a_release_outranks_its_pre_releases() {
        assert!(v("1.0.0") > v("1.0.0-rc2"));
        assert!(v("1.0.0-rc2") > v("1.0.0-rc1"));
        // Dot-separated fields, so `rc.10` is a number and beats `rc.2`;
        // `rc10` is one word and sorts before `rc2`, as semver has it.
        assert!(v("1.0.0-rc.10") > v("1.0.0-rc.2"));
        assert!(v("1.0.0-rc10") < v("1.0.0-rc2"));
        assert!(v("1.0.0-alpha") < v("1.0.0-beta"));
        assert!(v("1.0.0-1") < v("1.0.0-alpha"));
        assert!(v("0.2.0") > v("0.1.9"));
    }

    #[test]
    fn a_requirement_takes_what_does_not_break_it() {
        let req = Req::parse("1.2.0").expect("a requirement");
        assert!(req.allows(&v("1.2.0")));
        assert!(req.allows(&v("1.9.3")));
        assert!(!req.allows(&v("1.1.9")));
        assert!(!req.allows(&v("2.0.0")));
        assert_eq!(Req::parse("^1.2.0"), Req::parse("1.2.0"));
    }

    #[test]
    fn below_one_the_minor_breaks() {
        let req = Req::parse("0.3.1").expect("a requirement");
        assert!(req.allows(&v("0.3.1")));
        assert!(req.allows(&v("0.3.9")));
        assert!(!req.allows(&v("0.4.0")));
        assert!(!req.allows(&v("0.3.0")));
    }

    #[test]
    fn a_pre_release_is_not_taken_unless_it_was_asked_for() {
        let req = Req::parse("1.2.0").expect("a requirement");
        assert!(!req.allows(&v("1.3.0-rc1")));
        let rc = Req::parse("1.2.0-rc1").expect("a requirement");
        assert!(rc.allows(&v("1.2.0-rc2")));
        assert!(rc.allows(&v("1.2.0")));
    }

    #[test]
    fn the_best_is_the_newest_that_fits() {
        let have: Vec<Version> = ["0.9.0", "1.0.0", "1.4.2", "1.4.10", "2.0.0"]
            .iter()
            .map(|s| v(s))
            .collect();
        let req = Req::parse("1.0.0").expect("a requirement");
        assert_eq!(
            req.best(&have).map(|v| v.to_string()),
            Some("1.4.10".into())
        );
        let none = Req::parse("3.0.0").expect("a requirement");
        assert_eq!(none.best(&have), None);
    }

    #[test]
    fn two_requirements_meet_when_nothing_breaks_between_them() {
        let a = Req::parse("1.2.0").expect("a requirement");
        let b = Req::parse("1.4.0").expect("a requirement");
        let c = Req::parse("2.0.0").expect("a requirement");
        assert!(a.compatible_with(&b));
        assert!(!a.compatible_with(&c));
        assert_eq!(a.strictest(&b), b);
        let d = Req::parse("0.1.0").expect("a requirement");
        let e = Req::parse("0.2.0").expect("a requirement");
        assert!(!d.compatible_with(&e));
    }
}
