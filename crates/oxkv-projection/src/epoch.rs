//! Projection epochs and policy cohorts.
//!
//! A projection epoch has a generation and a policy version. The policy
//! version is part of the epoch identity. A canary cohort therefore serves a
//! different epoch from its control cohort.
//!
//! The generation is the monotone integer that a [`ProjectionCursor`] carries.
//! The policy version binds the epoch to one policy body without a rebuild.
//!
//! [`ProjectionCursor`]: oxkv_contracts::ProjectionCursor

use oxkv_contracts::version::{Hash32, PolicyVersion, ProjectionEpoch};

use crate::codec::{CanonicalEncode, Encoder};
use crate::error::ProjectionError;

/// The identity of one projection epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectionEpochState {
    generation: ProjectionEpoch,
    policy_version: PolicyVersion,
}

impl ProjectionEpochState {
    /// Creates an epoch identity.
    #[must_use]
    pub const fn new(generation: ProjectionEpoch, policy_version: PolicyVersion) -> Self {
        Self {
            generation,
            policy_version,
        }
    }

    /// Returns the epoch generation.
    #[must_use]
    pub const fn generation(self) -> ProjectionEpoch {
        self.generation
    }

    /// Returns the bound policy version.
    #[must_use]
    pub const fn policy_version(self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the canonical epoch identity digest.
    ///
    /// The digest covers the generation and the policy version. A canary
    /// cohort and its control cohort produce different digests.
    #[must_use]
    pub fn identity_digest(self) -> Hash32 {
        Hash32::of(&self.canonical_bytes())
    }

    /// Returns a copy with a new policy version and the same generation.
    #[must_use]
    pub const fn with_policy_version(self, policy_version: PolicyVersion) -> Self {
        Self {
            generation: self.generation,
            policy_version,
        }
    }

    /// Returns the next epoch generation.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochOverflow`] when the counter is at its
    /// maximum.
    pub fn bump(self) -> Result<Self, ProjectionError> {
        let generation = self
            .generation
            .get()
            .checked_add(1)
            .ok_or(ProjectionError::EpochOverflow)?;
        Ok(Self {
            generation: ProjectionEpoch::new(generation),
            policy_version: self.policy_version,
        })
    }
}

impl CanonicalEncode for ProjectionEpochState {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.u64(self.generation.get());
        encoder.bytes(self.policy_version.as_bytes());
        encoder.finish()
    }
}

/// One policy cohort.
///
/// A canary rollout uses two cohorts. Each cohort names its policy version and
/// its own epoch generation. The two cohorts therefore hold different
/// projection epochs at the same time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyCohort {
    name: String,
    policy_version: PolicyVersion,
    epoch_generation: ProjectionEpoch,
}

impl PolicyCohort {
    /// Creates a policy cohort.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        policy_version: PolicyVersion,
        epoch_generation: ProjectionEpoch,
    ) -> Self {
        Self {
            name: name.into(),
            policy_version,
            epoch_generation,
        }
    }

    /// Returns the cohort name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the policy version of the cohort.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the epoch generation of the cohort.
    #[must_use]
    pub const fn epoch_generation(&self) -> ProjectionEpoch {
        self.epoch_generation
    }

    /// Returns the projection epoch identity of the cohort.
    ///
    /// The identity includes the policy version.
    #[must_use]
    pub const fn epoch_state(&self) -> ProjectionEpochState {
        ProjectionEpochState::new(self.epoch_generation, self.policy_version)
    }
}
