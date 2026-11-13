//! The durable hydration journal.
//!
//! The journal records the phase, the snapshot identity, every verified page,
//! the applied tail watermark, and the swap commit. A restart loads the journal
//! and resumes at the last durable boundary. A generation becomes visible only
//! after the swap commit.

use std::collections::BTreeSet;

use oxkv_contracts::version::{ContentDigest, PageSequence, ScopeIdentity, ScopeVersionVector};

/// One durable hydration phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HydrationPhase {
    /// The replica holds no generation.
    #[default]
    Cold,
    /// The replica transfers and verifies snapshot pages.
    Transfer,
    /// The replica verified every snapshot page.
    Verified,
    /// The replica applies the diff tail.
    Tail,
    /// The replica recorded the swap commit and will publish the new generation.
    Swap,
    /// The replica published the new generation.
    Published,
    /// The replica abandoned the new generation.
    Aborted,
}

impl HydrationPhase {
    /// Returns the canonical phase text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Transfer => "transfer",
            Self::Verified => "verified",
            Self::Tail => "tail",
            Self::Swap => "swap",
            Self::Published => "published",
            Self::Aborted => "aborted",
        }
    }

    /// Returns true when the generation is visible to a reader.
    #[must_use]
    pub const fn is_published(self) -> bool {
        matches!(self, Self::Published)
    }

    /// Returns true when the phase stops hydration.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Published | Self::Aborted)
    }
}

/// The durable record of one replica hydration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationJournal {
    /// The current durable phase.
    pub phase: HydrationPhase,
    /// The scope identity of the snapshot.
    pub scope_identity: ScopeIdentity,
    /// The version vector that hydration must reach.
    pub target_version: ScopeVersionVector,
    /// The expected snapshot page count.
    pub expected_pages: u64,
    /// The expected snapshot content digest.
    pub content_digest: ContentDigest,
    /// The generation that hydration builds.
    pub generation: u64,
    /// The generation that the pointer published before hydration.
    pub published_before: Option<u64>,
    /// Every verified page sequence.
    pub verified: BTreeSet<PageSequence>,
    /// The applied tail watermark.
    pub applied_watermark: u64,
    /// The generation named by the durable swap commit.
    pub swap_commit: Option<u64>,
}

impl HydrationJournal {
    /// Creates a journal at the transfer phase.
    #[must_use]
    pub fn new(
        scope_identity: ScopeIdentity,
        target_version: ScopeVersionVector,
        expected_pages: u64,
        content_digest: ContentDigest,
        generation: u64,
        published_before: Option<u64>,
    ) -> Self {
        Self {
            phase: HydrationPhase::Transfer,
            scope_identity,
            target_version,
            expected_pages,
            content_digest,
            generation,
            published_before,
            verified: BTreeSet::new(),
            applied_watermark: 0,
            swap_commit: None,
        }
    }

    /// Returns true when the journal names the same snapshot identity.
    #[must_use]
    pub fn has_identity(
        &self,
        scope_identity: &ScopeIdentity,
        target_version: &ScopeVersionVector,
        expected_pages: u64,
        content_digest: ContentDigest,
    ) -> bool {
        self.scope_identity == *scope_identity
            && self.target_version == *target_version
            && self.expected_pages == expected_pages
            && self.content_digest == content_digest
    }

    /// Returns true when the page sequence is verified.
    #[must_use]
    pub fn is_verified(&self, sequence: PageSequence) -> bool {
        self.verified.contains(&sequence)
    }

    /// Records one verified page. The method is idempotent.
    pub fn record_verified(&mut self, sequence: PageSequence) {
        self.verified.insert(sequence);
    }

    /// Returns the number of verified pages in the contiguous prefix.
    #[must_use]
    pub fn verified_prefix(&self) -> u64 {
        let mut sequence = 0_u64;
        while self.verified.contains(&PageSequence::new(sequence)) {
            sequence = sequence.saturating_add(1);
        }
        sequence
    }

    /// Returns the first page sequence that the journal did not verify.
    #[must_use]
    pub fn first_unverified(&self) -> PageSequence {
        PageSequence::new(self.verified_prefix())
    }

    /// Returns true when every page in the range is verified.
    #[must_use]
    pub fn covers(&self, page_count: u64) -> bool {
        (0..page_count).all(|sequence| self.verified.contains(&PageSequence::new(sequence)))
    }

    /// Advances the applied watermark. The method rejects a gap.
    ///
    /// Returns `false` when the sequence is not the next contiguous value.
    #[must_use]
    pub fn advance_watermark(&mut self, sequence: u64) -> bool {
        if sequence == self.applied_watermark.saturating_add(1) {
            self.applied_watermark = sequence;
            true
        } else {
            false
        }
    }
}

/// The durable journal seam.
pub trait HydrationJournalStore {
    /// Loads the durable journal.
    fn load_journal(&self) -> Option<HydrationJournal>;

    /// Saves one journal state.
    fn save_journal(&mut self, journal: &HydrationJournal);
}

/// An in-memory journal store.
#[derive(Debug, Clone, Default)]
pub struct MemoryJournalStore {
    journal: Option<HydrationJournal>,
}

impl MemoryJournalStore {
    /// Creates an empty store.
    #[must_use]
    pub const fn new() -> Self {
        Self { journal: None }
    }

    /// Returns the stored journal.
    #[must_use]
    pub const fn journal(&self) -> Option<&HydrationJournal> {
        self.journal.as_ref()
    }
}

impl HydrationJournalStore for MemoryJournalStore {
    fn load_journal(&self) -> Option<HydrationJournal> {
        self.journal.clone()
    }

    fn save_journal(&mut self, journal: &HydrationJournal) {
        self.journal = Some(journal.clone());
    }
}
