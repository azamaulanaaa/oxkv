//! Golden vector runner for tier placement, leases, seals, and movement.
//!
//! Every vector file is append-only for one contract version. A later essay
//! adds a case to the matching file. A released case does not change.

mod support;

use oxkv_contracts::version::{ContentDigest, ContentSeal, LeaseTerm, PageSequence, ShardId};
use oxkv_tier::{
    ArtifactId, DurabilityClass, LeaseAuthority, LeaseDuration, LeaseTime, PlacementFacts,
    PlacementPlan, PlacementPolicy, PlacementRule, ReplicaId, SealedContent, ShardRole, Tier,
    TierStack, TierStatus, WarmupBudget, WarmupCursor, WarmupItem, WriterId,
};

use support::GoldenCase;

fn page_artifact(page: u64) -> ArtifactId {
    ArtifactId::new(ShardId::new(vec![1]), 4, PageSequence::new(page))
}

fn artifact() -> ArtifactId {
    page_artifact(1)
}

fn authority() -> LeaseAuthority {
    LeaseAuthority::new(ShardId::new(vec![1]))
}

fn alice() -> WriterId {
    WriterId::new("alice")
}

fn bob() -> WriterId {
    WriterId::new("bob")
}

#[test]
fn lease_golden_vectors_are_pinned() {
    let file = support::load(include_str!("vectors/lease.json"));
    assert_eq!(file.contract, "lease/v1");
    assert_eq!(file.cases.len(), 5);
    for case in &file.cases {
        match case.name.as_str() {
            "stale writer after a handover" => stale_writer_case(case),
            "expired authority lease" => expired_lease_case(case),
            "held authority lease" => held_lease_case(case),
            "replica authority lease refused" => replica_lease_case(case),
            "term advances after release" => term_advance_case(case),
            other => panic!("the lease case {other} is unknown"),
        }
    }
}

fn stale_writer_case(case: &GoldenCase) {
    let mut authority = authority();
    let first = authority
        .acquire(
            ShardRole::Authority,
            alice(),
            LeaseTime::new(0),
            LeaseDuration::new(5),
        )
        .expect("the first lease succeeds");
    let second = authority
        .acquire(
            ShardRole::Authority,
            bob(),
            LeaseTime::new(5),
            LeaseDuration::new(5),
        )
        .expect("the second lease succeeds");
    assert_eq!(
        second.term().get(),
        support::expected_u64(case, "lease_term")
    );
    let error = authority
        .check_write(&first.fence(), LeaseTime::new(6))
        .expect_err("the stale writer is fenced");
    assert_eq!(error.code(), support::expected_str(case, "code"));
}

fn expired_lease_case(case: &GoldenCase) {
    let mut authority = authority();
    let grant = authority
        .acquire(
            ShardRole::Authority,
            alice(),
            LeaseTime::new(0),
            LeaseDuration::new(5),
        )
        .expect("the lease succeeds");
    let error = authority
        .check_write(&grant.fence(), LeaseTime::new(5))
        .expect_err("the expired lease is refused");
    assert_eq!(error.code(), support::expected_str(case, "code"));
}

fn held_lease_case(case: &GoldenCase) {
    let mut authority = authority();
    authority
        .acquire(
            ShardRole::Authority,
            alice(),
            LeaseTime::new(0),
            LeaseDuration::new(5),
        )
        .expect("the first lease succeeds");
    let error = authority
        .acquire(
            ShardRole::Authority,
            bob(),
            LeaseTime::new(1),
            LeaseDuration::new(5),
        )
        .expect_err("the held lease refuses the second writer");
    assert_eq!(error.code(), support::expected_str(case, "code"));
}

fn replica_lease_case(case: &GoldenCase) {
    let mut authority = authority();
    let error = authority
        .acquire(
            ShardRole::Replica,
            WriterId::new("replica-a"),
            LeaseTime::new(0),
            LeaseDuration::new(5),
        )
        .expect_err("the replica is refused");
    assert_eq!(error.code(), support::expected_str(case, "code"));
    assert_eq!(authority.last_term(), LeaseTerm::new(0));
}

fn term_advance_case(case: &GoldenCase) {
    let mut authority = authority();
    let first = authority
        .acquire(
            ShardRole::Authority,
            alice(),
            LeaseTime::new(0),
            LeaseDuration::new(5),
        )
        .expect("the first lease succeeds");
    authority
        .release(&first.fence())
        .expect("the lease releases");
    let second = authority
        .acquire(
            ShardRole::Authority,
            bob(),
            LeaseTime::new(1),
            LeaseDuration::new(5),
        )
        .expect("the second lease succeeds");
    assert_eq!(
        second.term().get(),
        support::expected_u64(case, "lease_term")
    );
    assert!(support::expected_code(case, "code").is_none());
}

#[test]
fn seal_golden_vectors_are_pinned() {
    let file = support::load(include_str!("vectors/seal.json"));
    assert_eq!(file.contract, "content-seal/v1");
    assert_eq!(file.cases.len(), 4);
    for case in &file.cases {
        match case.name.as_str() {
            "content digest ignores the lease term" => digest_case(case),
            "sealed page survives a lease change" => survives_case(case),
            "stale seal after a valid digest" => stale_seal_case(case),
            "checksum failure before the stale seal" => checksum_case(case),
            other => panic!("the seal case {other} is unknown"),
        }
    }
}

fn digest_case(case: &GoldenCase) {
    let first = SealedContent::seal(b"page-bytes".to_vec(), LeaseTerm::new(1));
    let second = SealedContent::seal(b"page-bytes".to_vec(), LeaseTerm::new(2));
    assert_eq!(
        first.digest().to_hex(),
        support::expected_str(case, "digest_term_1")
    );
    assert_eq!(
        second.digest().to_hex(),
        support::expected_str(case, "digest_term_2")
    );
    assert_eq!(
        first.digest() == second.digest(),
        support::expected_bool(case, "equal")
    );
}

fn survives_case(case: &GoldenCase) {
    let sealed = SealedContent::seal(b"page-bytes".to_vec(), LeaseTerm::new(1));
    assert!(support::expected_code(case, "read_code").is_none());
    sealed.verify_read().expect("the sealed page is valid");
    assert_eq!(
        String::from_utf8_lossy(sealed.bytes()),
        support::expected_str(case, "bytes")
    );
}

fn stale_seal_case(case: &GoldenCase) {
    let sealed = SealedContent::seal(b"page-bytes".to_vec(), LeaseTerm::new(1));
    assert!(support::expected_code(case, "read_code").is_none());
    sealed.verify_read().expect("the digest and seal are valid");
    let error = sealed
        .verify_for_write(LeaseTerm::new(2))
        .expect_err("the stale seal is refused");
    assert_eq!(error.code(), support::expected_str(case, "write_code"));
}

fn checksum_case(case: &GoldenCase) {
    let digest = ContentDigest::of_bytes(b"expected");
    let seal = ContentSeal::new(digest, LeaseTerm::new(1), None);
    let sealed = SealedContent::from_parts(b"corrupt".to_vec(), digest, seal);
    let read_error = sealed
        .verify_read()
        .expect_err("the corrupt artifact is refused");
    assert_eq!(read_error.code(), support::expected_str(case, "read_code"));
    let write_error = sealed
        .verify_for_write(LeaseTerm::new(2))
        .expect_err("the corrupt artifact is refused");
    assert_eq!(
        write_error.code(),
        support::expected_str(case, "write_code")
    );
}

#[test]
fn placement_golden_vectors_are_pinned() {
    let file = support::load(include_str!("vectors/placement.json"));
    assert_eq!(file.contract, "placement/v1");
    assert_eq!(file.cases.len(), 4);
    for case in &file.cases {
        match case.name.as_str() {
            "hot access stays in ram" => placement_case(case, false),
            "pinned segment stays in ram" => pinned_case(case),
            "cold large segment lands in object" => object_case(case),
            "a contradicting plan is refused" => violation_case(case),
            other => panic!("the placement case {other} is unknown"),
        }
    }
}

fn policy() -> PlacementPolicy {
    PlacementPolicy::new(
        1,
        vec![
            PlacementRule::new(Tier::Ram, DurabilityClass::Volatile)
                .with_max_version_distance(2)
                .with_min_access_bucket(4),
            PlacementRule::new(Tier::Disk, DurabilityClass::DurableLocal).with_max_size_class(3),
            PlacementRule::new(Tier::Object, DurabilityClass::DurableObject),
        ],
    )
}

fn hot_facts() -> PlacementFacts {
    PlacementFacts::new(ShardId::new(vec![1]), 7, 9, 1, 5, false)
}

fn placement_case(case: &GoldenCase, pinned: bool) {
    let facts = PlacementFacts::new(ShardId::new(vec![1]), 7, 9, 1, 5, pinned);
    let placement = policy()
        .evaluate(&facts)
        .expect("the policy returns a placement");
    assert_eq!(placement.tier.name(), support::expected_str(case, "tier"));
    assert_eq!(
        placement.durability.name(),
        support::expected_str(case, "durability")
    );
}

fn pinned_case(case: &GoldenCase) {
    let facts = PlacementFacts::new(ShardId::new(vec![1]), 7, 9, 1, 1, true);
    let policy = PlacementPolicy::new(
        1,
        vec![
            PlacementRule::new(Tier::Ram, DurabilityClass::Volatile).with_pinned(true),
            PlacementRule::new(Tier::Object, DurabilityClass::DurableObject),
        ],
    );
    let placement = policy
        .evaluate(&facts)
        .expect("the policy returns a placement");
    assert_eq!(placement.tier.name(), support::expected_str(case, "tier"));
    assert_eq!(
        placement.durability.name(),
        support::expected_str(case, "durability")
    );
}

fn object_case(case: &GoldenCase) {
    let facts = PlacementFacts::new(ShardId::new(vec![1]), 7, 20, 5, 0, false);
    let placement = policy()
        .evaluate(&facts)
        .expect("the policy returns a placement");
    assert_eq!(placement.tier.name(), support::expected_str(case, "tier"));
    assert_eq!(
        placement.durability.name(),
        support::expected_str(case, "durability")
    );
}

fn violation_case(case: &GoldenCase) {
    let facts = hot_facts();
    let plan = PlacementPlan::new(artifact(), Tier::Object);
    let error = policy()
        .validate_plan(&facts, &plan)
        .expect_err("the plan contradicts the policy");
    assert_eq!(error.code(), support::expected_str(case, "code"));
}

#[test]
fn tier_golden_vectors_are_pinned() {
    let file = support::load(include_str!("vectors/tier.json"));
    assert_eq!(file.contract, "tier-movement/v1");
    assert_eq!(file.cases.len(), 4);
    for case in &file.cases {
        match case.name.as_str() {
            "only copy eviction is refused" => only_copy_case(case),
            "unavailable tier falls back to object" => fallback_case(case),
            "warmup budget keeps a cursor" => warmup_case(case),
            "move preserves identity and bytes" => move_case(case),
            other => panic!("the tier case {other} is unknown"),
        }
    }
}

fn stack_with(tiers: &[Tier], bytes: &[u8]) -> TierStack {
    let mut stack = TierStack::memory();
    for tier in tiers {
        stack
            .put(*tier, artifact(), bytes.to_vec())
            .expect("the memory stack has every tier");
    }
    stack
}

fn only_copy_case(case: &GoldenCase) {
    let bytes = b"page-bytes";
    let digest = ContentDigest::of_bytes(bytes);
    let mut stack = stack_with(&[Tier::Disk], bytes);
    let error = stack
        .evict(&artifact(), digest, Tier::Disk)
        .expect_err("the only copy is refused");
    assert_eq!(error.code(), support::expected_str(case, "code"));
}

fn fallback_case(case: &GoldenCase) {
    let bytes = b"page-bytes";
    let mut stack = stack_with(&[Tier::Object], bytes);
    stack.set_status(Tier::Ram, TierStatus::Unavailable);
    let (tier, found) = stack
        .read_through(&artifact())
        .expect("the object tier answers");
    assert_eq!(tier.name(), support::expected_str(case, "tier"));
    assert_eq!(
        String::from_utf8_lossy(&found),
        support::expected_str(case, "bytes")
    );
}

fn warmup_case(case: &GoldenCase) {
    let mut stack = TierStack::memory();
    stack
        .put(Tier::Object, page_artifact(1), b"aaaa".to_vec())
        .expect("the object tier accepts the first page");
    stack
        .put(Tier::Object, page_artifact(2), b"bbbb".to_vec())
        .expect("the object tier accepts the second page");
    let items = [
        WarmupItem::new(page_artifact(1), ContentDigest::of_bytes(b"aaaa")),
        WarmupItem::new(page_artifact(2), ContentDigest::of_bytes(b"bbbb")),
    ];
    let outcome = stack.warmup(&items, WarmupCursor::start(), WarmupBudget::new(1, 8));
    assert_eq!(
        outcome.stop_code(),
        Some(support::expected_str(case, "code"))
    );
    assert_eq!(
        outcome.cursor().next_index() as u64,
        support::expected_u64(case, "cursor")
    );
    assert_eq!(
        outcome.promoted().len() as u64,
        support::expected_u64(case, "promoted")
    );
}

fn move_case(case: &GoldenCase) {
    let bytes = b"page-bytes";
    let digest = ContentDigest::of_bytes(bytes);
    let mut stack = stack_with(&[Tier::Disk], bytes);
    let movement = stack
        .move_artifact(&artifact(), digest, Tier::Object)
        .expect("the move succeeds");
    assert_eq!(
        movement.source().name(),
        support::expected_str(case, "source")
    );
    assert_eq!(
        movement.target().name(),
        support::expected_str(case, "target")
    );
    let (_, found) = stack
        .read_through(&artifact())
        .expect("the object tier answers");
    assert_eq!(
        String::from_utf8_lossy(&found),
        support::expected_str(case, "bytes")
    );
}

#[test]
fn every_golden_file_names_a_contract() {
    for text in [
        include_str!("vectors/lease.json"),
        include_str!("vectors/seal.json"),
        include_str!("vectors/placement.json"),
        include_str!("vectors/tier.json"),
    ] {
        let file = support::load(text);
        assert_ne!(file.contract, "");
        assert!(!file.cases.is_empty());
        assert!(
            file.cases
                .iter()
                .all(|case| support::has_case(&file, &case.name))
        );
    }
}

#[test]
fn a_local_tab_lease_is_never_an_authority_lease() {
    let lease = oxkv_tier::LocalTabLease::new(ReplicaId::new("replica-a"), "tab-a", 1);
    assert!(!lease.holds_authority_write_lease());
    assert!(!lease.may_commit_data());
}

#[test]
fn a_sealed_page_survives_an_authority_handover() {
    let mut authority = authority();
    let first = authority
        .acquire(
            ShardRole::Authority,
            alice(),
            LeaseTime::new(0),
            LeaseDuration::new(5),
        )
        .expect("the first lease succeeds");
    let sealed = SealedContent::seal(b"page-bytes".to_vec(), first.term());
    authority
        .release(&first.fence())
        .expect("the lease releases");
    let second = authority
        .acquire(
            ShardRole::Authority,
            bob(),
            LeaseTime::new(5),
            LeaseDuration::new(5),
        )
        .expect("the second lease succeeds");
    assert_ne!(first.term(), second.term());
    sealed
        .verify_read()
        .expect("the new leader serves the same sealed bytes");
    assert_eq!(sealed.bytes(), b"page-bytes");
    assert_eq!(
        sealed.verify_for_write(second.term()),
        Err(oxkv_tier::TierError::StaleEpoch)
    );
}
