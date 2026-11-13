//! The durable client outbox.
//!
//! The outbox holds intents before submission. It orders entries per key with
//! FIFO, keeps at most one entry per key in flight, and aligns every entry with
//! the scheduler states `queued`, `in_flight`, and `terminal`.
//!
//! An authority receipt reconciles one entry. A late authority verdict wins
//! for the data, even when the client abandoned the entry locally.

use oxkv_contracts::receipt::{IdempotencyKey, ReceiptEnvelope, ReceiptOutcome, ReceiptState};

use crate::admit::{AdmissionError, check_capacity, check_dependencies};
use crate::entry::{LocalDisposition, OutboxEntry, RequestDigest, SchedulerState};
use crate::receipt::receipt_outcome;

/// An error from outbox reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutboxError {
    /// The outbox holds no entry with the key.
    #[error("The outbox entry is not found.")]
    NotFound {
        /// The unknown key.
        key: IdempotencyKey,
    },
    /// The key already names a different body.
    #[error("The idempotency key names a different body.")]
    KeyReuse {
        /// The reused key.
        key: IdempotencyKey,
    },
    /// A terminal outcome cannot change.
    #[error("The terminal receipt cannot change.")]
    Immutable {
        /// The current terminal state.
        current: ReceiptState,
        /// The new terminal state.
        next: ReceiptState,
    },
    /// The entry already has a terminal outcome.
    #[error("The outbox entry has a terminal outcome.")]
    TerminalEntry {
        /// The terminal receipt state.
        state: ReceiptState,
    },
    /// The entry cannot leave the scheduler state.
    #[error("The outbox entry is not terminal.")]
    NotTerminal {
        /// The current scheduler state.
        state: SchedulerState,
    },
    /// A terminal receipt has no terminal outcome fields.
    #[error("The receipt has no terminal outcome.")]
    MissingOutcome {
        /// The receipt state.
        state: ReceiptState,
    },
}

impl OutboxError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "receipt.not_found",
            Self::KeyReuse { .. } => "write.idempotency_key_reuse",
            Self::Immutable { .. }
            | Self::TerminalEntry { .. }
            | Self::MissingOutcome { .. }
            | Self::NotTerminal { .. } => "receipt.state_invalid",
        }
    }
}

/// The result of one authority receipt reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconciliation {
    /// The authority has no terminal verdict yet.
    Queued,
    /// The authority closed the entry with one terminal outcome.
    Terminal(ReceiptOutcome),
}

impl Reconciliation {
    /// Returns true when the reconciliation carries a terminal outcome.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal(_))
    }

    /// Returns the terminal outcome.
    #[must_use]
    pub const fn outcome(self) -> Option<ReceiptOutcome> {
        match self {
            Self::Queued => None,
            Self::Terminal(outcome) => Some(outcome),
        }
    }
}

/// The durable client outbox.
#[derive(Debug, Clone)]
pub struct Outbox {
    entries: Vec<OutboxEntry>,
    capacity: usize,
    next_arrival: u64,
}

impl Outbox {
    /// Creates an empty outbox with a size budget.
    #[must_use]
    pub const fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            capacity,
            next_arrival: 0,
        }
    }

    /// Returns the size budget.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the entry count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when the outbox holds no entry.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns every entry in arrival order.
    #[must_use]
    pub fn entries(&self) -> &[OutboxEntry] {
        &self.entries
    }

    /// Returns the local dispositions in arrival order.
    ///
    /// These values never appear in a receipt vector.
    #[must_use]
    pub fn local_dispositions(&self) -> Vec<LocalDisposition> {
        self.entries.iter().map(OutboxEntry::disposition).collect()
    }

    /// Returns one entry by idempotency key.
    #[must_use]
    pub fn entry(&self, key: &IdempotencyKey) -> Option<&OutboxEntry> {
        self.entries
            .iter()
            .find(|entry| entry.idempotency_key() == key)
    }

    /// Returns the terminal outcome of one entry.
    #[must_use]
    pub fn terminal_outcome(&self, key: &IdempotencyKey) -> Option<ReceiptOutcome> {
        self.entry(key).and_then(OutboxEntry::terminal_outcome)
    }

    /// Counts the queued entries on one key.
    #[must_use]
    pub fn queued_on_key(&self, key: &[u8]) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                entry.scheduler_state() == SchedulerState::Queued && entry.mutates_key(key)
            })
            .count()
    }

    /// Counts the in-flight entries on one key.
    #[must_use]
    pub fn in_flight_on_key(&self, key: &[u8]) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                entry.scheduler_state() == SchedulerState::InFlight && entry.mutates_key(key)
            })
            .count()
    }

    /// Counts the terminal entries on one key.
    #[must_use]
    pub fn terminal_on_key(&self, key: &[u8]) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                entry.scheduler_state() == SchedulerState::Terminal && entry.mutates_key(key)
            })
            .count()
    }

    /// Adds one entry to the outbox.
    ///
    /// A duplicate key and body returns the stored entry. The outbox assigns
    /// the arrival sequence at admission.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError::Full`] when the outbox is full. Returns
    /// [`AdmissionError::KeyReuse`] when the key names a different body.
    /// Returns [`AdmissionError::DependencyCycle`] for an absent, forward, or
    /// circular edge. Returns [`AdmissionError::CrossShardEdge`] for an edge
    /// across shards.
    pub fn admit(&mut self, entry: OutboxEntry) -> Result<&OutboxEntry, AdmissionError> {
        if let Some(index) = self
            .entries
            .iter()
            .position(|stored| stored.idempotency_key() == entry.idempotency_key())
        {
            if self.entries[index].body_digest() == entry.body_digest() {
                return Ok(&self.entries[index]);
            }
            return Err(AdmissionError::KeyReuse);
        }
        check_capacity(self.entries.len(), self.capacity)?;
        let mut entry = entry;
        entry.set_arrival(self.next_arrival);
        check_dependencies(&entry, &self.entries)?;
        self.next_arrival = self.next_arrival.saturating_add(1);
        let index = self.entries.len();
        self.entries.push(entry);
        Ok(&self.entries[index])
    }

    /// Returns the next entry that the scheduler may select.
    #[must_use]
    pub fn select_next(&self) -> Option<&OutboxEntry> {
        self.entries.iter().find(|entry| self.is_ready(entry))
    }

    /// Marks the next ready entry in flight and returns it.
    pub fn claim_next(&mut self) -> Option<&OutboxEntry> {
        let index = self.entries.iter().position(|entry| self.is_ready(entry))?;
        self.entries[index].set_scheduler_state(SchedulerState::InFlight);
        Some(&self.entries[index])
    }

    /// Records a local cancel request.
    ///
    /// The outbox writes no receipt.
    ///
    /// # Errors
    ///
    /// Returns [`OutboxError::NotFound`] for an unknown key. Returns
    /// [`OutboxError::KeyReuse`] for a different body. Returns
    /// [`OutboxError::TerminalEntry`] after a terminal authority verdict.
    pub fn request_cancel(
        &mut self,
        key: &IdempotencyKey,
        body_digest: RequestDigest,
    ) -> Result<&OutboxEntry, OutboxError> {
        let index = self.index_of(key)?;
        if self.entries[index].body_digest() != body_digest {
            return Err(OutboxError::KeyReuse { key: key.clone() });
        }
        if let Some(outcome) = self.entries[index].terminal_outcome() {
            return Err(OutboxError::TerminalEntry {
                state: outcome.state(),
            });
        }
        if self.entries[index].disposition() != LocalDisposition::Abandoned {
            self.entries[index].set_disposition(LocalDisposition::CancelRequested);
        }
        Ok(&self.entries[index])
    }

    /// Closes one entry locally with the `Abandoned` disposition.
    ///
    /// The outbox writes no receipt. A late authority verdict can still
    /// reconcile the entry.
    ///
    /// # Errors
    ///
    /// Returns [`OutboxError::NotFound`] for an unknown key. Returns
    /// [`OutboxError::KeyReuse`] for a different body. Returns
    /// [`OutboxError::TerminalEntry`] after a terminal authority verdict.
    pub fn mark_abandoned(
        &mut self,
        key: &IdempotencyKey,
        body_digest: RequestDigest,
    ) -> Result<&OutboxEntry, OutboxError> {
        let index = self.index_of(key)?;
        if self.entries[index].body_digest() != body_digest {
            return Err(OutboxError::KeyReuse { key: key.clone() });
        }
        if let Some(outcome) = self.entries[index].terminal_outcome() {
            return Err(OutboxError::TerminalEntry {
                state: outcome.state(),
            });
        }
        self.entries[index].set_disposition(LocalDisposition::Abandoned);
        self.entries[index].set_scheduler_state(SchedulerState::Terminal);
        Ok(&self.entries[index])
    }

    /// Applies one authority receipt to the outbox.
    ///
    /// A late authority verdict wins for the data. An abandoned entry that
    /// the authority commits returns [`Reconciliation::Terminal`] with
    /// [`ReceiptOutcome::Committed`].
    ///
    /// # Errors
    ///
    /// Returns [`OutboxError::NotFound`] for an unknown key. Returns
    /// [`OutboxError::KeyReuse`] for a different body. Returns
    /// [`OutboxError::Immutable`] for a second terminal verdict that differs
    /// from the stored one. Returns [`OutboxError::MissingOutcome`] when a
    /// terminal envelope lacks its outcome fields.
    pub fn reconcile(
        &mut self,
        receipt: &ReceiptEnvelope,
        body_digest: RequestDigest,
    ) -> Result<Reconciliation, OutboxError> {
        let key = receipt.idempotency_key();
        let index = self.index_of(key)?;
        if self.entries[index].body_digest() != body_digest {
            return Err(OutboxError::KeyReuse { key: key.clone() });
        }
        if !receipt.is_terminal() {
            if self.entries[index].scheduler_state() == SchedulerState::Queued
                && self.entries[index].disposition() == LocalDisposition::Queued
            {
                self.entries[index].set_scheduler_state(SchedulerState::InFlight);
            }
            return Ok(Reconciliation::Queued);
        }
        let outcome = receipt_outcome(receipt).ok_or(OutboxError::MissingOutcome {
            state: receipt.state(),
        })?;
        if let Some(current) = self.entries[index].terminal_outcome() {
            if current == outcome {
                return Ok(Reconciliation::Terminal(outcome));
            }
            return Err(OutboxError::Immutable {
                current: current.state(),
                next: receipt.state(),
            });
        }
        self.entries[index].set_terminal_outcome(Some(outcome));
        self.entries[index].set_scheduler_state(SchedulerState::Terminal);
        Ok(Reconciliation::Terminal(outcome))
    }

    /// Removes one terminal entry after the acknowledgement marker is durable.
    ///
    /// # Errors
    ///
    /// Returns [`OutboxError::NotFound`] for an unknown key. Returns
    /// [`OutboxError::NotTerminal`] when the scheduler still holds the entry.
    pub fn acknowledge(&mut self, key: &IdempotencyKey) -> Result<OutboxEntry, OutboxError> {
        let index = self.index_of(key)?;
        let state = self.entries[index].scheduler_state();
        if !state.is_terminal() {
            return Err(OutboxError::NotTerminal { state });
        }
        Ok(self.entries.remove(index))
    }

    fn index_of(&self, key: &IdempotencyKey) -> Result<usize, OutboxError> {
        self.entries
            .iter()
            .position(|entry| entry.idempotency_key() == key)
            .ok_or_else(|| OutboxError::NotFound { key: key.clone() })
    }

    fn is_ready(&self, candidate: &OutboxEntry) -> bool {
        if candidate.disposition() != LocalDisposition::Queued
            || candidate.scheduler_state() != SchedulerState::Queued
        {
            return false;
        }
        for dependency in candidate.dependencies() {
            if self
                .entry(dependency)
                .is_some_and(|entry| entry.scheduler_state() != SchedulerState::Terminal)
            {
                return false;
            }
        }
        self.entries.iter().all(|other| {
            if !candidate.shares_key_with(other) {
                return true;
            }
            if other.scheduler_state() == SchedulerState::InFlight {
                return false;
            }
            !(other.arrival() < candidate.arrival()
                && other.scheduler_state() != SchedulerState::Terminal)
        })
    }
}
