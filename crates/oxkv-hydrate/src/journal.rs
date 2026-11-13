//! The durable hydration journal.

use std::collections::BTreeSet;

use oxkv_contracts::version::{PageSequence, ScopeVersionVector};

use crate::page::SnapshotPage;
use crate::snapshot::SnapshotDescriptor;

/// The identity of one replica generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenerationId(u64);

impl GenerationId {
    /// Creates a generation identity.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl core::fmt::Display for GenerationId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, formatter)
    }
}

/// One durable hydration phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Phase {
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

impl Phase {
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

/// The durable record of one hydration.
///
/// The journal records the phase, the snapshot identity, every verified page,
/// the applied diff watermark, and the swap commit. A restart resumes at the
/// last durable boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Journal {
    /// The current durable phase.
    pub phase: Phase,
    /// The snapshot descriptor.
    pub descriptor: SnapshotDescriptor,
    /// The version vector that the tail must reach.
    pub target_vector: ScopeVersionVector,
    /// The number of tail frames that must apply before publication.
    pub tail_length: u64,
    /// The generation that hydration builds.
    pub generation: GenerationId,
    /// The generation that the pointer published before hydration.
    pub published_before: Option<GenerationId>,
    /// Every verified page sequence.
    pub verified: BTreeSet<PageSequence>,
    /// The verified logical bytes.
    pub verified_bytes: u64,
    /// The applied tail watermark.
    pub applied_watermark: u64,
    /// The generation named by the durable swap commit.
    pub swap_commit: Option<GenerationId>,
}

impl Journal {
    /// Creates a journal at the transfer phase.
    #[must_use]
    pub fn new(
        descriptor: SnapshotDescriptor,
        target_vector: ScopeVersionVector,
        tail_length: u64,
        generation: GenerationId,
        published_before: Option<GenerationId>,
    ) -> Self {
        Self {
            phase: Phase::Transfer,
            descriptor,
            target_vector,
            tail_length,
            generation,
            published_before,
            verified: BTreeSet::new(),
            verified_bytes: 0,
            applied_watermark: 0,
            swap_commit: None,
        }
    }

    /// Returns true when the descriptor identity matches this journal.
    #[must_use]
    pub fn has_identity(&self, descriptor: &SnapshotDescriptor) -> bool {
        self.descriptor.same_identity(descriptor)
    }

    /// Returns true when the page sequence is verified.
    #[must_use]
    pub fn is_verified(&self, sequence: PageSequence) -> bool {
        self.verified.contains(&sequence)
    }

    /// Records one verified page. The method is idempotent.
    pub fn record_verified(&mut self, page: &SnapshotPage) {
        if self.verified.insert(page.page_sequence) {
            self.verified_bytes = self.verified_bytes.saturating_add(page.uncompressed_length);
        }
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
