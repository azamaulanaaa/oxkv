//! Hash-linked audit records for the catalog shard.
//!
//! Every rule change appends one audit record. The record names the actor,
//! role, policy identity, both policy versions, cohort, result, reason, and
//! dry-run digest. The chain is append-only and hash-linked.

use oxkv_contracts::receipt::{AuditReference, AuditSequence, ReceiptState};
use oxkv_contracts::version::{DigestDomain, Hash32, PolicyVersion, ShardId};

use crate::canonical;
use crate::catalog::catalog_shard_id;
use crate::lifecycle::{Cohort, Role};
use crate::policy::PolicyId;

/// One audit entry before the chain adds its sequence and hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// The authenticated actor.
    pub actor: String,
    /// The actor role.
    pub role: Role,
    /// The policy identity.
    pub policy_id: PolicyId,
    /// The policy version before the change.
    pub from_version: PolicyVersion,
    /// The policy version after the change.
    pub to_version: PolicyVersion,
    /// The cohort.
    pub cohort: Cohort,
    /// The change result.
    pub result: ReceiptState,
    /// The reason code for a rejected or conflicted change.
    pub reason: Option<&'static str>,
    /// The dry-run digest that joined the approval.
    pub dry_run_digest: Option<oxkv_contracts::version::ContentDigest>,
}

/// One immutable audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyAuditRecord {
    sequence: AuditSequence,
    shard: ShardId,
    entry: AuditEntry,
    previous_hash: Hash32,
    record_hash: Hash32,
}

impl PolicyAuditRecord {
    /// Returns the sequence in the per-shard chain.
    #[must_use]
    pub const fn sequence(&self) -> AuditSequence {
        self.sequence
    }

    /// Returns the catalog shard.
    #[must_use]
    pub const fn shard(&self) -> &ShardId {
        &self.shard
    }

    /// Returns the entry.
    #[must_use]
    pub const fn entry(&self) -> &AuditEntry {
        &self.entry
    }

    /// Returns the previous record hash.
    #[must_use]
    pub const fn previous_hash(&self) -> Hash32 {
        self.previous_hash
    }

    /// Returns the record hash.
    #[must_use]
    pub const fn record_hash(&self) -> Hash32 {
        self.record_hash
    }

    /// Returns a receipt audit reference for this record.
    #[must_use]
    pub fn reference(&self) -> AuditReference {
        AuditReference::new(self.shard.clone(), self.sequence, self.record_hash)
    }

    fn recompute(&self) -> Hash32 {
        let mut bytes = Vec::new();
        canonical::push_bytes(&mut bytes, self.previous_hash.as_bytes());
        canonical::push_str(&mut bytes, &self.entry.actor);
        canonical::push_str(&mut bytes, self.entry.role.name());
        canonical::push_str(&mut bytes, self.entry.policy_id.as_str());
        canonical::push_bytes(&mut bytes, self.entry.from_version.as_bytes());
        canonical::push_bytes(&mut bytes, self.entry.to_version.as_bytes());
        canonical::push_str(&mut bytes, self.entry.cohort.id());
        canonical::push_str(&mut bytes, self.entry.cohort.kind().name());
        canonical::push_tag(&mut bytes, self.entry.result.wire_tag());
        canonical::push_str(&mut bytes, self.entry.reason.unwrap_or(""));
        match self.entry.dry_run_digest {
            Some(digest) => {
                canonical::push_bool(&mut bytes, true);
                canonical::push_bytes(&mut bytes, digest.as_bytes());
            }
            None => canonical::push_bool(&mut bytes, false),
        }
        Hash32::of_domain(DigestDomain::AuditRecord, &bytes)
    }
}

/// The append-only audit log of the catalog shard.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyAuditLog {
    records: Vec<PolicyAuditRecord>,
}

impl PolicyAuditLog {
    /// Creates an empty audit log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one entry and returns the record.
    pub fn append(&mut self, entry: AuditEntry) -> PolicyAuditRecord {
        let sequence = AuditSequence::new(self.records.len() as u64 + 1);
        let previous_hash = self.records.last().map_or_else(
            || Hash32::from_bytes([0; 32]),
            PolicyAuditRecord::record_hash,
        );
        let mut record = PolicyAuditRecord {
            sequence,
            shard: catalog_shard_id(),
            entry,
            previous_hash,
            record_hash: Hash32::from_bytes([0; 32]),
        };
        record.record_hash = record.recompute();
        self.records.push(record.clone());
        record
    }

    /// Returns the record count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns true when the log has no record.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Returns every record in append order.
    #[must_use]
    pub fn records(&self) -> &[PolicyAuditRecord] {
        &self.records
    }

    /// Verifies every chain link and record hash.
    #[must_use]
    pub fn verify(&self) -> bool {
        let mut previous = Hash32::from_bytes([0; 32]);
        for record in &self.records {
            if record.previous_hash != previous || record.record_hash != record.recompute() {
                return false;
            }
            previous = record.record_hash;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::CohortKind;

    fn entry() -> AuditEntry {
        AuditEntry {
            actor: "operator-1".to_owned(),
            role: Role::PlatformOperator,
            policy_id: PolicyId::parse("tenant_policy").expect("the policy identity is valid"),
            from_version: PolicyVersion::from_bytes([1; 32]),
            to_version: PolicyVersion::from_bytes([2; 32]),
            cohort: Cohort::new("tenant-a", CohortKind::Canary),
            result: ReceiptState::Committed,
            reason: None,
            dry_run_digest: None,
        }
    }

    #[test]
    fn an_appended_chain_verifies() {
        let mut log = PolicyAuditLog::new();
        let first = log.append(entry());
        let second = log.append(entry());
        assert_eq!(first.sequence(), AuditSequence::new(1));
        assert_eq!(second.sequence(), AuditSequence::new(2));
        assert_eq!(second.previous_hash(), first.record_hash());
        assert!(log.verify());
    }

    #[test]
    fn the_record_hash_covers_the_entry() {
        let mut log = PolicyAuditLog::new();
        let first = log.append(entry());
        let mut other_entry = entry();
        other_entry.actor = "operator-2".to_owned();
        let second = log.append(other_entry);
        assert_ne!(first.record_hash(), second.record_hash());
    }
}
