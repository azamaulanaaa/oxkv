//! Page and directory codec identifiers.

use core::fmt;

/// The codec identifier of one snapshot body object.
///
/// The 1.0 contract defines the identity codec only. A reader rejects any
/// other identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CodecId(u16);

impl CodecId {
    /// The identity codec. It stores bytes without a transform.
    pub const IDENTITY: Self = Self(0);

    /// Creates a codec identifier.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the numeric identifier.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Returns true when the codec is the identity codec.
    #[must_use]
    pub const fn is_identity(self) -> bool {
        self.0 == Self::IDENTITY.0
    }
}

impl fmt::Display for CodecId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}
