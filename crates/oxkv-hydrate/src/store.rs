//! The durable store seam and the in-memory test adapter.
//!
//! The store owns the journal, the generations, and one published pointer. A
//! publish is a single pointer assignment. A reader either sees the old
//! generation or the new generation.

use std::collections::BTreeMap;

use oxkv_contracts::version::{PageSequence, ScopeVersionVector, VersionVector};

use crate::error::HydrationError;
use crate::journal::{GenerationId, Journal};
use crate::page::SnapshotPage;
use crate::snapshot::SnapshotDescriptor;
use crate::tail::DiffFrame;

/// One stored page. The adapter keeps the received frame bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPage {
    /// The exact bytes that the transport served.
    pub raw: Vec<u8>,
    /// The verified page.
    pub page: SnapshotPage,
}

/// One replica generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    /// The generation identity.
    pub id: GenerationId,
    /// The snapshot descriptor that built the generation.
    pub descriptor: SnapshotDescriptor,
    /// The verified pages by sequence.
    pub pages: BTreeMap<PageSequence, StoredPage>,
    /// The visible rows.
    pub rows: BTreeMap<Vec<u8>, Vec<u8>>,
    /// The applied version vector.
    pub version: VersionVector,
    /// True when every page is verified and sealed.
    pub complete: bool,
}

impl Generation {
    /// Creates an empty generation.
    #[must_use]
    pub fn new(id: GenerationId, descriptor: SnapshotDescriptor) -> Self {
        Self {
            id,
            version: descriptor.version_vector.as_vector().clone(),
            descriptor,
            pages: BTreeMap::new(),
            rows: BTreeMap::new(),
            complete: false,
        }
    }

    /// Returns one visible value.
    #[must_use]
    pub fn read(&self, key: &[u8]) -> Option<&[u8]> {
        self.rows.get(key).map(Vec::as_slice)
    }

    /// Returns the applied version vector.
    #[must_use]
    pub fn version_vector(&self) -> ScopeVersionVector {
        ScopeVersionVector::from_vector(self.version.clone())
    }

    /// Applies one verified page. The method is idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::CorruptPage`] when the page payload does not
    /// match its record count.
    pub fn apply_page(&mut self, raw: Vec<u8>, page: SnapshotPage) -> Result<(), HydrationError> {
        if self.pages.contains_key(&page.page_sequence) {
            return Ok(());
        }
        for (key, value) in page.records()? {
            self.rows.insert(key, value);
        }
        self.pages
            .insert(page.page_sequence, StoredPage { raw, page });
        Ok(())
    }

    /// Applies one diff frame.
    ///
    /// The applied version vector never decreases. A frame below the current
    /// shard version keeps the higher version.
    pub fn apply_diff(&mut self, frame: &DiffFrame) {
        if let Some(value) = &frame.value {
            self.rows.insert(frame.key.clone(), value.clone());
        } else {
            self.rows.remove(&frame.key);
        }
        let current = self
            .version
            .get(&frame.shard_id)
            .map_or(frame.commit_version, |current| {
                current.max(frame.commit_version)
            });
        self.version.set(frame.shard_id.clone(), current);
    }

    /// Returns the resident bytes of this generation.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        let rows = self
            .rows
            .iter()
            .map(|(key, value)| {
                u64::try_from(key.len().saturating_add(value.len())).unwrap_or(u64::MAX)
            })
            .sum::<u64>();
        let pages = self
            .pages
            .values()
            .map(|stored| u64::try_from(stored.raw.len()).unwrap_or(u64::MAX))
            .sum::<u64>();
        rows.saturating_add(pages)
    }

    /// Returns a deterministic hash of the visible state.
    #[must_use]
    pub fn state_hash(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"oxkv/generation-state/v1\0");
        hasher.update(self.descriptor.identity.shard_id.as_bytes());
        hasher.update(&self.descriptor.identity.commit_version.get().to_le_bytes());
        hasher.update(self.descriptor.identity.content_digest.as_bytes());
        hasher.update(self.descriptor.manifest_digest.as_bytes());
        hasher.update(self.descriptor.policy_version.as_bytes());
        hasher.update(self.descriptor.scope_identity.as_bytes());
        for (key, value) in &self.rows {
            hasher.update(&u64::try_from(key.len()).unwrap_or(u64::MAX).to_le_bytes());
            hasher.update(key);
            hasher.update(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
            hasher.update(value);
        }
        for (shard_id, commit_version) in self.version.iter() {
            hasher.update(shard_id.as_bytes());
            hasher.update(&commit_version.get().to_le_bytes());
        }
        hasher.update(&[u8::from(self.complete)]);
        *hasher.finalize().as_bytes()
    }
}

/// A read-only view of the published generation.
pub trait GenerationReader {
    /// Returns the published generation identity.
    fn published_generation(&self) -> Option<GenerationId>;

    /// Returns one generation.
    fn generation(&self, id: GenerationId) -> Option<&Generation>;

    /// Returns the published generation.
    fn published(&self) -> Option<&Generation> {
        self.published_generation()
            .and_then(|id| self.generation(id))
    }
}

/// The durable storage seam.
pub trait DurableStore: GenerationReader {
    /// Loads the durable journal.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::Storage`] when the store cannot read.
    fn load_journal(&self) -> Result<Option<Journal>, HydrationError>;

    /// Saves one journal state.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::Storage`] when the store cannot write.
    fn save_journal(&mut self, journal: &Journal) -> Result<(), HydrationError>;

    /// Begins one new generation. The method is idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::Storage`] when the store cannot write.
    fn begin_generation(
        &mut self,
        id: GenerationId,
        descriptor: &SnapshotDescriptor,
    ) -> Result<(), HydrationError>;

    /// Discards one unpublished generation.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::Storage`] when the store cannot write.
    fn discard_generation(&mut self, id: GenerationId) -> Result<(), HydrationError>;

    /// Writes one verified page into a generation.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::Storage`] when the store cannot write.
    fn write_page(
        &mut self,
        id: GenerationId,
        raw: &[u8],
        page: &SnapshotPage,
    ) -> Result<(), HydrationError>;

    /// Marks one generation complete.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::Storage`] when the store cannot write.
    fn complete_generation(&mut self, id: GenerationId) -> Result<(), HydrationError>;

    /// Applies one contiguous diff frame to a generation.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::Storage`] when the store cannot write.
    fn apply_diff(&mut self, id: GenerationId, frame: &DiffFrame) -> Result<(), HydrationError>;

    /// Publishes one complete generation with one pointer swap.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::Storage`] when the generation is absent or
    /// not complete.
    fn publish(&mut self, id: GenerationId) -> Result<(), HydrationError>;

    /// Returns the next free generation identity.
    fn next_generation_id(&self) -> GenerationId;

    /// Returns every generation identity.
    fn generation_ids(&self) -> Vec<GenerationId>;
}

/// An in-memory durable store.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    journal: Option<Journal>,
    generations: BTreeMap<GenerationId, Generation>,
    published: Option<GenerationId>,
}

impl MemoryStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the current journal.
    #[must_use]
    pub const fn journal(&self) -> Option<&Journal> {
        self.journal.as_ref()
    }

    /// Returns the number of stored generations.
    #[must_use]
    pub fn generation_count(&self) -> usize {
        self.generations.len()
    }

    /// Returns the published generation identity.
    #[must_use]
    pub const fn published_id(&self) -> Option<GenerationId> {
        self.published
    }
}

impl GenerationReader for MemoryStore {
    fn published_generation(&self) -> Option<GenerationId> {
        self.published
    }

    fn generation(&self, id: GenerationId) -> Option<&Generation> {
        self.generations.get(&id)
    }
}

impl DurableStore for MemoryStore {
    fn load_journal(&self) -> Result<Option<Journal>, HydrationError> {
        Ok(self.journal.clone())
    }

    fn save_journal(&mut self, journal: &Journal) -> Result<(), HydrationError> {
        self.journal = Some(journal.clone());
        Ok(())
    }

    fn begin_generation(
        &mut self,
        id: GenerationId,
        descriptor: &SnapshotDescriptor,
    ) -> Result<(), HydrationError> {
        self.generations
            .entry(id)
            .or_insert_with(|| Generation::new(id, descriptor.clone()));
        Ok(())
    }

    fn discard_generation(&mut self, id: GenerationId) -> Result<(), HydrationError> {
        if self.published != Some(id) {
            self.generations.remove(&id);
        }
        Ok(())
    }

    fn write_page(
        &mut self,
        id: GenerationId,
        raw: &[u8],
        page: &SnapshotPage,
    ) -> Result<(), HydrationError> {
        let generation = self
            .generations
            .get_mut(&id)
            .ok_or_else(|| HydrationError::storage(format!("generation {id} is absent")))?;
        generation.apply_page(raw.to_vec(), page.clone())
    }

    fn complete_generation(&mut self, id: GenerationId) -> Result<(), HydrationError> {
        let generation = self
            .generations
            .get_mut(&id)
            .ok_or_else(|| HydrationError::storage(format!("generation {id} is absent")))?;
        generation.complete = true;
        Ok(())
    }

    fn apply_diff(&mut self, id: GenerationId, frame: &DiffFrame) -> Result<(), HydrationError> {
        let generation = self
            .generations
            .get_mut(&id)
            .ok_or_else(|| HydrationError::storage(format!("generation {id} is absent")))?;
        generation.apply_diff(frame);
        Ok(())
    }

    fn publish(&mut self, id: GenerationId) -> Result<(), HydrationError> {
        let generation = self
            .generations
            .get(&id)
            .ok_or_else(|| HydrationError::storage(format!("generation {id} is absent")))?;
        if !generation.complete {
            return Err(HydrationError::storage(format!(
                "generation {id} is not complete"
            )));
        }
        self.published = Some(id);
        Ok(())
    }

    fn next_generation_id(&self) -> GenerationId {
        let next = self
            .generations
            .keys()
            .next_back()
            .map_or(1, |id| id.get().saturating_add(1));
        GenerationId::new(next)
    }

    fn generation_ids(&self) -> Vec<GenerationId> {
        self.generations.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::version::{CommitVersion, LeaseTerm, PrefixBounds, ShardId};

    use super::{DiffFrame, Generation, GenerationId};
    use crate::page::SnapshotPage;
    use crate::snapshot::SnapshotDraft;

    #[test]
    fn a_lower_diff_version_does_not_lower_the_applied_vector() {
        let bounds = PrefixBounds::new(vec![b'a'], vec![b'z']);
        let page = SnapshotPage::with_records(
            0,
            vec![(b"alpha".to_vec(), b"one".to_vec())],
            bounds.clone(),
        );
        let snapshot = SnapshotDraft::sample(bounds, LeaseTerm::new(1))
            .seal(vec![page])
            .expect("the sample snapshot seals");
        let mut generation = Generation::new(GenerationId::new(1), snapshot.descriptor());
        let shard = ShardId::new(vec![1]);
        generation.apply_diff(&DiffFrame::new(
            1,
            shard.clone(),
            CommitVersion::new(5),
            b"alpha".to_vec(),
            Some(b"two".to_vec()),
        ));
        generation.apply_diff(&DiffFrame::new(
            2,
            shard.clone(),
            CommitVersion::new(3),
            b"alpha".to_vec(),
            Some(b"three".to_vec()),
        ));
        assert_eq!(generation.version.get(&shard), Some(CommitVersion::new(5)));
    }
}
