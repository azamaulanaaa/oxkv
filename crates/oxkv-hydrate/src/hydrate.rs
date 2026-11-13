//! The resumable hydration state machine.

use std::collections::BTreeMap;

use oxkv_contracts::version::{PageSequence, ScopeVersionVector};

use crate::budget::ScopeBudget;
use crate::error::{HydrationError, HydrationProgress};
use crate::journal::{Journal, Phase};
use crate::page::SnapshotPage;
use crate::snapshot::{SealedSnapshot, SnapshotDescriptor, directory_and_digest};
use crate::store::DurableStore;
use crate::tail::DiffFrame;
use crate::transport::Transport;

/// One hydration request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationRequest {
    /// The snapshot descriptor.
    pub descriptor: SnapshotDescriptor,
    /// The version vector that the tail must reach.
    pub target_vector: ScopeVersionVector,
    /// The number of tail frames that must apply before publication.
    pub tail_length: u64,
}

impl HydrationRequest {
    /// Creates a snapshot-only request.
    #[must_use]
    pub fn from_snapshot(snapshot: &SealedSnapshot, tail_length: u64) -> Self {
        let descriptor = snapshot.descriptor();
        Self {
            target_vector: descriptor.version_vector.clone(),
            descriptor,
            tail_length,
        }
    }

    /// Creates a request with an explicit tail target.
    #[must_use]
    pub fn with_target(
        descriptor: SnapshotDescriptor,
        target_vector: ScopeVersionVector,
        tail_length: u64,
    ) -> Self {
        Self {
            descriptor,
            target_vector,
            tail_length,
        }
    }
}

/// The outcome of a start call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    /// Hydration started with a new generation.
    Started,
    /// Hydration resumed at the last durable boundary.
    Resumed,
    /// The requested snapshot is already published.
    AlreadyPublished,
}

/// The outcome of one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// The hydrator verified and stored one page.
    PageVerified {
        /// The page sequence.
        sequence: PageSequence,
        /// The logical page bytes.
        bytes: u64,
    },
    /// The hydrator verified every page and checked the content digest.
    TransferComplete,
    /// The hydrator can apply the tail or publish the generation.
    TailReady,
    /// The hydrator waits for a tail frame.
    WaitingForTail {
        /// The next sequence that the hydrator needs.
        next_sequence: u64,
    },
    /// The hydrator published the new generation.
    Published {
        /// The published generation.
        generation: crate::journal::GenerationId,
        /// The deterministic state hash.
        state_hash: [u8; 32],
    },
    /// The generation was already published.
    AlreadyPublished,
}

/// The outcome of one queued frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameOutcome {
    /// The frame applied and the watermark advanced.
    Applied {
        /// The applied watermark.
        watermark: u64,
    },
    /// The frame is a duplicate and changed no state.
    Duplicate {
        /// The applied watermark.
        watermark: u64,
    },
    /// The frame arrived before the snapshot completed. It waits in memory.
    Deferred,
    /// The fragment is missing. Request the next sequence again.
    Missing {
        /// The next sequence that the hydrator needs.
        next_sequence: u64,
    },
    /// The frame applied and the generation published.
    Published {
        /// The published generation.
        generation: crate::journal::GenerationId,
        /// The deterministic state hash.
        state_hash: [u8; 32],
    },
}

/// One report after hydration stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationReport {
    /// The visible progress.
    pub progress: HydrationProgress,
    /// The published state hash.
    pub state_hash: Option<[u8; 32]>,
    /// The number of verified pages.
    pub verified_pages: u64,
    /// The applied tail watermark.
    pub frames_applied: u64,
}

/// A recorded hydration trace for tests and operations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HydrationTrace {
    /// The recorded steps.
    pub steps: Vec<StepOutcome>,
    /// The recorded frame outcomes.
    pub frames: Vec<FrameOutcome>,
}

/// The hydration state machine.
pub struct Hydrator<'a, S, T>
where
    S: DurableStore,
    T: Transport,
{
    store: &'a mut S,
    transport: &'a mut T,
    budget: ScopeBudget,
    max_page_attempts: u32,
    page_budget_bytes: u64,
    state: Option<Journal>,
    pending: BTreeMap<u64, DiffFrame>,
    peak_page_bytes: u64,
    trace: HydrationTrace,
}

impl<'a, S, T> Hydrator<'a, S, T>
where
    S: DurableStore,
    T: Transport,
{
    /// Creates a hydrator.
    pub fn new(store: &'a mut S, transport: &'a mut T) -> Self {
        Self {
            store,
            transport,
            budget: ScopeBudget::unlimited(),
            max_page_attempts: 3,
            page_budget_bytes: u64::MAX,
            state: None,
            pending: BTreeMap::new(),
            peak_page_bytes: 0,
            trace: HydrationTrace::default(),
        }
    }

    /// Sets the scope budget.
    #[must_use]
    pub const fn with_budget(mut self, budget: ScopeBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Sets the maximum page bytes.
    #[must_use]
    pub const fn with_page_budget(mut self, page_budget_bytes: u64) -> Self {
        self.page_budget_bytes = page_budget_bytes;
        self
    }

    /// Sets the maximum attempts for one page.
    #[must_use]
    pub const fn with_max_page_attempts(mut self, attempts: u32) -> Self {
        self.max_page_attempts = attempts;
        self
    }

    /// Returns the largest page that the hydrator held in flight.
    #[must_use]
    pub const fn peak_page_bytes(&self) -> u64 {
        self.peak_page_bytes
    }

    /// Returns the recorded trace.
    #[must_use]
    pub const fn trace(&self) -> &HydrationTrace {
        &self.trace
    }

    /// Returns the durable journal.
    #[must_use]
    pub const fn journal(&self) -> Option<&Journal> {
        self.state.as_ref()
    }

    /// Returns the store under hydration.
    #[must_use]
    pub const fn store(&self) -> &S {
        self.store
    }

    /// Starts or resumes one hydration.
    ///
    /// A new generation starts beside the served generation. A journal with
    /// the same identity resumes at the last durable boundary. A different
    /// identity discards the unpublished generation and starts again.
    ///
    /// # Errors
    ///
    /// Returns a named format, budget, or storage error.
    pub fn start(&mut self, request: &HydrationRequest) -> Result<StartOutcome, HydrationError> {
        let served_bytes = self
            .store
            .published()
            .map_or(0, crate::store::Generation::resident_bytes);
        self.budget.plan(&request.descriptor, served_bytes)?;
        let existing = self.store.load_journal()?;
        if let Some(journal) = &existing {
            if journal.phase.is_published() {
                if journal.has_identity(&request.descriptor) {
                    self.state = Some(journal.clone());
                    return Ok(StartOutcome::AlreadyPublished);
                }
            } else if journal.has_identity(&request.descriptor)
                && journal.target_vector == request.target_vector
                && journal.tail_length == request.tail_length
            {
                let generation = journal.generation;
                self.store
                    .begin_generation(generation, &request.descriptor)?;
                self.state = Some(journal.clone());
                return Ok(StartOutcome::Resumed);
            }
        }
        self.begin_new(request)
    }

    fn begin_new(&mut self, request: &HydrationRequest) -> Result<StartOutcome, HydrationError> {
        let published = self.store.published_generation();
        for id in self.store.generation_ids() {
            if Some(id) != published {
                self.store.discard_generation(id)?;
            }
        }
        let generation = self.store.next_generation_id();
        self.store
            .begin_generation(generation, &request.descriptor)?;
        let journal = Journal::new(
            request.descriptor.clone(),
            request.target_vector.clone(),
            request.tail_length,
            generation,
            published,
        );
        self.persist(&journal)?;
        self.pending.clear();
        Ok(StartOutcome::Started)
    }

    /// Runs one state transition.
    ///
    /// # Errors
    ///
    /// Returns a named format, transport, or storage error.
    pub fn step(&mut self) -> Result<StepOutcome, HydrationError> {
        let mut journal = self.state.clone().ok_or(HydrationError::NotStarted)?;
        let outcome = match journal.phase {
            Phase::Transfer => self.step_transfer(&mut journal)?,
            Phase::Verified => self.step_verified(&mut journal)?,
            Phase::Tail => self.step_tail(&mut journal)?,
            Phase::Swap => self.step_swap(&journal)?,
            Phase::Published => StepOutcome::AlreadyPublished,
            Phase::Cold | Phase::Aborted => return Err(HydrationError::NotStarted),
        };
        self.trace.steps.push(outcome.clone());
        Ok(outcome)
    }

    /// Runs until the hydrator publishes or needs a tail frame.
    ///
    /// # Errors
    ///
    /// Returns a named format, transport, or storage error.
    pub fn run(&mut self) -> Result<HydrationReport, HydrationError> {
        loop {
            match self.step()? {
                StepOutcome::Published { .. }
                | StepOutcome::AlreadyPublished
                | StepOutcome::WaitingForTail { .. } => break,
                StepOutcome::PageVerified { .. }
                | StepOutcome::TransferComplete
                | StepOutcome::TailReady => {}
            }
        }
        Ok(self.report())
    }

    /// Queues one prefetched diff frame.
    ///
    /// A frame before the verified snapshot waits in memory. A frame after the
    /// watermark is a duplicate. A frame beyond the next sequence forces a
    /// re-request.
    ///
    /// # Errors
    ///
    /// Returns a named storage error.
    pub fn queue_frame(&mut self, frame: DiffFrame) -> Result<FrameOutcome, HydrationError> {
        let mut journal = self.state.clone().ok_or(HydrationError::NotStarted)?;
        let outcome = match journal.phase {
            Phase::Transfer | Phase::Verified | Phase::Swap => {
                self.pending.insert(frame.sequence, frame);
                FrameOutcome::Deferred
            }
            Phase::Tail => self.apply_frame(&mut journal, frame)?,
            Phase::Published => FrameOutcome::Duplicate {
                watermark: journal.applied_watermark,
            },
            Phase::Cold | Phase::Aborted => return Err(HydrationError::NotStarted),
        };
        self.trace.frames.push(outcome.clone());
        Ok(outcome)
    }

    /// Returns the current visible progress.
    #[must_use]
    pub fn progress(&self) -> HydrationProgress {
        let journal = self
            .state
            .clone()
            .or_else(|| self.store.load_journal().ok().flatten());
        let published_vector = self
            .store
            .published()
            .map(crate::store::Generation::version_vector);
        let cold_vector = published_vector.clone();
        journal.map_or_else(
            || HydrationProgress {
                phase: Phase::Cold,
                verified_bytes: 0,
                verified_pages: 0,
                total_pages: 0,
                published_vector: cold_vector,
                target_vector: ScopeVersionVector::new(),
                total_bytes: 0,
            },
            |journal| HydrationProgress {
                phase: journal.phase,
                verified_bytes: journal.verified_bytes,
                verified_pages: u64::try_from(journal.verified.len()).unwrap_or(u64::MAX),
                total_pages: journal.descriptor.page_count,
                published_vector,
                target_vector: journal.target_vector.clone(),
                total_bytes: journal.descriptor.snapshot_bytes,
            },
        )
    }

    /// Returns one report for the current state.
    #[must_use]
    pub fn report(&self) -> HydrationReport {
        let journal = self.state.clone();
        HydrationReport {
            progress: self.progress(),
            state_hash: self
                .store
                .published()
                .map(crate::store::Generation::state_hash),
            verified_pages: journal.as_ref().map_or(0, |journal| {
                u64::try_from(journal.verified.len()).unwrap_or(u64::MAX)
            }),
            frames_applied: journal.map_or(0, |journal| journal.applied_watermark),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn step_transfer(&mut self, journal: &mut Journal) -> Result<StepOutcome, HydrationError> {
        if journal.covers(journal.descriptor.page_count) {
            return self.finish_transfer(journal);
        }
        let sequence = journal.first_unverified();
        let identity = journal.descriptor.identity.clone();
        let mut last_error = None;
        for _ in 0..self.max_page_attempts {
            match self.transport.fetch_page(&identity, sequence) {
                Ok(bytes) => match self.verify_page(journal, sequence, &bytes) {
                    Ok(page) => {
                        self.store.write_page(journal.generation, &bytes, &page)?;
                        journal.record_verified(&page);
                        self.persist(journal)?;
                        return Ok(StepOutcome::PageVerified {
                            sequence,
                            bytes: page.uncompressed_length,
                        });
                    }
                    Err(error) if error.is_refetchable() => {
                        last_error = Some(error);
                    }
                    Err(error) => return Err(error),
                },
                Err(error) => {
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            HydrationError::transport(sequence, "the transport returned no page")
        }))
    }

    fn verify_page(
        &mut self,
        journal: &Journal,
        expected: PageSequence,
        bytes: &[u8],
    ) -> Result<SnapshotPage, HydrationError> {
        let page = SnapshotPage::decode(bytes)?;
        if page.page_sequence != expected
            || page.shard_id != journal.descriptor.identity.shard_id
            || page.commit_version != journal.descriptor.identity.commit_version
        {
            return Err(HydrationError::IdentityMismatch);
        }
        page.check_bounds(&journal.descriptor.shard_bounds)?;
        if page.uncompressed_length > self.page_budget_bytes {
            return Err(HydrationError::PageTooLarge {
                length: page.uncompressed_length,
                limit: self.page_budget_bytes,
            });
        }
        self.peak_page_bytes = self.peak_page_bytes.max(page.uncompressed_length);
        Ok(page)
    }

    fn finish_transfer(&mut self, journal: &mut Journal) -> Result<StepOutcome, HydrationError> {
        let pages = {
            let generation = self
                .store
                .generation(journal.generation)
                .ok_or_else(|| HydrationError::storage("the new generation is absent"))?;
            generation
                .pages
                .values()
                .map(|stored| stored.page.clone())
                .collect::<Vec<_>>()
        };
        if u64::try_from(pages.len()).unwrap_or(u64::MAX) != journal.descriptor.page_count {
            return Err(HydrationError::TruncatedSnapshot {
                expected: usize::try_from(journal.descriptor.page_count).unwrap_or(usize::MAX),
                actual: pages.len(),
            });
        }
        let (_, digest) = directory_and_digest(&journal.descriptor, &pages);
        if digest != journal.descriptor.identity.content_digest {
            return Err(HydrationError::CorruptPage {
                page_id: None,
                byte_offset: 0,
            });
        }
        self.store.complete_generation(journal.generation)?;
        journal.phase = Phase::Verified;
        self.persist(journal)?;
        Ok(StepOutcome::TransferComplete)
    }

    fn step_verified(&mut self, journal: &mut Journal) -> Result<StepOutcome, HydrationError> {
        if journal.tail_length == 0 {
            self.begin_swap(journal)?;
        } else {
            journal.phase = Phase::Tail;
            self.persist(journal)?;
        }
        Ok(StepOutcome::TailReady)
    }

    fn step_tail(&mut self, journal: &mut Journal) -> Result<StepOutcome, HydrationError> {
        self.drain_pending(journal)?;
        if journal.applied_watermark >= journal.tail_length {
            self.begin_swap(journal)?;
            return Ok(StepOutcome::TailReady);
        }
        Ok(StepOutcome::WaitingForTail {
            next_sequence: journal.applied_watermark.saturating_add(1),
        })
    }

    fn step_swap(&mut self, journal: &Journal) -> Result<StepOutcome, HydrationError> {
        self.store.publish(journal.generation)?;
        let mut published = journal.clone();
        published.phase = Phase::Published;
        self.persist(&published)?;
        let state_hash = self
            .store
            .published()
            .map_or([0; 32], crate::store::Generation::state_hash);
        Ok(StepOutcome::Published {
            generation: published.generation,
            state_hash,
        })
    }

    fn begin_swap(&mut self, journal: &mut Journal) -> Result<(), HydrationError> {
        journal.phase = Phase::Swap;
        journal.swap_commit = Some(journal.generation);
        self.persist(journal)
    }

    fn apply_frame(
        &mut self,
        journal: &mut Journal,
        frame: DiffFrame,
    ) -> Result<FrameOutcome, HydrationError> {
        if frame.sequence <= journal.applied_watermark {
            return Ok(FrameOutcome::Duplicate {
                watermark: journal.applied_watermark,
            });
        }
        if frame.sequence > journal.applied_watermark.saturating_add(1) {
            self.pending.insert(frame.sequence, frame);
            return Ok(FrameOutcome::Missing {
                next_sequence: journal.applied_watermark.saturating_add(1),
            });
        }
        self.pending.insert(frame.sequence, frame);
        self.drain_pending(journal)?;
        if journal.applied_watermark >= journal.tail_length {
            self.begin_swap(journal)?;
            if let StepOutcome::Published {
                generation,
                state_hash,
            } = self.step_swap(journal)?
            {
                return Ok(FrameOutcome::Published {
                    generation,
                    state_hash,
                });
            }
        }
        Ok(FrameOutcome::Applied {
            watermark: journal.applied_watermark,
        })
    }

    fn drain_pending(&mut self, journal: &mut Journal) -> Result<(), HydrationError> {
        let mut changed = false;
        while let Some(frame) = self
            .pending
            .remove(&journal.applied_watermark.saturating_add(1))
        {
            self.store.apply_diff(journal.generation, &frame)?;
            if !journal.advance_watermark(frame.sequence) {
                break;
            }
            changed = true;
        }
        if changed {
            self.persist(journal)?;
        }
        Ok(())
    }

    fn persist(&mut self, journal: &Journal) -> Result<(), HydrationError> {
        self.store.save_journal(journal)?;
        self.state = Some(journal.clone());
        Ok(())
    }
}
