//! The three-tier ladder and its storage driver seam.
//!
//! A shard spans three tiers: RAM, local disk, and object storage. A
//! [`TierDriver`] supplies storage only. It does not choose a tier and it does
//! not change a byte.
//!
//! [`TierStack`] compares tier state and decides which driver answers. It
//! returns named errors and preserves artifact identity across a move.

use core::fmt;
use std::collections::BTreeMap;

use oxkv_contracts::version::{ContentDigest, PageSequence, ShardId};

use crate::error::TierError;

/// One storage tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tier {
    /// The volatile page cache.
    Ram,
    /// The local write-ahead region and immutable segments.
    Disk,
    /// The remote immutable object store.
    Object,
}

impl Tier {
    /// Every tier from the hottest to the coldest.
    pub const ALL: &'static [Self] = &[Self::Ram, Self::Disk, Self::Object];

    /// Returns the lowercase tier name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ram => "ram",
            Self::Disk => "disk",
            Self::Object => "object",
        }
    }

    /// Returns the position of the tier in the ladder.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Ram => 0,
            Self::Disk => 1,
            Self::Object => 2,
        }
    }

    /// Returns true when this tier is colder than the other tier.
    #[must_use]
    pub const fn is_colder_than(self, other: Self) -> bool {
        self.rank() > other.rank()
    }

    /// Returns true when this tier is hotter than the other tier.
    #[must_use]
    pub const fn is_hotter_than(self, other: Self) -> bool {
        self.rank() < other.rank()
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The injected state of one tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TierStatus {
    /// The tier answers reads and writes.
    #[default]
    Available,
    /// The tier did not answer.
    Unavailable,
}

impl TierStatus {
    /// Returns true when the tier is available.
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// The injected state of all three tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TierAvailability {
    ram: TierStatus,
    disk: TierStatus,
    object: TierStatus,
}

impl TierAvailability {
    /// Creates the state of all three tiers.
    #[must_use]
    pub const fn new(ram: TierStatus, disk: TierStatus, object: TierStatus) -> Self {
        Self { ram, disk, object }
    }

    /// Returns the state where every tier answers.
    #[must_use]
    pub const fn all_available() -> Self {
        Self::new(
            TierStatus::Available,
            TierStatus::Available,
            TierStatus::Available,
        )
    }

    /// Returns the state of one tier.
    #[must_use]
    pub const fn status(self, tier: Tier) -> TierStatus {
        match tier {
            Tier::Ram => self.ram,
            Tier::Disk => self.disk,
            Tier::Object => self.object,
        }
    }

    /// Sets the state of one tier.
    pub fn set(&mut self, tier: Tier, status: TierStatus) {
        match tier {
            Tier::Ram => self.ram = status,
            Tier::Disk => self.disk = status,
            Tier::Object => self.object = status,
        }
    }

    /// Returns a copy with one tier state set.
    #[must_use]
    pub fn with(mut self, tier: Tier, status: TierStatus) -> Self {
        self.set(tier, status);
        self
    }
}

impl Default for TierAvailability {
    fn default() -> Self {
        Self::all_available()
    }
}

/// The stable identity of one artifact across every tier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactId {
    shard_id: ShardId,
    segment_version: u64,
    page_sequence: PageSequence,
}

impl ArtifactId {
    /// Creates an artifact identity.
    #[must_use]
    pub const fn new(shard_id: ShardId, segment_version: u64, page_sequence: PageSequence) -> Self {
        Self {
            shard_id,
            segment_version,
            page_sequence,
        }
    }

    /// Returns the shard identity.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the segment version.
    #[must_use]
    pub const fn segment_version(&self) -> u64 {
        self.segment_version
    }

    /// Returns the page sequence.
    #[must_use]
    pub const fn page_sequence(&self) -> PageSequence {
        self.page_sequence
    }
}

/// A driver that supplies storage for one tier.
///
/// The driver stores and returns the exact bytes that it receives. It does not
/// choose a placement, decide a verdict, or seal an artifact. The caller
/// reports tier state through [`TierAvailability`].
pub trait TierDriver {
    /// Returns the tier that this driver serves.
    fn tier(&self) -> Tier;

    /// Returns the exact bytes for one artifact.
    fn get(&self, artifact: &ArtifactId) -> Option<Vec<u8>>;

    /// Stores the exact bytes for one artifact.
    fn put(&mut self, artifact: ArtifactId, bytes: Vec<u8>);

    /// Removes one artifact and returns its bytes.
    fn remove(&mut self, artifact: &ArtifactId) -> Option<Vec<u8>>;

    /// Returns the number of stored artifacts.
    fn artifact_count(&self) -> usize;
}

/// A deterministic in-memory tier driver for tests and wasm.
#[derive(Debug, Clone)]
pub struct MemoryTierDriver {
    tier: Tier,
    artifacts: BTreeMap<ArtifactId, Vec<u8>>,
}

impl MemoryTierDriver {
    /// Creates an empty driver for one tier.
    #[must_use]
    pub const fn new(tier: Tier) -> Self {
        Self {
            tier,
            artifacts: BTreeMap::new(),
        }
    }

    /// Returns true when the driver holds no artifact.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }
}

impl TierDriver for MemoryTierDriver {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn get(&self, artifact: &ArtifactId) -> Option<Vec<u8>> {
        self.artifacts.get(artifact).cloned()
    }

    fn put(&mut self, artifact: ArtifactId, bytes: Vec<u8>) {
        self.artifacts.insert(artifact, bytes);
    }

    fn remove(&mut self, artifact: &ArtifactId) -> Option<Vec<u8>> {
        self.artifacts.remove(artifact)
    }

    fn artifact_count(&self) -> usize {
        self.artifacts.len()
    }
}

/// The tier ladder with one driver per tier.
///
/// The stack holds no clock and no randomness. The caller injects tier state.
pub struct TierStack {
    drivers: Vec<Box<dyn TierDriver>>,
    availability: TierAvailability,
}

impl TierStack {
    /// Creates a stack from one driver per tier.
    ///
    /// The first driver for a tier answers for that tier.
    #[must_use]
    pub fn new(
        drivers: impl IntoIterator<Item = Box<dyn TierDriver>>,
        availability: TierAvailability,
    ) -> Self {
        Self {
            drivers: drivers.into_iter().collect(),
            availability,
        }
    }

    /// Creates a stack with one empty memory driver per tier.
    #[must_use]
    pub fn memory() -> Self {
        Self::new(
            vec![
                Box::new(MemoryTierDriver::new(Tier::Ram)) as Box<dyn TierDriver>,
                Box::new(MemoryTierDriver::new(Tier::Disk)) as Box<dyn TierDriver>,
                Box::new(MemoryTierDriver::new(Tier::Object)) as Box<dyn TierDriver>,
            ],
            TierAvailability::all_available(),
        )
    }

    /// Returns the injected state of one tier.
    #[must_use]
    pub const fn status(&self, tier: Tier) -> TierStatus {
        self.availability.status(tier)
    }

    /// Sets the injected state of one tier.
    pub fn set_status(&mut self, tier: Tier, status: TierStatus) {
        self.availability.set(tier, status);
    }

    /// Returns the number of stored artifacts in one tier.
    #[must_use]
    pub fn artifact_count(&self, tier: Tier) -> usize {
        self.driver(tier).map_or(0, TierDriver::artifact_count)
    }

    /// Returns true when one tier has a verified copy.
    #[must_use]
    pub fn has_verified_copy(
        &self,
        artifact: &ArtifactId,
        tier: Tier,
        digest: ContentDigest,
    ) -> bool {
        if !self.status(tier).is_available() {
            return false;
        }
        let Some(bytes) = self.driver(tier).and_then(|driver| driver.get(artifact)) else {
            return false;
        };
        ContentDigest::of_bytes(&bytes) == digest
    }

    /// Reads one artifact from one tier.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::TierUnavailable`] when the tier is unavailable.
    /// Returns [`TierError::ArtifactMissing`] when the tier has no artifact.
    pub fn read_from(&self, artifact: &ArtifactId, tier: Tier) -> Result<Vec<u8>, TierError> {
        if !self.status(tier).is_available() {
            return Err(TierError::TierUnavailable);
        }
        self.driver(tier)
            .and_then(|driver| driver.get(artifact))
            .ok_or(TierError::ArtifactMissing)
    }

    /// Reads one artifact from the hottest available tier.
    ///
    /// The function skips an unavailable tier and a tier that misses.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::TierUnavailable`] when no available tier has the
    /// artifact.
    pub fn read_through(&self, artifact: &ArtifactId) -> Result<(Tier, Vec<u8>), TierError> {
        for tier in Tier::ALL {
            if !self.status(*tier).is_available() {
                continue;
            }
            if let Some(bytes) = self.driver(*tier).and_then(|driver| driver.get(artifact)) {
                return Ok((*tier, bytes));
            }
        }
        Err(TierError::TierUnavailable)
    }

    /// Stores one artifact in one tier.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::TierUnavailable`] when the tier does not answer.
    pub fn put(
        &mut self,
        tier: Tier,
        artifact: ArtifactId,
        bytes: Vec<u8>,
    ) -> Result<(), TierError> {
        if !self.status(tier).is_available() {
            return Err(TierError::TierUnavailable);
        }
        let Some(driver) = self.driver_mut(tier) else {
            return Err(TierError::TierUnavailable);
        };
        driver.put(artifact, bytes);
        Ok(())
    }

    /// Returns the artifact length from the hottest available tier.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::TierUnavailable`] when no available tier holds the
    /// artifact.
    pub fn artifact_len(&self, artifact: &ArtifactId) -> Result<usize, TierError> {
        let tier = self.find_copy(artifact)?;
        Ok(self.read_from(artifact, tier)?.len())
    }

    pub(crate) fn find_copy(&self, artifact: &ArtifactId) -> Result<Tier, TierError> {
        let mut saw_unavailable = false;
        for tier in Tier::ALL {
            if !self.status(*tier).is_available() {
                saw_unavailable = true;
                continue;
            }
            if self
                .driver(*tier)
                .and_then(|driver| driver.get(artifact))
                .is_some()
            {
                return Ok(*tier);
            }
        }
        if saw_unavailable {
            Err(TierError::TierUnavailable)
        } else {
            Err(TierError::ArtifactMissing)
        }
    }

    pub(crate) fn driver(&self, tier: Tier) -> Option<&(dyn TierDriver + '_)> {
        for driver in &self.drivers {
            if driver.tier() == tier {
                return Some(&**driver);
            }
        }
        None
    }

    pub(crate) fn driver_mut(&mut self, tier: Tier) -> Option<&mut (dyn TierDriver + '_)> {
        for driver in &mut self.drivers {
            if driver.tier() == tier {
                return Some(&mut **driver);
            }
        }
        None
    }
}
