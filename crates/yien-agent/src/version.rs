//! CLI versions and the version ranges profile rules are verified against.

use std::fmt;

/// A `major.minor.patch` CLI version.
///
/// ```
/// use yien_agent::Version;
///
/// let v: Version = "2.1.295".parse().unwrap();
/// assert_eq!(v, Version::new(2, 1, 295));
/// assert!(v > Version::new(2, 1, 9));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Major component.
    pub major: u64,
    /// Minor component.
    pub minor: u64,
    /// Patch component.
    pub patch: u64,
}

impl Version {
    /// A version from its three components.
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A version that is not `N`, `N.N` or `N.N.N`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid version {0:?}; expected N, N.N or N.N.N")]
pub struct VersionError(pub String);

impl std::str::FromStr for Version {
    type Err = VersionError;

    /// Missing components are zero: `2.1` is `2.1.0`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || VersionError(s.to_owned());
        let mut parts = s.trim().split('.');
        let mut next = |required: bool| -> Result<u64, VersionError> {
            match parts.next() {
                None if !required => Ok(0),
                None => Err(err()),
                Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
                    p.parse().map_err(|_| err())
                }
                Some(_) => Err(err()),
            }
        };
        let version = Version::new(next(true)?, next(false)?, next(false)?);
        if parts.next().is_some() {
            return Err(err());
        }
        Ok(version)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
}

/// A comma-separated list of comparators that must all hold, such as
/// `">=2.1.295, <3"`. Empty or `*` matches every version.
///
/// ```
/// use yien_agent::{Version, VersionReq};
///
/// let req: VersionReq = ">=2.1.295, <3".parse().unwrap();
/// assert!(req.matches(Version::new(2, 5, 0)));
/// assert!(!req.matches(Version::new(3, 0, 0)));
/// assert!(!req.matches(Version::new(2, 1, 294)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VersionReq {
    comparators: Vec<(Op, Version)>,
}

impl VersionReq {
    /// Matches every version.
    pub fn any() -> Self {
        Self::default()
    }

    /// Whether `version` satisfies every comparator.
    pub fn matches(&self, version: Version) -> bool {
        self.comparators.iter().all(|(op, v)| match op {
            Op::Eq => version == *v,
            Op::Gt => version > *v,
            Op::Ge => version >= *v,
            Op::Lt => version < *v,
            Op::Le => version <= *v,
        })
    }

    /// The lowest version the range can admit, used to tell which of two
    /// rules is newer. `None` when the range has no lower bound.
    pub fn lower_bound(&self) -> Option<Version> {
        self.comparators
            .iter()
            .filter(|(op, _)| matches!(op, Op::Eq | Op::Gt | Op::Ge))
            .map(|(_, v)| *v)
            .max()
    }
}

impl std::str::FromStr for VersionReq {
    type Err = VersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() || trimmed == "*" {
            return Ok(Self::any());
        }
        let mut comparators = Vec::new();
        for part in trimmed.split(',') {
            let part = part.trim();
            let (op, rest) = if let Some(r) = part.strip_prefix(">=") {
                (Op::Ge, r)
            } else if let Some(r) = part.strip_prefix("<=") {
                (Op::Le, r)
            } else if let Some(r) = part.strip_prefix('>') {
                (Op::Gt, r)
            } else if let Some(r) = part.strip_prefix('<') {
                (Op::Lt, r)
            } else if let Some(r) = part.strip_prefix('=') {
                (Op::Eq, r)
            } else {
                (Op::Eq, part)
            };
            let version = rest
                .trim()
                .parse()
                .map_err(|_| VersionError(s.to_owned()))?;
            comparators.push((op, version));
        }
        Ok(Self { comparators })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_versions_fill_with_zero() {
        assert_eq!("3".parse::<Version>().unwrap(), Version::new(3, 0, 0));
        assert_eq!("2.1".parse::<Version>().unwrap(), Version::new(2, 1, 0));
    }

    #[test]
    fn malformed_versions_are_errors() {
        for bad in [
            "",
            "a.b",
            "1..2",
            "1.2.3.4",
            "-1",
            "1.2.x",
            "99999999999999999999",
        ] {
            assert!(bad.parse::<Version>().is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn every_operator_is_honoured() {
        let v = Version::new(2, 0, 0);
        assert!("=2".parse::<VersionReq>().unwrap().matches(v));
        assert!("2.0.0".parse::<VersionReq>().unwrap().matches(v));
        assert!(">1.9".parse::<VersionReq>().unwrap().matches(v));
        assert!("<=2".parse::<VersionReq>().unwrap().matches(v));
        assert!(!"<2".parse::<VersionReq>().unwrap().matches(v));
        assert!(!">2".parse::<VersionReq>().unwrap().matches(v));
        assert!("*".parse::<VersionReq>().unwrap().matches(v));
        assert!("".parse::<VersionReq>().unwrap().matches(v));
    }

    #[test]
    fn malformed_ranges_are_errors() {
        for bad in [">=", ">= 2, <", "~2", ">=2,,<3"] {
            assert!(bad.parse::<VersionReq>().is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn lower_bound_is_the_highest_floor() {
        let req: VersionReq = ">=2.0, >2.1, <3".parse().unwrap();
        assert_eq!(req.lower_bound(), Some(Version::new(2, 1, 0)));
        assert_eq!("<3".parse::<VersionReq>().unwrap().lower_bound(), None);
    }
}
