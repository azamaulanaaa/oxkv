//! Asynchronous audit witness delivery.
//!
//! The commit path appends a checkpoint to a queue. The queue does not block.
//! A separate worker drains the queue and delivers checkpoints to the external
//! witness.

use std::collections::VecDeque;

use crate::audit::AuditCheckpoint;

/// A non-blocking checkpoint sink.
///
/// An implementation must not block. The commit path calls this method after
/// the write is durable.
pub trait WitnessSink {
    /// Queues one checkpoint for asynchronous delivery.
    fn enqueue(&mut self, checkpoint: AuditCheckpoint);
}

/// A queue that models the external witness.
#[derive(Debug, Clone, Default)]
pub struct WitnessQueue {
    pending: VecDeque<AuditCheckpoint>,
    delivered: Vec<AuditCheckpoint>,
}

impl WitnessQueue {
    /// Creates an empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of checkpoints that wait for delivery.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Returns the number of delivered checkpoints.
    #[must_use]
    pub fn delivered_len(&self) -> usize {
        self.delivered.len()
    }

    /// Returns the pending checkpoints.
    #[must_use]
    pub fn pending(&self) -> &VecDeque<AuditCheckpoint> {
        &self.pending
    }

    /// Returns the delivered checkpoints.
    #[must_use]
    pub fn delivered(&self) -> &[AuditCheckpoint] {
        &self.delivered
    }

    /// Returns true when no checkpoint waits for delivery.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Delivers one pending checkpoint.
    pub fn deliver_next(&mut self) -> Option<AuditCheckpoint> {
        let checkpoint = self.pending.pop_front()?;
        self.delivered.push(checkpoint.clone());
        Some(checkpoint)
    }

    /// Delivers every pending checkpoint.
    pub fn deliver_all(&mut self) -> Vec<AuditCheckpoint> {
        let mut delivered = Vec::new();
        while let Some(checkpoint) = self.deliver_next() {
            delivered.push(checkpoint);
        }
        delivered
    }
}

impl WitnessSink for WitnessQueue {
    fn enqueue(&mut self, checkpoint: AuditCheckpoint) {
        self.pending.push_back(checkpoint);
    }
}
