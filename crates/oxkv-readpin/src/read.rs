//! The read contract: pin modes, admission, staleness, and read-your-writes.
//!
//! A read resolves against a replica local pin or an authority read pin. The
//! default pin mode is `AtLeast`. The default read mode is bounded staleness.
//! The default result mode is exact.
//!
//! The replica serves only at or below its applied version vector. The applied
//! vector is the local commit ceiling. A read never consults the authority for
//! the answer. It uses the authority only for the staleness measure and the
//! compaction floor.

use oxkv_contracts::version::{CommitVersion, ScopeVersionVector, ShardId, VersionVector};

use crate::bound::{StalenessBound, StalenessMeasure};
use crate::pin::{AuthorityReadPin, PinError, ReplicaLocalPin};
use crate::session::SessionToken;
use crate::time::Timestamp;

/// The read mode of one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionMode {
    /// Serve locally within a declared bound. Local visibility may lag a
    /// terminal receipt.
    #[default]
    BoundedStaleness,
    /// Block or fail when the replica cannot apply the session minimum.
    ReadYourWrites,
}

impl SessionMode {
    /// Returns true for read-your-writes.
    #[must_use]
    pub const fn is_read_your_writes(self) -> bool {
        matches!(self, Self::ReadYourWrites)
    }
}

/// The pin mode of one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PinMode {
    /// Serve the pin version or a newer version.
    #[default]
    AtLeast,
    /// Serve exactly the pin version.
    Exactly,
}

/// The result mode of one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResultMode {
    /// Every targeted shard must serve the pin.
    #[default]
    Exact,
    /// The read may return a coverage descriptor with missing shards.
    Coverage,
}

/// The hydration state of a replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HydrationState {
    /// The replica is cold. It reports its page progress.
    Cold {
        /// The number of verified pages.
        verified_pages: u64,
        /// The target page count.
        target_pages: u64,
    },
    /// The replica is hydrated.
    Hydrated,
}

impl HydrationState {
    /// Returns true when the replica is hydrated.
    #[must_use]
    pub const fn is_hydrated(self) -> bool {
        matches!(self, Self::Hydrated)
    }
}

/// The page residency of a replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Residency {
    /// The requested page is resident.
    Resident,
    /// The requested page is not resident.
    NotResident,
}

impl Residency {
    /// Returns true when the page is resident.
    #[must_use]
    pub const fn is_resident(self) -> bool {
        matches!(self, Self::Resident)
    }
}

/// The named failure of one read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// The read pin expired or is unreadable.
    #[error(transparent)]
    Pin(#[from] PinError),
    /// The replica has not applied the required version vector minimum.
    #[error("The replica has not applied the session minimum.")]
    BehindCommit {
        /// The version vector minimum.
        minimum: VersionVector,
        /// The applied version vector.
        applied: VersionVector,
    },
    /// The replica is cold.
    #[error("The replica is not hydrated.")]
    NotHydrated {
        /// The number of verified pages.
        verified_pages: u64,
        /// The target page count.
        target_pages: u64,
    },
    /// The requested page is not resident.
    #[error("The page is not resident on the replica.")]
    NotResident,
    /// The read cannot meet its staleness bound.
    #[error("The read is stale.")]
    Stale {
        /// The declared bound.
        bound: StalenessBound,
        /// The measured staleness.
        measured: StalenessMeasure,
    },
}

impl ReadError {
    /// Returns the canonical error code.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Pin(error) => error.code(),
            Self::BehindCommit { .. } => "read.behind_commit",
            Self::NotHydrated { .. } => "read.not_hydrated",
            Self::NotResident => "read.not_resident",
            Self::Stale { .. } => "read.stale",
        }
    }

    /// Returns the measured staleness of a stale read.
    #[must_use]
    pub const fn measured(&self) -> Option<StalenessMeasure> {
        match self {
            Self::Stale { measured, .. } => Some(*measured),
            _ => None,
        }
    }
}

/// A blocked read-your-writes request.
///
/// The caller can apply diffs and retry. The caller can also close the call
/// with [`BlockedRead::into_error`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedRead {
    minimum: VersionVector,
    applied: VersionVector,
}

impl BlockedRead {
    /// Returns the version vector minimum.
    #[must_use]
    pub const fn minimum(&self) -> &VersionVector {
        &self.minimum
    }

    /// Returns the applied version vector.
    #[must_use]
    pub const fn applied(&self) -> &VersionVector {
        &self.applied
    }

    /// Returns the canonical error code.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn code(&self) -> &'static str {
        "read.behind_commit"
    }

    /// Converts the block into the named read error.
    #[must_use]
    pub fn into_error(self) -> ReadError {
        ReadError::BehindCommit {
            minimum: self.minimum,
            applied: self.applied,
        }
    }
}

/// The coverage descriptor of a coverage result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageDescriptor {
    answered: Vec<ShardId>,
    missing: Vec<ShardId>,
    served: VersionVector,
}

impl CoverageDescriptor {
    /// Returns the answered shards.
    #[must_use]
    pub fn answered(&self) -> &[ShardId] {
        &self.answered
    }

    /// Returns the missing shards.
    #[must_use]
    pub fn missing(&self) -> &[ShardId] {
        &self.missing
    }

    /// Returns the served version vector.
    #[must_use]
    pub const fn served(&self) -> &VersionVector {
        &self.served
    }

    /// Returns true when every targeted shard answered.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// A served read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResponse {
    served: VersionVector,
    staleness: StalenessMeasure,
    pin_mode: PinMode,
    coverage: Option<CoverageDescriptor>,
}

impl ReadResponse {
    fn new(
        served: VersionVector,
        staleness: StalenessMeasure,
        pin_mode: PinMode,
        coverage: Option<CoverageDescriptor>,
    ) -> Self {
        Self {
            served,
            staleness,
            pin_mode,
            coverage,
        }
    }

    /// Returns the served version vector.
    #[must_use]
    pub const fn served(&self) -> &VersionVector {
        &self.served
    }

    /// Returns the staleness measure.
    #[must_use]
    pub const fn staleness(&self) -> StalenessMeasure {
        self.staleness
    }

    /// Returns the pin mode of the read.
    #[must_use]
    pub const fn pin_mode(&self) -> PinMode {
        self.pin_mode
    }

    /// Returns the coverage descriptor of a coverage result.
    #[must_use]
    pub const fn coverage(&self) -> Option<&CoverageDescriptor> {
        self.coverage.as_ref()
    }

    /// Returns true when the result is exact.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        self.coverage.is_none()
    }

    /// Returns the served vector as a scope version vector.
    ///
    /// Essay 19 can put this value in a `SearchCursor` to make a search and a
    /// read share one pin.
    #[must_use]
    pub fn search_version_vector(&self) -> ScopeVersionVector {
        ScopeVersionVector::from_vector(self.served.clone())
    }
}

/// The outcome of one read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    /// The read served data.
    Served(ReadResponse),
    /// The replica must apply the session minimum before it can serve.
    Blocked(BlockedRead),
    /// The read failed with a named error and no data.
    Failed(ReadError),
}

impl ReadOutcome {
    /// Returns the served response.
    #[must_use]
    pub const fn served(&self) -> Option<&ReadResponse> {
        match self {
            Self::Served(response) => Some(response),
            Self::Blocked(_) | Self::Failed(_) => None,
        }
    }

    /// Returns the blocked read.
    #[must_use]
    pub const fn blocked(&self) -> Option<&BlockedRead> {
        match self {
            Self::Blocked(blocked) => Some(blocked),
            Self::Served(_) | Self::Failed(_) => None,
        }
    }

    /// Returns the named error.
    #[must_use]
    pub const fn error(&self) -> Option<&ReadError> {
        match self {
            Self::Failed(error) => Some(error),
            Self::Served(_) | Self::Blocked(_) => None,
        }
    }

    /// Returns the canonical error code of a blocked or failed read.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::Served(_) => None,
            Self::Blocked(blocked) => Some(blocked.code()),
            Self::Failed(error) => Some(error.code()),
        }
    }

    /// Returns true when the read served data.
    #[must_use]
    pub const fn is_served(&self) -> bool {
        matches!(self, Self::Served(_))
    }

    /// Converts the outcome into a result.
    ///
    /// # Errors
    ///
    /// Returns the named error of a failed read. Returns
    /// [`ReadError::BehindCommit`] for a blocked read.
    pub fn into_result(self) -> Result<ReadResponse, ReadError> {
        match self {
            Self::Served(response) => Ok(response),
            Self::Blocked(blocked) => Err(blocked.into_error()),
            Self::Failed(error) => Err(error),
        }
    }
}

/// The pin of one read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadTarget {
    /// A bounded authority pin.
    Authority(AuthorityReadPin),
    /// A local replica pin.
    Local(ReplicaLocalPin),
}

impl ReadTarget {
    /// Returns the pinned version vector.
    #[must_use]
    pub const fn version_vector(&self) -> &ScopeVersionVector {
        match self {
            Self::Authority(pin) => pin.version_vector(),
            Self::Local(pin) => pin.version_vector(),
        }
    }

    /// Checks the pin against the current time and compaction floor.
    ///
    /// A local pin never expires.
    ///
    /// # Errors
    ///
    /// Returns [`PinError`] when an authority pin expired or is unreadable.
    pub fn check(&self, now: Timestamp, authority_floor: &VersionVector) -> Result<(), PinError> {
        match self {
            Self::Authority(pin) => pin.check(now, authority_floor),
            Self::Local(_) => Ok(()),
        }
    }
}

/// One read request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    /// The read pin.
    pub target: ReadTarget,
    /// The pin mode.
    pub pin_mode: PinMode,
    /// The session read mode.
    pub mode: SessionMode,
    /// The declared staleness bound.
    pub bound: StalenessBound,
    /// The result mode.
    pub result_mode: ResultMode,
    /// Require the target page to be resident.
    pub requires_residency: bool,
}

impl ReadRequest {
    /// Creates a bounded-staleness read with an exact result and no residency
    /// requirement.
    #[must_use]
    pub const fn new(target: ReadTarget, bound: StalenessBound) -> Self {
        Self {
            target,
            pin_mode: PinMode::AtLeast,
            mode: SessionMode::BoundedStaleness,
            bound,
            result_mode: ResultMode::Exact,
            requires_residency: false,
        }
    }

    /// Sets the pin mode.
    #[must_use]
    pub const fn with_pin_mode(mut self, pin_mode: PinMode) -> Self {
        self.pin_mode = pin_mode;
        self
    }

    /// Sets the session read mode.
    #[must_use]
    pub const fn with_mode(mut self, mode: SessionMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the result mode.
    #[must_use]
    pub const fn with_result_mode(mut self, result_mode: ResultMode) -> Self {
        self.result_mode = result_mode;
        self
    }

    /// Sets the residency requirement.
    #[must_use]
    pub const fn with_residency_requirement(mut self, required: bool) -> Self {
        self.requires_residency = required;
        self
    }
}

/// The read view of one replica.
///
/// The applied vector is the local commit ceiling. The high-water mark is
/// monotone and persists across restart. A read never serves below the
/// session minimum and never above the applied vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaView {
    applied: VersionVector,
    high_water: VersionVector,
    last_contact: Timestamp,
    hydration: HydrationState,
    residency: Residency,
}

impl ReplicaView {
    /// Creates a replica read view.
    #[must_use]
    pub fn new(
        applied: VersionVector,
        last_contact: Timestamp,
        hydration: HydrationState,
        residency: Residency,
    ) -> Self {
        Self {
            high_water: applied.clone(),
            applied,
            last_contact,
            hydration,
            residency,
        }
    }

    /// Returns the applied version vector.
    #[must_use]
    pub const fn applied(&self) -> &VersionVector {
        &self.applied
    }

    /// Returns the persisted high-water mark.
    #[must_use]
    pub const fn high_water(&self) -> &VersionVector {
        &self.high_water
    }

    /// Returns the last contact time with the authority.
    #[must_use]
    pub const fn last_contact(&self) -> Timestamp {
        self.last_contact
    }

    /// Returns the hydration state.
    #[must_use]
    pub const fn hydration(&self) -> HydrationState {
        self.hydration
    }

    /// Returns the residency of the target page.
    #[must_use]
    pub const fn residency(&self) -> Residency {
        self.residency
    }

    /// Returns true when the replica is hydrated.
    #[must_use]
    pub const fn is_hydrated(&self) -> bool {
        self.hydration.is_hydrated()
    }

    /// Returns the local replica pin.
    #[must_use]
    pub fn local_pin(&self) -> ReplicaLocalPin {
        ReplicaLocalPin::from_applied(self.applied.clone())
    }

    /// Applies an observed version vector.
    ///
    /// The applied vector and the high-water mark never decrease. The call
    /// returns true when the applied vector changed.
    pub fn apply(&mut self, observed: &VersionVector) -> bool {
        let mut changed = false;
        for (shard_id, observed_version) in observed.iter() {
            if self
                .applied
                .get(shard_id)
                .is_none_or(|version| version < observed_version)
            {
                self.applied.set(shard_id.clone(), observed_version);
                changed = true;
            }
            if self
                .high_water
                .get(shard_id)
                .is_none_or(|version| version < observed_version)
            {
                self.high_water.set(shard_id.clone(), observed_version);
            }
        }
        changed
    }

    /// Records authority contact at the given time.
    pub const fn record_contact(&mut self, now: Timestamp) {
        self.last_contact = now;
    }

    /// Sets the hydration state.
    pub const fn set_hydration(&mut self, hydration: HydrationState) {
        self.hydration = hydration;
    }

    /// Sets the page residency.
    pub const fn set_residency(&mut self, residency: Residency) {
        self.residency = residency;
    }

    /// Returns true when the applied vector entails the minimum.
    #[must_use]
    pub fn meets(&self, minimum: &VersionVector) -> bool {
        self.applied.entails(minimum)
    }
}

/// The authority state that a read needs.
///
/// The watermark measures version lag. The pin floor measures pin readability.
/// The time measures replication silence. None of these values require a read
/// from the leader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityView {
    watermark: VersionVector,
    pin_floor: VersionVector,
    now: Timestamp,
}

impl AuthorityView {
    /// Creates an authority view.
    #[must_use]
    pub const fn new(watermark: VersionVector, pin_floor: VersionVector, now: Timestamp) -> Self {
        Self {
            watermark,
            pin_floor,
            now,
        }
    }

    /// Creates an offline authority view.
    ///
    /// The watermark equals the local applied vector, so the version lag is
    /// zero. The pin floor is empty. The silence measure reports the offline
    /// duration.
    #[must_use]
    pub fn offline(applied: &VersionVector, now: Timestamp) -> Self {
        Self {
            watermark: applied.clone(),
            pin_floor: VersionVector::new(),
            now,
        }
    }

    /// Returns the authority watermark.
    #[must_use]
    pub const fn watermark(&self) -> &VersionVector {
        &self.watermark
    }

    /// Returns the pin compaction floor.
    #[must_use]
    pub const fn pin_floor(&self) -> &VersionVector {
        &self.pin_floor
    }

    /// Returns the injected time of the read.
    #[must_use]
    pub const fn now(&self) -> Timestamp {
        self.now
    }
}

/// Evaluates one read against a replica and the authority view.
///
/// The function is pure. It reads no clock, draws no randomness, and performs
/// no I/O. The session minimum applies to the shards in the read pin. A shard
/// outside the pin does not block the read.
#[must_use]
pub fn evaluate(
    request: &ReadRequest,
    session: &SessionToken,
    replica: &ReplicaView,
    authority: &AuthorityView,
) -> ReadOutcome {
    if let Err(error) = request.target.check(authority.now(), authority.pin_floor()) {
        return ReadOutcome::Failed(ReadError::Pin(error));
    }
    if let HydrationState::Cold {
        verified_pages,
        target_pages,
    } = replica.hydration()
    {
        return ReadOutcome::Failed(ReadError::NotHydrated {
            verified_pages,
            target_pages,
        });
    }
    if request.requires_residency && !replica.residency().is_resident() {
        return ReadOutcome::Failed(ReadError::NotResident);
    }
    let pin = request.target.version_vector().as_vector();
    let required = session_requirement(session.minimum(), pin);
    if request.mode.is_read_your_writes() && !replica.meets(&required) {
        return blocked(&required, replica.applied());
    }
    match request.pin_mode {
        PinMode::Exactly => evaluate_exact(request, replica, authority, pin, &required),
        PinMode::AtLeast => evaluate_at_least(request, replica, authority, pin, &required),
    }
}

fn session_requirement(minimum: &VersionVector, pin: &VersionVector) -> VersionVector {
    let mut required = VersionVector::new();
    for (shard_id, commit_version) in minimum.iter() {
        if pin.get(shard_id).is_some() {
            required.set(shard_id.clone(), commit_version);
        }
    }
    required
}

fn evaluate_exact(
    request: &ReadRequest,
    replica: &ReplicaView,
    authority: &AuthorityView,
    pin: &VersionVector,
    required: &VersionVector,
) -> ReadOutcome {
    if !pin.entails(required) {
        return ReadOutcome::Failed(ReadError::BehindCommit {
            minimum: required.clone(),
            applied: pin.clone(),
        });
    }
    if !replica.applied().entails(pin) {
        return behind(request.mode, pin, replica.applied());
    }
    let measured = measure(pin, authority, replica);
    if !request.bound.permits(measured) {
        return ReadOutcome::Failed(ReadError::Stale {
            bound: request.bound,
            measured,
        });
    }
    ReadOutcome::Served(ReadResponse::new(
        pin.clone(),
        measured,
        PinMode::Exactly,
        None,
    ))
}

fn evaluate_at_least(
    request: &ReadRequest,
    replica: &ReplicaView,
    authority: &AuthorityView,
    pin: &VersionVector,
    required: &VersionVector,
) -> ReadOutcome {
    if !replica.meets(required) {
        return behind(request.mode, required, replica.applied());
    }
    if request.result_mode == ResultMode::Exact && !replica.applied().entails(pin) {
        return behind(request.mode, pin, replica.applied());
    }
    let served = replica.applied().clone();
    let measured = measure(&served, authority, replica);
    if !request.bound.permits(measured) {
        return ReadOutcome::Failed(ReadError::Stale {
            bound: request.bound,
            measured,
        });
    }
    let coverage = match request.result_mode {
        ResultMode::Exact => None,
        ResultMode::Coverage => Some(coverage(pin, &served)),
    };
    ReadOutcome::Served(ReadResponse::new(
        served,
        measured,
        PinMode::AtLeast,
        coverage,
    ))
}

fn behind(mode: SessionMode, minimum: &VersionVector, applied: &VersionVector) -> ReadOutcome {
    if mode.is_read_your_writes() {
        blocked(minimum, applied)
    } else {
        ReadOutcome::Failed(ReadError::BehindCommit {
            minimum: minimum.clone(),
            applied: applied.clone(),
        })
    }
}

fn blocked(minimum: &VersionVector, applied: &VersionVector) -> ReadOutcome {
    ReadOutcome::Blocked(BlockedRead {
        minimum: minimum.clone(),
        applied: applied.clone(),
    })
}

fn measure(
    served: &VersionVector,
    authority: &AuthorityView,
    replica: &ReplicaView,
) -> StalenessMeasure {
    let version_lag = authority
        .watermark()
        .iter()
        .map(|(shard_id, watermark)| {
            watermark
                .get()
                .saturating_sub(served.get(shard_id).map_or(0, CommitVersion::get))
        })
        .max()
        .unwrap_or(0);
    let silence = authority.now().duration_since(replica.last_contact());
    StalenessMeasure::new(version_lag, silence)
}

fn coverage(pin: &VersionVector, served: &VersionVector) -> CoverageDescriptor {
    let mut answered = Vec::new();
    let mut missing = Vec::new();
    for (shard_id, pin_version) in pin.iter() {
        match served.get(shard_id) {
            Some(served_version) if served_version >= pin_version => {
                answered.push(shard_id.clone());
            }
            _ => missing.push(shard_id.clone()),
        }
    }
    CoverageDescriptor {
        answered,
        missing,
        served: served.clone(),
    }
}
