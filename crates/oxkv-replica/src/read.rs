//! Read, search, pin, and staleness contracts.
//!
//! A read pins one version vector. A read and a search at one pin agree. A
//! read over its staleness bound returns [`ReplicaError::Stale`] with no data.
//! The default mode is bounded staleness. `AtLeast` is the default pin mode.
//!
//! Time is monitoring data only. The caller injects the current time. No read
//! verdict reads a clock.

use oxkv_contracts::version::{PageId, ScopeVersionVector};

use crate::error::ReplicaError;
use crate::index::{IndexVersion, SearchHit};

/// The hydration state of one replica.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HydrationState {
    /// No generation is published.
    Cold,
    /// A generation is in progress.
    Hydrating {
        /// The verified byte count.
        verified_bytes: u64,
        /// The target byte count.
        target_bytes: u64,
    },
    /// A generation is published.
    Hydrated,
}

impl HydrationState {
    /// Returns true when no generation is published.
    #[must_use]
    pub const fn is_cold(&self) -> bool {
        matches!(self, Self::Cold)
    }

    /// Returns the verified and target byte counts.
    #[must_use]
    pub const fn progress(&self) -> (u64, u64) {
        match self {
            Self::Hydrating {
                verified_bytes,
                target_bytes,
            } => (*verified_bytes, *target_bytes),
            Self::Cold | Self::Hydrated => (0, 0),
        }
    }
}

/// A read pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    /// Serve the given version or a newer version.
    AtLeast(ScopeVersionVector),
    /// Serve exactly the given version.
    Exactly(ScopeVersionVector),
}

impl Pin {
    /// Creates an `AtLeast` pin.
    #[must_use]
    pub const fn at_least(versions: ScopeVersionVector) -> Self {
        Self::AtLeast(versions)
    }

    /// Creates an `Exactly` pin.
    #[must_use]
    pub const fn exactly(versions: ScopeVersionVector) -> Self {
        Self::Exactly(versions)
    }

    /// Returns the pinned version vector.
    #[must_use]
    pub const fn versions(&self) -> &ScopeVersionVector {
        match self {
            Self::AtLeast(versions) | Self::Exactly(versions) => versions,
        }
    }
}

/// The maximum allowed version lag of one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StalenessBound {
    /// No version bound. The caller accepts offline data.
    Unbounded,
    /// The replica may lag by this many versions at most.
    MaxVersions(u64),
}

impl StalenessBound {
    /// Returns true when the measured lag is inside the bound.
    #[must_use]
    pub const fn admits(self, lag: u64) -> bool {
        match self {
            Self::Unbounded => true,
            Self::MaxVersions(bound) => lag <= bound,
        }
    }

    /// Returns the numeric bound.
    #[must_use]
    pub const fn max_versions(self) -> Option<u64> {
        match self {
            Self::Unbounded => None,
            Self::MaxVersions(bound) => Some(bound),
        }
    }
}

/// One point read request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    /// The canonical key.
    pub key: Vec<u8>,
    /// The read pin.
    pub pin: Pin,
    /// The staleness bound.
    pub bound: StalenessBound,
}

impl ReadRequest {
    /// Creates a read request.
    #[must_use]
    pub const fn new(key: Vec<u8>, pin: Pin, bound: StalenessBound) -> Self {
        Self { key, pin, bound }
    }
}

/// One point read response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResponse {
    /// The canonical key.
    pub key: Vec<u8>,
    /// The value at the served pin.
    pub value: Vec<u8>,
    /// The served version vector.
    pub served: ScopeVersionVector,
    /// The requested staleness bound.
    pub bound: StalenessBound,
    /// The measured version lag.
    pub lag_versions: u64,
    /// The page that delivered the value. `None` means a diff tail entry.
    pub page_id: Option<PageId>,
}

/// One search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// The query text.
    pub text: String,
    /// The search pin.
    pub pin: Pin,
    /// The staleness bound.
    pub bound: StalenessBound,
    /// The maximum hit count.
    pub limit: usize,
    /// The expected tokenizer version. `None` uses the scope tokenizer.
    pub tokenizer_version: Option<oxkv_contracts::version::TokenizerVersion>,
}

impl SearchRequest {
    /// Creates a search request.
    #[must_use]
    pub fn new(text: impl Into<String>, pin: Pin, bound: StalenessBound, limit: usize) -> Self {
        Self {
            text: text.into(),
            pin,
            bound,
            limit,
            tokenizer_version: None,
        }
    }

    /// Sets the expected tokenizer version.
    #[must_use]
    pub fn with_tokenizer(
        mut self,
        tokenizer_version: oxkv_contracts::version::TokenizerVersion,
    ) -> Self {
        self.tokenizer_version = Some(tokenizer_version);
        self
    }
}

/// One search response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResponse {
    /// The hits in score order, then canonical key order.
    pub hits: Vec<SearchHit>,
    /// The served version vector.
    pub served: ScopeVersionVector,
    /// The requested staleness bound.
    pub bound: StalenessBound,
    /// The measured version lag.
    pub lag_versions: u64,
    /// The tokenizer version of the index.
    pub tokenizer_version: oxkv_contracts::version::TokenizerVersion,
    /// The index version.
    pub index_version: IndexVersion,
}

/// The exposed status of one replica.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaStatus {
    /// The applied version vector.
    pub applied: ScopeVersionVector,
    /// The authority watermark.
    pub authority_watermark: ScopeVersionVector,
    /// The lag in versions.
    pub lag_versions: u64,
    /// The lag in injected time units.
    pub lag_time: Option<u64>,
    /// The hydration state.
    pub hydration: HydrationState,
}

pub(crate) fn resolve_pin(applied: &ScopeVersionVector, pin: &Pin) -> Result<(), ReplicaError> {
    let requested = pin.versions();
    if !applied.as_vector().entails(requested.as_vector()) {
        return Err(ReplicaError::BehindCommit {
            applied: crate::versions::max_version(applied),
            requested: crate::versions::max_version(requested),
        });
    }
    if matches!(pin, Pin::Exactly(_)) && requested.as_vector() != applied.as_vector() {
        return Err(ReplicaError::PinExpired {
            oldest: crate::versions::max_version(applied),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::version::{CommitVersion, ScopeVersionVector, ShardId, VersionVector};

    use super::{Pin, StalenessBound, resolve_pin};

    fn vector(entries: &[(&[u8], u64)]) -> ScopeVersionVector {
        ScopeVersionVector::from_vector(VersionVector::from_entries(
            entries.iter().map(|(shard, version)| {
                (ShardId::new(shard.to_vec()), CommitVersion::new(*version))
            }),
        ))
    }

    #[test]
    fn the_bound_admits_lag_at_or_below_the_limit() {
        assert!(StalenessBound::MaxVersions(3).admits(3));
        assert!(!StalenessBound::MaxVersions(3).admits(4));
        assert!(StalenessBound::Unbounded.admits(u64::MAX));
    }

    #[test]
    fn an_at_least_pin_accepts_a_newer_vector() {
        let applied = vector(&[(&[1], 12)]);
        assert!(resolve_pin(&applied, &Pin::at_least(vector(&[(&[1], 10)]))).is_ok());
    }

    #[test]
    fn an_exact_pin_rejects_a_newer_vector() {
        let applied = vector(&[(&[1], 12)]);
        assert_eq!(
            resolve_pin(&applied, &Pin::exactly(vector(&[(&[1], 10)])))
                .expect_err("the pin is old")
                .code(),
            "read.pin_expired"
        );
    }

    #[test]
    fn a_pin_above_the_applied_vector_is_behind_commit() {
        let applied = vector(&[(&[1], 12)]);
        assert_eq!(
            resolve_pin(&applied, &Pin::exactly(vector(&[(&[1], 13)])))
                .expect_err("the pin is ahead")
                .code(),
            "read.behind_commit"
        );
    }
}
