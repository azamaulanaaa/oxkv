//! Named hydration errors and visible progress.

use core::fmt;

use oxkv_contracts::version::{PageId, PageSequence, ScopeVersionVector, SnapshotPageVersion};

use crate::journal::Phase;

/// Visible progress of one hydration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HydrationProgress {
    /// The current durable phase.
    pub phase: Phase,
    /// The number of verified logical bytes.
    pub verified_bytes: u64,
    /// The number of verified pages.
    pub verified_pages: u64,
    /// The number of pages in the snapshot.
    pub total_pages: u64,
    /// The logical snapshot byte count.
    pub total_bytes: u64,
    /// The version vector of the published generation.
    pub published_vector: Option<ScopeVersionVector>,
    /// The version vector that hydration must reach.
    pub target_vector: ScopeVersionVector,
}

impl HydrationProgress {
    /// Returns the version lag from the published generation to the target.
    ///
    /// The value is the sum of the per-shard differences. A missing published
    /// generation counts every target entry as lag.
    #[must_use]
    pub fn lag_versions(&self) -> u64 {
        self.target_vector
            .as_vector()
            .iter()
            .map(|(shard_id, target)| {
                let published = self
                    .published_vector
                    .as_ref()
                    .and_then(|vector| vector.get(shard_id))
                    .map_or(0, oxkv_contracts::version::CommitVersion::get);
                target.get().saturating_sub(published)
            })
            .sum()
    }

    /// Returns true when no page is verified.
    #[must_use]
    pub const fn is_cold(&self) -> bool {
        self.published_vector.is_none() && self.verified_pages == 0
    }
}

/// A named hydration error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HydrationError {
    /// The snapshot major version is not supported.
    #[error(
        "The snapshot major version {found} is not supported. This reader supports major {supported_major}."
    )]
    UnsupportedVersion {
        /// The version that the artifact declares.
        found: SnapshotPageVersion,
        /// The major version that this reader supports.
        supported_major: u16,
    },
    /// The snapshot requires a feature that this reader does not implement.
    #[error("The snapshot requires the unknown feature bit {feature}.")]
    MissingRequiredFeature {
        /// The lowest unknown feature bit.
        feature: u64,
    },
    /// A page failed its integrity check.
    #[error("The page {page_id:?} is corrupt at byte offset {byte_offset}.")]
    CorruptPage {
        /// The page identity, when the frame exposes it.
        page_id: Option<PageId>,
        /// The byte offset of the failed region.
        byte_offset: usize,
    },
    /// A page lies outside the snapshot bounds.
    #[error("The page sequence {page_sequence} lies outside the snapshot bounds.")]
    PageOutOfBounds {
        /// The page sequence.
        page_sequence: PageSequence,
    },
    /// A page is larger than the configured page budget.
    #[error("The page has {length} bytes. The page budget is {limit} bytes.")]
    PageTooLarge {
        /// The offered page length.
        length: u64,
        /// The configured page budget.
        limit: u64,
    },
    /// The snapshot ends before the expected byte count.
    #[error("The snapshot is truncated. Expected {expected} bytes and found {actual}.")]
    TruncatedSnapshot {
        /// The expected byte count.
        expected: usize,
        /// The available byte count.
        actual: usize,
    },
    /// A cold replica cannot serve the read.
    #[error("The scope is not hydrated. Phase {phase}. The snapshot has {total_bytes} bytes.")]
    NotHydrated {
        /// The current progress.
        progress: HydrationProgress,
        /// The total logical snapshot size.
        total_bytes: u64,
        /// The phase name.
        phase: &'static str,
    },
    /// The hydration peak exceeds the scope budget.
    #[error(
        "The hydration peak needs {planned_peak_bytes} bytes. The budget is {budget_bytes} bytes."
    )]
    HydrationBudgetExceeded {
        /// The planned peak storage.
        planned_peak_bytes: u64,
        /// The allowed storage.
        budget_bytes: u64,
    },
    /// The requested scope identity differs from the journal identity.
    #[error("The requested scope identity differs from the journal identity.")]
    IdentityMismatch,
    /// The transport cannot serve a page.
    #[error("The transport cannot serve page sequence {page_sequence}: {message}")]
    Transport {
        /// The requested page sequence.
        page_sequence: PageSequence,
        /// The transport diagnostic.
        message: String,
    },
    /// The durable store failed.
    #[error("The durable store failed: {message}")]
    Storage {
        /// The store diagnostic.
        message: String,
    },
    /// The hydration machine has no active request.
    #[error("The hydrator has no active request. Call start first.")]
    NotStarted,
    /// The hydration is already published.
    #[error("The hydration is already published.")]
    AlreadyPublished,
}

impl HydrationError {
    /// Returns the canonical registry code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedVersion { .. } => "format.unsupported_version",
            Self::MissingRequiredFeature { .. } => "format.missing_required_feature",
            Self::CorruptPage { .. } => "format.corrupt_page",
            Self::PageOutOfBounds { .. } => "format.page_out_of_bounds",
            Self::PageTooLarge { .. } => "format.page_too_large",
            Self::TruncatedSnapshot { .. } => "format.truncated_snapshot",
            Self::NotHydrated { .. } => "read.not_hydrated",
            Self::HydrationBudgetExceeded { .. } => "scope.hydration_budget_exceeded",
            Self::IdentityMismatch => "scope.identity_mismatch",
            Self::Transport { .. } => "sync.shard_unavailable",
            Self::Storage { .. } | Self::NotStarted | Self::AlreadyPublished => {
                "internal.invariant_violation"
            }
        }
    }

    /// Creates a storage error.
    #[must_use]
    pub fn storage(message: impl Into<String>) -> Self {
        Self::Storage {
            message: message.into(),
        }
    }

    /// Creates a transport error.
    #[must_use]
    pub fn transport(page_sequence: PageSequence, message: impl Into<String>) -> Self {
        Self::Transport {
            page_sequence,
            message: message.into(),
        }
    }

    /// Returns true when the retry class permits a refetch.
    #[must_use]
    pub const fn is_refetchable(&self) -> bool {
        matches!(
            self,
            Self::CorruptPage { .. } | Self::TruncatedSnapshot { .. }
        )
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}
