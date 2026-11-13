//! Resume cursors for the sync diff stream.
//!
//! A resume cursor is bounded by authority diff retention. A cursor that is
//! past its retention time, or below the authority diff floor, returns
//! `sync.cursor_expired` and offers a snapshot.

use oxkv_contracts::cursor::{CursorError, CursorToken, SyncCursor};
use oxkv_contracts::version::{CommitVersion, ScopeVersionVector, ShardId};

use crate::time::{DurationMillis, Timestamp};

/// The reason that a resume cursor expired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorExpiryReason {
    /// The cursor is past its retention time.
    Age {
        /// The issue time of the cursor.
        issued_at: Timestamp,
        /// The expiry time of the cursor.
        expires_at: Timestamp,
        /// The time of the resume attempt.
        now: Timestamp,
    },
    /// The cursor is below the authority diff floor.
    BelowRetentionFloor {
        /// The shard that moved past the cursor.
        shard_id: ShardId,
        /// The cursor position for the shard. `None` means the cursor has no
        /// entry for the shard.
        cursor_version: Option<CommitVersion>,
        /// The oldest version that the authority still retains.
        floor: CommitVersion,
    },
}

/// An expired resume cursor.
///
/// The error offers a snapshot. The caller must not resume the diff stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorExpired {
    reason: CursorExpiryReason,
}

impl CursorExpired {
    /// Creates a cursor expiry value.
    #[must_use]
    pub const fn new(reason: CursorExpiryReason) -> Self {
        Self { reason }
    }

    /// Returns the expiry reason.
    #[must_use]
    pub const fn reason(&self) -> &CursorExpiryReason {
        &self.reason
    }

    /// Returns the canonical error code.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn code(&self) -> &'static str {
        "sync.cursor_expired"
    }

    /// Returns true. The caller may start a snapshot fallback.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn offers_snapshot(&self) -> bool {
        true
    }
}

/// The result of a resume attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeOutcome {
    /// The authority still retains every diff after the cursor.
    Resume,
    /// The cursor expired. The authority offers a snapshot.
    SnapshotRequired(CursorExpired),
}

impl ResumeOutcome {
    /// Returns the canonical error code when the cursor expired.
    #[must_use]
    pub const fn error_code(&self) -> Option<&'static str> {
        match self {
            Self::Resume => None,
            Self::SnapshotRequired(error) => Some(error.code()),
        }
    }

    /// Returns true when the outcome offers a snapshot.
    #[must_use]
    pub const fn offers_snapshot(&self) -> bool {
        match self {
            Self::Resume => false,
            Self::SnapshotRequired(error) => error.offers_snapshot(),
        }
    }

    /// Returns true when the cursor can resume.
    #[must_use]
    pub const fn is_resume(&self) -> bool {
        matches!(self, Self::Resume)
    }
}

/// A resume cursor with an injected expiry time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeCursor {
    cursor: SyncCursor,
    issued_at: Timestamp,
    expires_at: Timestamp,
}

impl ResumeCursor {
    /// Creates a resume cursor from a sync cursor and a diff retention rule.
    #[must_use]
    pub fn new(cursor: SyncCursor, issued_at: Timestamp, diff_retention: DurationMillis) -> Self {
        Self {
            cursor,
            issued_at,
            expires_at: issued_at.saturating_add(diff_retention),
        }
    }

    /// Creates a resume cursor from an opaque sync cursor token.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError`] when the token is malformed, tampered, or from
    /// another cursor type.
    pub fn from_token(
        token: &[u8],
        issued_at: Timestamp,
        diff_retention: DurationMillis,
    ) -> Result<Self, CursorError> {
        let cursor = SyncCursor::from_token(token)?;
        Ok(Self::new(cursor, issued_at, diff_retention))
    }

    /// Returns the inner sync cursor.
    #[must_use]
    pub const fn sync_cursor(&self) -> &SyncCursor {
        &self.cursor
    }

    /// Returns the cursor version vector.
    #[must_use]
    pub const fn version_vector(&self) -> &ScopeVersionVector {
        &self.cursor.version_vector
    }

    /// Returns the issue time.
    #[must_use]
    pub const fn issued_at(&self) -> Timestamp {
        self.issued_at
    }

    /// Returns the expiry time.
    #[must_use]
    pub const fn expires_at(&self) -> Timestamp {
        self.expires_at
    }

    /// Returns true when the cursor is past its retention time.
    #[must_use]
    pub const fn is_expired(&self, now: Timestamp) -> bool {
        now.get() >= self.expires_at.get()
    }

    /// Attempts to resume against the current time and diff floor.
    #[must_use]
    pub fn resume(&self, now: Timestamp, authority_floor: &ScopeVersionVector) -> ResumeOutcome {
        if self.is_expired(now) {
            return ResumeOutcome::SnapshotRequired(CursorExpired::new(CursorExpiryReason::Age {
                issued_at: self.issued_at,
                expires_at: self.expires_at,
                now,
            }));
        }
        for (shard_id, floor) in authority_floor.as_vector().iter() {
            match self.cursor.version_vector.get(shard_id) {
                Some(cursor_version) if cursor_version >= floor => {}
                cursor_version => {
                    return ResumeOutcome::SnapshotRequired(CursorExpired::new(
                        CursorExpiryReason::BelowRetentionFloor {
                            shard_id: shard_id.clone(),
                            cursor_version,
                            floor,
                        },
                    ));
                }
            }
        }
        ResumeOutcome::Resume
    }

    /// Encodes the cursor as one versioned opaque token.
    #[must_use]
    pub fn to_token(&self) -> Vec<u8> {
        self.cursor.to_token()
    }
}
