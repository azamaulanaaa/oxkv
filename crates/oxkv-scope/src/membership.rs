//! Exact scope membership.
//!
//! Membership is exact at a complete coverage pin. One add enters the scope and
//! one remove leaves it. A record that re-enters receives a new add. The crate
//! stores logical members only. It does not store pages or the search index.
//!
//! A table range includes the system sub-namespace. Membership rejects a
//! system key at the row level, because a user scope admits user keys only.

use std::collections::BTreeMap;

use oxkv_contracts::version::{
    KeyVersion, PolicyVersion, ProjectionEpoch, ProjectionWatermarks, ScopeIdentity,
    ScopeVersionVector,
};

use crate::declaration::ScopeStats;
use crate::diff::{Churn, DiffInterval, DiffRecord};
use crate::error::ScopeError;
use crate::version::ScopeVersion;

/// The policy version and projection behind one materialization.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeMaterialization {
    /// The scope identity.
    pub scope_identity: ScopeIdentity,
    /// The policy version used to filter records.
    pub policy_version: PolicyVersion,
    /// The projection epoch.
    pub projection_epoch: ProjectionEpoch,
    /// The projection watermarks per fact source.
    pub projection_watermarks: ProjectionWatermarks,
}

impl ScopeMaterialization {
    /// Creates one materialization record.
    #[must_use]
    pub const fn new(
        scope_identity: ScopeIdentity,
        policy_version: PolicyVersion,
        projection_epoch: ProjectionEpoch,
        projection_watermarks: ProjectionWatermarks,
    ) -> Self {
        Self {
            scope_identity,
            policy_version,
            projection_epoch,
            projection_watermarks,
        }
    }
}

/// One logical scope member.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Member {
    /// The record key.
    pub key: Vec<u8>,
    /// The version of the record.
    pub key_version: KeyVersion,
    /// The logical byte count of the record.
    pub byte_len: u64,
}

/// The result of one read at a local pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalRead<'a> {
    /// The record is a member.
    Present(&'a Member),
    /// The record is not a member at the pin.
    Absent,
    /// The local pin does not include the requested pin.
    NotHydrated {
        /// The requested pin.
        requested: ScopeVersionVector,
        /// The local pin.
        local: ScopeVersionVector,
    },
    /// The requested pin is behind the local pin and is not retained.
    NotResident {
        /// The requested pin.
        requested: ScopeVersionVector,
        /// The local pin.
        local: ScopeVersionVector,
    },
}

impl LocalRead<'_> {
    /// Returns true when the read found a member.
    #[must_use]
    pub const fn is_present(&self) -> bool {
        matches!(self, Self::Present(_))
    }

    /// Returns true when the read is a known absence.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

/// The exact logical membership of one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    materialization: ScopeMaterialization,
    version: ScopeVersion,
    members: BTreeMap<Vec<u8>, Member>,
    bytes: u64,
    churn: Vec<Churn>,
}

impl Membership {
    /// Creates an empty membership.
    #[must_use]
    pub fn new(materialization: ScopeMaterialization) -> Self {
        Self {
            materialization,
            version: ScopeVersion::new(),
            members: BTreeMap::new(),
            bytes: 0,
            churn: Vec::new(),
        }
    }

    /// Returns the materialization record.
    #[must_use]
    pub const fn materialization(&self) -> &ScopeMaterialization {
        &self.materialization
    }

    /// Returns the scope identity.
    #[must_use]
    pub const fn identity(&self) -> ScopeIdentity {
        self.materialization.scope_identity
    }

    /// Returns the local pin.
    #[must_use]
    pub const fn local_pin(&self) -> &ScopeVersionVector {
        self.version.as_vector()
    }

    /// Returns the scope version.
    #[must_use]
    pub const fn version(&self) -> &ScopeVersion {
        &self.version
    }

    /// Returns the member count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Returns true when the scope has no member.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Returns the member byte count.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Returns the scope statistics for one shard count.
    #[must_use]
    pub fn stats(&self, shard_count: u64) -> ScopeStats {
        ScopeStats::new(
            u64::try_from(self.members.len()).unwrap_or(u64::MAX),
            self.bytes,
            shard_count,
        )
    }

    /// Returns one member.
    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<&Member> {
        self.members.get(key)
    }

    /// Returns true when the key is a member.
    #[must_use]
    pub fn contains(&self, key: &[u8]) -> bool {
        self.members.contains_key(key)
    }

    /// Iterates over the members in canonical key order.
    pub fn iter(&self) -> impl Iterator<Item = &Member> {
        self.members.values()
    }

    /// Returns the churn for every applied interval.
    #[must_use]
    pub fn churn(&self) -> &[Churn] {
        &self.churn
    }

    /// Applies one diff interval at one target version.
    ///
    /// The method checks every record before it changes the membership. A
    /// failed interval leaves the membership and the version unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::IdentityMismatch`] for another scope identity.
    /// Returns [`ScopeError::BaseVersionMismatch`] for a wrong base vector.
    /// Returns [`ScopeError::VersionRegression`] for a decreasing target.
    /// Returns [`ScopeError::RemoveUnknownMember`] for a remove of a non-member.
    /// Returns [`ScopeError::SystemKey`] for a system key.
    /// Returns [`ScopeError::ByteCountOverflow`] for an overflowing byte count.
    pub fn apply(&mut self, interval: &DiffInterval) -> Result<Churn, ScopeError> {
        if interval.scope_identity() != self.identity() {
            return Err(ScopeError::IdentityMismatch {
                expected: self.identity(),
                found: interval.scope_identity(),
            });
        }
        if !self.version.same_as(interval.base()) {
            return Err(ScopeError::BaseVersionMismatch);
        }
        for record in interval.records() {
            if crate::key::is_system_key(record.key()) {
                return Err(ScopeError::SystemKey {
                    key: record.key().to_vec(),
                });
            }
        }
        let mut next_version = self.version.clone();
        next_version.advance_to(interval.target())?;
        let mut members = self.members.clone();
        let mut bytes = self.bytes;
        for record in interval.records() {
            match record {
                DiffRecord::Add(record) => {
                    if let Some(previous) = members.remove(&record.key) {
                        bytes = bytes
                            .checked_sub(previous.byte_len)
                            .ok_or(ScopeError::ByteCountOverflow)?;
                    }
                    bytes = bytes
                        .checked_add(record.byte_len)
                        .ok_or(ScopeError::ByteCountOverflow)?;
                    members.insert(
                        record.key.clone(),
                        Member {
                            key: record.key.clone(),
                            key_version: record.key_version,
                            byte_len: record.byte_len,
                        },
                    );
                }
                DiffRecord::Remove(record) => {
                    let previous = members.remove(&record.key).ok_or_else(|| {
                        ScopeError::RemoveUnknownMember {
                            key: record.key.clone(),
                        }
                    })?;
                    bytes = bytes
                        .checked_sub(previous.byte_len)
                        .ok_or(ScopeError::ByteCountOverflow)?;
                }
            }
        }
        self.members = members;
        self.bytes = bytes;
        self.version = next_version;
        self.materialization.policy_version = interval.policy_version();
        let churn = interval.churn();
        self.churn.push(churn.clone());
        Ok(churn)
    }

    /// Reads one key at the local pin.
    ///
    /// A missing key is a known absence. It is not an unreplicated key.
    #[must_use]
    pub fn read_local(&self, key: &[u8]) -> LocalRead<'_> {
        match self.members.get(key) {
            Some(member) => LocalRead::Present(member),
            None => LocalRead::Absent,
        }
    }

    /// Reads one key at a requested pin.
    ///
    /// The method answers only at the local pin. A higher pin is not hydrated.
    /// A lower pin is not resident.
    #[must_use]
    pub fn read_at(&self, key: &[u8], pin: &ScopeVersionVector) -> LocalRead<'_> {
        if self.version.same_as(pin) {
            return self.read_local(key);
        }
        if self.version.entails(pin) {
            LocalRead::NotResident {
                requested: pin.clone(),
                local: self.version.as_vector().clone(),
            }
        } else {
            LocalRead::NotHydrated {
                requested: pin.clone(),
                local: self.version.as_vector().clone(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffRecord, RemoveReason};
    use oxkv_contracts::version::{CommitVersion, ShardId, VersionVector};

    const USER_KEY: &[u8] = b"\x01accounts\x00\x10alice\x00\x00";
    const SYSTEM_KEY: &[u8] = b"\x01accounts\x00\xff\x01\x10receipt\x00\x00";

    fn identity() -> ScopeIdentity {
        ScopeIdentity::from_bytes([3; 32])
    }

    fn materialization() -> ScopeMaterialization {
        ScopeMaterialization::new(
            identity(),
            PolicyVersion::from_bytes([1; 32]),
            ProjectionEpoch::new(1),
            ProjectionWatermarks::new(),
        )
    }

    fn vector(entries: &[(&[u8], u64)]) -> ScopeVersionVector {
        let mut vector = VersionVector::new();
        for (shard, version) in entries {
            vector.set(
                ShardId::new((*shard).to_vec()),
                CommitVersion::new(*version),
            );
        }
        ScopeVersionVector::from_vector(vector)
    }

    fn interval(
        interval: u64,
        policy_byte: u8,
        base: ScopeVersionVector,
        target: ScopeVersionVector,
        records: Vec<DiffRecord>,
    ) -> DiffInterval {
        DiffInterval::new(
            identity(),
            interval,
            PolicyVersion::from_bytes([policy_byte; 32]),
            base,
            target,
            records,
        )
        .expect("the interval is valid")
    }

    #[test]
    fn an_add_and_a_remove_change_membership() {
        let mut membership = Membership::new(materialization());
        membership
            .apply(&interval(
                1,
                1,
                ScopeVersionVector::new(),
                vector(&[(&[1], 1)]),
                vec![
                    DiffRecord::add(b"a".to_vec(), KeyVersion::new(1), 10),
                    DiffRecord::add(b"b".to_vec(), KeyVersion::new(1), 20),
                ],
            ))
            .expect("the interval applies");
        assert_eq!(membership.len(), 2);
        assert_eq!(membership.bytes(), 30);
        membership
            .apply(&interval(
                2,
                1,
                vector(&[(&[1], 1)]),
                vector(&[(&[1], 2)]),
                vec![DiffRecord::remove(
                    b"a".to_vec(),
                    KeyVersion::new(2),
                    RemoveReason::RecordChanged,
                )],
            ))
            .expect("the interval applies");
        assert_eq!(membership.len(), 1);
        assert_eq!(membership.bytes(), 20);
        assert_eq!(membership.read_local(b"a"), LocalRead::Absent);
        assert_eq!(membership.churn()[1].removes, 1);
    }

    #[test]
    fn a_record_that_reenters_gets_a_new_add() {
        let mut membership = Membership::new(materialization());
        membership
            .apply(&interval(
                1,
                1,
                ScopeVersionVector::new(),
                vector(&[(&[1], 1)]),
                vec![DiffRecord::add(b"a".to_vec(), KeyVersion::new(1), 1)],
            ))
            .expect("the add applies");
        membership
            .apply(&interval(
                2,
                1,
                vector(&[(&[1], 1)]),
                vector(&[(&[1], 2)]),
                vec![DiffRecord::remove(
                    b"a".to_vec(),
                    KeyVersion::new(2),
                    RemoveReason::RecordChanged,
                )],
            ))
            .expect("the remove applies");
        membership
            .apply(&interval(
                3,
                1,
                vector(&[(&[1], 2)]),
                vector(&[(&[1], 3)]),
                vec![DiffRecord::add(b"a".to_vec(), KeyVersion::new(3), 1)],
            ))
            .expect("the second add applies");
        assert_eq!(membership.churn()[2].adds, 1);
        assert_eq!(
            membership.get(b"a").map(|member| member.key_version),
            Some(KeyVersion::new(3))
        );
    }

    #[test]
    fn a_remove_of_a_non_member_fails_without_a_change() {
        let mut membership = Membership::new(materialization());
        let result = membership.apply(&interval(
            1,
            1,
            ScopeVersionVector::new(),
            vector(&[(&[1], 1)]),
            vec![DiffRecord::remove(
                b"a".to_vec(),
                KeyVersion::new(1),
                RemoveReason::RecordChanged,
            )],
        ));
        assert!(matches!(
            result,
            Err(ScopeError::RemoveUnknownMember { .. })
        ));
        assert!(membership.is_empty());
        assert!(membership.local_pin().is_empty());
    }

    #[test]
    fn a_version_regression_fails_without_a_change() {
        let mut membership = Membership::new(materialization());
        membership
            .apply(&interval(
                1,
                1,
                ScopeVersionVector::new(),
                vector(&[(&[1], 4)]),
                Vec::new(),
            ))
            .expect("the first interval applies");
        let result = DiffInterval::new(
            identity(),
            2,
            PolicyVersion::from_bytes([1; 32]),
            vector(&[(&[1], 4)]),
            vector(&[(&[1], 3)]),
            Vec::new(),
        );
        assert!(matches!(result, Err(ScopeError::VersionRegression { .. })));
        assert_eq!(membership.local_pin(), &vector(&[(&[1], 4)]));
    }

    #[test]
    fn an_offline_read_distinguishes_absence_from_lag() {
        let mut membership = Membership::new(materialization());
        membership
            .apply(&interval(
                1,
                1,
                ScopeVersionVector::new(),
                vector(&[(&[1], 1)]),
                Vec::new(),
            ))
            .expect("the interval applies");
        assert!(matches!(
            membership.read_at(b"missing", &vector(&[(&[1], 1)])),
            LocalRead::Absent
        ));
        assert!(matches!(
            membership.read_at(b"missing", &vector(&[(&[1], 2)])),
            LocalRead::NotHydrated { .. }
        ));
    }

    #[test]
    fn a_policy_change_updates_the_materialization() {
        let mut membership = Membership::new(materialization());
        membership
            .apply(&interval(
                1,
                2,
                ScopeVersionVector::new(),
                vector(&[(&[1], 1)]),
                Vec::new(),
            ))
            .expect("the interval applies");
        assert_eq!(
            membership.materialization().policy_version,
            PolicyVersion::from_bytes([2; 32])
        );
    }

    #[test]
    fn a_real_system_key_is_rejected_in_membership() {
        let mut membership = Membership::new(materialization());
        let error = membership
            .apply(&interval(
                1,
                1,
                ScopeVersionVector::new(),
                vector(&[(&[1], 1)]),
                vec![DiffRecord::add(SYSTEM_KEY.to_vec(), KeyVersion::new(1), 1)],
            ))
            .expect_err("the system key is rejected");
        assert!(matches!(error, ScopeError::SystemKey { .. }));
        assert_eq!("scope.system_key_rejected", error.code());
        assert!(membership.is_empty());
        assert!(membership.local_pin().is_empty());
    }

    #[test]
    fn a_real_user_key_is_accepted_in_membership() {
        let mut membership = Membership::new(materialization());
        membership
            .apply(&interval(
                1,
                1,
                ScopeVersionVector::new(),
                vector(&[(&[1], 1)]),
                vec![DiffRecord::add(USER_KEY.to_vec(), KeyVersion::new(1), 10)],
            ))
            .expect("the user key is accepted");
        assert!(membership.contains(USER_KEY));
        assert_eq!(membership.len(), 1);
    }
}
