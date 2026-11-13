//! The read-only replica view.
//!
//! A cold replica returns `read.not_hydrated` with progress. A hydrated replica
//! serves the published generation. During a new hydration, the published
//! pointer still names the old generation.

use oxkv_contracts::version::ScopeVersionVector;

use crate::error::{HydrationError, HydrationProgress};
use crate::journal::Phase;
use crate::store::{DurableStore, Generation};

/// A read-only view of one replica store.
#[derive(Debug)]
pub struct Replica<'a, S>
where
    S: DurableStore,
{
    store: &'a S,
}

impl<'a, S> Replica<'a, S>
where
    S: DurableStore,
{
    /// Creates a replica view.
    pub const fn new(store: &'a S) -> Self {
        Self { store }
    }

    /// Returns the published generation.
    #[must_use]
    pub fn published(&self) -> Option<&Generation> {
        self.store.published()
    }

    /// Returns true when the replica serves a complete generation.
    #[must_use]
    pub fn is_hydrated(&self) -> bool {
        self.store
            .published()
            .is_some_and(|generation| generation.complete)
    }

    /// Reads one key.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::NotHydrated`] with progress on a cold replica.
    pub fn read(&self, key: &[u8]) -> Result<Option<Vec<u8>>, HydrationError> {
        let published = self.store.published();
        if let Some(generation) = published
            && generation.complete
        {
            return Ok(generation.read(key).map(Vec::from));
        }
        let progress = self.progress();
        Err(HydrationError::NotHydrated {
            total_bytes: progress.total_bytes,
            phase: progress.phase.name(),
            progress,
        })
    }

    /// Returns the published version vector.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::NotHydrated`] with progress on a cold replica.
    pub fn version(&self) -> Result<ScopeVersionVector, HydrationError> {
        if let Some(generation) = self.store.published()
            && generation.complete
        {
            return Ok(generation.version_vector());
        }
        let progress = self.progress();
        Err(HydrationError::NotHydrated {
            total_bytes: progress.total_bytes,
            phase: progress.phase.name(),
            progress,
        })
    }

    /// Returns the current hydration progress.
    #[must_use]
    pub fn progress(&self) -> HydrationProgress {
        let journal = self.store.load_journal().ok().flatten();
        let published_vector = self.store.published().map(Generation::version_vector);
        let cold_vector = published_vector.clone();
        journal.map_or_else(
            || HydrationProgress {
                phase: Phase::Cold,
                verified_bytes: 0,
                verified_pages: 0,
                total_pages: 0,
                total_bytes: 0,
                published_vector: cold_vector,
                target_vector: ScopeVersionVector::new(),
            },
            |journal| HydrationProgress {
                phase: journal.phase,
                verified_bytes: journal.verified_bytes,
                verified_pages: u64::try_from(journal.verified.len()).unwrap_or(u64::MAX),
                total_pages: journal.descriptor.page_count,
                total_bytes: journal.descriptor.snapshot_bytes,
                published_vector,
                target_vector: journal.target_vector.clone(),
            },
        )
    }
}
