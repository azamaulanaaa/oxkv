//! Golden vectors and acceptance tests for the scope contract.

use oxkv_contracts::Hash32;
use oxkv_contracts::keyspace::TableName;
use oxkv_contracts::version::{
    CommitVersion, KeyVersion, PageId, PolicyVersion, ProjectionEpoch, ProjectionWatermarks,
    ScopeVersionVector, ShardId, VersionVector,
};
use oxkv_scope::{
    AdmissionLimit, AdmissionLimits, DiffInterval, DiffLog, DiffRecord, EvictionReason, FieldName,
    LocalRead, Membership, Ordering, PolicySelector, ProjectionSelector, QueryMode, RemoveReason,
    ReplicaResidency, ResidencyError, ResidencyPage, RuleSetSelector, ScopeDeclaration, ScopeError,
    ScopeMaterialization, ScopeRow, ScopeStats, ShardAssignment, ShardPlan, ShardResponse,
    SubscribeBudget, TablePrefix,
};

const IDENTITY: &str = "1dd8c63f6dbd28e14a3c0f134f1845bd76976c42754fca437ab70b52e256f79f";
const DIFF_DIGEST: &str = "1945de309a8e2b0a9e9d999fc64ef8b1a85838d6d8d1265505a91972edee0969";

fn declaration() -> ScopeDeclaration {
    ScopeDeclaration::new(
        [
            TablePrefix::parse("accounts").expect("valid table"),
            TablePrefix::parse("ledger_entry").expect("valid table"),
            TablePrefix::parse("audit_log").expect("valid table"),
        ],
        PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
        ProjectionSelector::fields([
            FieldName::new("title").expect("valid field"),
            FieldName::new("body").expect("valid field"),
        ])
        .expect("valid projection"),
        Ordering::Field(FieldName::new("title").expect("valid field")),
        AdmissionLimits::new(1_000, 1_000_000, 3),
    )
    .expect("valid declaration")
}

fn materialization() -> ScopeMaterialization {
    ScopeMaterialization::new(
        declaration().identity(),
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

fn interval() -> DiffInterval {
    DiffInterval::new(
        declaration().identity(),
        1,
        PolicyVersion::from_bytes([1; 32]),
        ScopeVersionVector::new(),
        vector(&[(&[1], 1)]),
        vec![
            DiffRecord::add(b"accounts/1".to_vec(), KeyVersion::new(1), 10),
            DiffRecord::remove(
                b"accounts/2".to_vec(),
                KeyVersion::new(2),
                RemoveReason::RecordChanged,
            ),
        ],
    )
    .expect("valid interval")
}

fn plan() -> ShardPlan {
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
    ShardPlan::build(&declaration(), &assignments).expect("the plan builds")
}

fn pin(plan: &ShardPlan, version: u64) -> ScopeVersionVector {
    let mut vector = VersionVector::new();
    for shard_id in plan.shards() {
        vector.set(shard_id.clone(), CommitVersion::new(version));
    }
    ScopeVersionVector::from_vector(vector)
}

#[test]
fn the_same_query_gives_the_same_golden_identity() {
    let reordered = ScopeDeclaration::new(
        [
            TablePrefix::parse("audit_log").expect("valid table"),
            TablePrefix::parse("accounts").expect("valid table"),
            TablePrefix::parse("ledger_entry").expect("valid table"),
        ],
        PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
        ProjectionSelector::fields([
            FieldName::new("body").expect("valid field"),
            FieldName::new("title").expect("valid field"),
        ])
        .expect("valid projection"),
        Ordering::Field(FieldName::new("title").expect("valid field")),
        AdmissionLimits::new(1_000, 1_000_000, 3),
    )
    .expect("valid declaration");
    assert_eq!(declaration().identity().to_hex(), IDENTITY);
    assert_eq!(reordered.identity().to_hex(), IDENTITY);
    assert_eq!(interval().to_bytes().len(), 181);
    assert_eq!(Hash32::of(&interval().to_bytes()).to_hex(), DIFF_DIGEST);
}

#[test]
fn a_record_that_stops_matching_produces_one_remove() {
    let mut membership = Membership::new(materialization());
    membership
        .apply(
            &DiffInterval::new(
                declaration().identity(),
                1,
                PolicyVersion::from_bytes([1; 32]),
                ScopeVersionVector::new(),
                vector(&[(&[1], 1)]),
                vec![
                    DiffRecord::add(b"accounts/1".to_vec(), KeyVersion::new(1), 10),
                    DiffRecord::add(b"accounts/2".to_vec(), KeyVersion::new(1), 20),
                ],
            )
            .expect("valid add interval"),
        )
        .expect("the add interval applies");
    let churn = membership
        .apply(
            &DiffInterval::new(
                declaration().identity(),
                2,
                PolicyVersion::from_bytes([1; 32]),
                vector(&[(&[1], 1)]),
                vector(&[(&[1], 2)]),
                vec![DiffRecord::remove(
                    b"accounts/1".to_vec(),
                    KeyVersion::new(2),
                    RemoveReason::RecordChanged,
                )],
            )
            .expect("valid remove interval"),
        )
        .expect("the remove interval applies");
    assert_eq!(churn.adds, 0);
    assert_eq!(churn.removes, 1);
    assert_eq!(membership.len(), 1);
    assert!(!membership.contains(b"accounts/1"));
    assert!(membership.read_local(b"accounts/1").is_absent());
}

#[test]
fn a_cross_prefix_scope_gives_one_version_vector() {
    let plan = plan();
    let pin = pin(&plan, 5);
    let responses: Vec<ShardResponse> = plan
        .shards()
        .map(|shard| {
            ShardResponse::served(
                shard.clone(),
                CommitVersion::new(5),
                vec![ScopeRow::new(shard.as_bytes().to_vec(), KeyVersion::new(1))],
            )
        })
        .collect();
    let exact = plan
        .query(QueryMode::Exact, &pin, &responses)
        .expect("the query is exact")
        .exact()
        .expect("the result is exact")
        .clone();
    assert_eq!(plan.len(), 3);
    assert_eq!(exact.pin().as_vector().len(), 3);
    assert_eq!(exact.rows().len(), 3);
}

#[test]
fn an_unreachable_shard_gives_a_coverage_result() {
    let plan = plan();
    let pin = pin(&plan, 5);
    let responses = vec![
        ShardResponse::served(
            ShardId::new(vec![1]),
            CommitVersion::new(5),
            vec![ScopeRow::new(b"a".to_vec(), KeyVersion::new(1))],
        ),
        ShardResponse::unreachable(ShardId::new(vec![2])),
        ShardResponse::served(
            ShardId::new(vec![3]),
            CommitVersion::new(5),
            vec![ScopeRow::new(b"c".to_vec(), KeyVersion::new(1))],
        ),
    ];
    let coverage = plan
        .query(QueryMode::Coverage, &pin, &responses)
        .expect("coverage mode succeeds")
        .coverage()
        .expect("the result is coverage")
        .clone();
    assert!(!coverage.claims_exact());
    assert!(coverage.is_partial());
    assert_eq!(coverage.descriptor().missing(), &[ShardId::new(vec![2])]);
    assert_eq!(coverage.descriptor().answered().len(), 2);
}

#[test]
fn an_over_limit_subscribe_fails_with_no_data() {
    let declaration = ScopeDeclaration::new(
        [TablePrefix::parse("accounts").expect("valid table")],
        PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
        ProjectionSelector::all(),
        Ordering::CanonicalKey,
        AdmissionLimits::new(2, 100, 1),
    )
    .expect("valid declaration");
    let error = declaration
        .admit(
            ScopeStats::new(3, 10, 1),
            SubscribeBudget::new(1_000, 1_000),
        )
        .expect_err("the scope is over the member limit");
    assert_eq!(
        error,
        ScopeError::OverLimit {
            limit: AdmissionLimit::Members,
            limit_value: 2,
            observed: 3
        }
    );
    assert_eq!(error.code(), "scope.over_limit");
}

#[test]
fn a_policy_change_opens_a_new_interval_while_old_diffs_parse() {
    let mut log = DiffLog::new(declaration().identity());
    let first = DiffInterval::new(
        declaration().identity(),
        1,
        PolicyVersion::from_bytes([1; 32]),
        ScopeVersionVector::new(),
        vector(&[(&[1], 1)]),
        vec![DiffRecord::add(
            b"accounts/1".to_vec(),
            KeyVersion::new(1),
            10,
        )],
    )
    .expect("valid first interval");
    let first_bytes = first.to_bytes();
    log.push(first.clone()).expect("the first interval applies");
    let second = DiffInterval::new(
        declaration().identity(),
        2,
        PolicyVersion::from_bytes([2; 32]),
        vector(&[(&[1], 1)]),
        vector(&[(&[1], 2)]),
        vec![DiffRecord::remove(
            b"accounts/1".to_vec(),
            KeyVersion::new(2),
            RemoveReason::PolicyChanged,
        )],
    )
    .expect("valid second interval");
    log.push(second).expect("the second interval applies");
    assert_eq!(log.intervals().len(), 2);
    assert_eq!(
        log.current_policy_version(),
        Some(PolicyVersion::from_bytes([2; 32]))
    );
    assert_eq!(
        DiffInterval::from_bytes(&first_bytes).expect("the old interval parses"),
        first
    );
}

#[test]
fn an_offline_scope_read_answers_at_the_local_pin() {
    let mut membership = Membership::new(materialization());
    membership
        .apply(
            &DiffInterval::new(
                declaration().identity(),
                1,
                PolicyVersion::from_bytes([1; 32]),
                ScopeVersionVector::new(),
                vector(&[(&[1], 1)]),
                vec![DiffRecord::add(
                    b"accounts/1".to_vec(),
                    KeyVersion::new(1),
                    10,
                )],
            )
            .expect("valid interval"),
        )
        .expect("the interval applies");
    assert!(matches!(
        membership.read_at(b"accounts/1", &vector(&[(&[1], 1)])),
        LocalRead::Present(_)
    ));
    assert!(matches!(
        membership.read_at(b"accounts/1", &vector(&[(&[1], 2)])),
        LocalRead::NotHydrated { .. }
    ));
    membership
        .apply(
            &DiffInterval::new(
                declaration().identity(),
                2,
                PolicyVersion::from_bytes([1; 32]),
                vector(&[(&[1], 1)]),
                vector(&[(&[1], 2)]),
                vec![DiffRecord::remove(
                    b"accounts/1".to_vec(),
                    KeyVersion::new(2),
                    RemoveReason::RecordChanged,
                )],
            )
            .expect("valid remove interval"),
        )
        .expect("the remove interval applies");
    assert!(matches!(
        membership.read_at(b"accounts/1", &vector(&[(&[1], 2)])),
        LocalRead::Absent
    ));
    assert!(matches!(
        membership.read_at(b"accounts/1", &vector(&[(&[1], 1)])),
        LocalRead::NotResident { .. }
    ));
}

#[test]
fn eviction_does_not_change_membership() {
    let mut membership = Membership::new(materialization());
    membership
        .apply(
            &DiffInterval::new(
                declaration().identity(),
                1,
                PolicyVersion::from_bytes([1; 32]),
                ScopeVersionVector::new(),
                vector(&[(&[1], 1)]),
                vec![DiffRecord::add(
                    b"accounts/1".to_vec(),
                    KeyVersion::new(1),
                    10,
                )],
            )
            .expect("valid interval"),
        )
        .expect("the interval applies");
    let before = membership.clone();
    let mut residency = ReplicaResidency::new();
    let other_bounds = TablePrefix::parse("other_table")
        .expect("valid table")
        .bounds()
        .clone();
    let out_of_scope = ResidencyPage::new(
        PageId::from_bytes([1; 32]),
        ShardId::new(vec![1]),
        other_bounds,
        CommitVersion::new(1),
        Vec::new(),
    );
    let page_id = out_of_scope.page_id;
    let _ = residency.insert(out_of_scope);
    let evicted = residency
        .evict(
            &page_id,
            &[TablePrefix::parse("accounts")
                .expect("valid table")
                .bounds()
                .clone()],
            &membership,
        )
        .expect("the out-of-scope page leaves");
    assert_eq!(evicted.reason, EvictionReason::OutOfScope);
    assert_eq!(membership, before);
    assert!(membership.contains(b"accounts/1"));

    let in_scope = ResidencyPage::new(
        PageId::from_bytes([2; 32]),
        ShardId::new(vec![1]),
        TablePrefix::parse("accounts")
            .expect("valid table")
            .bounds()
            .clone(),
        CommitVersion::new(1),
        vec![b"accounts/1".to_vec()],
    );
    let page_id = in_scope.page_id;
    let _ = residency.insert(in_scope);
    assert_eq!(
        residency
            .evict(
                &page_id,
                &[TablePrefix::parse("accounts")
                    .expect("valid table")
                    .bounds()
                    .clone()],
                &membership,
            )
            .expect_err("the in-scope page stays"),
        ResidencyError::InScopeMember {
            page_id,
            key: b"accounts/1".to_vec()
        }
    );
    assert_eq!(membership, before);
}
