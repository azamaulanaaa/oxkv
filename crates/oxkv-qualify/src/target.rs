//! Target labels for the scenario matrix.
//!
//! A target labels one run. It does not change a verdict or a byte. The same
//! scenario definition runs on every target. A target profile holds only
//! descriptive data for the report.

/// One target in the matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Target {
    /// A native host.
    Native,
    /// A browser-shaped host. The core sees no browser-specific branch.
    Browser,
    /// A native host again. The third run closes the round trip.
    NativeAgain,
}

impl Target {
    /// Every target in matrix order.
    pub const ALL: [Self; 3] = [Self::Native, Self::Browser, Self::NativeAgain];

    /// Returns the canonical target name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Browser => "browser",
            Self::NativeAgain => "native-again",
        }
    }

    /// Returns true when the target is browser-shaped.
    #[must_use]
    pub const fn is_browser(self) -> bool {
        matches!(self, Self::Browser)
    }
}

impl core::fmt::Display for Target {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Descriptive data about one target.
///
/// The suite uses these values in the report only. A semantic path does not
/// read them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetProfile {
    /// The target label.
    pub target: Target,
    /// True when the host offers threads.
    pub threads: bool,
    /// The pointer width in bits.
    pub pointer_width: u32,
}

impl TargetProfile {
    /// Returns the profile for one target.
    #[must_use]
    pub const fn for_target(target: Target) -> Self {
        match target {
            Target::Native | Target::NativeAgain => Self {
                target,
                threads: true,
                pointer_width: 64,
            },
            Target::Browser => Self {
                target,
                threads: false,
                pointer_width: 32,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Target, TargetProfile};

    #[test]
    fn every_target_has_one_name() {
        assert_eq!(Target::ALL.len(), 3);
        assert_eq!(Target::Native.name(), "native");
        assert_eq!(Target::Browser.name(), "browser");
        assert_eq!(Target::NativeAgain.name(), "native-again");
    }

    #[test]
    fn a_profile_describes_the_target_without_a_verdict() {
        for target in Target::ALL {
            let profile = TargetProfile::for_target(target);
            assert_eq!(profile.target, target);
            assert_eq!(profile.threads, !target.is_browser());
        }
    }
}
