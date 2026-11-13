//! End-to-end tests for the rule lifecycle.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::keyspace::TableName;
use oxkv_contracts::receipt::ReceiptState;
use oxkv_contracts::version::{
    ContentDigest, FactSourceId, PolicyAstVersion, PolicyVersion, ProjectionEpoch,
};
use oxkv_rule::{
    ActivationRequest, ApprovalSet, Approver, AuditVisibility, ChangeClass, Cohort, CostBound,
    DryRun, DryRunReport, DryRunSample, FactReadSet, InjectedTime, LifecycleStep, NoticeFeed,
    NoticeKind, PolicyCatalog, PolicyContent, PolicyId, ProjectionStamp, PromotionRequest,
    RetentionClass, Role, RollbackRequest, RolloutObservation, RolloutTick, RuleBody, RuleEffect,
    RuleId, RuleLifecycle, RuleManifest, RuleMetadata, RuleScope, RuleSelector,
    SUPPORTED_POLICY_AST_VERSION, SampleRow, StructuralDiff, Verdict,
};

const BASE_EPOCH: ProjectionEpoch = ProjectionEpoch::new(41);
const AUTHOR: &str = "author-1";

fn rule_id() -> RuleId {
    RuleId::parse("tenant_scope").expect("the rule identity is valid")
}

fn policy_id() -> PolicyId {
    PolicyId::parse("tenant_policy").expect("the policy identity is valid")
}

fn manifest() -> RuleManifest {
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

fn body(ast: &[u8]) -> RuleBody {
    let mut paths = BTreeSet::new();
    paths.insert("owner".to_owned());
    RuleBody::new(ast.to_vec(), paths, 5, 2).expect("the body is valid")
}

fn body_with_paths(ast: &[u8], paths: &[&str], steps: u64) -> RuleBody {
    let paths = paths.iter().map(|path| (*path).to_owned()).collect();
    RuleBody::new(ast.to_vec(), paths, steps, 2).expect("the body is valid")
}

fn policy(ast: &[u8]) -> PolicyContent {
    policy_with_body(body(ast))
}

fn policy_with_body(body: RuleBody) -> PolicyContent {
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

struct Fixture {
    manifest: RuleManifest,
    old: PolicyContent,
    candidate: PolicyContent,
    report: DryRunReport,
    control: Cohort,
    canary: Cohort,
}

fn fixture() -> Fixture {
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

fn seeded() -> (RuleLifecycle, Fixture) {
    let fixture = fixture();
    let mut lifecycle = RuleLifecycle::bootstrap(fixture.manifest.clone(), fixture.old.clone())
        .expect("the lifecycle starts");
    lifecycle
        .assign_initial(fixture.control.clone(), fixture.old.version(), BASE_EPOCH)
        .expect("the initial cohort pins the old version");
    (lifecycle, fixture)
}

fn approvals(digest: ContentDigest) -> ApprovalSet {
    ApprovalSet::new(
        digest,
        vec![
            Approver::new(Role::TableOwner, "owner-1"),
            Approver::new(Role::PlatformOperator, "operator-1"),
            Approver::new(Role::Security, "sec-1"),
        ],
    )
}

fn request(fixture: &Fixture, cohort: Cohort) -> ActivationRequest {
    ActivationRequest {
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

#[test]
fn a_rule_body_change_creates_a_new_policy_version_and_keeps_the_manifest_digest() {
    let fixture = fixture();
    let manifest_digest = fixture.manifest.digest();
    let changed = fixture
        .old
        .with_rule_body(&rule_id(), body(b"deny owner"))
        .expect("the rule exists");
    assert_ne!(changed.version(), fixture.old.version());
    assert_eq!(fixture.manifest.digest(), manifest_digest);
    assert_eq!(fixture.candidate.version(), changed.version());
}

#[test]
fn a_dry_run_failure_blocks_activation() {
    let (mut lifecycle, fixture) = seeded();
    let catalog_len = lifecycle.catalog().len();
    let mut request = request(&fixture, fixture.canary.clone());
    request.dry_run = None;
    let change = lifecycle.activate(request);
    assert_eq!(change.receipt.state(), ReceiptState::Rejected);
    assert_eq!(change.receipt.reason_code(), Some("rule.dry_run_failed"));
    assert_eq!(lifecycle.active_version(&fixture.canary), None);
    assert_eq!(lifecycle.catalog().len(), catalog_len);
}

#[test]
fn an_unpinned_sample_fails_the_dry_run() {
    let fixture = fixture();
    let old = fixture
        .old
        .compile(&fixture.manifest)
        .expect("the old policy compiles");
    let candidate = fixture
        .candidate
        .compile(&fixture.manifest)
        .expect("the candidate compiles");
    let sample = DryRunSample::unpinned(vec![SampleRow::new(
        "tenant-a",
        "get",
        "row-1",
        Verdict::Allow,
        Verdict::Deny,
    )]);
    let error = DryRun::run(&old, &candidate, &sample).expect_err("the sample is not pinned");
    assert_eq!(error.code(), "rule.dry_run_failed");
}

#[test]
fn the_approval_gate_rejects_a_read_widen_with_one_approver() {
    let (mut lifecycle, fixture) = seeded();
    let mut request = request(&fixture, fixture.canary.clone());
    request.change_class = ChangeClass::ReadWiden;
    request.approvals = ApprovalSet::new(
        fixture.report.digest(),
        vec![Approver::new(Role::Security, "sec-1")],
    );
    let change = lifecycle.activate(request);
    assert_eq!(change.receipt.state(), ReceiptState::Rejected);
    assert_eq!(change.receipt.reason_code(), Some("rule.approval_required"));
    assert_eq!(
        lifecycle.active_version(&fixture.control),
        Some(fixture.old.version())
    );
    assert_eq!(lifecycle.active_version(&fixture.canary), None);
}

#[test]
fn a_canary_and_a_control_cohort_serve_different_policy_versions() {
    let (mut lifecycle, fixture) = seeded();
    let change = lifecycle.activate(request(&fixture, fixture.canary.clone()));
    assert!(change.is_committed());
    assert_eq!(
        lifecycle.active_version(&fixture.control),
        Some(fixture.old.version())
    );
    assert_eq!(
        lifecycle.active_version(&fixture.canary),
        Some(fixture.candidate.version())
    );
    let control = lifecycle
        .active_projection(&fixture.control)
        .expect("the control cohort has a projection");
    let canary = lifecycle
        .active_projection(&fixture.canary)
        .expect("the canary cohort has a projection");
    assert_eq!(control.epoch(), BASE_EPOCH);
    assert_ne!(canary.epoch(), BASE_EPOCH);
    assert_eq!(canary.policy_version(), Some(fixture.candidate.version()));
}

#[test]
fn a_predicate_change_with_the_same_facts_does_not_invalidate_the_projection() {
    let fixture = fixture();
    let facts = fixture
        .manifest
        .rule(&rule_id())
        .expect("the rule exists")
        .fact_reads
        .clone();
    let old = ProjectionStamp::for_canary(BASE_EPOCH, fixture.old.version(), &facts);
    let new = ProjectionStamp::for_canary(BASE_EPOCH, fixture.candidate.version(), &facts);
    assert_ne!(old.epoch(), new.epoch());
    assert!(!old.requires_rebuild(new));
}

#[test]
fn promotion_moves_the_control_cohort_to_the_candidate() {
    let (mut lifecycle, fixture) = seeded();
    let _ = lifecycle.activate(request(&fixture, fixture.canary.clone()));
    let change = lifecycle
        .promote(&PromotionRequest {
            cohort: fixture.canary.clone(),
            actor: "operator-1".to_owned(),
            role: Role::PlatformOperator,
            at: InjectedTime::new(100),
        })
        .expect("the rollout promotes");
    assert_eq!(change.receipt.state(), ReceiptState::Committed);
    assert_eq!(change.notice.kind(), NoticeKind::Promoted);
    assert_eq!(
        lifecycle.active_version(&fixture.control),
        Some(fixture.candidate.version())
    );
    assert_eq!(
        lifecycle.active_version(&fixture.canary),
        Some(fixture.candidate.version())
    );
    assert!(lifecycle.rollout(&fixture.canary).is_none());
}

#[test]
fn a_rollback_restores_an_earlier_version_without_rewriting_history() {
    let (mut lifecycle, fixture) = seeded();
    let _ = lifecycle.activate(request(&fixture, fixture.canary.clone()));
    lifecycle
        .promote(&PromotionRequest {
            cohort: fixture.canary.clone(),
            actor: "operator-1".to_owned(),
            role: Role::PlatformOperator,
            at: InjectedTime::new(100),
        })
        .expect("the rollout promotes");
    let catalog_len = lifecycle.catalog().len();
    let old_bytes = lifecycle
        .catalog()
        .resolve(fixture.old.version())
        .expect("the old version is retained")
        .canonical_bytes()
        .to_vec();
    let audit_len = lifecycle.audit().len();
    let change = lifecycle.rollback(&RollbackRequest {
        policy_id: policy_id(),
        cohort: fixture.control.clone(),
        base_version: fixture.candidate.version(),
        target_version: fixture.old.version(),
        actor: "operator-1".to_owned(),
        role: Role::PlatformOperator,
        projection_base_epoch: BASE_EPOCH,
        at: InjectedTime::new(200),
    });
    assert_eq!(change.receipt.state(), ReceiptState::Committed);
    assert_eq!(
        lifecycle.active_version(&fixture.control),
        Some(fixture.old.version())
    );
    assert_eq!(lifecycle.catalog().len(), catalog_len);
    assert_eq!(
        lifecycle
            .catalog()
            .resolve(fixture.old.version())
            .expect("the old version is retained")
            .canonical_bytes(),
        old_bytes
    );
    assert_eq!(lifecycle.audit().len(), audit_len + 1);
    assert!(lifecycle.audit().verify());
}

#[test]
fn a_lifecycle_notice_uses_the_operational_feed() {
    let (mut lifecycle, fixture) = seeded();
    let change = lifecycle.activate(request(&fixture, fixture.canary.clone()));
    assert_eq!(change.notice.feed(), NoticeFeed::Operational);
    assert!(!change.notice.is_business_event());
    assert_eq!(change.notice.kind(), NoticeKind::Activated);
}

#[test]
fn a_policy_catalog_record_is_immutable() {
    let fixture = fixture();
    let mut catalog = PolicyCatalog::new();
    let version = catalog.insert(fixture.old.clone());
    let bytes = catalog
        .resolve(version)
        .expect("the record resolves")
        .canonical_bytes()
        .to_vec();
    let generation = catalog
        .resolve(version)
        .expect("the record resolves")
        .generation();
    let _ = catalog.insert(fixture.old.clone());
    let _ = catalog.insert(fixture.candidate.clone());
    let record = catalog.resolve(version).expect("the record resolves");
    assert_eq!(record.canonical_bytes(), bytes);
    assert_eq!(record.generation(), generation);
    assert_eq!(catalog.len(), 2);
}

#[test]
fn an_unknown_policy_version_fails() {
    let (mut lifecycle, fixture) = seeded();
    let unknown = PolicyVersion::from_bytes([9; 32]);
    let change = lifecycle.rollback(&RollbackRequest {
        policy_id: policy_id(),
        cohort: fixture.control.clone(),
        base_version: fixture.old.version(),
        target_version: unknown,
        actor: "operator-1".to_owned(),
        role: Role::PlatformOperator,
        projection_base_epoch: BASE_EPOCH,
        at: InjectedTime::new(200),
    });
    assert_eq!(change.receipt.state(), ReceiptState::Rejected);
    assert_eq!(
        change.receipt.reason_code(),
        Some("scope.policy_version_unknown")
    );
    assert_eq!(
        lifecycle.active_version(&fixture.control),
        Some(fixture.old.version())
    );
}

#[test]
fn a_third_live_version_returns_the_rollout_limit() {
    let (mut lifecycle, fixture) = seeded();
    let _ = lifecycle.activate(request(&fixture, fixture.canary.clone()));
    let third = policy(b"allow status");
    let old_compiled = fixture
        .old
        .compile(&fixture.manifest)
        .expect("the old policy compiles");
    let third_compiled = third
        .compile(&fixture.manifest)
        .expect("the third policy compiles");
    let report =
        DryRun::run(&old_compiled, &third_compiled, &sample()).expect("the dry run succeeds");
    let mut request = request(&fixture, Cohort::control("tenant-c"));
    request.candidate = third;
    request.dry_run = Some(report.clone());
    request.approvals = approvals(report.digest());
    let change = lifecycle.activate(request);
    assert_eq!(change.receipt.state(), ReceiptState::Rejected);
    assert_eq!(change.receipt.reason_code(), Some("rule.rollout_limit"));
}

#[test]
fn a_stale_base_version_returns_conflict() {
    let (mut lifecycle, fixture) = seeded();
    let mut request = request(&fixture, fixture.control.clone());
    request.base_version = PolicyVersion::from_bytes([7; 32]);
    let change = lifecycle.activate(request);
    assert_eq!(change.receipt.state(), ReceiptState::Conflict);
    assert_eq!(change.receipt.reason_code(), Some("write.version_conflict"));
    assert_eq!(
        lifecycle.active_version(&fixture.control),
        Some(fixture.old.version())
    );
}

#[test]
fn a_compile_failure_returns_rule_compile_failed() {
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
    assert_eq!(change.receipt.state(), ReceiptState::Rejected);
    assert_eq!(change.receipt.reason_code(), Some("rule.compile_failed"));
}

#[test]
fn an_unknown_path_returns_manifest_rule_unknown_path() {
    let (mut lifecycle, fixture) = seeded();
    let candidate = policy_with_body(body_with_paths(b"deny secret", &["secret"], 5));
    let mut request = request(&fixture, fixture.canary.clone());
    request.candidate = candidate;
    let change = lifecycle.activate(request);
    assert_eq!(change.receipt.state(), ReceiptState::Rejected);
    assert_eq!(
        change.receipt.reason_code(),
        Some("manifest.rule_unknown_path")
    );
}

#[test]
fn a_rule_over_its_cost_bound_returns_manifest_rule_unbounded() {
    let (mut lifecycle, fixture) = seeded();
    let candidate = policy_with_body(body_with_paths(b"deny owner", &["owner"], 101));
    let mut request = request(&fixture, fixture.canary.clone());
    request.candidate = candidate;
    let change = lifecycle.activate(request);
    assert_eq!(change.receipt.state(), ReceiptState::Rejected);
    assert_eq!(
        change.receipt.reason_code(),
        Some("manifest.rule_unbounded")
    );
}

#[test]
fn an_empty_ast_returns_policy_parse_error() {
    let error = RuleBody::new(Vec::new(), BTreeSet::new(), 1, 1).expect_err("the AST is empty");
    assert_eq!(error.code(), "policy.parse_error");
}

#[test]
fn an_unknown_ast_version_returns_policy_ast_unsupported() {
    let fixture = fixture();
    let mut rules = BTreeMap::new();
    rules.insert(rule_id(), body(b"allow owner"));
    let candidate = PolicyContent::new(
        policy_id(),
        PolicyAstVersion::new(2),
        rules,
        CostBound::new(100, 8),
    )
    .expect("the policy is valid");
    let error = candidate
        .compile(&fixture.manifest)
        .expect_err("the AST version is not supported");
    assert_eq!(error.code(), "policy.ast_unsupported");
}

#[test]
fn a_canary_over_budget_aborts_and_restores_the_prior_pointer() {
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
    assert_eq!(change.receipt.state(), ReceiptState::Rejected);
    assert_eq!(change.receipt.reason_code(), Some("rule.rollout_limit"));
    assert_eq!(change.notice.kind(), NoticeKind::Aborted);
    assert_eq!(
        lifecycle.active_version(&fixture.canary),
        Some(fixture.old.version())
    );
    assert!(lifecycle.rollout(&fixture.canary).is_none());
    assert!(lifecycle.audit().verify());
}

#[test]
fn a_rollout_that_reaches_its_deadline_promotes() {
    let (mut lifecycle, fixture) = seeded();
    let _ = lifecycle.activate(request(&fixture, fixture.canary.clone()));
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
    assert_eq!(change.receipt.state(), ReceiptState::Committed);
    assert_eq!(change.notice.kind(), NoticeKind::Promoted);
    assert_eq!(
        lifecycle.active_version(&fixture.control),
        Some(fixture.candidate.version())
    );
}

#[test]
fn a_read_widen_rollout_waits_for_manual_promotion() {
    let (mut lifecycle, fixture) = seeded();
    let mut request = request(&fixture, fixture.canary.clone());
    request.change_class = ChangeClass::ReadWiden;
    let change = lifecycle.activate(request);
    assert!(change.is_committed());
    let step = lifecycle
        .tick(&RolloutTick {
            cohort: fixture.canary.clone(),
            observation: RolloutObservation::new(3),
            actor: "operator-1".to_owned(),
            role: Role::PlatformOperator,
            at: InjectedTime::new(1_000),
        })
        .expect("the rollout ticks");
    assert_eq!(step, LifecycleStep::Continue);
    assert_eq!(
        lifecycle.active_version(&fixture.control),
        Some(fixture.old.version())
    );
    let promoted = lifecycle
        .promote(&PromotionRequest {
            cohort: fixture.canary.clone(),
            actor: "operator-1".to_owned(),
            role: Role::PlatformOperator,
            at: InjectedTime::new(1_001),
        })
        .expect("the manual promotion succeeds");
    assert!(promoted.is_committed());
}

#[test]
fn an_empty_policy_catalog_returns_manifest_policy_catalog_missing() {
    let fixture = fixture();
    let error = RuleLifecycle::from_parts(fixture.manifest, PolicyCatalog::new())
        .expect_err("the catalog is missing");
    assert_eq!(error.code(), "manifest.policy_catalog_missing");
}

#[test]
fn a_structural_diff_names_the_changed_rule() {
    let fixture = fixture();
    let old = fixture
        .old
        .compile(&fixture.manifest)
        .expect("the old policy compiles");
    let candidate = fixture
        .candidate
        .compile(&fixture.manifest)
        .expect("the candidate compiles");
    let report = DryRun::run(&old, &candidate, &sample()).expect("the dry run succeeds");
    assert_eq!(report.diff().structural.rules_changed, vec![rule_id()]);
    assert_eq!(report.diff().structural.rules_added, Vec::<RuleId>::new());
    assert_eq!(
        report.diff().structural,
        StructuralDiff {
            rules_added: Vec::new(),
            rules_removed: Vec::new(),
            rules_changed: vec![rule_id()],
            paths_added: Vec::new(),
            paths_removed: Vec::new(),
            budget_changed: false,
        }
    );
    assert_eq!(report.diff().semantic.len(), 1);
}
