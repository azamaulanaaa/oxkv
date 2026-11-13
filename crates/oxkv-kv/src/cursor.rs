//! Bidirectional cursors with a pinned commit version.

use oxkv_contracts::CursorDirection;
use oxkv_contracts::version::{CommitVersion, DigestDomain, Hash32};

use crate::codec::Decoder;
use crate::error::KvError;

/// The cursor token magic bytes.
const CURSOR_MAGIC: [u8; 4] = *b"OXKC";
/// The cursor token format version.
pub(crate) const CURSOR_VERSION: u16 = 1;
/// The checksum length in bytes.
const CHECKSUM_LENGTH: usize = 32;
/// The fixed token header length.
const HEADER_LENGTH: usize = 4 + 2;

/// A bidirectional cursor.
///
/// The cursor pins one commit version and one resume position. A scan at the
/// pinned version returns the same remaining sequence on every continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    snapshot_version: CommitVersion,
    position: Option<Vec<u8>>,
    direction: CursorDirection,
}

impl Cursor {
    /// Creates a cursor at the start of a scan.
    #[must_use]
    pub const fn start(snapshot_version: CommitVersion, direction: CursorDirection) -> Self {
        Self {
            snapshot_version,
            position: None,
            direction,
        }
    }

    /// Creates a cursor after one returned key.
    #[must_use]
    pub const fn after(
        snapshot_version: CommitVersion,
        last_key: Vec<u8>,
        direction: CursorDirection,
    ) -> Self {
        Self {
            snapshot_version,
            position: Some(last_key),
            direction,
        }
    }

    /// Returns the pinned commit version.
    #[must_use]
    pub const fn snapshot_version(&self) -> CommitVersion {
        self.snapshot_version
    }

    /// Returns the last returned key.
    #[must_use]
    pub fn position(&self) -> Option<&[u8]> {
        self.position.as_deref()
    }

    /// Returns the scan direction.
    #[must_use]
    pub const fn direction(&self) -> CursorDirection {
        self.direction
    }

    /// Encodes the cursor as one opaque token.
    ///
    /// The token has a fixed big-endian form and a BLAKE3 checksum. The bytes
    /// are identical on native and `wasm32`.
    #[must_use]
    pub fn to_token(&self) -> Vec<u8> {
        let mut token = Vec::with_capacity(
            HEADER_LENGTH
                + 8
                + 1
                + 1
                + 8
                + self.position.as_ref().map_or(0, Vec::len)
                + CHECKSUM_LENGTH,
        );
        token.extend_from_slice(&CURSOR_MAGIC);
        token.extend_from_slice(&CURSOR_VERSION.to_be_bytes());
        token.extend_from_slice(&self.snapshot_version.get().to_be_bytes());
        token.push(self.direction.tag());
        token.push(u8::from(self.position.is_some()));
        push_slice(&mut token, self.position.as_deref().unwrap_or(&[]));
        let checksum = Hash32::of_domain(DigestDomain::CursorToken, &token);
        token.extend_from_slice(checksum.as_bytes());
        token
    }

    /// Decodes one opaque cursor token.
    ///
    /// # Errors
    ///
    /// Returns [`KvError::UnknownVersion`] for an unknown token format
    /// version. Returns [`KvError::MalformedCursor`] for a bad magic value, a
    /// bad body, a bad checksum, or trailing bytes.
    pub fn from_token(token: &[u8]) -> Result<Self, KvError> {
        if token.len() < HEADER_LENGTH + 8 + 1 + 1 + 8 + CHECKSUM_LENGTH {
            return Err(KvError::MalformedCursor);
        }
        let checksum_start = token.len() - CHECKSUM_LENGTH;
        let expected = Hash32::of_domain(DigestDomain::CursorToken, &token[..checksum_start]);
        if expected.as_bytes().as_slice() != &token[checksum_start..] {
            return Err(KvError::MalformedCursor);
        }
        let mut decoder = Decoder::new(&token[..checksum_start]);
        let magic = decoder.take(4).ok_or(KvError::MalformedCursor)?;
        if magic != CURSOR_MAGIC {
            return Err(KvError::MalformedCursor);
        }
        let version = decoder.u16().ok_or(KvError::MalformedCursor)?;
        if version != CURSOR_VERSION {
            return Err(KvError::UnknownVersion {
                version: u64::from(version),
            });
        }
        let snapshot_version = CommitVersion::new(decoder.u64().ok_or(KvError::MalformedCursor)?);
        let direction = CursorDirection::from_tag(decoder.u8().ok_or(KvError::MalformedCursor)?)
            .ok_or(KvError::MalformedCursor)?;
        let has_position = decoder.u8().ok_or(KvError::MalformedCursor)?;
        let position = decoder.bytes().ok_or(KvError::MalformedCursor)?.to_vec();
        let position = match has_position {
            0 if position.is_empty() => None,
            1 => Some(position),
            _ => return Err(KvError::MalformedCursor),
        };
        if !decoder.is_finished() {
            return Err(KvError::MalformedCursor);
        }
        Ok(Self {
            snapshot_version,
            position,
            direction,
        })
    }
}

fn push_slice(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    bytes.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Cursor {
        Cursor::after(
            CommitVersion::new(7),
            b"b".to_vec(),
            CursorDirection::Ascending,
        )
    }

    #[test]
    fn a_cursor_round_trips_through_its_token() {
        assert_eq!(Cursor::from_token(&sample().to_token()), Ok(sample()));
        let start = Cursor::start(CommitVersion::new(1), CursorDirection::Descending);
        assert_eq!(Cursor::from_token(&start.to_token()), Ok(start));
    }

    #[test]
    fn a_tampered_token_fails() {
        let mut token = sample().to_token();
        let last = token.len() - 1;
        token[last] ^= 1;
        assert_eq!(Cursor::from_token(&token), Err(KvError::MalformedCursor));
    }

    #[test]
    fn an_unknown_token_version_fails() {
        let mut token = sample().to_token();
        token[4..6].copy_from_slice(&2_u16.to_be_bytes());
        let checksum_start = token.len() - CHECKSUM_LENGTH;
        let checksum = Hash32::of_domain(DigestDomain::CursorToken, &token[..checksum_start]);
        token[checksum_start..].copy_from_slice(checksum.as_bytes());
        assert_eq!(
            Cursor::from_token(&token),
            Err(KvError::UnknownVersion { version: 2 })
        );
    }

    #[test]
    fn a_truncated_token_fails() {
        let token = sample().to_token();
        for length in 0..token.len() {
            assert_eq!(
                Cursor::from_token(&token[..length]),
                Err(KvError::MalformedCursor)
            );
        }
    }

    #[test]
    fn an_empty_resume_key_is_valid() {
        let cursor = Cursor::after(
            CommitVersion::new(1),
            Vec::new(),
            CursorDirection::Descending,
        );
        assert_eq!(Cursor::from_token(&cursor.to_token()), Ok(cursor));
    }
}
