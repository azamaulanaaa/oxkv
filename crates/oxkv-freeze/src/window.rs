//! The format reader window and the wire read window.
//!
//! A 1.x reader accepts the current format and one back. A 1.x wire peer
//! accepts the current protocol and one extra read version for the major.
//! An unknown version returns a named error.

use core::fmt;

use oxkv_contracts::version::{SnapshotPageVersion, SyncWireVersion};

/// The error code for an unsupported format version.
pub const UNSUPPORTED_FORMAT_CODE: &str = "format.unsupported_version";

/// The error code for an unsupported wire version.
pub const UNSUPPORTED_WIRE_CODE: &str = "sync.unsupported_version";

/// The format versions that one reader accepts.
///
/// The window holds the current format and one format back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderWindow {
    current: SnapshotPageVersion,
    back: Option<SnapshotPageVersion>,
}

impl ReaderWindow {
    /// Creates a reader window from the current format and one format back.
    #[must_use]
    pub const fn new(current: SnapshotPageVersion, back: Option<SnapshotPageVersion>) -> Self {
        Self { current, back }
    }

    /// Returns the 1.0 reader window.
    ///
    /// The window accepts format `1.0` and the previous minor, `0.12`.
    #[must_use]
    pub const fn one_zero() -> Self {
        Self::new(
            SnapshotPageVersion::new(1, 0),
            Some(SnapshotPageVersion::new(0, 12)),
        )
    }

    /// Returns the current format version.
    #[must_use]
    pub const fn current(&self) -> SnapshotPageVersion {
        self.current
    }

    /// Returns the one format back.
    #[must_use]
    pub const fn back(&self) -> Option<SnapshotPageVersion> {
        self.back
    }

    /// Returns every accepted format version.
    #[must_use]
    pub fn accepted_versions(&self) -> Vec<SnapshotPageVersion> {
        core::iter::once(self.current).chain(self.back).collect()
    }

    /// Returns true when the reader accepts the format version.
    #[must_use]
    pub fn is_accepted(&self, version: SnapshotPageVersion) -> bool {
        version == self.current || self.back == Some(version)
    }

    /// Checks one format version against the reader window.
    ///
    /// # Errors
    ///
    /// Returns [`WindowError`] when the version is outside the window. The
    /// error code is [`UNSUPPORTED_FORMAT_CODE`].
    pub fn accepts(&self, version: SnapshotPageVersion) -> Result<(), WindowError> {
        if self.is_accepted(version) {
            return Ok(());
        }
        Err(WindowError {
            found: version,
            accepted: self.accepted_versions(),
        })
    }
}

/// An error from the format reader window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowError {
    found: SnapshotPageVersion,
    accepted: Vec<SnapshotPageVersion>,
}

impl WindowError {
    /// The error code for an unsupported format version.
    pub const CODE: &'static str = UNSUPPORTED_FORMAT_CODE;

    /// Returns the rejected format version.
    #[must_use]
    pub const fn found(&self) -> SnapshotPageVersion {
        self.found
    }

    /// Returns every accepted format version.
    #[must_use]
    pub fn accepted(&self) -> &[SnapshotPageVersion] {
        &self.accepted
    }
}

impl fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let accepted = self
            .accepted
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            formatter,
            "The reader does not accept format version {}. It accepts {}.",
            self.found, accepted
        )
    }
}

impl std::error::Error for WindowError {}

/// The wire versions that one peer accepts.
///
/// The peer accepts one write version and one extra read version for the
/// major. A tab cannot be force-upgraded, so the read window is wider than the
/// write window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireWindow {
    current: SyncWireVersion,
    extra_read: Option<SyncWireVersion>,
}

impl WireWindow {
    /// Creates a wire window from the current protocol and one extra read
    /// version.
    #[must_use]
    pub const fn new(current: SyncWireVersion, extra_read: Option<SyncWireVersion>) -> Self {
        Self {
            current,
            extra_read,
        }
    }

    /// Returns the 1.0 wire window.
    ///
    /// The peer writes protocol version 1 and reads protocol versions 1 and 2.
    #[must_use]
    pub const fn one_zero() -> Self {
        Self::new(SyncWireVersion::new(1), Some(SyncWireVersion::new(2)))
    }

    /// Returns the current write version.
    #[must_use]
    pub const fn current(self) -> SyncWireVersion {
        self.current
    }

    /// Returns the extra read version.
    #[must_use]
    pub const fn extra_read(self) -> Option<SyncWireVersion> {
        self.extra_read
    }

    /// Returns true when the peer accepts the protocol version for a write.
    #[must_use]
    pub fn accepts_write(self, version: SyncWireVersion) -> bool {
        version == self.current
    }

    /// Checks one protocol version against the read window.
    ///
    /// # Errors
    ///
    /// Returns [`WireWindowError`] when the version is outside the read window.
    /// The error code is [`UNSUPPORTED_WIRE_CODE`].
    pub fn accepts_read(self, version: SyncWireVersion) -> Result<(), WireWindowError> {
        if version == self.current || self.extra_read == Some(version) {
            return Ok(());
        }
        Err(WireWindowError {
            found: version,
            current: self.current,
            extra_read: self.extra_read,
        })
    }
}

/// An error from the wire read window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireWindowError {
    found: SyncWireVersion,
    current: SyncWireVersion,
    extra_read: Option<SyncWireVersion>,
}

impl WireWindowError {
    /// The error code for an unsupported wire version.
    pub const CODE: &'static str = UNSUPPORTED_WIRE_CODE;

    /// Returns the rejected protocol version.
    #[must_use]
    pub const fn found(self) -> SyncWireVersion {
        self.found
    }

    /// Returns the current write version.
    #[must_use]
    pub const fn current(self) -> SyncWireVersion {
        self.current
    }

    /// Returns the extra read version.
    #[must_use]
    pub const fn extra_read(self) -> Option<SyncWireVersion> {
        self.extra_read
    }
}

impl fmt::Display for WireWindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.extra_read {
            Some(extra) => write!(
                formatter,
                "The peer does not accept wire version {}. It reads {} and {}.",
                self.found, self.current, extra
            ),
            None => write!(
                formatter,
                "The peer does not accept wire version {}. It reads {}.",
                self.found, self.current
            ),
        }
    }
}

impl std::error::Error for WireWindowError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_zero_reads_the_current_format_and_one_back() {
        let window = ReaderWindow::one_zero();
        assert!(window.is_accepted(SnapshotPageVersion::new(1, 0)));
        assert!(window.is_accepted(SnapshotPageVersion::new(0, 12)));
        assert!(!window.is_accepted(SnapshotPageVersion::new(1, 1)));
    }

    #[test]
    fn an_unknown_major_returns_the_named_code() {
        let error = ReaderWindow::one_zero()
            .accepts(SnapshotPageVersion::new(2, 0))
            .expect_err("major 2 is outside the window");
        assert_eq!(WindowError::CODE, UNSUPPORTED_FORMAT_CODE);
        assert_eq!(error.found(), SnapshotPageVersion::new(2, 0));
    }

    #[test]
    fn the_wire_window_is_wider_for_reads() {
        let window = WireWindow::one_zero();
        assert!(window.accepts_write(SyncWireVersion::new(1)));
        assert!(!window.accepts_write(SyncWireVersion::new(2)));
        assert!(window.accepts_read(SyncWireVersion::new(1)).is_ok());
        assert!(window.accepts_read(SyncWireVersion::new(2)).is_ok());
        assert_eq!(
            window
                .accepts_read(SyncWireVersion::new(3))
                .expect_err("version 3 is outside the read window")
                .found(),
            SyncWireVersion::new(3)
        );
        assert_eq!(WireWindowError::CODE, UNSUPPORTED_WIRE_CODE);
    }
}
