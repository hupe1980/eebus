//! `specificationVersion`: what a peer says it speaks, and what to do about it.
//!
//! Every SPINE datagram header carries one, and the SPINE implementation guide §2.5 is
//! strict about its shape: `[1-9][0-9]*\.[0-9]+\.[0-9]+`, three parts, no build metadata,
//! no leading zero on the major. The same rule applies to the versions in detailed
//! discovery and in use-case discovery.
//!
//! The strictness is not pedantry. A peer that sends `TS1.3.0` or `0.3.0` is a peer whose
//! header parser this implementation cannot make assumptions about, and the certification
//! test `TC_SPINE_COMP_006` asks a device either to refuse such a datagram outright or —
//! tolerated for now, with a warning — to answer it anyway. This module refuses, which is
//! what the test calls the recommended behaviour, while tolerating the one deviation the
//! same test permits: a leading `v` or `V`.
//!
//! ```
//! use eebus::spine::SpecVersion;
//!
//! assert_eq!(SpecVersion::parse("1.3.0"), Some(SpecVersion { major: 1, minor: 3, patch: 0 }));
//! assert_eq!(SpecVersion::parse("v1.3.0"), SpecVersion::parse("1.3.0"), "a leading v is tolerated");
//!
//! assert_eq!(SpecVersion::parse("TS1.3.0"), None);
//! assert_eq!(SpecVersion::parse("0.3.0"), None, "the major does not start at zero");
//! assert_eq!(SpecVersion::parse("1.3"), None, "three parts, not two");
//! ```

/// A parsed `specificationVersion`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SpecVersion {
    /// The major version. A change here is a break.
    pub major: u32,
    /// The minor version. Higher than ours is forward-compatible.
    pub minor: u32,
    /// The patch version.
    pub patch: u32,
}

/// The SPINE version this implementation speaks.
pub const SUPPORTED: SpecVersion = SpecVersion {
    major: 1,
    minor: 3,
    patch: 0,
};

impl SpecVersion {
    /// Parses a version string, or [`None`] if it does not match §2.5's pattern.
    ///
    /// A leading `v` or `V` is accepted, which `TC_SPINE_COMP_006` names as the one
    /// deviation a device may tolerate on reception. Nothing is tolerated on the way out:
    /// what this crate sends is [`SUPPORTED`].
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let mut parts = text.split('.');
        let major = parts.next()?;
        let minor = parts.next()?;
        let patch = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        // `[1-9][0-9]*` for the major: no leading zero, and not zero itself.
        if major.starts_with('0') {
            return None;
        }
        Some(Self {
            major: number(major)?,
            minor: number(minor)?,
            patch: number(patch)?,
        })
    }

    /// Whether a peer announcing this version can be talked to.
    ///
    /// The major has to match: SPINE's major version is where the datagram shape itself
    /// could change, and guessing is worse than refusing. A higher minor or patch is
    /// fine, which is what `TC_SPINE_COMP_002` requires — a 1.4.0 peer talking to this
    /// 1.3.0 implementation is a normal case, not an error.
    pub fn is_compatible_with(self, ours: Self) -> bool {
        self.major == ours.major
    }
}

impl core::fmt::Display for SpecVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Parses one component: digits only, and short enough to be a version rather than an
/// attempt at an integer overflow.
fn number(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 9 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// What to make of the `specificationVersion` a datagram carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionCheck {
    /// Speakable: the same major, whatever the minor.
    Compatible(SpecVersion),
    /// A major version this implementation does not speak.
    Incompatible(SpecVersion),
    /// Not a version string at all under §2.5's rule.
    Malformed,
    /// The header carried none.
    ///
    /// The schema makes the element optional and peers in the field omit it, so this is
    /// tolerated rather than refused — but it is reported, because it is a deviation.
    Absent,
}

impl VersionCheck {
    /// Whether the datagram should be processed.
    pub fn is_acceptable(self) -> bool {
        matches!(self, VersionCheck::Compatible(_) | VersionCheck::Absent)
    }
}

/// Checks the version a datagram announced against what this implementation speaks.
pub fn check(version: Option<&str>) -> VersionCheck {
    let Some(version) = version else {
        return VersionCheck::Absent;
    };
    match SpecVersion::parse(version) {
        None => VersionCheck::Malformed,
        Some(parsed) if parsed.is_compatible_with(SUPPORTED) => VersionCheck::Compatible(parsed),
        Some(parsed) => VersionCheck::Incompatible(parsed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TC_SPINE_COMP_005`: the format the implementation guide §2.5 fixes.
    #[test]
    fn tc_spine_comp_005_the_version_format_is_exact() {
        assert!(SpecVersion::parse("1.3.0").is_some());
        assert!(SpecVersion::parse("10.0.12").is_some());

        for bad in [
            "TS1.3.0",
            "0.3.0",
            "01.3.0",
            "1.3",
            "1.3.0.1",
            "1.3.x",
            "",
            "1..0",
            "1.3.0-rc1",
        ] {
            assert_eq!(SpecVersion::parse(bad), None, "{bad:?} is not a version");
        }
    }

    /// `TC_SPINE_COMP_006`: a leading `v` is the one deviation a device may tolerate.
    #[test]
    fn tc_spine_comp_006_a_leading_v_is_tolerated_and_the_rest_is_not() {
        let expected = SpecVersion {
            major: 1,
            minor: 3,
            patch: 0,
        };
        assert_eq!(SpecVersion::parse("v1.3.0"), Some(expected));
        assert_eq!(SpecVersion::parse("V1.3.0"), Some(expected));

        // Everything else on the test's list is refused.
        for bad in ["TS1.3.0", "V0.3.0", "v0.3.0", "0.3.0"] {
            assert!(!check(Some(bad)).is_acceptable(), "{bad:?}");
        }
        // "2.0.0" parses but is a major this implementation does not speak.
        assert_eq!(
            check(Some("2.0.0")),
            VersionCheck::Incompatible(SpecVersion {
                major: 2,
                minor: 0,
                patch: 0
            })
        );
    }

    /// `TC_SPINE_COMP_002`: a peer one minor version ahead is an ordinary peer.
    #[test]
    fn tc_spine_comp_002_a_higher_minor_is_forward_compatible() {
        assert!(check(Some("1.4.0")).is_acceptable());
        assert!(check(Some("1.3.7")).is_acceptable());
        assert!(check(Some("1.0.0")).is_acceptable());
    }

    /// The element is optional in the schema, and peers omit it.
    #[test]
    fn an_absent_version_is_tolerated() {
        assert_eq!(check(None), VersionCheck::Absent);
        assert!(check(None).is_acceptable());
    }

    #[test]
    fn a_component_long_enough_to_overflow_is_not_a_version() {
        assert_eq!(SpecVersion::parse("1.3.99999999999999999999"), None);
    }

    /// The version in every datagram header is the version discovery announces.
    ///
    /// They are two constants — [`SUPPORTED`] here and `SPINE_VERSION` in
    /// [`discovery`](super::super::discovery), which is a string because that is what the
    /// wire carries. Changing one and not the other gives a node that claims 1.3.0 in its
    /// headers and something else in its `specificationVersionList`, which nothing else
    /// would notice.
    #[test]
    fn the_announced_version_and_the_header_version_are_one_version() {
        assert_eq!(
            SpecVersion::parse(super::super::discovery::SPINE_VERSION),
            Some(SUPPORTED),
            "`SPINE_VERSION` and `SUPPORTED` have drifted apart"
        );
    }
}
