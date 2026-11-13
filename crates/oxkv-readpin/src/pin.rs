//! Authority read pins and replica local pins.
//!
//! The two pin types have different lifetimes. An [`AuthorityReadPin`] is
//! bounded by authority retention. A [`ReplicaLocalPin`] is the applied
//! version vector of a replica and stays readable offline.

use oxkv_contracts::version::{
    CommitVersion, ManifestDigest, ScopeIdentity, ScopeVersionVector, ShardId, VersionVector,
};

use crate::time::{DurationMillis, Timestamp};

/// The named failure of an authority read pin.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    /// The authority pin is past its retention time.
    #[error("The authority pin expired.")]
    Expired {
        /// The time at which the pin expires.
        expires_at: Timestamp,
        /// The time of the check.
        now: Timestamp,
    },
    /// The authority pin names a version below the compaction floor.
    #[error("The authority pin is below the read floor for one shard.")]
    Unreadable {
        /// The shard that moved past the pin.
        shard_id: ShardId,
        /// The version in the pin.
        pin_version: CommitVersion,
        /// The oldest readable version for the shard.
        floor: CommitVersion,
    },
}

impl PinError {
    /// Returns the canonical error code.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Expired { .. } => "read.pin_expired",
            Self::Unreadable { .. } => "read.pin_unreadable",
        }
    }
}

/// A bounded pin for a read on the authority.
///
/// The pin names a scope version vector and an expiry time. The authority
/// keeps the pin readable until the expiry time and while the named versions
/// stay at or above the compaction floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityReadPin {
    scope_identity: ScopeIdentity,
    manifest_digest: ManifestDigest,
    version_vector: ScopeVersionVector,
    issued_at: Timestamp,
    expires_at: Timestamp,
}

impl AuthorityReadPin {
    /// Creates an authority read pin.
    #[must_use]
    pub const fn new(
        scope_identity: ScopeIdentity,
        manifest_digest: ManifestDigest,
        version_vector: ScopeVersionVector,
        issued_at: Timestamp,
        expires_at: Timestamp,
    ) -> Self {
        Self {
            scope_identity,
            manifest_digest,
            version_vector,
            issued_at,
            expires_at,
        }
    }

    /// Creates an authority read pin from an injected retention rule.
    #[must_use]
    pub fn from_retention(
        scope_identity: ScopeIdentity,
        manifest_digest: ManifestDigest,
        version_vector: ScopeVersionVector,
        issued_at: Timestamp,
        retention: DurationMillis,
    ) -> Self {
        Self::new(
            scope_identity,
            manifest_digest,
            version_vector,
            issued_at,
            issued_at.saturating_add(retention),
        )
    }

    /// Returns the scope identity.
    #[must_use]
    pub const fn scope_identity(&self) -> ScopeIdentity {
        self.scope_identity
    }

    /// Returns the manifest digest.
    #[must_use]
    pub const fn manifest_digest(&self) -> ManifestDigest {
        self.manifest_digest
    }

    /// Returns the pinned version vector.
    #[must_use]
    pub const fn version_vector(&self) -> &ScopeVersionVector {
        &self.version_vector
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

    /// Returns true when the pin is past its retention time.
    #[must_use]
    pub const fn is_expired(&self, now: Timestamp) -> bool {
        now.get() >= self.expires_at.get()
    }

    /// Checks the pin against the current time and compaction floor.
    ///
    /// The check tests the expiry time first. It then tests every pinned
    /// shard against the floor.
    ///
    /// # Errors
    ///
    /// Returns [`PinError::Expired`] when the pin is past its retention time.
    /// Returns [`PinError::Unreadable`] when a pinned version is below the
    /// compaction floor.
    pub fn check(&self, now: Timestamp, authority_floor: &VersionVector) -> Result<(), PinError> {
        if self.is_expired(now) {
            return Err(PinError::Expired {
                expires_at: self.expires_at,
                now,
            });
        }
        for (shard_id, pin_version) in self.version_vector.as_vector().iter() {
            let Some(floor) = authority_floor.get(shard_id) else {
                continue;
            };
            if pin_version < floor {
                return Err(PinError::Unreadable {
                    shard_id: shard_id.clone(),
                    pin_version,
                    floor,
                });
            }
        }
        Ok(())
    }
}

/// The applied version vector of a replica.
///
/// The type has no expiry field. A local pin stays readable offline. It does
/// not expire with authority retention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaLocalPin {
    version_vector: ScopeVersionVector,
}

impl ReplicaLocalPin {
    /// Creates a replica local pin.
    #[must_use]
    pub const fn new(version_vector: ScopeVersionVector) -> Self {
        Self { version_vector }
    }

    /// Creates a replica local pin from an applied version vector.
    #[must_use]
    pub fn from_applied(applied: VersionVector) -> Self {
        Self::new(ScopeVersionVector::from_vector(applied))
    }

    /// Returns the applied version vector.
    #[must_use]
    pub const fn version_vector(&self) -> &ScopeVersionVector {
        &self.version_vector
    }

    /// Returns true. A local pin is readable without authority contact.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn is_readable_offline(&self) -> bool {
        true
    }

    /// Returns false. Authority retention does not expire a local pin.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn expires_with_authority_retention(&self) -> bool {
        false
    }
}
