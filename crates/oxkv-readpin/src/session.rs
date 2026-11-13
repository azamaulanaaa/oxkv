//! Session tokens and the session read mode.
//!
//! A session token carries a version vector minimum. It never carries a
//! scalar. The type has no constructor that accepts a commit version alone.
//!
//! In read-your-writes mode, the client raises the minimum to the commit
//! version of a terminal receipt.

use oxkv_contracts::receipt::ReceiptEnvelope;
use oxkv_contracts::version::{CommitVersion, SessionMinimum, ShardId, VersionVector};

/// The canonical magic bytes of a session token.
const MAGIC: [u8; 4] = *b"STV1";

/// An error from session token bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionTokenError {
    /// The magic bytes are wrong.
    #[error("The session token magic is invalid.")]
    InvalidMagic,
    /// The token is shorter than its declared fields.
    #[error("The session token is truncated.")]
    Truncated,
    /// The token has bytes after the last field.
    #[error("The session token has trailing bytes.")]
    TrailingBytes,
    /// A length field does not fit in memory.
    #[error("The session token length is out of range.")]
    LengthOverflow,
}

/// A session token with a version vector minimum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionToken {
    minimum: VersionVector,
}

impl SessionToken {
    /// Creates a session token from a version vector minimum.
    #[must_use]
    pub fn new(minimum: &SessionMinimum) -> Self {
        Self {
            minimum: minimum.as_vector().clone(),
        }
    }

    /// Creates an empty session token.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            minimum: VersionVector::new(),
        }
    }

    /// Returns the version vector minimum.
    #[must_use]
    pub const fn minimum(&self) -> &VersionVector {
        &self.minimum
    }

    /// Returns the minimum as a typed version vector value.
    #[must_use]
    pub fn as_minimum(&self) -> SessionMinimum {
        SessionMinimum::from_vector(self.minimum.clone())
    }

    /// Raises the minimum to every entry of another vector.
    ///
    /// The operation is monotone. It returns true when the minimum changed.
    pub fn raise(&mut self, required: &VersionVector) -> bool {
        let mut changed = false;
        for (shard_id, required_version) in required.iter() {
            let current = self.minimum.get(shard_id);
            if current.is_none_or(|version| version < required_version) {
                self.minimum.set(shard_id.clone(), required_version);
                changed = true;
            }
        }
        changed
    }

    /// Raises the minimum for one shard.
    ///
    /// The operation is monotone. It returns true when the minimum changed.
    pub fn raise_to(&mut self, shard_id: ShardId, commit_version: CommitVersion) -> bool {
        let current = self.minimum.get(&shard_id);
        if current.is_some_and(|version| version >= commit_version) {
            return false;
        }
        self.minimum.set(shard_id, commit_version);
        true
    }

    /// Raises the minimum to the commit version of a terminal receipt.
    ///
    /// The call does nothing when the receipt has no commit version. It
    /// returns true when the minimum changed.
    pub fn raise_to_receipt(&mut self, receipt: &ReceiptEnvelope) -> bool {
        let Some(commit_version) = receipt.commit_version() else {
            return false;
        };
        self.raise_to(receipt.audit().shard_id.clone(), commit_version)
    }

    /// Returns true when the minimum entails the given vector.
    #[must_use]
    pub fn covers(&self, required: &VersionVector) -> bool {
        self.minimum.entails(required)
    }

    /// Encodes the token as canonical bytes.
    ///
    /// The layout is `"STV1"`, a `u32` entry count, then one entry per shard.
    /// An entry is a `u64` shard length, the shard bytes, and a `u64` commit
    /// version. The entries use canonical shard order.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let entry_count = u32::try_from(self.minimum.len()).unwrap_or(u32::MAX);
        let mut bytes = Vec::with_capacity(8 + self.minimum.len() * 24);
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&entry_count.to_be_bytes());
        for (shard_id, commit_version) in self.minimum.iter() {
            let shard = shard_id.as_bytes();
            let shard_len = u64::try_from(shard.len()).unwrap_or(u64::MAX);
            bytes.extend_from_slice(&shard_len.to_be_bytes());
            bytes.extend_from_slice(shard);
            bytes.extend_from_slice(&commit_version.get().to_be_bytes());
        }
        bytes
    }

    /// Decodes a session token from canonical bytes.
    ///
    /// # Errors
    ///
    /// Returns [`SessionTokenError::InvalidMagic`] when the magic bytes are
    /// wrong. Returns [`SessionTokenError::Truncated`] when a field is
    /// missing. Returns [`SessionTokenError::TrailingBytes`] when bytes
    /// remain after the last entry. Returns
    /// [`SessionTokenError::LengthOverflow`] when a length does not fit.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SessionTokenError> {
        if bytes.len() < 8 {
            return Err(SessionTokenError::Truncated);
        }
        if bytes[..4] != MAGIC {
            return Err(SessionTokenError::InvalidMagic);
        }
        let entry_count = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let mut minimum = VersionVector::new();
        let mut offset = 8_usize;
        for _ in 0..entry_count {
            let shard_len = read_u64(bytes, &mut offset)?;
            let shard_len =
                usize::try_from(shard_len).map_err(|_| SessionTokenError::LengthOverflow)?;
            let end = offset
                .checked_add(shard_len)
                .ok_or(SessionTokenError::LengthOverflow)?;
            let shard = bytes.get(offset..end).ok_or(SessionTokenError::Truncated)?;
            offset = end;
            let commit_version = read_u64(bytes, &mut offset)?;
            minimum.set(
                ShardId::new(shard.to_vec()),
                CommitVersion::new(commit_version),
            );
        }
        if offset != bytes.len() {
            return Err(SessionTokenError::TrailingBytes);
        }
        Ok(Self { minimum })
    }
}

fn read_u64(bytes: &[u8], offset: &mut usize) -> Result<u64, SessionTokenError> {
    let end = offset
        .checked_add(8)
        .ok_or(SessionTokenError::LengthOverflow)?;
    let field = bytes
        .get(*offset..end)
        .ok_or(SessionTokenError::Truncated)?;
    let mut raw = [0_u8; 8];
    raw.copy_from_slice(field);
    *offset = end;
    Ok(u64::from_be_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_token_round_trips() {
        let mut minimum = VersionVector::new();
        minimum.set(ShardId::new(vec![1]), CommitVersion::new(5));
        minimum.set(ShardId::new(vec![2]), CommitVersion::new(9));
        let token = SessionToken::new(&SessionMinimum::from_vector(minimum));
        let bytes = token.to_bytes();
        assert_eq!(SessionToken::from_bytes(&bytes), Ok(token));
    }

    #[test]
    fn a_tampered_session_token_fails() {
        let token = SessionToken::empty();
        let mut bytes = token.to_bytes();
        bytes[0] = b'X';
        assert_eq!(
            SessionToken::from_bytes(&bytes),
            Err(SessionTokenError::InvalidMagic)
        );
    }
}
