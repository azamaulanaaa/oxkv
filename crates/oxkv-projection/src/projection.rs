//! The logical projection state.
//!
//! A projection is a versioned copy of facts inside the consuming shard. It
//! carries an epoch and one watermark per fact source. It never accepts a
//! client write and it is never a second truth.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::ProjectionCursor;
use oxkv_contracts::version::{
    FactVersion, Hash32, PolicyVersion, ProjectionEpoch, ProjectionWatermarks,
};

use crate::epoch::{PolicyCohort, ProjectionEpochState};
use crate::error::ProjectionError;
use crate::fact::{FactDeclaration, FactId, FactSet, FactVersionDistance, StoredFact};
use crate::feed::{FactChange, ProjectionDiff, ProjectionSnapshot};
use crate::invalidate::{InvalidationCause, InvalidationRecord};
use crate::staleness::{
    AuthorizationMode, AuthorizationReport, FactSourceVersions, QueueDeadline, StalenessReport,
};
use crate::verdict::{Decision, FactRead, RuleId, Verdict};

/// The result of one projection apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApplyOutcome {
    /// The frame changed the projection.
    Applied,
    /// The frame was a duplicate. The projection did not change.
    Duplicate,
}

/// The limits of one rule read set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReadSetLimits {
    max_facts: u32,
    max_sources: u32,
}

impl ReadSetLimits {
    /// Creates read-set limits.
    #[must_use]
    pub const fn new(max_facts: u32, max_sources: u32) -> Self {
        Self {
            max_facts,
            max_sources,
        }
    }

    /// Returns the maximum number of facts.
    #[must_use]
    pub const fn max_facts(self) -> u32 {
        self.max_facts
    }

    /// Returns the maximum number of fact sources.
    #[must_use]
    pub const fn max_sources(self) -> u32 {
        self.max_sources
    }
}

/// A checked set of facts that one rule reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadSet {
    facts: Vec<FactId>,
}

impl ReadSet {
    /// Returns the facts in canonical identity order.
    #[must_use]
    pub fn facts(&self) -> &[FactId] {
        &self.facts
    }

    /// Returns the number of facts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.facts.len()
    }

    /// Returns true when the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }
}

/// One projection content generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProjectionContent {
    watermarks: ProjectionWatermarks,
    facts: BTreeMap<FactId, StoredFact>,
}

/// The state of one projection.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectionStatus {
    /// The projection holds trusted facts.
    Live(ProjectionContent),
    /// The projection failed closed. It waits for a rebuild.
    Invalidated { cause: InvalidationCause },
}

/// One versioned fact projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    fact_set: FactSet,
    epoch: ProjectionEpochState,
    status: ProjectionStatus,
}

impl Projection {
    /// Creates an empty live projection.
    #[must_use]
    pub fn new(fact_set: FactSet, epoch: ProjectionEpochState) -> Self {
        Self {
            fact_set,
            epoch,
            status: ProjectionStatus::Live(ProjectionContent::default()),
        }
    }

    /// Creates an empty live projection for one policy cohort.
    #[must_use]
    pub fn for_cohort(fact_set: FactSet, cohort: &PolicyCohort) -> Self {
        Self::new(fact_set, cohort.epoch_state())
    }

    /// Returns the fact set.
    #[must_use]
    pub const fn fact_set(&self) -> &FactSet {
        &self.fact_set
    }

    /// Returns the epoch identity.
    #[must_use]
    pub const fn epoch_state(&self) -> ProjectionEpochState {
        self.epoch
    }

    /// Returns the epoch generation.
    #[must_use]
    pub const fn epoch(&self) -> ProjectionEpoch {
        self.epoch.generation()
    }

    /// Returns the policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.epoch.policy_version()
    }

    /// Returns true when the projection is live.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        matches!(self.status, ProjectionStatus::Live(_))
    }

    /// Returns the invalidation cause when the projection failed closed.
    #[must_use]
    pub const fn invalidation_cause(&self) -> Option<InvalidationCause> {
        match self.status {
            ProjectionStatus::Invalidated { cause } => Some(cause),
            ProjectionStatus::Live(_) => None,
        }
    }

    /// Returns the live content.
    fn content(&self) -> Result<&ProjectionContent, ProjectionError> {
        match &self.status {
            ProjectionStatus::Live(content) => Ok(content),
            ProjectionStatus::Invalidated { .. } => Err(ProjectionError::EpochInvalid),
        }
    }

    /// Returns the watermarks.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn watermarks(&self) -> Result<&ProjectionWatermarks, ProjectionError> {
        Ok(&self.content()?.watermarks)
    }

    /// Returns one stored fact.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn fact(&self, fact_id: &FactId) -> Result<Option<&StoredFact>, ProjectionError> {
        Ok(self.content()?.facts.get(fact_id))
    }

    /// Returns every stored fact in canonical identity order.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn facts(&self) -> Result<Vec<&StoredFact>, ProjectionError> {
        Ok(self.content()?.facts.values().collect())
    }

    /// Rejects a client write.
    ///
    /// # Errors
    ///
    /// Always returns [`ProjectionError::ClientWriteRejected`].
    #[allow(clippy::unused_self)]
    pub fn apply_client_write(&mut self) -> Result<(), ProjectionError> {
        Err(ProjectionError::ClientWriteRejected)
    }

    /// Invalidates the projection and bumps the epoch.
    ///
    /// The projection holds no facts after this call. It fails closed until a
    /// snapshot rebuilds it.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochInvalid`] when the projection already
    /// failed closed. Returns [`ProjectionError::EpochOverflow`] when the
    /// epoch counter is full.
    pub fn invalidate(
        &mut self,
        cause: InvalidationCause,
    ) -> Result<InvalidationRecord, ProjectionError> {
        self.content()?;
        let previous_epoch = self.epoch;
        let new_epoch = previous_epoch.bump()?;
        self.epoch = new_epoch;
        self.status = ProjectionStatus::Invalidated { cause };
        Ok(InvalidationRecord {
            cause,
            previous_epoch,
            new_epoch,
        })
    }

    /// Rebinds the projection to a new policy version without a rebuild.
    ///
    /// A predicate change that reads the same facts with the same schema uses
    /// this method.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn rebind_policy(&mut self, policy_version: PolicyVersion) -> Result<(), ProjectionError> {
        self.content()?;
        self.epoch = self.epoch.with_policy_version(policy_version);
        Ok(())
    }

    /// Rebuilds an invalidated projection under a new fact set.
    ///
    /// The fact set may add or remove facts after an invalidation. The
    /// snapshot epoch must be at or above the invalidated epoch.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochInvalid`] when the projection is still
    /// live. Returns every error of [`Projection::apply_snapshot`] when the
    /// snapshot is invalid.
    pub fn rebuild(
        &mut self,
        fact_set: FactSet,
        snapshot: &ProjectionSnapshot,
    ) -> Result<ApplyOutcome, ProjectionError> {
        if self.is_valid() {
            return Err(ProjectionError::EpochInvalid);
        }
        let previous = self.fact_set.clone();
        self.fact_set = fact_set;
        match self.apply_snapshot(snapshot) {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.fact_set = previous;
                Err(error)
            }
        }
    }

    /// Applies one snapshot atomically.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::ChecksumFailed`] when the snapshot checksum
    /// fails. Returns [`ProjectionError::FactSetMismatch`] when the snapshot
    /// names another fact set. Returns [`ProjectionError::WatermarkRegression`]
    /// when a source moves backward. Returns [`ProjectionError::EpochInvalid`]
    /// when the snapshot epoch is stale.
    pub fn apply_snapshot(
        &mut self,
        snapshot: &ProjectionSnapshot,
    ) -> Result<ApplyOutcome, ProjectionError> {
        snapshot.verify()?;
        if snapshot.fact_set_digest() != self.fact_set.digest() {
            return Err(ProjectionError::FactSetMismatch);
        }
        let next = Self::content_from_snapshot(&self.fact_set, snapshot)?;
        let current = match &self.status {
            ProjectionStatus::Live(content) => Some(content),
            ProjectionStatus::Invalidated { .. } => None,
        };
        if let Some(current) = current {
            if snapshot.epoch() < self.epoch.generation() {
                return Err(ProjectionError::EpochInvalid);
            }
            if snapshot.epoch() == self.epoch.generation()
                && snapshot.policy_version() != self.epoch.policy_version()
            {
                return Err(ProjectionError::EpochInvalid);
            }
            Self::check_snapshot_watermarks(current, snapshot)?;
        } else if snapshot.epoch() < self.epoch.generation() {
            return Err(ProjectionError::EpochInvalid);
        }
        let outcome = if current.is_some_and(|content| content == &next)
            && snapshot.epoch() == self.epoch.generation()
        {
            ApplyOutcome::Duplicate
        } else {
            ApplyOutcome::Applied
        };
        self.epoch = ProjectionEpochState::new(snapshot.epoch(), snapshot.policy_version());
        self.status = ProjectionStatus::Live(next);
        Ok(outcome)
    }

    /// Applies one ordered diff atomically.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::ChecksumFailed`] when the diff checksum
    /// fails. Returns [`ProjectionError::EpochInvalid`] when the diff names
    /// another epoch or an undeclared change. Returns
    /// [`ProjectionError::WatermarkRegression`] when a fact version or source
    /// watermark moves backward.
    pub fn apply_diff(&mut self, diff: &ProjectionDiff) -> Result<ApplyOutcome, ProjectionError> {
        diff.verify()?;
        let current = self.content()?.clone();
        if diff.epoch() != self.epoch.generation() {
            return Err(ProjectionError::EpochInvalid);
        }
        if !self
            .fact_set
            .iter()
            .any(|(_, declaration)| declaration.source_id() == *diff.source())
        {
            return Err(ProjectionError::FactSetMismatch);
        }
        let mut next = current.clone();
        let current_watermark = next
            .watermarks
            .get(diff.source())
            .unwrap_or_else(|| FactVersion::new(0));
        if diff.watermark() < current_watermark {
            return Err(ProjectionError::WatermarkRegression {
                fact_source: diff.source().clone(),
                current: current_watermark,
                incoming: diff.watermark(),
            });
        }
        for change in diff.changes() {
            Self::apply_change(&self.fact_set, &mut next, diff, change)?;
        }
        next.watermarks.set(diff.source().clone(), diff.watermark());
        let outcome = if next == current {
            ApplyOutcome::Duplicate
        } else {
            ApplyOutcome::Applied
        };
        self.status = ProjectionStatus::Live(next);
        Ok(outcome)
    }

    /// Validates a read set against the declaration and the limits.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::UnknownFact`] for an undeclared fact.
    /// Returns [`ProjectionError::ReadSetExceeded`] above the fact cap.
    /// Returns [`ProjectionError::FanInExceeded`] above a source cap.
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn read_set(
        &self,
        facts: &[FactId],
        limits: ReadSetLimits,
    ) -> Result<ReadSet, ProjectionError> {
        self.content()?;
        let mut unique = BTreeSet::new();
        for fact_id in facts {
            if self.fact_set.get(fact_id).is_none() {
                return Err(ProjectionError::UnknownFact {
                    fact_id: fact_id.clone(),
                });
            }
            unique.insert(fact_id.clone());
        }
        let facts_count = u32::try_from(unique.len()).unwrap_or(u32::MAX);
        if facts_count > limits.max_facts() {
            return Err(ProjectionError::ReadSetExceeded {
                facts: facts_count,
                cap: limits.max_facts(),
            });
        }
        let mut sources = BTreeSet::new();
        for fact_id in &unique {
            let declaration =
                self.fact_set
                    .get(fact_id)
                    .ok_or_else(|| ProjectionError::UnknownFact {
                        fact_id: fact_id.clone(),
                    })?;
            sources.insert(declaration.source_id());
        }
        let sources_count = u32::try_from(sources.len()).unwrap_or(u32::MAX);
        if sources_count > limits.max_sources() {
            return Err(ProjectionError::FanInExceeded {
                sources: sources_count,
                cap: limits.max_sources(),
            });
        }
        for fact_id in &unique {
            let declaration =
                self.fact_set
                    .get(fact_id)
                    .ok_or_else(|| ProjectionError::UnknownFact {
                        fact_id: fact_id.clone(),
                    })?;
            if sources_count > declaration.fan_in_cap() {
                return Err(ProjectionError::FanInExceeded {
                    sources: sources_count,
                    cap: declaration.fan_in_cap(),
                });
            }
        }
        Ok(ReadSet {
            facts: unique.into_iter().collect(),
        })
    }

    /// Measures the version distance for every source in a read set.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn staleness(
        &self,
        read_set: &ReadSet,
        observed: &FactSourceVersions,
    ) -> Result<Vec<StalenessReport>, ProjectionError> {
        let content = self.content()?;
        let mut bounds = BTreeMap::new();
        for fact_id in read_set.facts() {
            let declaration =
                self.fact_set
                    .get(fact_id)
                    .ok_or_else(|| ProjectionError::UnknownFact {
                        fact_id: fact_id.clone(),
                    })?;
            let source = declaration.source_id();
            let bound = declaration.staleness_bound();
            bounds
                .entry(source)
                .and_modify(|current: &mut FactVersionDistance| {
                    if bound < *current {
                        *current = bound;
                    }
                })
                .or_insert(bound);
        }
        let reports = bounds
            .into_iter()
            .map(|(source, bound)| {
                let watermark = content
                    .watermarks
                    .get(&source)
                    .unwrap_or_else(|| FactVersion::new(0));
                StalenessReport::new(source.clone(), watermark, observed.get(&source), bound)
            })
            .collect();
        Ok(reports)
    }

    /// Authorizes one read set against the staleness bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn authorize(
        &self,
        read_set: &ReadSet,
        observed: &FactSourceVersions,
        mode: AuthorizationMode,
        deadline: QueueDeadline,
    ) -> Result<AuthorizationReport, ProjectionError> {
        let staleness = self.staleness(read_set, observed)?;
        Ok(AuthorizationReport::new(staleness, mode, deadline))
    }

    /// Reads the fact versions of one checked read set.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::FactUnavailable`] when a fact is absent.
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn read_facts(&self, read_set: &ReadSet) -> Result<Vec<FactRead>, ProjectionError> {
        let content = self.content()?;
        let mut reads = Vec::with_capacity(read_set.len());
        for fact_id in read_set.facts() {
            let fact =
                content
                    .facts
                    .get(fact_id)
                    .ok_or_else(|| ProjectionError::FactUnavailable {
                        fact_id: fact_id.clone(),
                    })?;
            reads.push(FactRead::new(fact_id.clone(), fact.fact_version()));
        }
        Ok(reads)
    }

    /// Builds one verdict over a checked read set.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::FactUnavailable`] when a fact is absent.
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn verdict(
        &self,
        rule: RuleId,
        decision: Decision,
        reason: Option<&'static str>,
        read_set: &ReadSet,
    ) -> Result<Verdict, ProjectionError> {
        let facts_read = self.read_facts(read_set)?;
        let watermarks = self.content()?.watermarks.clone();
        Ok(Verdict::new(
            self.epoch,
            watermarks,
            facts_read,
            rule,
            decision,
            reason,
            self.fact_set.digest(),
        ))
    }

    /// Returns the current projection cursor.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn cursor(&self) -> Result<ProjectionCursor, ProjectionError> {
        let content = self.content()?;
        Ok(ProjectionCursor::new(
            content.watermarks.clone(),
            self.epoch.generation(),
        ))
    }

    /// Builds a snapshot from the current content.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn snapshot(&self) -> Result<ProjectionSnapshot, ProjectionError> {
        let content = self.content()?;
        Ok(ProjectionSnapshot::new(
            self.epoch.generation(),
            self.epoch.policy_version(),
            self.fact_set.digest(),
            content.watermarks.clone(),
            content.facts.values().cloned().collect(),
        ))
    }

    /// Returns the content digest of the current projection.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EpochInvalid`] when the projection failed
    /// closed.
    pub fn content_digest(&self) -> Result<Hash32, ProjectionError> {
        Ok(self.snapshot()?.checksum())
    }

    fn content_from_snapshot(
        fact_set: &FactSet,
        snapshot: &ProjectionSnapshot,
    ) -> Result<ProjectionContent, ProjectionError> {
        let mut facts = BTreeMap::new();
        for stored in snapshot.facts() {
            let declaration =
                fact_set
                    .get(stored.fact_id())
                    .ok_or_else(|| ProjectionError::UnknownFact {
                        fact_id: stored.fact_id().clone(),
                    })?;
            if !stored.matches(declaration) {
                return Err(ProjectionError::FactTypeMismatch {
                    fact_id: stored.fact_id().clone(),
                });
            }
            if facts
                .insert(stored.fact_id().clone(), stored.clone())
                .is_some()
            {
                return Err(ProjectionError::DuplicateFact {
                    fact_id: stored.fact_id().clone(),
                });
            }
        }
        Ok(ProjectionContent {
            watermarks: snapshot.watermarks().clone(),
            facts,
        })
    }

    fn check_snapshot_watermarks(
        current: &ProjectionContent,
        snapshot: &ProjectionSnapshot,
    ) -> Result<(), ProjectionError> {
        for (source, version) in current.watermarks.iter() {
            let incoming = snapshot
                .watermarks()
                .get(source)
                .unwrap_or_else(|| FactVersion::new(0));
            if incoming < version {
                return Err(ProjectionError::WatermarkRegression {
                    fact_source: source.clone(),
                    current: version,
                    incoming,
                });
            }
        }
        Ok(())
    }

    fn apply_change(
        fact_set: &FactSet,
        content: &mut ProjectionContent,
        diff: &ProjectionDiff,
        change: &FactChange,
    ) -> Result<(), ProjectionError> {
        let declaration: &FactDeclaration =
            fact_set
                .get(change.fact_id())
                .ok_or_else(|| ProjectionError::UnknownFact {
                    fact_id: change.fact_id().clone(),
                })?;
        if declaration.source_id() != *diff.source() {
            return Err(ProjectionError::FactSetMismatch);
        }
        if change.fact_version() > diff.watermark() {
            return Err(ProjectionError::EpochInvalid);
        }
        let current_version = content
            .facts
            .get(change.fact_id())
            .map(StoredFact::fact_version);
        if let Some(current_version) = current_version {
            if change.fact_version() < current_version {
                return Err(ProjectionError::WatermarkRegression {
                    fact_source: diff.source().clone(),
                    current: current_version,
                    incoming: change.fact_version(),
                });
            }
            if change.fact_version() == current_version {
                match change.value() {
                    Some(value)
                        if content
                            .facts
                            .get(change.fact_id())
                            .is_some_and(|fact| fact.value() == value) =>
                    {
                        return Ok(());
                    }
                    Some(_) => {
                        return Err(ProjectionError::WatermarkRegression {
                            fact_source: diff.source().clone(),
                            current: current_version,
                            incoming: change.fact_version(),
                        });
                    }
                    None => {
                        content.facts.remove(change.fact_id());
                        return Ok(());
                    }
                }
            }
        }
        match change.value() {
            Some(value) => {
                if value.canonical_type() != declaration.canonical_type() {
                    return Err(ProjectionError::FactTypeMismatch {
                        fact_id: change.fact_id().clone(),
                    });
                }
                content.facts.insert(
                    change.fact_id().clone(),
                    StoredFact::new(
                        change.fact_id().clone(),
                        diff.source().clone(),
                        change.fact_version(),
                        value.clone(),
                    ),
                );
            }
            None => {
                content.facts.remove(change.fact_id());
            }
        }
        Ok(())
    }
}
