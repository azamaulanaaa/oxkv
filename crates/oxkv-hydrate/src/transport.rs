//! The snapshot page transport seam.
//!
//! The transport serves one encoded page for one snapshot identity and one
//! page sequence. The hydrator verifies the page checksum before it writes the
//! page.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::version::{PageSequence, SnapshotIdentity};

use crate::error::HydrationError;
use crate::snapshot::SealedSnapshot;

/// The snapshot page transport.
pub trait Transport {
    /// Fetches one encoded page frame.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::Transport`] when the transport cannot serve
    /// the page.
    fn fetch_page(
        &mut self,
        identity: &SnapshotIdentity,
        page_sequence: PageSequence,
    ) -> Result<Vec<u8>, HydrationError>;
}

/// An in-memory transport for tests.
#[derive(Debug, Clone, Default)]
pub struct MemoryTransport {
    pages: BTreeMap<PageSequence, Vec<u8>>,
    corrupt_once: BTreeSet<PageSequence>,
    corrupt_always: BTreeSet<PageSequence>,
    fetches: BTreeMap<PageSequence, u64>,
}

impl MemoryTransport {
    /// Creates an empty transport.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a transport that serves every page of one sealed snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: &SealedSnapshot) -> Self {
        let mut transport = Self::new();
        for page in &snapshot.pages {
            transport.insert(page.page_sequence, page.encode());
        }
        transport
    }

    /// Inserts one encoded page.
    pub fn insert(&mut self, sequence: PageSequence, bytes: Vec<u8>) {
        self.pages.insert(sequence, bytes);
    }

    /// Removes one page. A fetch then returns a transport error.
    pub fn remove(&mut self, sequence: PageSequence) {
        self.pages.remove(&sequence);
    }

    /// Marks one page for one corrupt response. The next fetch flips one byte.
    pub fn corrupt_once(&mut self, sequence: PageSequence) {
        self.corrupt_once.insert(sequence);
    }

    /// Marks one page for every fetch. Each fetch flips one byte.
    pub fn corrupt_always(&mut self, sequence: PageSequence) {
        self.corrupt_always.insert(sequence);
    }

    /// Returns the fetch count for one sequence.
    #[must_use]
    pub fn fetch_count(&self, sequence: PageSequence) -> u64 {
        self.fetches.get(&sequence).copied().unwrap_or(0)
    }

    /// Returns every fetch count.
    #[must_use]
    pub const fn fetches(&self) -> &BTreeMap<PageSequence, u64> {
        &self.fetches
    }
}

impl Transport for MemoryTransport {
    fn fetch_page(
        &mut self,
        _identity: &SnapshotIdentity,
        page_sequence: PageSequence,
    ) -> Result<Vec<u8>, HydrationError> {
        let count = self.fetches.entry(page_sequence).or_insert(0);
        *count = count.saturating_add(1);
        let mut bytes =
            self.pages.get(&page_sequence).cloned().ok_or_else(|| {
                HydrationError::transport(page_sequence, "the page is not available")
            })?;
        let corrupt = self.corrupt_always.contains(&page_sequence)
            || self.corrupt_once.remove(&page_sequence);
        if corrupt && let Some(last) = bytes.last_mut() {
            *last ^= 0xff;
        }
        Ok(bytes)
    }
}

impl Transport for &mut MemoryTransport {
    fn fetch_page(
        &mut self,
        identity: &SnapshotIdentity,
        page_sequence: PageSequence,
    ) -> Result<Vec<u8>, HydrationError> {
        (*self).fetch_page(identity, page_sequence)
    }
}

impl Transport for Box<MemoryTransport> {
    fn fetch_page(
        &mut self,
        identity: &SnapshotIdentity,
        page_sequence: PageSequence,
    ) -> Result<Vec<u8>, HydrationError> {
        self.as_mut().fetch_page(identity, page_sequence)
    }
}
