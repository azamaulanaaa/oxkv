//! The read-only client replica.
//!
//! A replica owns one scope, one mirror generation, one local index, one
//! outbox, and one local tab lease. It has no write engine. The mirror changes
//! only from authority pages and authority diff frames.
//!
//! Hydration builds a new generation beside the served generation. Publication
//! swaps one pointer. A reader sees the old generation or the new generation.
//! A cold replica returns `read.not_hydrated` with progress.

use std::collections::BTreeMap;

use oxkv_contracts::version::{ContentDigest, LeaseTerm, ScopeVersionVector};

use crate::error::ReplicaError;
use crate::index::{IndexVersion, ReplicaIndex, Scorer};
use crate::journal::{HydrationJournal, HydrationJournalStore, HydrationPhase};
use crate::mirror::{DiffFrame, Ingest, Mirror, RowEntry};
use crate::outbox::{Outbox, OutboxRecord};
use crate::page::{ScopeSnapshot, ScopeSnapshotPage};
use crate::read::{
    HydrationState, ReadRequest, ReadResponse, ReplicaStatus, SearchRequest, SearchResponse,
    StalenessBound, resolve_pin,
};
use crate::residency::{DeviceBudget, EvictionReport, ResidencyReport};
use crate::scope::Scope;
use crate::tabs::{CursorAdvancer, LocalTabLease, ReplicaIdentity, TabId};
use crate::versions::{merge_max, version_lag};

/// The digest value that means "not declared".
const UNKNOWN_DIGEST: ContentDigest = ContentDigest::from_bytes([0; 32]);

/// One hydration target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationTarget {
    /// The target byte count.
    pub target_bytes: u64,
    /// The target version vector.
    pub target_version: ScopeVersionVector,
    /// The expected snapshot page count. Zero means "not declared".
    pub expected_pages: u64,
    /// The expected snapshot content digest. The zero digest means "not declared".
    pub content_digest: ContentDigest,
}

impl HydrationTarget {
    /// Creates a hydration target without a declared page count or digest.
    #[must_use]
    pub const fn new(target_bytes: u64, target_version: ScopeVersionVector) -> Self {
        Self {
            target_bytes,
            target_version,
            expected_pages: 0,
            content_digest: UNKNOWN_DIGEST,
        }
    }

    /// Creates a hydration target from one scope snapshot.
    #[must_use]
    pub fn for_snapshot(snapshot: &ScopeSnapshot) -> Self {
        Self {
            target_bytes: snapshot.resident_bytes(),
            target_version: snapshot.scope_version.clone(),
            expected_pages: snapshot.page_count,
            content_digest: snapshot.content_digest,
        }
    }

    /// Sets the expected page count and content digest.
    #[must_use]
    pub const fn with_expected(
        mut self,
        expected_pages: u64,
        content_digest: ContentDigest,
    ) -> Self {
        self.expected_pages = expected_pages;
        self.content_digest = content_digest;
        self
    }
}

/// The result of one published hydration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationReport {
    /// The published generation.
    pub generation: u64,
    /// The published version vector.
    pub published_version: ScopeVersionVector,
    /// The verified byte count.
    pub verified_bytes: u64,
    /// The hydration peak byte count.
    pub peak_bytes: u64,
    /// The published page count.
    pub page_count: u64,
}

/// The configuration of one replica.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaConfig {
    /// The replica identity.
    pub identity: ReplicaIdentity,
    /// The device budget.
    pub device_budget: DeviceBudget,
    /// The manifest scorer.
    pub scorer: Scorer,
}

impl ReplicaConfig {
    /// Creates a replica configuration with the default scorer.
    #[must_use]
    pub const fn new(identity: ReplicaIdentity, device_budget: DeviceBudget) -> Self {
        Self {
            identity,
            device_budget,
            scorer: Scorer::DEFAULT,
        }
    }

    /// Sets the manifest scorer.
    #[must_use]
    pub const fn with_scorer(mut self, scorer: Scorer) -> Self {
        self.scorer = scorer;
        self
    }
}

#[derive(Debug, Clone)]
struct Staging {
    mirror: Mirror,
    target_version: ScopeVersionVector,
    target_bytes: u64,
}

/// The read-only client replica.
#[derive(Debug, Clone)]
pub struct Replica {
    pub(crate) config: ReplicaConfig,
    pub(crate) scope: Scope,
    pub(crate) mirror: Option<Mirror>,
    pub(crate) index: Option<ReplicaIndex>,
    pub(crate) outbox: Outbox,
    pub(crate) lease: LocalTabLease,
    pub(crate) hydration: HydrationState,
    staging: Option<Staging>,
    journal: Option<HydrationJournal>,
    watermark: ScopeVersionVector,
    last_contact_at: Option<u64>,
    generation: u64,
    hydration_peak_bytes: u64,
    evicted_pages: u64,
    evicted_bytes: u64,
}

impl Replica {
    /// Creates a cold replica.
    #[must_use]
    pub fn new(config: ReplicaConfig, scope: Scope) -> Self {
        let lease = LocalTabLease::new(config.identity.clone());
        Self {
            config,
            scope,
            mirror: None,
            index: None,
            outbox: Outbox::new(),
            lease,
            hydration: HydrationState::Cold,
            staging: None,
            journal: None,
            watermark: ScopeVersionVector::new(),
            last_contact_at: None,
            generation: 0,
            hydration_peak_bytes: 0,
            evicted_pages: 0,
            evicted_bytes: 0,
        }
    }

    /// Creates a replica that resumes from one durable journal.
    #[must_use]
    pub fn with_journal(config: ReplicaConfig, scope: Scope, journal: HydrationJournal) -> Self {
        let mut replica = Self::new(config, scope);
        replica.generation = journal.published_before.unwrap_or(0);
        replica.journal = Some(journal);
        replica
    }

    /// Creates a replica from the journal in one durable store.
    ///
    /// # Errors
    ///
    /// Returns a named error when the store cannot read the journal or the
    /// stored journal is corrupt.
    pub fn resume(
        config: ReplicaConfig,
        scope: Scope,
        store: &impl HydrationJournalStore,
    ) -> Result<Self, ReplicaError> {
        match store.load_journal()? {
            Some(journal) => Ok(Self::with_journal(config, scope, journal)),
            None => Ok(Self::new(config, scope)),
        }
    }

    /// Returns the hydration journal.
    #[must_use]
    pub const fn journal(&self) -> Option<&HydrationJournal> {
        self.journal.as_ref()
    }

    /// Takes the hydration journal for durable storage.
    #[must_use]
    pub fn take_journal(&mut self) -> Option<HydrationJournal> {
        self.journal.take()
    }

    /// Saves the hydration journal to one durable store.
    ///
    /// # Errors
    ///
    /// Returns a named error when the store cannot write the journal.
    pub fn persist_journal(
        &self,
        store: &mut impl HydrationJournalStore,
    ) -> Result<(), ReplicaError> {
        if let Some(journal) = &self.journal {
            store.save_journal(journal)?;
        }
        Ok(())
    }

    /// Returns the scope.
    #[must_use]
    pub const fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Returns the replica identity.
    #[must_use]
    pub const fn identity(&self) -> &ReplicaIdentity {
        &self.config.identity
    }

    /// Returns true when a generation is published.
    #[must_use]
    pub const fn is_hydrated(&self) -> bool {
        self.mirror.is_some()
    }

    /// Returns the hydration state.
    #[must_use]
    pub const fn hydration_state(&self) -> &HydrationState {
        &self.hydration
    }

    /// Returns the applied version vector.
    #[must_use]
    pub fn applied_version(&self) -> ScopeVersionVector {
        self.mirror
            .as_ref()
            .map_or_else(ScopeVersionVector::new, |mirror| mirror.applied().clone())
    }

    /// Returns the authority watermark.
    #[must_use]
    pub fn authority_watermark(&self) -> &ScopeVersionVector {
        &self.watermark
    }

    /// Returns the version lag.
    #[must_use]
    pub fn lag_versions(&self) -> u64 {
        version_lag(&self.applied_version(), &self.watermark)
    }

    /// Returns the status at one injected time.
    #[must_use]
    pub fn status(&self, now: u64) -> ReplicaStatus {
        let applied = self.applied_version();
        let lag = version_lag(&applied, &self.watermark);
        ReplicaStatus {
            applied,
            authority_watermark: self.watermark.clone(),
            lag_versions: lag,
            lag_time: self.last_contact_at.map(|then| now.saturating_sub(then)),
            hydration: self.hydration.clone(),
        }
    }

    /// Records one authority contact and its watermark.
    ///
    /// The watermark never decreases. A lower observation is merged with the
    /// current watermark.
    pub fn note_authority(&mut self, watermark: &ScopeVersionVector, now: u64) {
        merge_max(&mut self.watermark, watermark);
        if let Some(mirror) = &self.mirror {
            merge_max(&mut self.watermark, mirror.applied());
        }
        let contact = self.last_contact_at.map_or(now, |then| then.max(now));
        self.last_contact_at = Some(contact);
    }

    /// Starts a new hydration generation.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::HydrationInProgress`] when another hydration is
    /// active. Returns [`ReplicaError::HydrationBudgetExceeded`] when the
    /// target does not fit beside the served generation.
    pub fn begin_hydration(&mut self, target: HydrationTarget) -> Result<(), ReplicaError> {
        if self.staging.is_some() {
            return Err(ReplicaError::HydrationInProgress);
        }
        let served_bytes = self.mirror.as_ref().map_or(0, Mirror::resident_bytes);
        let budget = self.config.device_budget;
        if target.target_bytes > budget.max_resident_bytes {
            return Err(ReplicaError::DeviceBudgetExceeded {
                required: target.target_bytes,
                budget: budget.max_resident_bytes,
            });
        }
        let peak = served_bytes.saturating_add(target.target_bytes);
        if peak > budget.max_resident_bytes {
            return Err(ReplicaError::HydrationBudgetExceeded {
                required: peak,
                budget: budget.max_resident_bytes,
            });
        }
        let resumable = self.journal.as_ref().is_some_and(|journal| {
            journal.has_identity(
                &self.scope.identity(),
                &target.target_version,
                target.expected_pages,
                target.content_digest,
            ) && !journal.phase.is_terminal()
        });
        let generation = if resumable {
            self.journal
                .as_ref()
                .map_or(self.generation + 1, |journal| journal.generation)
        } else {
            self.generation + 1
        };
        let staging_ceiling = budget.max_resident_bytes.saturating_sub(served_bytes);
        self.staging = Some(Staging {
            mirror: Mirror::new(self.scope.clone(), staging_ceiling, generation),
            target_version: target.target_version.clone(),
            target_bytes: target.target_bytes,
        });
        if !resumable {
            let published_before = self.mirror.as_ref().map(|_| self.generation);
            self.journal = Some(HydrationJournal::new(
                self.scope.identity(),
                target.target_version,
                target.expected_pages,
                target.content_digest,
                generation,
                published_before,
            ));
        }
        self.hydration = HydrationState::Hydrating {
            verified_bytes: 0,
            target_bytes: target.target_bytes,
        };
        Ok(())
    }

    /// Ingests one page into the staging generation.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::HydrationNotStarted`] when no hydration is
    /// active. Returns the page verification errors from the mirror. Returns
    /// [`ReplicaError::HydrationBudgetExceeded`] when the staging generation
    /// exceeds its part of the device budget.
    pub fn hydrate_page(&mut self, page: ScopeSnapshotPage) -> Result<Ingest, ReplicaError> {
        let Some(staging) = &mut self.staging else {
            return Err(ReplicaError::HydrationNotStarted);
        };
        let sequence = page.header.page_sequence;
        let result = match staging.mirror.ingest_page(page) {
            Ok(result) => result,
            Err(ReplicaError::ResourceExhausted { required, .. }) => {
                return Err(ReplicaError::HydrationBudgetExceeded {
                    required,
                    budget: self.config.device_budget.max_resident_bytes,
                });
            }
            Err(error) => return Err(error),
        };
        if let Some(journal) = &mut self.journal {
            journal.record_verified(sequence);
        }
        self.hydration = HydrationState::Hydrating {
            verified_bytes: staging.mirror.resident_bytes(),
            target_bytes: staging.target_bytes,
        };
        Ok(result)
    }

    /// Publishes the staging generation with one pointer swap.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::HydrationNotStarted`] when no hydration is
    /// active. Returns [`ReplicaError::DeviceBudgetExceeded`] when the final
    /// generation does not fit. Returns [`ReplicaError::HydrationBudgetExceeded`]
    /// when the peak does not fit. Returns [`ReplicaError::VersionRegression`]
    /// when the new generation is below the served generation.
    pub fn publish_hydration(&mut self) -> Result<HydrationReport, ReplicaError> {
        let Some(staging) = self.staging.take() else {
            return Err(ReplicaError::HydrationNotStarted);
        };
        let index = ReplicaIndex::build(&self.scope, staging.mirror.rows(), self.config.scorer);
        if let Some(served) = &self.mirror
            && !staging
                .mirror
                .applied()
                .as_vector()
                .entails(served.applied().as_vector())
        {
            self.restore_hydration();
            return Err(ReplicaError::VersionRegression);
        }
        let final_bytes = staging
            .mirror
            .resident_bytes()
            .saturating_add(index.resident_bytes());
        let served_bytes = self.mirror.as_ref().map_or(0, Mirror::resident_bytes);
        let peak = served_bytes.saturating_add(final_bytes);
        let budget = self.config.device_budget;
        if final_bytes > budget.max_resident_bytes {
            self.restore_hydration();
            return Err(ReplicaError::DeviceBudgetExceeded {
                required: final_bytes,
                budget: budget.max_resident_bytes,
            });
        }
        if peak > budget.max_resident_bytes {
            self.restore_hydration();
            return Err(ReplicaError::HydrationBudgetExceeded {
                required: peak,
                budget: budget.max_resident_bytes,
            });
        }
        let overhead = peak.saturating_sub(final_bytes);
        if overhead > budget.max_hydration_overhead_bytes {
            self.restore_hydration();
            return Err(ReplicaError::HydrationBudgetExceeded {
                required: peak,
                budget: final_bytes.saturating_add(budget.max_hydration_overhead_bytes),
            });
        }
        let page_count = u64::try_from(staging.mirror.page_count()).unwrap_or(u64::MAX);
        let expected_pages = self
            .journal
            .as_ref()
            .map_or(0, |journal| journal.expected_pages);
        if expected_pages != 0 && page_count != expected_pages {
            self.restore_hydration();
            return Err(ReplicaError::PartialGeneration {
                expected: expected_pages,
                found: page_count,
            });
        }
        let expected_digest = self
            .journal
            .as_ref()
            .map_or(UNKNOWN_DIGEST, |journal| journal.content_digest);
        if expected_digest != UNKNOWN_DIGEST {
            let found = staging.mirror.snapshot_digest();
            if found != expected_digest {
                self.restore_hydration();
                return Err(ReplicaError::ContentDigestMismatch {
                    expected: expected_digest,
                    found,
                });
            }
        }
        if let Some(journal) = &mut self.journal {
            journal.phase = HydrationPhase::Swap;
            journal.swap_commit = Some(staging.mirror.generation());
        }
        let published_version = staging.mirror.applied().clone();
        self.generation = staging.mirror.generation();
        self.hydration_peak_bytes = self.hydration_peak_bytes.max(peak);
        self.mirror = Some(staging.mirror);
        self.index = Some(index);
        merge_max(&mut self.watermark, &staging.target_version);
        merge_max(&mut self.watermark, &published_version);
        if let Some(journal) = &mut self.journal {
            journal.phase = HydrationPhase::Published;
        }
        self.hydration = HydrationState::Hydrated;
        Ok(HydrationReport {
            generation: self.generation,
            published_version,
            verified_bytes: final_bytes,
            peak_bytes: peak,
            page_count,
        })
    }

    /// Aborts the staging generation.
    pub fn abort_hydration(&mut self) {
        self.staging = None;
        if let Some(journal) = &mut self.journal {
            journal.phase = HydrationPhase::Aborted;
        }
        self.restore_hydration();
    }

    fn restore_hydration(&mut self) {
        self.hydration = if self.mirror.is_some() {
            HydrationState::Hydrated
        } else {
            HydrationState::Cold
        };
    }

    /// Hydrates a complete `ScopeSnapshot`.
    ///
    /// A failure keeps the served generation readable. The staging generation
    /// is discarded.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::ScopePinMismatch`] for another scope identity.
    /// Returns the page and budget errors from the hydration methods.
    pub fn hydrate(&mut self, snapshot: ScopeSnapshot) -> Result<HydrationReport, ReplicaError> {
        if snapshot.scope_identity != self.scope.identity() {
            return Err(ReplicaError::ScopePinMismatch);
        }
        let authority_watermark = snapshot.authority_watermark.clone();
        self.begin_hydration(HydrationTarget::for_snapshot(&snapshot))?;
        for page in snapshot.pages {
            if let Err(error) = self.hydrate_page(page) {
                self.abort_hydration();
                return Err(error);
            }
        }
        if let Some(journal) = &mut self.journal {
            journal.phase = HydrationPhase::Verified;
        }
        match self.publish_hydration() {
            Ok(report) => {
                merge_max(&mut self.watermark, &authority_watermark);
                Ok(report)
            }
            Err(error) => {
                self.abort_hydration();
                Err(error)
            }
        }
    }

    /// Reads one key at one pin.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::NotHydrated`] for a cold replica.
    /// Returns [`ReplicaError::Stale`] with no data when the lag exceeds the
    /// bound. Returns the pin errors from [`resolve_pin`]. Returns
    /// [`ReplicaError::NotFound`] when the key has no value at the pin.
    pub fn get(&self, request: &ReadRequest) -> Result<ReadResponse, ReplicaError> {
        let (mirror, _) = self.published()?;
        resolve_pin(mirror.applied(), &request.pin)?;
        let lag_versions = version_lag(mirror.applied(), &self.watermark);
        check_bound(request.bound, lag_versions)?;
        let entry = mirror.row(&request.key).ok_or(ReplicaError::NotFound)?;
        let value = entry.value.clone().ok_or(ReplicaError::NotFound)?;
        Ok(ReadResponse {
            key: request.key.clone(),
            value,
            served: mirror.applied().clone(),
            bound: request.bound,
            lag_versions,
            page_id: entry.page_id,
        })
    }

    /// Searches the local index at one pin.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::NotHydrated`] for a cold replica. Returns
    /// [`ReplicaError::Stale`] with no hits when the lag exceeds the bound.
    /// Returns [`ReplicaError::IndexMismatch`] for a bad index digest. Returns
    /// [`ReplicaError::TokenizerMismatch`] for another tokenizer.
    pub fn search(&self, request: &SearchRequest) -> Result<SearchResponse, ReplicaError> {
        let (mirror, index) = self.published()?;
        resolve_pin(mirror.applied(), &request.pin)?;
        let lag_versions = version_lag(mirror.applied(), &self.watermark);
        check_bound(request.bound, lag_versions)?;
        index.verify()?;
        let expected = request
            .tokenizer_version
            .unwrap_or_else(|| self.scope.tokenizer_version());
        index.check_tokenizer(expected)?;
        Ok(SearchResponse {
            hits: index.search(&self.scope, &request.text, request.limit),
            served: mirror.applied().clone(),
            bound: request.bound,
            lag_versions,
            tokenizer_version: index.tokenizer_version(),
            index_version: index.index_version(),
        })
    }

    /// Returns the local index bytes.
    #[must_use]
    pub fn index_bytes(&self) -> Option<&[u8]> {
        self.index.as_ref().map(ReplicaIndex::bytes)
    }

    /// Returns the local index version.
    #[must_use]
    pub fn index_version(&self) -> Option<IndexVersion> {
        self.index.as_ref().map(ReplicaIndex::index_version)
    }

    /// Returns the local tokenizer version.
    #[must_use]
    pub fn tokenizer_version(&self) -> oxkv_contracts::version::TokenizerVersion {
        self.scope.tokenizer_version()
    }

    /// Returns the resident byte report.
    #[must_use]
    pub fn residency(&self) -> ResidencyReport {
        let mirror_bytes = self.mirror.as_ref().map_or(0, Mirror::resident_bytes);
        let hydration_bytes = self
            .staging
            .as_ref()
            .map_or(0, |staging| staging.mirror.resident_bytes());
        let index_bytes = self.index.as_ref().map_or(0, ReplicaIndex::resident_bytes);
        let outbox_bytes = self.outbox.resident_bytes();
        let total_bytes = mirror_bytes
            .saturating_add(hydration_bytes)
            .saturating_add(index_bytes);
        ResidencyReport {
            mirror_bytes,
            index_bytes,
            outbox_bytes,
            hydration_bytes,
            total_bytes,
            ceiling_bytes: self.config.device_budget.max_resident_bytes,
            hydration_peak_bytes: self.hydration_peak_bytes.max(total_bytes),
            evicted_pages: self.evicted_pages,
            evicted_bytes: self.evicted_bytes,
        }
    }

    /// Evicts superseded pages from the mirror and the staging generation.
    #[must_use]
    pub fn evict(&mut self) -> EvictionReport {
        let mut report = EvictionReport::default();
        if let Some(mirror) = &mut self.mirror {
            report = report.merge(mirror.evict_superseded());
        }
        if let Some(staging) = &mut self.staging {
            report = report.merge(staging.mirror.evict_superseded());
        }
        self.evicted_pages = self.evicted_pages.saturating_add(report.pages_removed);
        self.evicted_bytes = self.evicted_bytes.saturating_add(report.bytes_freed);
        report
    }

    /// Returns the outbox.
    #[must_use]
    pub const fn outbox(&self) -> &Outbox {
        &self.outbox
    }

    /// Adds one write intent to the outbox. The mirror does not change.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::IdempotencyKeyReuse`] when the key names another
    /// payload.
    pub fn enqueue_write(&mut self, record: OutboxRecord) -> Result<(), ReplicaError> {
        self.outbox.enqueue(record)
    }

    /// Rejects a local mutation of the mirror.
    ///
    /// The method always returns [`ReplicaError::LocalMutationRejected`]. The
    /// replica has no write engine.
    ///
    /// # Errors
    ///
    /// Always returns [`ReplicaError::LocalMutationRejected`].
    #[allow(clippy::unused_self)]
    pub fn apply_local_mutation(&mut self, _key: &[u8], _value: &[u8]) -> Result<(), ReplicaError> {
        Err(ReplicaError::LocalMutationRejected)
    }

    /// Acquires the local tab lease for one cursor advancer.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::TabLeaseHeld`] when another tab holds the lease.
    pub fn acquire_advancer(&mut self, tab: TabId) -> Result<CursorAdvancer, ReplicaError> {
        self.lease.acquire(tab)
    }

    /// Releases the local tab lease.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::StaleTabLease`] when the guard is stale.
    pub fn release_advancer(&mut self, advancer: &CursorAdvancer) -> Result<(), ReplicaError> {
        self.lease.release(advancer)
    }

    /// Applies one authority diff frame through the cursor advancer.
    ///
    /// The replica rebuilds its local index after an applied frame. One
    /// advancer exists per replica identity.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::StaleTabLease`] for a stale guard. Returns
    /// [`ReplicaError::NotHydrated`] for a cold replica. Returns
    /// [`ReplicaError::ResourceExhausted`] when the index would exceed the
    /// ceiling. Returns the mirror errors.
    pub fn advance_cursor(
        &mut self,
        advancer: &CursorAdvancer,
        diff: &DiffFrame,
    ) -> Result<Ingest, ReplicaError> {
        self.check_advancer(advancer)?;
        let Some(mirror) = &self.mirror else {
            return Err(not_hydrated(&self.hydration));
        };
        if diff.scope_identity != self.scope.identity() {
            return Err(ReplicaError::ScopePinMismatch);
        }
        let prospective = self.prospective_rows(mirror, diff);
        let index = ReplicaIndex::build(
            &self.scope,
            prospective
                .iter()
                .map(|(key, entry)| (key.as_slice(), entry)),
            self.config.scorer,
        );
        let projected = mirror
            .resident_bytes()
            .saturating_add(index.resident_bytes())
            .saturating_add(
                self.staging
                    .as_ref()
                    .map_or(0, |staging| staging.mirror.resident_bytes()),
            );
        if projected > self.config.device_budget.max_resident_bytes {
            return Err(ReplicaError::ResourceExhausted {
                required: projected,
                ceiling: self.config.device_budget.max_resident_bytes,
            });
        }
        let Some(mirror) = &mut self.mirror else {
            return Err(not_hydrated(&self.hydration));
        };
        let result = mirror.apply_diff(diff)?;
        if result == Ingest::Applied {
            self.index = Some(index);
        }
        Ok(result)
    }

    /// Returns the authority write lease. A replica never holds one.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn authority_lease(&self) -> Option<LeaseTerm> {
        None
    }

    /// Returns true when the replica holds an authority write lease.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn holds_authority_lease(&self) -> bool {
        false
    }

    fn published(&self) -> Result<(&Mirror, &ReplicaIndex), ReplicaError> {
        match (&self.mirror, &self.index) {
            (Some(mirror), Some(index)) => Ok((mirror, index)),
            _ => Err(not_hydrated(&self.hydration)),
        }
    }

    fn check_advancer(&self, advancer: &CursorAdvancer) -> Result<(), ReplicaError> {
        let matches = advancer.replica_id() == &self.config.identity
            && self.lease.holder() == Some(advancer.tab_id())
            && self.lease.epoch() == advancer.epoch();
        if matches {
            Ok(())
        } else {
            Err(ReplicaError::StaleTabLease {
                expected: self.lease.epoch(),
                found: advancer.epoch(),
            })
        }
    }

    fn prospective_rows(&self, mirror: &Mirror, diff: &DiffFrame) -> BTreeMap<Vec<u8>, RowEntry> {
        let mut rows: BTreeMap<Vec<u8>, RowEntry> = mirror
            .rows()
            .map(|(key, entry)| (key.to_vec(), entry.clone()))
            .collect();
        if !self.scope.admits_row(&diff.key) {
            rows.remove(&diff.key);
            return rows;
        }
        if rows
            .get(&diff.key)
            .is_some_and(|current| current.key_version >= diff.key_version)
        {
            return rows;
        }
        rows.insert(
            diff.key.clone(),
            RowEntry {
                key_version: diff.key_version,
                commit_version: diff.commit_version,
                value: diff.value.clone(),
                page_id: None,
            },
        );
        rows
    }
}

fn check_bound(bound: StalenessBound, lag_versions: u64) -> Result<(), ReplicaError> {
    if bound.admits(lag_versions) {
        Ok(())
    } else {
        Err(ReplicaError::Stale {
            bound: bound.max_versions().unwrap_or(0),
            lag: lag_versions,
        })
    }
}

fn not_hydrated(state: &HydrationState) -> ReplicaError {
    let (progress, target) = state.progress();
    ReplicaError::NotHydrated { progress, target }
}
