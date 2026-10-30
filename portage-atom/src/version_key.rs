//! A solver's handle on a version

use std::fmt;

use crate::version::Version;

/// A [`Version`] for code that copies and compares versions constantly, such as a solver
///
/// Ordered per [PMS 3.3](https://projects.gentoo.org/pms/9/pms.html#version-comparison), as the
/// version is. [`Self::to_version`] gives back what was written.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VersionKey(Version);

impl VersionKey {
    /// The key of `v`
    pub fn new(v: &Version) -> Self {
        Self(v.clone())
    }

    /// The version this was built from, spelled as it was written
    pub fn to_version(&self) -> Version {
        self.0.clone()
    }
}

impl From<&Version> for VersionKey {
    fn from(v: &Version) -> Self {
        Self::new(v)
    }
}

impl fmt::Display for VersionKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Debug for VersionKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "VersionKey({})", self.0)
    }
}
