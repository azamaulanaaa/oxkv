//! Sync cursor signing, validation, and resume decisions.

use oxkv_contracts::cursor::{CursorError, CursorToken, ScopeGeneration, SyncCursor};
use oxkv_contracts::version::{Hash32, ScopeIdentity, ScopeVersionVector};

use crate::codec::Encoder;
use crate::error::ErrorCode;

/// The scope identity and generation that bind one cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorContext {
    /// The canonical scope identity.
    pub scope_identity: ScopeIdentity,
    /// The scope generation.
    pub scope_generation: ScopeGeneration,
}

impl CursorContext {
    /// Creates a cursor context.
    #[must_use]
    pub const fn new(scope_identity: ScopeIdentity, scope_generation: ScopeGeneration) -> Self {
        Self {
            scope_identity,
            scope_generation,
        }
    }
}

/// A signer that authenticates resume cursors.
///
/// The key must stay stable across an authority restart. The tag is a keyed
/// `BLAKE3` hash over the cursor fields and the scope context. A client cannot
/// forge progress without the key.
#[derive(Clone)]
pub struct CursorSigner {
    key: [u8; 32],
}

impl CursorSigner {
    /// Creates a signer from a 32-byte key.
    #[must_use]
    pub const fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    /// Issues a signed cursor at one version vector.
    #[must_use]
    pub fn issue(
        &self,
        context: &CursorContext,
        version_vector: ScopeVersionVector,
        retention_floor: ScopeVersionVector,
    ) -> SyncCursor {
        let auth_tag = self.tag(context, &version_vector, &retention_floor);
        SyncCursor::new(
            context.scope_generation,
            version_vector,
            retention_floor,
            auth_tag,
        )
    }

    /// Verifies one opaque cursor token.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError::Invalid`] for a malformed, tampered, or
    /// wrongly-scoped token. Returns [`CursorError::TypeMismatch`] when the
    /// token belongs to another cursor type.
    pub fn verify(&self, context: &CursorContext, token: &[u8]) -> Result<SyncCursor, CursorError> {
        let cursor = SyncCursor::from_token(token)?;
        if cursor.scope_generation != context.scope_generation {
            return Err(CursorError::Invalid);
        }
        let expected = self.tag(context, &cursor.version_vector, &cursor.retention_floor);
        if expected != cursor.auth_tag {
            return Err(CursorError::Invalid);
        }
        Ok(cursor)
    }

    fn tag(
        &self,
        context: &CursorContext,
        version_vector: &ScopeVersionVector,
        retention_floor: &ScopeVersionVector,
    ) -> Hash32 {
        let mut encoder = Encoder::new();
        encoder.digest(context.scope_identity.as_bytes());
        encoder.u64(context.scope_generation.get());
        encoder.version_vector(version_vector);
        encoder.version_vector(retention_floor);
        Hash32::from_bytes(*blake3::keyed_hash(&self.key, &encoder.finish()).as_bytes())
    }
}

/// The outcome of a cursor validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorDecision {
    /// The cursor is readable. The authority can replay diffs.
    Resume(SyncCursor),
    /// The cursor is older than the retention window.
    Expired,
    /// The cursor is readable, but the gap exceeds the resume budget.
    GapTooLarge,
}

/// The authority side cursor policy.
#[derive(Clone)]
pub struct CursorPolicy {
    /// The cursor signer.
    pub signer: CursorSigner,
    /// The oldest scope version that the authority can serve.
    pub retention_floor: ScopeVersionVector,
    /// The newest scope version that the authority holds.
    pub current_version: ScopeVersionVector,
    /// The largest per-shard version gap that a diff resume can cover.
    pub max_resume_gap: u64,
    /// True when the authority can offer a snapshot fallback.
    pub snapshot_available: bool,
}

impl CursorPolicy {
    /// Creates a cursor policy.
    #[must_use]
    pub const fn new(
        signer: CursorSigner,
        retention_floor: ScopeVersionVector,
        current_version: ScopeVersionVector,
        max_resume_gap: u64,
        snapshot_available: bool,
    ) -> Self {
        Self {
            signer,
            retention_floor,
            current_version,
            max_resume_gap,
            snapshot_available,
        }
    }

    /// Verifies a token and decides whether the diff resume can continue.
    ///
    /// # Errors
    ///
    /// Returns the cursor error when the token is invalid, tampered, or from
    /// another cursor type.
    pub fn evaluate(
        &self,
        context: &CursorContext,
        token: &[u8],
    ) -> Result<CursorDecision, CursorError> {
        let cursor = self.signer.verify(context, token)?;
        if !cursor.is_readable_at(&self.retention_floor) {
            return Ok(CursorDecision::Expired);
        }
        if self.gap_exceeds_budget(&cursor) {
            return Ok(CursorDecision::GapTooLarge);
        }
        Ok(CursorDecision::Resume(cursor))
    }

    fn gap_exceeds_budget(&self, cursor: &SyncCursor) -> bool {
        self.current_version
            .as_vector()
            .iter()
            .any(|(shard_id, current)| {
                let applied = cursor
                    .version_vector
                    .get(shard_id)
                    .map_or(0, oxkv_contracts::version::CommitVersion::get);
                current.get().saturating_sub(applied) > self.max_resume_gap
            })
    }
}

/// Maps one cursor error to the frozen wire error code.
#[must_use]
pub const fn cursor_error_code(error: CursorError) -> ErrorCode {
    match error {
        CursorError::Invalid => ErrorCode::CursorInvalid,
        CursorError::UnsupportedVersion { .. } => ErrorCode::UnsupportedVersion,
        CursorError::TypeMismatch { .. } => ErrorCode::CursorTypeMismatch,
        CursorError::Expired { .. } => ErrorCode::CursorExpired,
    }
}
