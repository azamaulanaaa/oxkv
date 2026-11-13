//! Outbox admission rules.
//!
//! Admission checks the size budget and every explicit dependency edge. An
//! edge must name an earlier entry in the same shard. The outbox never infers
//! an edge and never drops an older queued entry.

use crate::entry::OutboxEntry;

/// An error from outbox admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    /// The outbox reached its size budget.
    #[error("The outbox is full.")]
    Full,
    /// The dependency edge is absent, forward, or circular.
    #[error("The dependency edge forms a cycle.")]
    DependencyCycle,
    /// The dependency edge names an entry in another shard.
    #[error("The dependency edge crosses a shard boundary.")]
    CrossShardEdge,
    /// The idempotency key already names a different body.
    #[error("The idempotency key names a different body.")]
    KeyReuse,
}

impl AdmissionError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Full => "outbox.full",
            Self::DependencyCycle => "outbox.dependency_cycle",
            Self::CrossShardEdge => "outbox.cross_shard_edge",
            Self::KeyReuse => "write.idempotency_key_reuse",
        }
    }
}

/// Checks the size budget.
pub(crate) fn check_capacity(entry_count: usize, capacity: usize) -> Result<(), AdmissionError> {
    if entry_count >= capacity {
        return Err(AdmissionError::Full);
    }
    Ok(())
}

/// Checks every dependency edge of one candidate entry.
///
/// An edge must name an earlier entry in the same shard. An absent or forward
/// reference cannot resolve, so admission reports it as a cycle.
pub(crate) fn check_dependencies(
    candidate: &OutboxEntry,
    entries: &[OutboxEntry],
) -> Result<(), AdmissionError> {
    for dependency in candidate.dependencies() {
        if dependency == candidate.idempotency_key() {
            return Err(AdmissionError::DependencyCycle);
        }
        let Some(earlier) = entries
            .iter()
            .find(|entry| entry.idempotency_key() == dependency)
        else {
            return Err(AdmissionError::DependencyCycle);
        };
        if earlier.target_shard() != candidate.target_shard() {
            return Err(AdmissionError::CrossShardEdge);
        }
        if earlier.arrival() >= candidate.arrival() {
            return Err(AdmissionError::DependencyCycle);
        }
    }
    Ok(())
}
