//! Dry run, staged rollout, canary, promotion, and rollback.
//!
//! The lifecycle is a sans-I/O state machine. Time, approval, and rollout
//! state enter through request values. The state machine returns a receipt, an
//! audit record, and an operational notice for every transition.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::receipt::{AuditReference, ReceiptId, ReceiptState, RequestId};
use oxkv_contracts::version::{
    ContentDigest, DigestDomain, Hash32, PolicyVersion, ProjectionEpoch,
};

use crate::audit::{AuditEntry, PolicyAuditLog, PolicyAuditRecord};
use crate::catalog::PolicyCatalog;
use crate::error::RuleError;
use crate::manifest::{FactReadSet, RuleId, RuleManifest};
use crate::policy::{CompiledPolicy, PolicyContent, PolicyId};

/// An injected time value in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct InjectedTime(u64);

impl InjectedTime {
    /// Creates a time value.
    #[must_use]
    pub const fn new(milliseconds: u64) -> Self {
        Self(milliseconds)
    }

    /// Returns the time value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns true when this time is at or after the other time.
    #[must_use]
    pub const fn is_at_or_after(self, other: Self) -> bool {
        self.0 >= other.0
    }
}

impl std::fmt::Display for InjectedTime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

/// One approval role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// The person who wrote the change.
    Author,
    /// The owner of the affected table.
    TableOwner,
    /// The operator who owns the cohort, budget, and rollback.
    PlatformOperator,
    /// The security reviewer for a read-widen change.
    Security,
}

impl Role {
    /// Returns the canonical role name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Author => "author",
            Self::TableOwner => "table_owner",
            Self::PlatformOperator => "platform_operator",
            Self::Security => "security",
        }
    }

    /// Returns the roles that one change class needs.
    #[must_use]
    pub const fn required_for(class: ChangeClass) -> &'static [Self] {
        match class {
            ChangeClass::ReadWiden | ChangeClass::WriteAllow => {
                &[Self::TableOwner, Self::PlatformOperator, Self::Security]
            }
            ChangeClass::ReadNarrow => &[Self::TableOwner],
            ChangeClass::WriteDeny => &[Self::TableOwner, Self::PlatformOperator],
            ChangeClass::Cost => &[Self::PlatformOperator],
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The blast-radius class of one change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeClass {
    /// The rule lets more rows reach a read scope.
    ReadWiden,
    /// The rule lets fewer rows reach a read scope.
    ReadNarrow,
    /// The rule rejects more writes.
    WriteDeny,
    /// The rule accepts more writes.
    WriteAllow,
    /// The rule changes only its cost.
    Cost,
}

impl ChangeClass {
    /// Returns the canonical class name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ReadWiden => "read_widen",
            Self::ReadNarrow => "read_narrow",
            Self::WriteDeny => "write_deny",
            Self::WriteAllow => "write_allow",
            Self::Cost => "cost",
        }
    }

    /// Returns true when the class may promote without a second review.
    #[must_use]
    pub const fn may_promote_automatically(self) -> bool {
        !matches!(self, Self::ReadWiden | Self::WriteAllow)
    }
}

/// One named approver.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Approver {
    /// The approver role.
    pub role: Role,
    /// The authenticated subject.
    pub subject: String,
}

impl Approver {
    /// Creates an approver.
    #[must_use]
    pub fn new(role: Role, subject: impl Into<String>) -> Self {
        Self {
            role,
            subject: subject.into(),
        }
    }
}

/// The approvals that join one dry-run digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalSet {
    diff_digest: ContentDigest,
    approvers: Vec<Approver>,
}

impl ApprovalSet {
    /// Creates an approval set for one dry-run digest.
    #[must_use]
    pub fn new(diff_digest: ContentDigest, approvers: Vec<Approver>) -> Self {
        let mut approvers = approvers;
        approvers.sort();
        approvers.dedup();
        Self {
            diff_digest,
            approvers,
        }
    }

    /// Creates an approval set with no approver.
    #[must_use]
    pub fn empty(diff_digest: ContentDigest) -> Self {
        Self::new(diff_digest, Vec::new())
    }

    /// Returns the joined dry-run digest.
    #[must_use]
    pub const fn diff_digest(&self) -> ContentDigest {
        self.diff_digest
    }

    /// Returns the approvers.
    #[must_use]
    pub fn approvers(&self) -> &[Approver] {
        &self.approvers
    }

    /// Returns true when every required role has an approver.
    ///
    /// The author cannot approve the own change. The approval must join the
    /// expected dry-run digest.
    #[must_use]
    pub fn satisfies(&self, class: ChangeClass, author: &str, expected: ContentDigest) -> bool {
        if self.diff_digest != expected {
            return false;
        }
        Role::required_for(class).iter().all(|role| {
            self.approvers
                .iter()
                .any(|approver| approver.role == *role && approver.subject != author)
        })
    }
}

/// The cohort unit of one rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CohortKind {
    /// A canary cohort.
    Canary,
    /// A control cohort.
    Control,
}

impl CohortKind {
    /// Returns the canonical kind name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Canary => "canary",
            Self::Control => "control",
        }
    }
}

/// One named cohort.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cohort {
    id: String,
    kind: CohortKind,
}

impl Cohort {
    /// Creates a cohort.
    #[must_use]
    pub fn new(id: impl Into<String>, kind: CohortKind) -> Self {
        Self {
            id: id.into(),
            kind,
        }
    }

    /// Creates a canary cohort.
    #[must_use]
    pub fn canary(id: impl Into<String>) -> Self {
        Self::new(id, CohortKind::Canary)
    }

    /// Creates a control cohort.
    #[must_use]
    pub fn control(id: impl Into<String>) -> Self {
        Self::new(id, CohortKind::Control)
    }

    /// Returns the cohort identity.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the cohort kind.
    #[must_use]
    pub const fn kind(&self) -> CohortKind {
        self.kind
    }
}

/// The projection identity of one cohort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectionStamp {
    epoch: ProjectionEpoch,
    policy_version: Option<PolicyVersion>,
    fact_digest: ContentDigest,
}

impl ProjectionStamp {
    /// Creates a control-cohort stamp.
    ///
    /// The control epoch is the base epoch. It does not include a policy
    /// version.
    #[must_use]
    pub fn for_control(base: ProjectionEpoch, facts: &FactReadSet) -> Self {
        Self {
            epoch: base,
            policy_version: None,
            fact_digest: facts.digest(),
        }
    }

    /// Creates a canary-cohort stamp.
    ///
    /// The canary epoch includes the policy version. Two policy versions can
    /// therefore project facts under two epochs at the same time.
    #[must_use]
    pub fn for_canary(base: ProjectionEpoch, version: PolicyVersion, facts: &FactReadSet) -> Self {
        Self {
            epoch: canary_epoch(base, version, facts),
            policy_version: Some(version),
            fact_digest: facts.digest(),
        }
    }

    /// Returns the projection epoch.
    #[must_use]
    pub const fn epoch(self) -> ProjectionEpoch {
        self.epoch
    }

    /// Returns the policy version that the epoch includes.
    #[must_use]
    pub const fn policy_version(self) -> Option<PolicyVersion> {
        self.policy_version
    }

    /// Returns the fact set digest.
    #[must_use]
    pub const fn fact_digest(self) -> ContentDigest {
        self.fact_digest
    }

    /// Returns true when the projection needs a rebuild.
    ///
    /// A predicate change that reads the same facts returns `false`.
    #[must_use]
    pub fn requires_rebuild(self, next: Self) -> bool {
        self.fact_digest != next.fact_digest
    }
}

fn canary_epoch(
    base: ProjectionEpoch,
    version: PolicyVersion,
    facts: &FactReadSet,
) -> ProjectionEpoch {
    let mut bytes = b"oxkv/projection-epoch/v1\0".to_vec();
    bytes.extend_from_slice(&base.get().to_le_bytes());
    bytes.extend_from_slice(version.as_bytes());
    bytes.extend_from_slice(facts.digest().as_bytes());
    let hash = Hash32::of(&bytes);
    let raw = hash.as_bytes();
    let value = u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]) & 0x7fff_ffff_ffff_ffff;
    ProjectionEpoch::new(base.get() ^ value)
}

/// One rule verdict in a dry-run sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Verdict {
    /// The rule admits the row.
    Allow,
    /// The rule rejects the row.
    Deny,
    /// The candidate evaluator failed.
    Error,
}

impl Verdict {
    /// Returns the canonical verdict name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Error => "error",
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::Allow => 1,
            Self::Deny => 2,
            Self::Error => 3,
        }
    }
}

/// One row in a pinned dry-run sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleRow {
    /// The tenant that owns the row.
    pub tenant: String,
    /// The operation name.
    pub operation: String,
    /// The row key text.
    pub key: String,
    /// The verdict of the active policy version.
    pub baseline: Verdict,
    /// The verdict of the candidate policy version.
    pub candidate: Verdict,
}

impl SampleRow {
    /// Creates one sample row.
    #[must_use]
    pub fn new(
        tenant: impl Into<String>,
        operation: impl Into<String>,
        key: impl Into<String>,
        baseline: Verdict,
        candidate: Verdict,
    ) -> Self {
        Self {
            tenant: tenant.into(),
            operation: operation.into(),
            key: key.into(),
            baseline,
            candidate,
        }
    }
}

/// The sample that one dry run evaluates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRunSample {
    snapshot_digest: Option<ContentDigest>,
    rows: Vec<SampleRow>,
}

impl DryRunSample {
    /// Creates a pinned sample.
    #[must_use]
    pub fn pinned(snapshot_digest: ContentDigest, rows: Vec<SampleRow>) -> Self {
        Self {
            snapshot_digest: Some(snapshot_digest),
            rows,
        }
    }

    /// Creates an unpinned sample.
    #[must_use]
    pub fn unpinned(rows: Vec<SampleRow>) -> Self {
        Self {
            snapshot_digest: None,
            rows,
        }
    }

    /// Returns the snapshot digest.
    #[must_use]
    pub const fn snapshot_digest(&self) -> Option<ContentDigest> {
        self.snapshot_digest
    }

    /// Returns true when the sample is pinned.
    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        self.snapshot_digest.is_some()
    }

    /// Returns the sample rows.
    #[must_use]
    pub fn rows(&self) -> &[SampleRow] {
        &self.rows
    }
}

/// The structural half of a dry-run diff.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StructuralDiff {
    /// Rule identities that the candidate adds.
    pub rules_added: Vec<RuleId>,
    /// Rule identities that the candidate removes.
    pub rules_removed: Vec<RuleId>,
    /// Rule identities whose body changes.
    pub rules_changed: Vec<RuleId>,
    /// Field paths that the candidate adds.
    pub paths_added: Vec<String>,
    /// Field paths that the candidate removes.
    pub paths_removed: Vec<String>,
    /// True when the cost model changes.
    pub budget_changed: bool,
}

impl StructuralDiff {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_ids(&mut bytes, &self.rules_added);
        push_ids(&mut bytes, &self.rules_removed);
        push_ids(&mut bytes, &self.rules_changed);
        push_strings(&mut bytes, &self.paths_added);
        push_strings(&mut bytes, &self.paths_removed);
        crate::canonical::push_bool(&mut bytes, self.budget_changed);
        bytes
    }
}

fn push_ids(bytes: &mut Vec<u8>, ids: &[RuleId]) {
    crate::canonical::push_u64(bytes, ids.len() as u64);
    for id in ids {
        crate::canonical::push_str(bytes, id.as_str());
    }
}

fn push_strings(bytes: &mut Vec<u8>, values: &[String]) {
    crate::canonical::push_u64(bytes, values.len() as u64);
    for value in values {
        crate::canonical::push_str(bytes, value);
    }
}

/// One verdict flip in the semantic half of a dry-run diff.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VerdictFlip {
    /// The tenant.
    pub tenant: String,
    /// The operation.
    pub operation: String,
    /// The row key text.
    pub key: String,
    /// The baseline verdict.
    pub from: Verdict,
    /// The candidate verdict.
    pub to: Verdict,
}

impl VerdictFlip {
    fn canonical_bytes(&self, bytes: &mut Vec<u8>) {
        crate::canonical::push_str(bytes, &self.tenant);
        crate::canonical::push_str(bytes, &self.operation);
        crate::canonical::push_str(bytes, &self.key);
        crate::canonical::push_tag(bytes, self.from.tag());
        crate::canonical::push_tag(bytes, self.to.tag());
    }
}

/// The full dry-run diff.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DryRunDiff {
    /// The structural half.
    pub structural: StructuralDiff,
    /// The semantic half.
    pub semantic: Vec<VerdictFlip>,
}

impl DryRunDiff {
    /// Returns the canonical diff bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = self.structural.canonical_bytes();
        crate::canonical::push_u64(&mut bytes, self.semantic.len() as u64);
        for flip in &self.semantic {
            flip.canonical_bytes(&mut bytes);
        }
        bytes
    }

    /// Returns the diff digest.
    #[must_use]
    pub fn digest(&self) -> ContentDigest {
        ContentDigest::of_bytes(&self.canonical_bytes())
    }

    /// Returns true when the diff has no change.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.structural == StructuralDiff::default() && self.semantic.is_empty()
    }
}

/// The report of one dry run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRunReport {
    diff: DryRunDiff,
    digest: ContentDigest,
    snapshot_digest: ContentDigest,
    old_version: PolicyVersion,
    candidate_version: PolicyVersion,
}

impl DryRunReport {
    /// Returns the diff.
    #[must_use]
    pub const fn diff(&self) -> &DryRunDiff {
        &self.diff
    }

    /// Returns the diff digest that joins the approval.
    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }

    /// Returns the pinned snapshot digest.
    #[must_use]
    pub const fn snapshot_digest(&self) -> ContentDigest {
        self.snapshot_digest
    }

    /// Returns the active policy version.
    #[must_use]
    pub const fn old_version(&self) -> PolicyVersion {
        self.old_version
    }

    /// Returns the candidate policy version.
    #[must_use]
    pub const fn candidate_version(&self) -> PolicyVersion {
        self.candidate_version
    }
}

/// The dry-run operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DryRun;

impl DryRun {
    /// Runs one candidate against one pinned sample.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::DryRunFailed`] when the sample is unpinned, has no
    /// row, or the candidate returns an error verdict.
    pub fn run(
        old: &CompiledPolicy,
        candidate: &CompiledPolicy,
        sample: &DryRunSample,
    ) -> Result<DryRunReport, RuleError> {
        let snapshot_digest = sample
            .snapshot_digest
            .ok_or_else(|| RuleError::DryRunFailed {
                detail: "The sample is not pinned.".to_owned(),
            })?;
        if sample.rows.is_empty() {
            return Err(RuleError::DryRunFailed {
                detail: "The sample has no row.".to_owned(),
            });
        }
        let mut rows = sample.rows.clone();
        rows.sort_by(|left, right| {
            (&left.tenant, &left.operation, &left.key).cmp(&(
                &right.tenant,
                &right.operation,
                &right.key,
            ))
        });
        let mut semantic = Vec::new();
        for row in &rows {
            if row.candidate == Verdict::Error {
                return Err(RuleError::DryRunFailed {
                    detail: format!(
                        "The candidate returned an error on tenant {} operation {} key {}.",
                        row.tenant, row.operation, row.key
                    ),
                });
            }
            if row.baseline != row.candidate {
                semantic.push(VerdictFlip {
                    tenant: row.tenant.clone(),
                    operation: row.operation.clone(),
                    key: row.key.clone(),
                    from: row.baseline,
                    to: row.candidate,
                });
            }
        }
        let structural = structural_diff(old, candidate);
        let diff = DryRunDiff {
            structural,
            semantic,
        };
        let digest = report_digest(snapshot_digest, old.version(), candidate.version(), &diff);
        Ok(DryRunReport {
            diff,
            digest,
            snapshot_digest,
            old_version: old.version(),
            candidate_version: candidate.version(),
        })
    }
}

fn structural_diff(old: &CompiledPolicy, candidate: &CompiledPolicy) -> StructuralDiff {
    let mut rules_added = Vec::new();
    let mut rules_removed = Vec::new();
    let mut rules_changed = Vec::new();
    let mut old_paths = BTreeSet::new();
    let mut new_paths = BTreeSet::new();
    for (id, rule) in old {
        for path in rule.body().read_paths() {
            old_paths.insert(path.to_owned());
        }
        match candidate.rule(id) {
            Some(next) => {
                if rule.body() != next.body() {
                    rules_changed.push(id.clone());
                }
            }
            None => rules_removed.push(id.clone()),
        }
    }
    for (id, rule) in candidate {
        for path in rule.body().read_paths() {
            new_paths.insert(path.to_owned());
        }
        if old.rule(id).is_none() {
            rules_added.push(id.clone());
        }
    }
    let paths_added = new_paths.difference(&old_paths).cloned().collect();
    let paths_removed = old_paths.difference(&new_paths).cloned().collect();
    StructuralDiff {
        rules_added,
        rules_removed,
        rules_changed,
        paths_added,
        paths_removed,
        budget_changed: old.budget() != candidate.budget(),
    }
}

fn report_digest(
    snapshot_digest: ContentDigest,
    old_version: PolicyVersion,
    candidate_version: PolicyVersion,
    diff: &DryRunDiff,
) -> ContentDigest {
    let mut bytes = b"oxkv/rule-dry-run/v1\0".to_vec();
    bytes.extend_from_slice(snapshot_digest.as_bytes());
    bytes.extend_from_slice(old_version.as_bytes());
    bytes.extend_from_slice(candidate_version.as_bytes());
    bytes.extend_from_slice(&diff.canonical_bytes());
    ContentDigest::of_bytes(&bytes)
}

/// One rule change receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleReceipt {
    receipt_id: ReceiptId,
    request_id: RequestId,
    policy_id: PolicyId,
    base_version: PolicyVersion,
    new_version: PolicyVersion,
    cohort: Cohort,
    role: Role,
    dry_run_digest: Option<ContentDigest>,
    state: ReceiptState,
    reason: Option<&'static str>,
    audit: AuditReference,
}

impl RuleReceipt {
    /// Returns the receipt identity.
    #[must_use]
    pub const fn receipt_id(&self) -> ReceiptId {
        self.receipt_id
    }

    /// Returns the request identity.
    #[must_use]
    pub const fn request_id(&self) -> RequestId {
        self.request_id
    }

    /// Returns the policy identity.
    #[must_use]
    pub const fn policy_id(&self) -> &PolicyId {
        &self.policy_id
    }

    /// Returns the base policy version.
    #[must_use]
    pub const fn base_version(&self) -> PolicyVersion {
        self.base_version
    }

    /// Returns the new policy version.
    #[must_use]
    pub const fn new_version(&self) -> PolicyVersion {
        self.new_version
    }

    /// Returns the cohort.
    #[must_use]
    pub const fn cohort(&self) -> &Cohort {
        &self.cohort
    }

    /// Returns the actor role.
    #[must_use]
    pub const fn role(&self) -> Role {
        self.role
    }

    /// Returns the dry-run digest.
    #[must_use]
    pub const fn dry_run_digest(&self) -> Option<ContentDigest> {
        self.dry_run_digest
    }

    /// Returns the receipt state.
    #[must_use]
    pub const fn state(&self) -> ReceiptState {
        self.state
    }

    /// Returns the reason code.
    #[must_use]
    pub const fn reason_code(&self) -> Option<&'static str> {
        self.reason
    }

    /// Returns the audit reference.
    #[must_use]
    pub const fn audit(&self) -> &AuditReference {
        &self.audit
    }

    /// Returns true when the change committed.
    #[must_use]
    pub const fn is_committed(&self) -> bool {
        matches!(self.state, ReceiptState::Committed)
    }
}

/// The kind of one lifecycle notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoticeKind {
    /// A candidate became active for one cohort.
    Activated,
    /// A canary version became the control version.
    Promoted,
    /// A canary over budget restored the prior pointer.
    Aborted,
    /// A rollback restored an earlier version.
    RolledBack,
}

impl NoticeKind {
    /// Returns the canonical notice name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Activated => "rule.activated",
            Self::Promoted => "rule.promoted",
            Self::Aborted => "rule.aborted",
            Self::RolledBack => "rule.rolled_back",
        }
    }
}

/// The feed that carries one lifecycle notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoticeFeed {
    /// The operational feed.
    Operational,
}

impl NoticeFeed {
    /// Returns the canonical feed name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Operational => "operational",
        }
    }
}

/// One lifecycle notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleNotice {
    kind: NoticeKind,
    policy_id: PolicyId,
    from_version: PolicyVersion,
    to_version: PolicyVersion,
    cohort: Cohort,
    state: ReceiptState,
    at: InjectedTime,
}

impl RuleNotice {
    /// Returns the notice kind.
    #[must_use]
    pub const fn kind(&self) -> NoticeKind {
        self.kind
    }

    /// Returns the policy identity.
    #[must_use]
    pub const fn policy_id(&self) -> &PolicyId {
        &self.policy_id
    }

    /// Returns the policy version before the change.
    #[must_use]
    pub const fn base_version(&self) -> PolicyVersion {
        self.from_version
    }

    /// Returns the policy version after the change.
    #[must_use]
    pub const fn new_version(&self) -> PolicyVersion {
        self.to_version
    }

    /// Returns the cohort.
    #[must_use]
    pub const fn cohort(&self) -> &Cohort {
        &self.cohort
    }

    /// Returns the change state.
    #[must_use]
    pub const fn state(&self) -> ReceiptState {
        self.state
    }

    /// Returns the injected time of the notice.
    #[must_use]
    pub const fn at(&self) -> InjectedTime {
        self.at
    }

    /// Returns the feed that carries the notice.
    ///
    /// A lifecycle notice uses the operational feed. It is not a business
    /// event.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn feed(&self) -> NoticeFeed {
        NoticeFeed::Operational
    }

    /// Returns true when the notice is a business event.
    ///
    /// The value is always false.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn is_business_event(&self) -> bool {
        false
    }
}

/// One rule change result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleChange {
    /// The receipt.
    pub receipt: RuleReceipt,
    /// The audit record.
    pub audit: PolicyAuditRecord,
    /// The operational notice.
    pub notice: RuleNotice,
}

impl RuleChange {
    /// Returns true when the change committed.
    #[must_use]
    pub const fn is_committed(&self) -> bool {
        self.receipt.is_committed()
    }
}

/// One activated rollout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rollout {
    cohort: Cohort,
    policy_id: PolicyId,
    base_version: PolicyVersion,
    candidate_version: PolicyVersion,
    base_epoch: ProjectionEpoch,
    change_class: ChangeClass,
    deadline: InjectedTime,
    max_operations: u64,
}

impl Rollout {
    /// Returns the cohort.
    #[must_use]
    pub const fn cohort(&self) -> &Cohort {
        &self.cohort
    }

    /// Returns the candidate version.
    #[must_use]
    pub const fn candidate_version(&self) -> PolicyVersion {
        self.candidate_version
    }

    /// Returns the base version.
    #[must_use]
    pub const fn base_version(&self) -> PolicyVersion {
        self.base_version
    }

    /// Returns the deadline.
    #[must_use]
    pub const fn deadline(&self) -> InjectedTime {
        self.deadline
    }

    /// Returns the change class.
    #[must_use]
    pub const fn change_class(&self) -> ChangeClass {
        self.change_class
    }

    /// Returns true when the time is at or after the deadline.
    #[must_use]
    pub const fn is_expired(&self, at: InjectedTime) -> bool {
        at.is_at_or_after(self.deadline)
    }
}

/// The observed state of one rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RolloutObservation {
    /// The operation count that the canary served.
    pub operations: u64,
}

impl RolloutObservation {
    /// Creates one observation.
    #[must_use]
    pub const fn new(operations: u64) -> Self {
        Self { operations }
    }
}

/// The result of one rollout tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleStep {
    /// The rollout continues.
    Continue,
    /// The rollout promoted the candidate.
    Promoted(RuleChange),
    /// The rollout aborted and restored the prior pointer.
    Aborted(RuleChange),
}

/// One activation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRequest {
    /// The policy identity.
    pub policy_id: PolicyId,
    /// The base policy version for the cohort.
    pub base_version: PolicyVersion,
    /// The candidate policy content.
    pub candidate: PolicyContent,
    /// The target cohort.
    pub cohort: Cohort,
    /// The authenticated author.
    pub author: String,
    /// The author role.
    pub role: Role,
    /// The blast-radius class.
    pub change_class: ChangeClass,
    /// The approvals.
    pub approvals: ApprovalSet,
    /// The dry-run report.
    pub dry_run: Option<DryRunReport>,
    /// The projection base epoch.
    pub projection_base_epoch: ProjectionEpoch,
    /// The rollout deadline.
    pub deadline: InjectedTime,
    /// The maximum operation count.
    pub max_operations: u64,
    /// The injected time.
    pub at: InjectedTime,
}

/// One rollback request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackRequest {
    /// The policy identity.
    pub policy_id: PolicyId,
    /// The cohort.
    pub cohort: Cohort,
    /// The active base version.
    pub base_version: PolicyVersion,
    /// The target earlier version.
    pub target_version: PolicyVersion,
    /// The authenticated actor.
    pub actor: String,
    /// The actor role.
    pub role: Role,
    /// The projection base epoch.
    pub projection_base_epoch: ProjectionEpoch,
    /// The injected time.
    pub at: InjectedTime,
}

/// One promotion request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionRequest {
    /// The cohort with the active rollout.
    pub cohort: Cohort,
    /// The authenticated actor.
    pub actor: String,
    /// The actor role.
    pub role: Role,
    /// The injected time.
    pub at: InjectedTime,
}

/// One rollout tick request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutTick {
    /// The cohort with the active rollout.
    pub cohort: Cohort,
    /// The observed operation count.
    pub observation: RolloutObservation,
    /// The authenticated actor.
    pub actor: String,
    /// The actor role.
    pub role: Role,
    /// The injected time.
    pub at: InjectedTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Assignment {
    cohort: Cohort,
    policy_id: PolicyId,
    version: PolicyVersion,
    projection: ProjectionStamp,
}

/// The rule lifecycle state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleLifecycle {
    manifest: RuleManifest,
    catalog: PolicyCatalog,
    audit: PolicyAuditLog,
    assignments: BTreeMap<String, Assignment>,
    rollouts: BTreeMap<String, Rollout>,
}

impl RuleLifecycle {
    /// Creates a lifecycle with one initial policy.
    ///
    /// The initial policy must compile against the manifest.
    ///
    /// # Errors
    ///
    /// Returns the compile error of the initial policy.
    pub fn bootstrap(manifest: RuleManifest, initial: PolicyContent) -> Result<Self, RuleError> {
        initial.compile(&manifest)?;
        let mut catalog = PolicyCatalog::new();
        let _ = catalog.insert(initial);
        Ok(Self {
            manifest,
            catalog,
            audit: PolicyAuditLog::new(),
            assignments: BTreeMap::new(),
            rollouts: BTreeMap::new(),
        })
    }

    /// Creates a lifecycle from one catalog.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::PolicyCatalogMissing`] when the catalog is empty.
    pub fn from_parts(manifest: RuleManifest, catalog: PolicyCatalog) -> Result<Self, RuleError> {
        if catalog.is_empty() {
            return Err(RuleError::PolicyCatalogMissing);
        }
        Ok(Self {
            manifest,
            catalog,
            audit: PolicyAuditLog::new(),
            assignments: BTreeMap::new(),
            rollouts: BTreeMap::new(),
        })
    }

    /// Returns the manifest.
    #[must_use]
    pub const fn manifest(&self) -> &RuleManifest {
        &self.manifest
    }

    /// Returns the catalog.
    #[must_use]
    pub const fn catalog(&self) -> &PolicyCatalog {
        &self.catalog
    }

    /// Returns the audit log.
    #[must_use]
    pub const fn audit(&self) -> &PolicyAuditLog {
        &self.audit
    }

    /// Returns the active policy version for one cohort.
    #[must_use]
    pub fn active_version(&self, cohort: &Cohort) -> Option<PolicyVersion> {
        self.assignments
            .get(cohort.id())
            .map(|assignment| assignment.version)
    }

    /// Returns the active projection stamp for one cohort.
    #[must_use]
    pub fn active_projection(&self, cohort: &Cohort) -> Option<ProjectionStamp> {
        self.assignments
            .get(cohort.id())
            .map(|assignment| assignment.projection)
    }

    /// Returns the active rollout for one cohort.
    #[must_use]
    pub fn rollout(&self, cohort: &Cohort) -> Option<&Rollout> {
        self.rollouts.get(cohort.id())
    }

    /// Sets one initial cohort pointer.
    ///
    /// The caller uses this method once to seed state that already exists.
    /// The method writes no audit record.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::PolicyVersionUnknown`] when the version is absent.
    /// Returns the compile error of the stored policy.
    pub fn assign_initial(
        &mut self,
        cohort: Cohort,
        version: PolicyVersion,
        base_epoch: ProjectionEpoch,
    ) -> Result<(), RuleError> {
        let record = self.catalog.resolve(version)?;
        let compiled = record.content().compile(&self.manifest)?;
        let projection = projection_for(&cohort, base_epoch, version, &compiled);
        self.assignments.insert(
            cohort.id().to_owned(),
            Assignment {
                cohort,
                policy_id: record.content().policy_id().clone(),
                version,
                projection,
            },
        );
        Ok(())
    }

    /// Activates one candidate for one cohort.
    ///
    /// The method always returns a receipt. A failed compile, dry run,
    /// approval gate, or rollout limit returns a `Rejected` receipt. A stale
    /// base returns a `Conflict` receipt.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn activate(&mut self, request: ActivationRequest) -> RuleChange {
        let key = request.cohort.id().to_owned();
        let assignment = self.assignments.get(&key);
        let from_version = assignment.map_or(request.base_version, |current| current.version);
        if let Some(current) = assignment
            && current.version != request.base_version
        {
            return self.record(
                NoticeKind::Activated,
                ReceiptState::Conflict,
                Some("write.version_conflict"),
                &request.policy_id,
                current.version,
                request.candidate.version(),
                &request.cohort,
                request.role,
                &request.author,
                None,
                request.at,
            );
        }
        if let Err(error) = self.catalog.resolve(request.base_version) {
            return self.record_error(
                NoticeKind::Activated,
                &error,
                &request.policy_id,
                from_version,
                request.candidate.version(),
                &request.cohort,
                request.role,
                &request.author,
                None,
                request.at,
            );
        }
        let compiled = match request.candidate.compile(&self.manifest) {
            Ok(compiled) => compiled,
            Err(error) => {
                return self.record_error(
                    NoticeKind::Activated,
                    &error,
                    &request.policy_id,
                    from_version,
                    request.candidate.version(),
                    &request.cohort,
                    request.role,
                    &request.author,
                    None,
                    request.at,
                );
            }
        };
        let Some(report) = request.dry_run else {
            return self.record(
                NoticeKind::Activated,
                ReceiptState::Rejected,
                Some("rule.dry_run_failed"),
                &request.policy_id,
                from_version,
                compiled.version(),
                &request.cohort,
                request.role,
                &request.author,
                None,
                request.at,
            );
        };
        if report.candidate_version() != compiled.version()
            || report.old_version() != request.base_version
        {
            return self.record(
                NoticeKind::Activated,
                ReceiptState::Rejected,
                Some("rule.dry_run_failed"),
                &request.policy_id,
                from_version,
                compiled.version(),
                &request.cohort,
                request.role,
                &request.author,
                Some(report.digest()),
                request.at,
            );
        }
        if !request
            .approvals
            .satisfies(request.change_class, &request.author, report.digest())
        {
            return self.record(
                NoticeKind::Activated,
                ReceiptState::Rejected,
                Some("rule.approval_required"),
                &request.policy_id,
                from_version,
                compiled.version(),
                &request.cohort,
                request.role,
                &request.author,
                Some(report.digest()),
                request.at,
            );
        }
        if self.would_exceed_rollout_limit(&request.policy_id, compiled.version()) {
            return self.record(
                NoticeKind::Activated,
                ReceiptState::Rejected,
                Some("rule.rollout_limit"),
                &request.policy_id,
                from_version,
                compiled.version(),
                &request.cohort,
                request.role,
                &request.author,
                Some(report.digest()),
                request.at,
            );
        }
        let version = self.catalog.insert(request.candidate);
        let projection = projection_for(
            &request.cohort,
            request.projection_base_epoch,
            version,
            &compiled,
        );
        self.assignments.insert(
            key.clone(),
            Assignment {
                cohort: request.cohort.clone(),
                policy_id: request.policy_id.clone(),
                version,
                projection,
            },
        );
        self.rollouts.insert(
            key,
            Rollout {
                cohort: request.cohort.clone(),
                policy_id: request.policy_id.clone(),
                base_version: request.base_version,
                candidate_version: version,
                base_epoch: request.projection_base_epoch,
                change_class: request.change_class,
                deadline: request.deadline,
                max_operations: request.max_operations,
            },
        );
        self.record(
            NoticeKind::Activated,
            ReceiptState::Committed,
            None,
            &request.policy_id,
            request.base_version,
            version,
            &request.cohort,
            request.role,
            &request.author,
            Some(report.digest()),
            request.at,
        )
    }

    /// Promotes the candidate of one active rollout to its control cohorts.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::RolloutMissing`] when the cohort has no rollout.
    /// Returns the compile error of the target policy.
    pub fn promote(&mut self, request: &PromotionRequest) -> Result<RuleChange, RuleError> {
        let Some(rollout) = self.rollouts.get(request.cohort.id()).cloned() else {
            return Err(RuleError::RolloutMissing);
        };
        self.promote_rollout(&rollout, &request.actor, request.role, request.at)
    }

    /// Runs one rollout tick.
    ///
    /// An over-budget canary aborts and restores the prior pointer. An expired
    /// rollout promotes when it is under budget.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::RolloutMissing`] when the cohort has no rollout.
    pub fn tick(&mut self, request: &RolloutTick) -> Result<LifecycleStep, RuleError> {
        let Some(rollout) = self.rollouts.get(request.cohort.id()).cloned() else {
            return Err(RuleError::RolloutMissing);
        };
        if request.observation.operations > rollout.max_operations {
            let change = self.abort_rollout(&rollout, &request.actor, request.role, request.at)?;
            return Ok(LifecycleStep::Aborted(change));
        }
        if rollout.is_expired(request.at) {
            if !rollout.change_class.may_promote_automatically() {
                return Ok(LifecycleStep::Continue);
            }
            let change =
                self.promote_rollout(&rollout, &request.actor, request.role, request.at)?;
            return Ok(LifecycleStep::Promoted(change));
        }
        Ok(LifecycleStep::Continue)
    }

    /// Rolls back one cohort to an earlier policy version.
    ///
    /// The method appends an audit record and returns a new receipt. It does
    /// not edit the catalog and it does not require the active version to
    /// compile.
    pub fn rollback(&mut self, request: &RollbackRequest) -> RuleChange {
        let key = request.cohort.id().to_owned();
        let Some(assignment) = self.assignments.get(&key) else {
            return self.record(
                NoticeKind::RolledBack,
                ReceiptState::Conflict,
                Some("write.version_conflict"),
                &request.policy_id,
                request.base_version,
                request.target_version,
                &request.cohort,
                request.role,
                &request.actor,
                None,
                request.at,
            );
        };
        if assignment.version != request.base_version {
            return self.record(
                NoticeKind::RolledBack,
                ReceiptState::Conflict,
                Some("write.version_conflict"),
                &request.policy_id,
                assignment.version,
                request.target_version,
                &request.cohort,
                request.role,
                &request.actor,
                None,
                request.at,
            );
        }
        let record = match self.catalog.resolve(request.target_version) {
            Ok(record) => record,
            Err(error) => {
                return self.record_error(
                    NoticeKind::RolledBack,
                    &error,
                    &request.policy_id,
                    request.base_version,
                    request.target_version,
                    &request.cohort,
                    request.role,
                    &request.actor,
                    None,
                    request.at,
                );
            }
        };
        let compiled = match record.content().compile(&self.manifest) {
            Ok(compiled) => compiled,
            Err(error) => {
                return self.record_error(
                    NoticeKind::RolledBack,
                    &error,
                    &request.policy_id,
                    request.base_version,
                    request.target_version,
                    &request.cohort,
                    request.role,
                    &request.actor,
                    None,
                    request.at,
                );
            }
        };
        let projection = projection_for(
            &request.cohort,
            request.projection_base_epoch,
            request.target_version,
            &compiled,
        );
        self.assignments.insert(
            key.clone(),
            Assignment {
                cohort: request.cohort.clone(),
                policy_id: request.policy_id.clone(),
                version: request.target_version,
                projection,
            },
        );
        self.rollouts.remove(&key);
        self.record(
            NoticeKind::RolledBack,
            ReceiptState::Committed,
            None,
            &request.policy_id,
            request.base_version,
            request.target_version,
            &request.cohort,
            request.role,
            &request.actor,
            None,
            request.at,
        )
    }

    fn would_exceed_rollout_limit(&self, policy_id: &PolicyId, candidate: PolicyVersion) -> bool {
        let mut live = BTreeSet::new();
        for assignment in self.assignments.values() {
            if &assignment.policy_id == policy_id {
                live.insert(assignment.version);
            }
        }
        live.insert(candidate);
        live.len() > 2
    }

    fn promote_rollout(
        &mut self,
        rollout: &Rollout,
        actor: &str,
        role: Role,
        at: InjectedTime,
    ) -> Result<RuleChange, RuleError> {
        let record = self.catalog.resolve(rollout.candidate_version)?;
        let compiled = record.content().compile(&self.manifest)?;
        let key = rollout.cohort.id().to_owned();
        let cohort_ids: Vec<String> = self
            .assignments
            .iter()
            .filter(|(_, assignment)| {
                assignment.policy_id == rollout.policy_id
                    && assignment.version == rollout.base_version
            })
            .map(|(key, _)| key.clone())
            .collect();
        for cohort_key in cohort_ids {
            if let Some(assignment) = self.assignments.get(&cohort_key) {
                let cohort = assignment.cohort.clone();
                let projection = projection_for(
                    &cohort,
                    rollout.base_epoch,
                    rollout.candidate_version,
                    &compiled,
                );
                self.assignments.insert(
                    cohort_key,
                    Assignment {
                        cohort,
                        policy_id: rollout.policy_id.clone(),
                        version: rollout.candidate_version,
                        projection,
                    },
                );
            }
        }
        if let Some(assignment) = self.assignments.get_mut(&key) {
            assignment.version = rollout.candidate_version;
            assignment.projection = projection_for(
                &rollout.cohort,
                rollout.base_epoch,
                rollout.candidate_version,
                &compiled,
            );
        }
        self.rollouts.remove(&key);
        Ok(self.record(
            NoticeKind::Promoted,
            ReceiptState::Committed,
            None,
            &rollout.policy_id,
            rollout.base_version,
            rollout.candidate_version,
            &rollout.cohort,
            role,
            actor,
            None,
            at,
        ))
    }

    fn abort_rollout(
        &mut self,
        rollout: &Rollout,
        actor: &str,
        role: Role,
        at: InjectedTime,
    ) -> Result<RuleChange, RuleError> {
        let record = self.catalog.resolve(rollout.base_version)?;
        let compiled = record.content().compile(&self.manifest)?;
        let key = rollout.cohort.id().to_owned();
        self.assignments.insert(
            key.clone(),
            Assignment {
                cohort: rollout.cohort.clone(),
                policy_id: rollout.policy_id.clone(),
                version: rollout.base_version,
                projection: projection_for(
                    &rollout.cohort,
                    rollout.base_epoch,
                    rollout.base_version,
                    &compiled,
                ),
            },
        );
        self.rollouts.remove(&key);
        Ok(self.record(
            NoticeKind::Aborted,
            ReceiptState::Rejected,
            Some("rule.rollout_limit"),
            &rollout.policy_id,
            rollout.candidate_version,
            rollout.base_version,
            &rollout.cohort,
            role,
            actor,
            None,
            at,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn record_error(
        &mut self,
        kind: NoticeKind,
        error: &RuleError,
        policy_id: &PolicyId,
        from_version: PolicyVersion,
        to_version: PolicyVersion,
        cohort: &Cohort,
        role: Role,
        actor: &str,
        dry_run_digest: Option<ContentDigest>,
        at: InjectedTime,
    ) -> RuleChange {
        let state = error.receipt_state().unwrap_or(ReceiptState::Rejected);
        self.record(
            kind,
            state,
            Some(error.code()),
            policy_id,
            from_version,
            to_version,
            cohort,
            role,
            actor,
            dry_run_digest,
            at,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        kind: NoticeKind,
        state: ReceiptState,
        reason: Option<&'static str>,
        policy_id: &PolicyId,
        from_version: PolicyVersion,
        to_version: PolicyVersion,
        cohort: &Cohort,
        role: Role,
        actor: &str,
        dry_run_digest: Option<ContentDigest>,
        at: InjectedTime,
    ) -> RuleChange {
        let audit = self.audit.append(AuditEntry {
            actor: actor.to_owned(),
            role,
            policy_id: policy_id.clone(),
            from_version,
            to_version,
            cohort: cohort.clone(),
            result: state,
            reason,
            dry_run_digest,
        });
        let reference = audit.reference();
        let receipt_id = receipt_id(&audit);
        let request_id = request_id(&audit);
        let receipt = RuleReceipt {
            receipt_id,
            request_id,
            policy_id: policy_id.clone(),
            base_version: from_version,
            new_version: to_version,
            cohort: cohort.clone(),
            role,
            dry_run_digest,
            state,
            reason,
            audit: reference,
        };
        let notice = RuleNotice {
            kind,
            policy_id: policy_id.clone(),
            from_version,
            to_version,
            cohort: cohort.clone(),
            state,
            at,
        };
        RuleChange {
            receipt,
            audit,
            notice,
        }
    }
}

fn projection_for(
    cohort: &Cohort,
    base: ProjectionEpoch,
    version: PolicyVersion,
    compiled: &CompiledPolicy,
) -> ProjectionStamp {
    let facts = compiled.fact_read_set();
    match cohort.kind() {
        CohortKind::Canary => ProjectionStamp::for_canary(base, version, &facts),
        CohortKind::Control => ProjectionStamp::for_control(base, &facts),
    }
}

fn receipt_id(record: &PolicyAuditRecord) -> ReceiptId {
    let mut bytes = b"oxkv/rule-receipt/v1\0".to_vec();
    bytes.extend_from_slice(record.record_hash().as_bytes());
    bytes.extend_from_slice(record.entry().cohort.id().as_bytes());
    let hash = Hash32::of_domain(DigestDomain::ReceiptId, &bytes);
    ReceiptId::from_bytes(*hash.as_bytes())
}

fn request_id(record: &PolicyAuditRecord) -> RequestId {
    let mut bytes = b"oxkv/rule-request/v1\0".to_vec();
    bytes.extend_from_slice(record.previous_hash().as_bytes());
    bytes.extend_from_slice(record.record_hash().as_bytes());
    let hash = Hash32::of_domain(DigestDomain::ReceiptId, &bytes);
    RequestId::from_bytes(*hash.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{CostBound, RuleEffect, RuleScope, RuleSelector};
    use crate::policy::{RuleBody, SUPPORTED_POLICY_AST_VERSION};
    use oxkv_contracts::keyspace::TableName;

    fn manifest() -> RuleManifest {
        let mut paths = BTreeSet::new();
        paths.insert("owner".to_owned());
        RuleManifest::new(vec![crate::manifest::RuleMetadata {
            id: RuleId::parse("tenant_scope").expect("the rule identity is valid"),
            effect: RuleEffect::ReadScope,
            scope: RuleScope::Tenant,
            selector: RuleSelector::new(
                TableName::parse_user("accounts").expect("the table name is valid"),
                paths,
            ),
            cost: CostBound::new(100, 8),
            strict: false,
            retention: crate::manifest::RetentionClass::AuditFloor,
            audit: crate::manifest::AuditVisibility::Visible,
            fact_reads: FactReadSet::new(),
        }])
        .expect("the manifest is valid")
    }

    fn policy(body: &[u8]) -> PolicyContent {
        let mut paths = BTreeSet::new();
        paths.insert("owner".to_owned());
        let mut rules = BTreeMap::new();
        rules.insert(
            RuleId::parse("tenant_scope").expect("the rule identity is valid"),
            RuleBody::new(body.to_vec(), paths, 5, 2).expect("the body is valid"),
        );
        PolicyContent::new(
            PolicyId::parse("tenant_policy").expect("the policy identity is valid"),
            SUPPORTED_POLICY_AST_VERSION,
            rules,
            CostBound::new(100, 8),
        )
        .expect("the policy is valid")
    }

    fn sample() -> DryRunSample {
        DryRunSample::pinned(
            ContentDigest::from_bytes([3; 32]),
            vec![SampleRow::new(
                "tenant-a",
                "get",
                "row-1",
                Verdict::Allow,
                Verdict::Deny,
            )],
        )
    }

    #[test]
    fn a_canary_epoch_includes_the_policy_version() {
        let facts = FactReadSet::new();
        let base = ProjectionEpoch::new(41);
        let control = ProjectionStamp::for_control(base, &facts);
        let first = ProjectionStamp::for_canary(base, PolicyVersion::from_bytes([1; 32]), &facts);
        let second = ProjectionStamp::for_canary(base, PolicyVersion::from_bytes([2; 32]), &facts);
        assert_eq!(control.epoch(), base);
        assert_ne!(first.epoch(), base);
        assert_ne!(first.epoch(), second.epoch());
        assert!(!first.requires_rebuild(second));
    }

    #[test]
    fn an_approval_needs_every_required_role() {
        let digest = ContentDigest::from_bytes([8; 32]);
        let approvals = ApprovalSet::new(digest, vec![Approver::new(Role::Security, "sec-1")]);
        assert!(!approvals.satisfies(ChangeClass::ReadWiden, "author-1", digest));
        let approvals = ApprovalSet::new(
            digest,
            vec![
                Approver::new(Role::TableOwner, "owner-1"),
                Approver::new(Role::PlatformOperator, "operator-1"),
                Approver::new(Role::Security, "sec-1"),
            ],
        );
        assert!(approvals.satisfies(ChangeClass::ReadWiden, "author-1", digest));
    }

    #[test]
    fn the_author_cannot_self_approve() {
        let digest = ContentDigest::from_bytes([8; 32]);
        let approvals = ApprovalSet::new(digest, vec![Approver::new(Role::TableOwner, "author-1")]);
        assert!(!approvals.satisfies(ChangeClass::ReadNarrow, "author-1", digest));
    }

    #[test]
    fn a_dry_run_rejects_an_unpinned_sample() {
        let manifest = manifest();
        let old = policy(b"allow owner")
            .compile(&manifest)
            .expect("the policy compiles");
        let candidate = policy(b"deny owner")
            .compile(&manifest)
            .expect("the policy compiles");
        let sample = DryRunSample::unpinned(vec![SampleRow::new(
            "tenant-a",
            "get",
            "row-1",
            Verdict::Allow,
            Verdict::Deny,
        )]);
        let result = DryRun::run(&old, &candidate, &sample);
        assert_eq!(result.unwrap_err().code(), "rule.dry_run_failed");
    }

    #[test]
    fn a_dry_run_is_byte_identical_for_one_sample() {
        let manifest = manifest();
        let old = policy(b"allow owner")
            .compile(&manifest)
            .expect("the policy compiles");
        let candidate = policy(b"deny owner")
            .compile(&manifest)
            .expect("the policy compiles");
        let first = DryRun::run(&old, &candidate, &sample()).expect("the dry run succeeds");
        let second = DryRun::run(&old, &candidate, &sample()).expect("the dry run succeeds");
        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.diff(), second.diff());
    }
}
