//! The pinned fact projection and the authorization staleness bound.
//!
//! Staleness is a version distance per fact source. Wall time never enters a
//! verdict. The authority injects the current fact versions and the queue
//! deadline.

use std::collections::BTreeMap;

use oxkv_contracts::version::{
    FactSourceId, FactVersion, Hash32, ProjectionEpoch, ProjectionWatermarks,
};

use crate::codec::{push_bytes, push_tag, push_u64};

/// The domain tag for the projection checksum.
const CHECKSUM_TAG: &[u8] = b"oxkv/projection-checksum/v1";

/// The origin of one projection write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectionWriteOrigin {
    /// The authority applies a projection feed.
    Authority,
    /// A client attempts a direct projection write.
    Client,
}

/// A projection failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProjectionError {
    /// The projection checksum does not match its contents.
    #[error("The projection checksum does not match.")]
    ChecksumFailed,
    /// A fact version is behind the projection watermark.
    #[error("The fact watermark went backwards.")]
    WatermarkRegression,
    /// A client attempted a direct projection write.
    #[error("A client cannot write a projection.")]
    ClientWriteRejected,
}

impl ProjectionError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ChecksumFailed => "projection.checksum_failed",
            Self::WatermarkRegression => "projection.watermark_regression",
            Self::ClientWriteRejected => "projection.client_write_rejected",
        }
    }
}

/// A versioned fact projection.
///
/// The checksum covers the epoch and every watermark. A projection with a
/// broken checksum fails closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactProjection {
    epoch: ProjectionEpoch,
    watermarks: ProjectionWatermarks,
    checksum: Hash32,
}

impl FactProjection {
    /// Creates a projection and computes its checksum.
    #[must_use]
    pub fn new(epoch: ProjectionEpoch, watermarks: ProjectionWatermarks) -> Self {
        let checksum = Self::compute_checksum(epoch, &watermarks);
        Self {
            epoch,
            watermarks,
            checksum,
        }
    }

    /// Creates a projection from explicit parts.
    ///
    /// Use this constructor to load a stored projection. A mismatched checksum
    /// fails at [`Self::verify`].
    #[must_use]
    pub fn from_parts(
        epoch: ProjectionEpoch,
        watermarks: ProjectionWatermarks,
        checksum: Hash32,
    ) -> Self {
        Self {
            epoch,
            watermarks,
            checksum,
        }
    }

    /// Returns the projection epoch.
    #[must_use]
    pub const fn epoch(&self) -> ProjectionEpoch {
        self.epoch
    }

    /// Returns the projection watermarks.
    #[must_use]
    pub const fn watermarks(&self) -> &ProjectionWatermarks {
        &self.watermarks
    }

    /// Returns the stored checksum.
    #[must_use]
    pub const fn checksum(&self) -> Hash32 {
        self.checksum
    }

    /// Checks the stored checksum against the contents.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::ChecksumFailed`] when the checksum does not
    /// match the epoch and watermarks.
    pub fn verify(&self) -> Result<(), ProjectionError> {
        let computed = Self::compute_checksum(self.epoch, &self.watermarks);
        if computed == self.checksum {
            Ok(())
        } else {
            Err(ProjectionError::ChecksumFailed)
        }
    }

    /// Admits one write from the given origin.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::ClientWriteRejected`] for a client origin.
    /// Returns [`ProjectionError::ChecksumFailed`] for a broken projection.
    pub fn admit_write(&self, origin: ProjectionWriteOrigin) -> Result<(), ProjectionError> {
        match origin {
            ProjectionWriteOrigin::Authority => self.verify(),
            ProjectionWriteOrigin::Client => Err(ProjectionError::ClientWriteRejected),
        }
    }

    /// Computes the checksum of an epoch and its watermarks.
    fn compute_checksum(epoch: ProjectionEpoch, watermarks: &ProjectionWatermarks) -> Hash32 {
        let mut buffer = Vec::new();
        push_tag(&mut buffer, CHECKSUM_TAG);
        push_u64(&mut buffer, epoch.get());
        push_u64(
            &mut buffer,
            u64::try_from(watermarks.iter().count()).unwrap_or(u64::MAX),
        );
        for (source, version) in watermarks.iter() {
            push_bytes(&mut buffer, source.as_str().as_bytes());
            push_u64(&mut buffer, version.get());
        }
        Hash32::of(&buffer)
    }
}

/// The maximum version distance for each fact source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalenessBound {
    default_max_distance: u64,
    per_source: BTreeMap<FactSourceId, u64>,
}

impl StalenessBound {
    /// Creates one bound for every fact source.
    #[must_use]
    pub fn uniform(max_distance: u64) -> Self {
        Self {
            default_max_distance: max_distance,
            per_source: BTreeMap::new(),
        }
    }

    /// Creates a bound with a default and source-specific entries.
    #[must_use]
    pub fn per_source(
        default_max_distance: u64,
        entries: impl IntoIterator<Item = (FactSourceId, u64)>,
    ) -> Self {
        Self {
            default_max_distance,
            per_source: entries.into_iter().collect(),
        }
    }

    /// Sets one source-specific bound.
    pub fn set_source(&mut self, source: FactSourceId, max_distance: u64) {
        self.per_source.insert(source, max_distance);
    }

    /// Returns the bound for one fact source.
    #[must_use]
    pub fn max_for(&self, source: &FactSourceId) -> u64 {
        self.per_source
            .get(source)
            .copied()
            .unwrap_or(self.default_max_distance)
    }
}

/// The authorization mode of one rule or table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthorizationMode {
    /// A stale write stays queued until the deadline.
    Default,
    /// A stale write rejects at once.
    Strict,
}

/// The authorization mode and staleness bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationPolicy {
    mode: AuthorizationMode,
    bound: StalenessBound,
}

impl AuthorizationPolicy {
    /// Creates an authorization policy.
    #[must_use]
    pub const fn new(mode: AuthorizationMode, bound: StalenessBound) -> Self {
        Self { mode, bound }
    }

    /// Creates the default behavior.
    #[must_use]
    pub const fn default_mode(bound: StalenessBound) -> Self {
        Self::new(AuthorizationMode::Default, bound)
    }

    /// Creates the strict behavior.
    #[must_use]
    pub const fn strict(bound: StalenessBound) -> Self {
        Self::new(AuthorizationMode::Strict, bound)
    }

    /// Returns the authorization mode.
    #[must_use]
    pub const fn mode(&self) -> AuthorizationMode {
        self.mode
    }

    /// Returns the staleness bound.
    #[must_use]
    pub const fn bound(&self) -> &StalenessBound {
        &self.bound
    }
}

/// The measured distance for one fact source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactDistance {
    /// The fact source.
    pub source: FactSourceId,
    /// The projection watermark.
    pub projected: FactVersion,
    /// The current version. A value of `None` means the source is incomplete.
    pub current: Option<FactVersion>,
    /// The version distance. A value of `None` means the distance is unknown.
    pub distance: Option<u64>,
    /// The bound for this source.
    pub bound: u64,
}

/// The measured staleness of one projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalenessReport {
    distances: Vec<FactDistance>,
    max_distance: Option<u64>,
    over_bound: bool,
    regression: bool,
    incomplete: bool,
}

impl StalenessReport {
    /// Measures the distance for each projected fact source.
    #[must_use]
    pub fn evaluate(
        projection: &FactProjection,
        current: &ProjectionWatermarks,
        bound: &StalenessBound,
    ) -> Self {
        let mut distances = Vec::new();
        let mut max_distance: Option<u64> = None;
        let mut over_bound = false;
        let mut regression = false;
        let mut incomplete = false;

        for (source, projected) in projection.watermarks().iter() {
            let current_version = current.get(source);
            let allowed = bound.max_for(source);
            let distance = match current_version {
                Some(version) if version >= projected => Some(version.get() - projected.get()),
                Some(_) => {
                    regression = true;
                    None
                }
                None => {
                    incomplete = true;
                    None
                }
            };
            if distance.is_none_or(|value| value > allowed) {
                over_bound = true;
            }
            if let Some(value) = distance {
                max_distance =
                    Some(max_distance.map_or(value, |current_max| current_max.max(value)));
            }
            distances.push(FactDistance {
                source: source.clone(),
                projected,
                current: current_version,
                distance,
                bound: allowed,
            });
        }

        Self {
            distances,
            max_distance,
            over_bound,
            regression,
            incomplete,
        }
    }

    /// Returns the distance for each source.
    #[must_use]
    pub fn distances(&self) -> &[FactDistance] {
        &self.distances
    }

    /// Returns the largest known distance.
    #[must_use]
    pub const fn max_distance(&self) -> Option<u64> {
        self.max_distance
    }

    /// Returns true when one source is over its bound.
    #[must_use]
    pub const fn over_bound(&self) -> bool {
        self.over_bound
    }

    /// Returns true when one source went backwards.
    #[must_use]
    pub const fn regression(&self) -> bool {
        self.regression
    }

    /// Returns true when one projected source is absent.
    #[must_use]
    pub const fn incomplete(&self) -> bool {
        self.incomplete
    }
}
