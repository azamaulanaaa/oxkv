#![allow(missing_docs)]

//! Shared fixtures for the rule golden vectors.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::keyspace::TableName;
use oxkv_contracts::receipt::{IdempotencyKey, ReceiptState, RequestId};
use oxkv_contracts::version::{ContentDigest, FactSourceId, PolicyVersion, ProjectionEpoch};
use oxkv_rule::{
    ActivationRequest, ApprovalSet, Approver, AuditVisibility, ChangeClass, Cohort, CostBound,
    DryRun, DryRunReport, DryRunSample, FactReadSet, InjectedTime, LifecycleStep, PolicyCatalog,
    PolicyContent, PolicyId, PromotionRequest, RetentionClass, Role, RollbackRequest,
    RolloutObservation, RolloutTick, RuleBody, RuleEffect, RuleId, RuleLifecycle, RuleManifest,
    RuleMetadata, RuleScope, RuleSelector, SUPPORTED_POLICY_AST_VERSION, SampleRow, Verdict,
};

pub(crate) const BASE_EPOCH: ProjectionEpoch = ProjectionEpoch::new(41);
pub(crate) const AUTHOR: &str = "author-1";

pub(crate) fn rule_id() -> RuleId {
    RuleId::parse("tenant_scope").expect("the rule identity is valid")
}

pub(crate) fn policy_id() -> PolicyId {
    PolicyId::parse("tenant_policy").expect("the policy identity is valid")
}

pub(crate) fn manifest() -> RuleManifest {
    let mut paths = BTreeSet::new();
    paths.insert("owner".to_owned());
    paths.insert("status".to_owned());
    let mut facts = FactReadSet::new();
    facts.insert(FactSourceId::new("tenant_owner"));
    RuleManifest::new(vec![RuleMetadata {
        id: rule_id(),
        effect: RuleEffect::ReadScope,
        scope: RuleScope::Tenant,
        selector: RuleSelector::new(
            TableName::parse_user("accounts").expect("the table name is valid"),
            paths,
        ),
        cost: CostBound::new(100, 8),
        strict: false,
        retention: RetentionClass::AuditFloor,
        audit: AuditVisibility::Visible,
        fact_reads: facts,
    }])
    .expect("the manifest is valid")
}

pub(crate) fn body(ast: &[u8]) -> RuleBody {
    let mut paths = BTreeSet::new();
    paths.insert("owner".to_owned());
    RuleBody::new(ast.to_vec(), paths, 5, 2).expect("the body is valid")
}

pub(crate) fn body_with_paths(ast: &[u8], paths: &[&str], steps: u64) -> RuleBody {
    let paths = paths.iter().map(|path| (*path).to_owned()).collect();
    RuleBody::new(ast.to_vec(), paths, steps, 2).expect("the body is valid")
}

pub(crate) fn policy(ast: &[u8]) -> PolicyContent {
    policy_with_body(body(ast))
}

pub(crate) fn policy_with_body(body: RuleBody) -> PolicyContent {
    let mut rules = BTreeMap::new();
    rules.insert(rule_id(), body);
    PolicyContent::new(
        policy_id(),
        SUPPORTED_POLICY_AST_VERSION,
        rules,
        CostBound::new(100, 8),
    )
    .expect("the policy is valid")
}

pub(crate) fn sample() -> DryRunSample {
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

pub(crate) struct Fixture {
    pub(crate) manifest: RuleManifest,
    pub(crate) old: PolicyContent,
    pub(crate) candidate: PolicyContent,
    pub(crate) report: DryRunReport,
    pub(crate) control: Cohort,
    pub(crate) canary: Cohort,
}

pub(crate) fn fixture() -> Fixture {
    let manifest = manifest();
    let old = policy(b"allow owner");
    let candidate = policy(b"deny owner");
    let old_compiled = old.compile(&manifest).expect("the old policy compiles");
    let candidate_compiled = candidate
        .compile(&manifest)
        .expect("the candidate compiles");
    let report =
        DryRun::run(&old_compiled, &candidate_compiled, &sample()).expect("the dry run succeeds");
    Fixture {
        manifest,
        old,
        candidate,
        report,
        control: Cohort::control("tenant-a"),
        canary: Cohort::canary("tenant-b"),
    }
}

pub(crate) fn seeded() -> (RuleLifecycle, Fixture) {
    let fixture = fixture();
    let mut lifecycle = RuleLifecycle::bootstrap(fixture.manifest.clone(), fixture.old.clone())
        .expect("the lifecycle starts");
    lifecycle
        .assign_initial(fixture.control.clone(), fixture.old.version(), BASE_EPOCH)
        .expect("the initial cohort pins the old version");
    (lifecycle, fixture)
}

pub(crate) fn approvals(digest: ContentDigest) -> ApprovalSet {
    ApprovalSet::new(
        digest,
        vec![
            Approver::new(Role::TableOwner, "owner-1"),
            Approver::new(Role::PlatformOperator, "operator-1"),
            Approver::new(Role::Security, "sec-1"),
        ],
    )
}

pub(crate) fn request(fixture: &Fixture, cohort: Cohort) -> ActivationRequest {
    ActivationRequest {
        idempotency_key: IdempotencyKey::new(cohort.id().as_bytes().to_vec()),
        request_id: RequestId::from_bytes([7; 32]),
        policy_id: policy_id(),
        base_version: fixture.old.version(),
        candidate: fixture.candidate.clone(),
        cohort,
        author: AUTHOR.to_owned(),
        role: Role::Author,
        change_class: ChangeClass::ReadNarrow,
        approvals: approvals(fixture.report.digest()),
        dry_run: Some(fixture.report.clone()),
        projection_base_epoch: BASE_EPOCH,
        deadline: InjectedTime::new(1_000),
        max_operations: 10,
        at: InjectedTime::new(10),
    }
}

pub(crate) fn rollback_request(
    fixture: &Fixture,
    base_version: PolicyVersion,
    target_version: PolicyVersion,
) -> RollbackRequest {
    RollbackRequest {
        idempotency_key: IdempotencyKey::new(
            [b"rollback-".as_slice(), base_version.as_bytes()].concat(),
        ),
        request_id: RequestId::from_bytes([8; 32]),
        policy_id: policy_id(),
        cohort: fixture.control.clone(),
        base_version,
        target_version,
        actor: "operator-1".to_owned(),
        role: Role::PlatformOperator,
        projection_base_epoch: BASE_EPOCH,
        at: InjectedTime::new(200),
    }
}

pub(crate) fn promotion_request(fixture: &Fixture) -> PromotionRequest {
    PromotionRequest {
        cohort: fixture.canary.clone(),
        actor: "operator-1".to_owned(),
        role: Role::PlatformOperator,
        at: InjectedTime::new(100),
    }
}

pub(crate) struct CaseResult {
    pub(crate) receipt: Option<ReceiptState>,
    pub(crate) reason: Option<&'static str>,
    pub(crate) facts: BTreeMap<&'static str, bool>,
}

impl CaseResult {
    fn empty() -> Self {
        Self {
            receipt: None,
            reason: None,
            facts: BTreeMap::new(),
        }
    }

    fn receipt(mut self, state: ReceiptState, reason: Option<&'static str>) -> Self {
        self.receipt = Some(state);
        self.reason = reason;
        self
    }

    fn code(mut self, code: &'static str) -> Self {
        self.reason = Some(code);
        self
    }

    fn fact(mut self, name: &'static str, value: bool) -> Self {
        self.facts.insert(name, value);
        self
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn run_case(name: &str) -> CaseResult {
    match name {
        "rule_body_change" | "content_address" => {
            let fixture = fixture();
            let manifest_digest = fixture.manifest.digest();
            let changed = fixture
                .old
                .with_rule_body(&rule_id(), body(b"deny owner"))
                .expect("the rule exists");
            let stable = changed.canonical_bytes() == fixture.candidate.canonical_bytes();
            CaseResult::empty()
                .fact(
                    "policy_version_changed",
                    changed.version() != fixture.old.version(),
                )
                .fact(
                    "manifest_digest_unchanged",
                    fixture.manifest.digest() == manifest_digest,
                )
                .fact("canonical_bytes_stable", stable)
        }
        "dry_run_failure" => {
            let (mut lifecycle, fixture) = seeded();
            let mut request = request(&fixture, fixture.canary.clone());
            request.dry_run = None;
            let change = lifecycle.activate(request);
            CaseResult::empty()
                .receipt(change.receipt.state(), change.receipt.reason_code())
                .fact(
                    "active_unchanged",
                    lifecycle.active_version(&fixture.canary).is_none(),
                )
        }
        "approval_gate" => {
            let (mut lifecycle, fixture) = seeded();
            let mut request = request(&fixture, fixture.canary.clone());
            request.change_class = ChangeClass::ReadWiden;
            request.approvals = ApprovalSet::new(
                fixture.report.digest(),
                vec![Approver::new(Role::Security, "sec-1")],
            );
            let change = lifecycle.activate(request);
            CaseResult::empty()
                .receipt(change.receipt.state(), change.receipt.reason_code())
                .fact(
                    "control_unchanged",
                    lifecycle.active_version(&fixture.control) == Some(fixture.old.version()),
                )
        }
        "canary_and_control" => {
            let (mut lifecycle, fixture) = seeded();
            let change = lifecycle.activate(request(&fixture, fixture.canary.clone()));
            let control = lifecycle
                .active_projection(&fixture.control)
                .expect("the control cohort has a projection");
            let canary = lifecycle
                .active_projection(&fixture.canary)
                .expect("the canary cohort has a projection");
            CaseResult::empty()
                .receipt(change.receipt.state(), change.receipt.reason_code())
                .fact(
                    "canary_differs",
                    lifecycle.active_version(&fixture.canary)
                        != lifecycle.active_version(&fixture.control),
                )
                .fact("control_epoch_is_base", control.epoch() == BASE_EPOCH)
                .fact(
                    "canary_epoch_includes_version",
                    canary.epoch() != BASE_EPOCH
                        && canary.policy_version() == Some(fixture.candidate.version()),
                )
        }
        "promotion" | "deadline_promotion" => {
            let (mut lifecycle, fixture) = seeded();
            let _ = lifecycle.activate(request(&fixture, fixture.canary.clone()));
            let change = if name == "promotion" {
                lifecycle
                    .promote(&promotion_request(&fixture))
                    .expect("the rollout promotes")
            } else {
                let step = lifecycle
                    .tick(&RolloutTick {
                        cohort: fixture.canary.clone(),
                        observation: RolloutObservation::new(3),
                        actor: "operator-1".to_owned(),
                        role: Role::PlatformOperator,
                        at: InjectedTime::new(1_000),
                    })
                    .expect("the rollout ticks");
                let LifecycleStep::Promoted(change) = step else {
                    panic!("the rollout must promote");
                };
                change
            };
            CaseResult::empty()
                .receipt(change.receipt.state(), change.receipt.reason_code())
                .fact(
                    "control_promoted",
                    lifecycle.active_version(&fixture.control) == Some(fixture.candidate.version()),
                )
        }
        "rollback" => {
            let (mut lifecycle, fixture) = seeded();
            let _ = lifecycle.activate(request(&fixture, fixture.canary.clone()));
            lifecycle
                .promote(&promotion_request(&fixture))
                .expect("the rollout promotes");
            let catalog_len = lifecycle.catalog().len();
            let old_bytes = lifecycle
                .catalog()
                .resolve(fixture.old.version())
                .expect("the old version is retained")
                .canonical_bytes()
                .to_vec();
            let audit_len = lifecycle.audit().len();
            let change = lifecycle.rollback(&rollback_request(
                &fixture,
                fixture.candidate.version(),
                fixture.old.version(),
            ));
            CaseResult::empty()
                .receipt(change.receipt.state(), change.receipt.reason_code())
                .fact(
                    "catalog_unchanged",
                    lifecycle.catalog().len() == catalog_len,
                )
                .fact(
                    "old_record_unchanged",
                    lifecycle
                        .catalog()
                        .resolve(fixture.old.version())
                        .expect("the old version is retained")
                        .canonical_bytes()
                        == old_bytes,
                )
                .fact("audit_grew", lifecycle.audit().len() == audit_len + 1)
        }
        "notice_feed" => {
            let (mut lifecycle, fixture) = seeded();
            let change = lifecycle.activate(request(&fixture, fixture.canary.clone()));
            CaseResult::empty()
                .receipt(change.receipt.state(), change.receipt.reason_code())
                .fact(
                    "operational_feed",
                    change.notice.feed().name() == "operational",
                )
                .fact("not_business_event", !change.notice.is_business_event())
        }
        "catalog_immutable" => {
            let fixture = fixture();
            let mut catalog = PolicyCatalog::new();
            let version = catalog.insert(fixture.old.clone());
            let bytes = catalog
                .resolve(version)
                .expect("the record resolves")
                .canonical_bytes()
                .to_vec();
            let _ = catalog.insert(fixture.old.clone());
            let _ = catalog.insert(fixture.candidate.clone());
            let record = catalog.resolve(version).expect("the record resolves");
            CaseResult::empty()
                .fact("record_unchanged", record.canonical_bytes() == bytes)
                .fact("record_count_is_two", catalog.len() == 2)
        }
        "unknown_policy_version" => {
            let (mut lifecycle, fixture) = seeded();
            let unknown = PolicyVersion::from_bytes([9; 32]);
            let change =
                lifecycle.rollback(&rollback_request(&fixture, fixture.old.version(), unknown));
            CaseResult::empty().receipt(change.receipt.state(), change.receipt.reason_code())
        }
        "rollout_limit" => {
            let (mut lifecycle, fixture) = seeded();
            let _ = lifecycle.activate(request(&fixture, fixture.canary.clone()));
            let third = policy(b"allow status");
            let old = fixture
                .old
                .compile(&fixture.manifest)
                .expect("the old policy compiles");
            let candidate = third
                .compile(&fixture.manifest)
                .expect("the third policy compiles");
            let report = DryRun::run(&old, &candidate, &sample()).expect("the dry run succeeds");
            let mut request = request(&fixture, Cohort::control("tenant-c"));
            request.candidate = third;
            request.dry_run = Some(report.clone());
            request.approvals = approvals(report.digest());
            let change = lifecycle.activate(request);
            CaseResult::empty().receipt(change.receipt.state(), change.receipt.reason_code())
        }
        "stale_base" => {
            let (mut lifecycle, fixture) = seeded();
            let mut request = request(&fixture, fixture.control.clone());
            request.base_version = PolicyVersion::from_bytes([7; 32]);
            let change = lifecycle.activate(request);
            CaseResult::empty().receipt(change.receipt.state(), change.receipt.reason_code())
        }
        "compile_failure" => {
            let (mut lifecycle, fixture) = seeded();
            let mut rules = BTreeMap::new();
            rules.insert(
                RuleId::parse("other_rule").expect("the rule identity is valid"),
                body(b"allow other"),
            );
            let candidate = PolicyContent::new(
                policy_id(),
                SUPPORTED_POLICY_AST_VERSION,
                rules,
                CostBound::new(100, 8),
            )
            .expect("the policy is valid");
            let mut request = request(&fixture, fixture.canary.clone());
            request.candidate = candidate;
            let change = lifecycle.activate(request);
            CaseResult::empty().receipt(change.receipt.state(), change.receipt.reason_code())
        }
        "unknown_path" => {
            let (mut lifecycle, fixture) = seeded();
            let candidate = policy_with_body(body_with_paths(b"deny secret", &["secret"], 5));
            let mut request = request(&fixture, fixture.canary.clone());
            request.candidate = candidate;
            let change = lifecycle.activate(request);
            CaseResult::empty().receipt(change.receipt.state(), change.receipt.reason_code())
        }
        "unbounded" => {
            let (mut lifecycle, fixture) = seeded();
            let candidate = policy_with_body(body_with_paths(b"deny owner", &["owner"], 101));
            let mut request = request(&fixture, fixture.canary.clone());
            request.candidate = candidate;
            let change = lifecycle.activate(request);
            CaseResult::empty().receipt(change.receipt.state(), change.receipt.reason_code())
        }
        "policy_parse_error" => {
            let error =
                RuleBody::new(Vec::new(), BTreeSet::new(), 1, 1).expect_err("the AST is empty");
            CaseResult::empty()
                .fact("is_error", true)
                .code(error.code())
        }
        "policy_ast_unsupported" => {
            let fixture = fixture();
            let mut rules = BTreeMap::new();
            rules.insert(rule_id(), body(b"allow owner"));
            let candidate = PolicyContent::new(
                policy_id(),
                oxkv_contracts::version::PolicyAstVersion::new(2),
                rules,
                CostBound::new(100, 8),
            )
            .expect("the policy is valid");
            let error = candidate
                .compile(&fixture.manifest)
                .expect_err("the AST version is not supported");
            CaseResult::empty().code(error.code())
        }
        "over_budget_abort" => {
            let (mut lifecycle, fixture) = seeded();
            let _ = lifecycle.activate(request(&fixture, fixture.canary.clone()));
            let step = lifecycle
                .tick(&RolloutTick {
                    cohort: fixture.canary.clone(),
                    observation: RolloutObservation::new(11),
                    actor: "operator-1".to_owned(),
                    role: Role::PlatformOperator,
                    at: InjectedTime::new(50),
                })
                .expect("the rollout ticks");
            let LifecycleStep::Aborted(change) = step else {
                panic!("the canary must abort");
            };
            CaseResult::empty()
                .receipt(change.receipt.state(), change.receipt.reason_code())
                .fact(
                    "prior_pointer_restored",
                    lifecycle.active_version(&fixture.canary) == Some(fixture.old.version()),
                )
        }
        other => panic!("the golden case {other} is not supported"),
    }
}
