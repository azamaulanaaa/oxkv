//! Exact and coverage result modes.
//!
//! An exact result requires every targeted shard to serve the pin. A coverage
//! result carries an explicit coverage descriptor. A caller cannot infer
//! completeness from a row count.
//!
//! A table range includes the system sub-namespace. The query filters a system
//! key at the row level, because a user scope admits user keys only.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::version::{
    CommitVersion, KeyVersion, ManifestDigest, MapVersion, PolicyVersion, ScopeIdentity,
    ScopeVersionVector, ShardId, VersionVector,
};

use crate::error::QueryError;
use crate::plan::ShardPlan;

/// The mode of one query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryMode {
    /// Every targeted shard must serve the pin.
    Exact,
    /// The query returns a coverage descriptor.
    Coverage,
}

/// The mode of one result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResultMode {
    /// Every targeted shard served the pin.
    Exact,
    /// The result carries a coverage descriptor.
    Coverage,
}

impl ResultMode {
    /// Returns the mode name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Coverage => "coverage",
        }
    }
}

/// The scope pin.
///
/// The pin is the tuple of manifest digest, policy version, map version, and
/// scope identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopePin {
    manifest_digest: ManifestDigest,
    policy_version: PolicyVersion,
    map_version: MapVersion,
    scope_identity: ScopeIdentity,
}

impl ScopePin {
    /// Creates a scope pin.
    #[must_use]
    pub const fn new(
        manifest_digest: ManifestDigest,
        policy_version: PolicyVersion,
        map_version: MapVersion,
        scope_identity: ScopeIdentity,
    ) -> Self {
        Self {
            manifest_digest,
            policy_version,
            map_version,
            scope_identity,
        }
    }

    /// Returns the manifest digest.
    #[must_use]
    pub const fn manifest_digest(self) -> ManifestDigest {
        self.manifest_digest
    }

    /// Returns the policy version.
    #[must_use]
    pub const fn policy_version(self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the map version.
    #[must_use]
    pub const fn map_version(self) -> MapVersion {
        self.map_version
    }

    /// Returns the scope identity.
    #[must_use]
    pub const fn scope_identity(self) -> ScopeIdentity {
        self.scope_identity
    }
}

/// One row in a scope result.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeRow {
    /// The record key.
    pub key: Vec<u8>,
    /// The version of the record.
    pub key_version: KeyVersion,
}

impl ScopeRow {
    /// Creates one scope row.
    #[must_use]
    pub const fn new(key: Vec<u8>, key_version: KeyVersion) -> Self {
        Self { key, key_version }
    }
}

/// One shard response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShardResponse {
    /// The shard served one commit version.
    Served {
        /// The shard identity.
        shard_id: ShardId,
        /// The served commit version.
        commit_version: CommitVersion,
        /// The rows at the served version.
        rows: Vec<ScopeRow>,
    },
    /// The shard is unreachable.
    Unreachable {
        /// The shard identity.
        shard_id: ShardId,
    },
}

impl ShardResponse {
    /// Creates a served response.
    #[must_use]
    pub const fn served(
        shard_id: ShardId,
        commit_version: CommitVersion,
        rows: Vec<ScopeRow>,
    ) -> Self {
        Self::Served {
            shard_id,
            commit_version,
            rows,
        }
    }

    /// Creates an unreachable response.
    #[must_use]
    pub const fn unreachable(shard_id: ShardId) -> Self {
        Self::Unreachable { shard_id }
    }

    /// Returns the shard identity.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        match self {
            Self::Served { shard_id, .. } | Self::Unreachable { shard_id } => shard_id,
        }
    }
}

/// One answered shard.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShardServed {
    /// The shard identity.
    pub shard_id: ShardId,
    /// The served commit version.
    pub commit_version: CommitVersion,
}

/// The coverage descriptor of one coverage result.
///
/// The descriptor lists the answered shards, the missing shards, and the
/// served versions. A complete descriptor does not change the result mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageDescriptor {
    answered: Vec<ShardServed>,
    missing: Vec<ShardId>,
    served: ScopeVersionVector,
}

impl CoverageDescriptor {
    fn new(answered: Vec<ShardServed>, missing: Vec<ShardId>, served: ScopeVersionVector) -> Self {
        Self {
            answered,
            missing,
            served,
        }
    }

    /// Returns the answered shards in shard order.
    #[must_use]
    pub fn answered(&self) -> &[ShardServed] {
        &self.answered
    }

    /// Returns the missing shards in shard order.
    #[must_use]
    pub fn missing(&self) -> &[ShardId] {
        &self.missing
    }

    /// Returns the served versions.
    #[must_use]
    pub const fn served(&self) -> &ScopeVersionVector {
        &self.served
    }

    /// Returns true when every targeted shard answered at the pin.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// An exact result.
///
/// The type exists only when every targeted shard served the pin. It has no
/// coverage descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactResult {
    identity: ScopeIdentity,
    pin: ScopeVersionVector,
    rows: Vec<ScopeRow>,
}

impl ExactResult {
    fn new(identity: ScopeIdentity, pin: ScopeVersionVector, rows: Vec<ScopeRow>) -> Self {
        Self {
            identity,
            pin,
            rows,
        }
    }

    /// Returns the result mode.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn mode(&self) -> ResultMode {
        ResultMode::Exact
    }

    /// Returns true because this is an exact result.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn claims_exact(&self) -> bool {
        true
    }

    /// Returns the scope identity.
    #[must_use]
    pub const fn identity(&self) -> ScopeIdentity {
        self.identity
    }

    /// Returns the pin.
    #[must_use]
    pub const fn pin(&self) -> &ScopeVersionVector {
        &self.pin
    }

    /// Returns the rows in sync order.
    #[must_use]
    pub fn rows(&self) -> &[ScopeRow] {
        &self.rows
    }

    /// Returns the row count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Returns true when the result has no row.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// A coverage result.
///
/// The type carries a coverage descriptor. It never claims an exact result,
/// even when the descriptor is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageResult {
    identity: ScopeIdentity,
    requested_pin: ScopeVersionVector,
    descriptor: CoverageDescriptor,
    rows: Vec<ScopeRow>,
}

impl CoverageResult {
    fn new(
        identity: ScopeIdentity,
        requested_pin: ScopeVersionVector,
        descriptor: CoverageDescriptor,
        rows: Vec<ScopeRow>,
    ) -> Self {
        Self {
            identity,
            requested_pin,
            descriptor,
            rows,
        }
    }

    /// Returns the result mode.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn mode(&self) -> ResultMode {
        ResultMode::Coverage
    }

    /// Returns false because a coverage result never claims exactness.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn claims_exact(&self) -> bool {
        false
    }

    /// Returns the scope identity.
    #[must_use]
    pub const fn identity(&self) -> ScopeIdentity {
        self.identity
    }

    /// Returns the requested pin.
    #[must_use]
    pub const fn requested_pin(&self) -> &ScopeVersionVector {
        &self.requested_pin
    }

    /// Returns the coverage descriptor.
    #[must_use]
    pub const fn descriptor(&self) -> &CoverageDescriptor {
        &self.descriptor
    }

    /// Returns the rows in sync order.
    #[must_use]
    pub fn rows(&self) -> &[ScopeRow] {
        &self.rows
    }

    /// Returns the row count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Returns true when the result has no row.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Returns true when at least one targeted shard is missing.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        !self.descriptor.is_complete()
    }
}

/// The outcome of one query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryOutcome {
    /// An exact result.
    Exact(ExactResult),
    /// A coverage result.
    Coverage(CoverageResult),
}

impl QueryOutcome {
    /// Returns the result mode.
    #[must_use]
    pub const fn mode(&self) -> ResultMode {
        match self {
            Self::Exact(_) => ResultMode::Exact,
            Self::Coverage(_) => ResultMode::Coverage,
        }
    }

    /// Returns the exact result.
    #[must_use]
    pub const fn exact(&self) -> Option<&ExactResult> {
        match self {
            Self::Exact(result) => Some(result),
            Self::Coverage(_) => None,
        }
    }

    /// Returns the coverage result.
    #[must_use]
    pub const fn coverage(&self) -> Option<&CoverageResult> {
        match self {
            Self::Exact(_) => None,
            Self::Coverage(result) => Some(result),
        }
    }
}

impl ShardPlan {
    /// Runs one query over the shard plan.
    ///
    /// Exact mode returns a named error and no data when one shard is
    /// unreachable or below the pin. Coverage mode returns a coverage result.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::PinIncomplete`] when the pin omits a planned shard.
    /// Returns [`QueryError::UnknownShard`] for a response for an unplanned
    /// shard. Returns [`QueryError::DuplicateResponse`] for two responses for
    /// one shard. Returns [`QueryError::ShardUnreachable`] or
    /// [`QueryError::ShardNotAtPin`] in exact mode. Returns
    /// [`QueryError::DuplicateRow`] for two rows with one key.
    pub fn query(
        &self,
        mode: QueryMode,
        pin: &ScopeVersionVector,
        responses: &[ShardResponse],
    ) -> Result<QueryOutcome, QueryError> {
        let mut response_map: BTreeMap<ShardId, Option<(CommitVersion, Vec<ScopeRow>)>> =
            BTreeMap::new();
        for response in responses {
            let shard_id = response.shard_id().clone();
            if self.entry(&shard_id).is_none() {
                return Err(QueryError::UnknownShard { shard_id });
            }
            let value = match response {
                ShardResponse::Served {
                    commit_version,
                    rows,
                    ..
                } => Some((*commit_version, rows.clone())),
                ShardResponse::Unreachable { .. } => None,
            };
            if response_map.insert(shard_id.clone(), value).is_some() {
                return Err(QueryError::DuplicateResponse { shard_id });
            }
        }
        match mode {
            QueryMode::Exact => self.query_exact(pin, &response_map),
            QueryMode::Coverage => self.query_coverage(pin, &response_map),
        }
    }

    fn query_exact(
        &self,
        pin: &ScopeVersionVector,
        response_map: &BTreeMap<ShardId, Option<(CommitVersion, Vec<ScopeRow>)>>,
    ) -> Result<QueryOutcome, QueryError> {
        let mut rows = Vec::new();
        let mut seen = BTreeSet::new();
        for shard_id in self.shards() {
            let requested = pin.get(shard_id).ok_or_else(|| QueryError::PinIncomplete {
                shard_id: shard_id.clone(),
            })?;
            let served = response_map
                .get(shard_id)
                .and_then(Option::as_ref)
                .ok_or_else(|| QueryError::ShardUnreachable {
                    shard_id: shard_id.clone(),
                })?;
            if served.0 != requested {
                return Err(QueryError::ShardNotAtPin {
                    shard_id: shard_id.clone(),
                    requested,
                    served: served.0,
                });
            }
            append_rows(&served.1, &mut seen, &mut rows)?;
        }
        Ok(QueryOutcome::Exact(ExactResult::new(
            self.identity(),
            pin.clone(),
            rows,
        )))
    }

    fn query_coverage(
        &self,
        pin: &ScopeVersionVector,
        response_map: &BTreeMap<ShardId, Option<(CommitVersion, Vec<ScopeRow>)>>,
    ) -> Result<QueryOutcome, QueryError> {
        let mut answered = Vec::new();
        let mut missing = Vec::new();
        let mut served_vector = VersionVector::new();
        let mut rows = Vec::new();
        let mut seen = BTreeSet::new();
        for shard_id in self.shards() {
            let requested = pin.get(shard_id).ok_or_else(|| QueryError::PinIncomplete {
                shard_id: shard_id.clone(),
            })?;
            match response_map.get(shard_id).and_then(Option::as_ref) {
                Some((served, served_rows)) if *served == requested => {
                    answered.push(ShardServed {
                        shard_id: shard_id.clone(),
                        commit_version: *served,
                    });
                    served_vector.set(shard_id.clone(), *served);
                    append_rows(served_rows, &mut seen, &mut rows)?;
                }
                _ => missing.push(shard_id.clone()),
            }
        }
        let descriptor = CoverageDescriptor::new(
            answered,
            missing,
            ScopeVersionVector::from_vector(served_vector),
        );
        Ok(QueryOutcome::Coverage(CoverageResult::new(
            self.identity(),
            pin.clone(),
            descriptor,
            rows,
        )))
    }
}

fn append_rows(
    rows: &[ScopeRow],
    seen: &mut BTreeSet<Vec<u8>>,
    output: &mut Vec<ScopeRow>,
) -> Result<(), QueryError> {
    let mut sorted = rows.to_vec();
    sorted.sort();
    for row in sorted {
        if crate::key::is_system_key(&row.key) {
            return Err(QueryError::SystemKey { key: row.key });
        }
        if !seen.insert(row.key.clone()) {
            return Err(QueryError::DuplicateRow { key: row.key });
        }
        output.push(row);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declaration::{
        AdmissionLimits, Ordering, PolicySelector, ProjectionSelector, RuleSetSelector,
        ScopeDeclaration, TablePrefix,
    };
    use crate::plan::ShardAssignment;
    use oxkv_contracts::keyspace::TableName;

    const USER_KEY: &[u8] = b"\x01accounts\x00\x10alice\x00\x00";
    const SYSTEM_KEY: &[u8] = b"\x01accounts\x00\xff\x01\x10receipt\x00\x00";

    fn plan() -> ShardPlan {
        let declaration = ScopeDeclaration::new(
            [
                TablePrefix::parse("accounts").expect("valid table"),
                TablePrefix::parse("ledger_entry").expect("valid table"),
                TablePrefix::parse("audit_log").expect("valid table"),
            ],
            PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
            ProjectionSelector::all(),
            Ordering::CanonicalKey,
            AdmissionLimits::unbounded(),
        )
        .expect("valid declaration");
        let assignments = [
            ("accounts", 1_u8),
            ("ledger_entry", 2_u8),
            ("audit_log", 3_u8),
        ]
        .map(|(table, shard)| {
            ShardAssignment::new(
                TableName::parse_user(table).expect("valid table"),
                ShardId::new(vec![shard]),
            )
        });
        ShardPlan::build(&declaration, &assignments).expect("the plan builds")
    }

    fn pin(plan: &ShardPlan, version: u64) -> ScopeVersionVector {
        let mut vector = VersionVector::new();
        for shard_id in plan.shards() {
            vector.set(shard_id.clone(), CommitVersion::new(version));
        }
        ScopeVersionVector::from_vector(vector)
    }

    #[test]
    fn an_exact_query_returns_one_vector() {
        let plan = plan();
        let pin = pin(&plan, 7);
        let responses: Vec<ShardResponse> = plan
            .shards()
            .map(|shard| {
                ShardResponse::served(
                    shard.clone(),
                    CommitVersion::new(7),
                    vec![ScopeRow::new(shard.as_bytes().to_vec(), KeyVersion::new(1))],
                )
            })
            .collect();
        let outcome = plan
            .query(QueryMode::Exact, &pin, &responses)
            .expect("the query is exact");
        let exact = outcome.exact().expect("the result is exact");
        assert_eq!(exact.mode(), ResultMode::Exact);
        assert_eq!(exact.pin().as_vector().len(), 3);
        assert_eq!(exact.len(), 3);
    }

    #[test]
    fn an_unreachable_shard_gives_a_coverage_result() {
        let plan = plan();
        let pin = pin(&plan, 7);
        let shard = ShardId::new(vec![2]);
        let responses = vec![
            ShardResponse::served(
                ShardId::new(vec![1]),
                CommitVersion::new(7),
                vec![ScopeRow::new(b"a".to_vec(), KeyVersion::new(1))],
            ),
            ShardResponse::unreachable(shard.clone()),
            ShardResponse::served(
                ShardId::new(vec![3]),
                CommitVersion::new(7),
                vec![ScopeRow::new(b"c".to_vec(), KeyVersion::new(1))],
            ),
        ];
        assert_eq!(
            plan.query(QueryMode::Exact, &pin, &responses)
                .expect_err("exact mode fails")
                .code(),
            "scope.shard_unreachable"
        );
        let outcome = plan
            .query(QueryMode::Coverage, &pin, &responses)
            .expect("coverage mode succeeds");
        let coverage = outcome.coverage().expect("the result is coverage");
        assert_eq!(coverage.mode(), ResultMode::Coverage);
        assert!(!coverage.claims_exact());
        assert!(coverage.is_partial());
        assert_eq!(coverage.descriptor().missing(), &[shard]);
        assert_eq!(coverage.descriptor().answered().len(), 2);
    }

    #[test]
    fn a_shard_below_the_pin_is_missing_in_coverage() {
        let plan = plan();
        let pin = pin(&plan, 7);
        let responses: Vec<ShardResponse> = plan
            .shards()
            .map(|shard| {
                let version = if *shard == ShardId::new(vec![2]) {
                    6
                } else {
                    7
                };
                ShardResponse::served(shard.clone(), CommitVersion::new(version), Vec::new())
            })
            .collect();
        let outcome = plan
            .query(QueryMode::Coverage, &pin, &responses)
            .expect("coverage mode succeeds");
        let coverage = outcome.coverage().expect("the result is coverage");
        assert_eq!(coverage.descriptor().missing(), &[ShardId::new(vec![2])]);
        assert_eq!(coverage.descriptor().served().as_vector().len(), 2);
    }

    #[test]
    fn a_complete_coverage_descriptor_still_has_coverage_mode() {
        let plan = plan();
        let pin = pin(&plan, 7);
        let responses: Vec<ShardResponse> = plan
            .shards()
            .map(|shard| ShardResponse::served(shard.clone(), CommitVersion::new(7), Vec::new()))
            .collect();
        let outcome = plan
            .query(QueryMode::Coverage, &pin, &responses)
            .expect("coverage mode succeeds");
        let coverage = outcome.coverage().expect("the result is coverage");
        assert!(coverage.descriptor().is_complete());
        assert!(!coverage.is_partial());
        assert!(!coverage.claims_exact());
    }

    #[test]
    fn a_pin_without_a_shard_fails() {
        let plan = plan();
        let mut vector = VersionVector::new();
        vector.set(ShardId::new(vec![1]), CommitVersion::new(7));
        let pin = ScopeVersionVector::from_vector(vector);
        let responses = vec![ShardResponse::unreachable(ShardId::new(vec![1]))];
        assert_eq!(
            plan.query(QueryMode::Coverage, &pin, &responses),
            Err(QueryError::PinIncomplete {
                shard_id: ShardId::new(vec![2])
            })
        );
    }

    #[test]
    fn a_real_system_key_is_rejected_in_a_scope_result() {
        let plan = plan();
        let pin = pin(&plan, 7);
        let responses: Vec<ShardResponse> = plan
            .shards()
            .map(|shard| {
                let rows = if *shard == ShardId::new(vec![1]) {
                    vec![ScopeRow::new(SYSTEM_KEY.to_vec(), KeyVersion::new(1))]
                } else {
                    Vec::new()
                };
                ShardResponse::served(shard.clone(), CommitVersion::new(7), rows)
            })
            .collect();
        let error = plan
            .query(QueryMode::Exact, &pin, &responses)
            .expect_err("the system key is rejected");
        assert!(matches!(error, QueryError::SystemKey { .. }));
        assert_eq!("scope.system_key_rejected", error.code());
    }

    #[test]
    fn a_real_user_key_is_accepted_in_a_scope_result() {
        let plan = plan();
        let pin = pin(&plan, 7);
        let responses: Vec<ShardResponse> = plan
            .shards()
            .map(|shard| {
                let rows = if *shard == ShardId::new(vec![1]) {
                    vec![ScopeRow::new(USER_KEY.to_vec(), KeyVersion::new(1))]
                } else {
                    Vec::new()
                };
                ShardResponse::served(shard.clone(), CommitVersion::new(7), rows)
            })
            .collect();
        let outcome = plan
            .query(QueryMode::Exact, &pin, &responses)
            .expect("the user key is accepted");
        let exact = outcome.exact().expect("the result is exact");
        assert_eq!(exact.rows().len(), 1);
        assert_eq!(exact.rows()[0].key, USER_KEY);
    }
}
