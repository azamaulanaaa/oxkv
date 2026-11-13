//! Behaviour tests for the policy projection crate.

use oxkv_contracts::ProjectionCursor;
use oxkv_contracts::receipt::ReceiptState;
use oxkv_contracts::version::{
    FactSourceId, FactVersion, PolicyVersion, ProjectionEpoch, ProjectionWatermarks, ShardId,
};
use oxkv_projection::{
    ApplyOutcome, AuthorizationDecision, AuthorizationMode, CanonicalType, Decision, FactChange,
    FactDeclaration, FactId, FactOwner, FactSet, FactSourceVersions, FactValue,
    FactVersionDistance, FeedResume, InvalidationCause, PolicyChange, PolicyCohort, Projection,
    ProjectionDiff, ProjectionEpochState, ProjectionError, ProjectionFeed, ProjectionFeedCursor,
    ProjectionSnapshot, QueueDeadline, ReadSetLimits, RuleId, StoredFact, Verdict,
    classify_fact_set_change, evaluate_invalidation, validate_cursor,
};

fn policy_one() -> PolicyVersion {
    PolicyVersion::from_bytes([1; 32])
}

fn policy_two() -> PolicyVersion {
    PolicyVersion::from_bytes([2; 32])
}

fn epoch_state(generation: u64, policy_version: PolicyVersion) -> ProjectionEpochState {
    ProjectionEpochState::new(ProjectionEpoch::new(generation), policy_version)
}

fn owner() -> FactOwner {
    FactOwner::Shard(ShardId::new(vec![2]))
}

fn fact_id() -> FactId {
    FactId::new("account_owner")
}

fn declaration(bound: u64) -> FactDeclaration {
    FactDeclaration::new(
        fact_id(),
        CanonicalType::Text,
        owner(),
        4,
        FactVersionDistance::new(bound),
    )
}

fn fact_set(bound: u64) -> FactSet {
    FactSet::from_declarations([declaration(bound)]).expect("the fact set is valid")
}

fn source() -> FactSourceId {
    owner().source_id()
}

fn watermark_map(version: u64) -> ProjectionWatermarks {
    let mut watermarks = ProjectionWatermarks::new();
    watermarks.set(source(), FactVersion::new(version));
    watermarks
}

fn stored(value: &str, version: u64) -> StoredFact {
    StoredFact::new(
        fact_id(),
        source(),
        FactVersion::new(version),
        FactValue::Text(value.to_owned()),
    )
}

fn base_snapshot(set: &FactSet, generation: u64, watermark: u64) -> ProjectionSnapshot {
    ProjectionSnapshot::new(
        ProjectionEpoch::new(generation),
        policy_one(),
        set.digest(),
        watermark_map(watermark),
        vec![stored("alice", watermark)],
    )
}

fn loaded_projection(set: &FactSet, generation: u64, watermark: u64) -> Projection {
    let mut projection = Projection::new(set.clone(), epoch_state(generation, policy_one()));
    projection
        .apply_snapshot(&base_snapshot(set, generation, watermark))
        .expect("the snapshot applies");
    projection
}

fn observed(version: u64) -> FactSourceVersions {
    let mut versions = FactSourceVersions::new();
    versions.set(source(), FactVersion::new(version));
    versions
}

fn read_set(projection: &Projection) -> oxkv_projection::ReadSet {
    projection
        .read_set(&[fact_id()], ReadSetLimits::new(4, 4))
        .expect("the read set is valid")
}

#[test]
fn a_non_strict_write_one_version_over_the_bound_stays_queued() {
    let set = fact_set(2);
    let projection = loaded_projection(&set, 1, 10);
    let report = projection
        .authorize(
            &read_set(&projection),
            &observed(13),
            AuthorizationMode::Default,
            QueueDeadline::Pending,
        )
        .expect("the projection is live");
    assert_eq!(
        report.decision,
        AuthorizationDecision::Queue {
            reason: oxkv_projection::PROJECTION_LAGGING
        }
    );
    assert_eq!(report.decision.receipt_state(), Some(ReceiptState::Queued));
    assert!(!report.decision.is_allowed());
    assert!(report.is_over_bound());
}

#[test]
fn the_same_write_at_the_deadline_is_rejected_with_queue_timeout() {
    let set = fact_set(2);
    let projection = loaded_projection(&set, 1, 10);
    let report = projection
        .authorize(
            &read_set(&projection),
            &observed(13),
            AuthorizationMode::Default,
            QueueDeadline::Reached,
        )
        .expect("the projection is live");
    assert_eq!(
        report.decision,
        AuthorizationDecision::Reject {
            reason: oxkv_projection::WRITE_QUEUE_TIMEOUT
        }
    );
    assert_eq!(
        report.decision.receipt_state(),
        Some(ReceiptState::Rejected)
    );
}

#[test]
fn a_strict_write_over_the_bound_is_rejected_with_stale_facts() {
    let set = fact_set(2);
    let projection = loaded_projection(&set, 1, 10);
    let report = projection
        .authorize(
            &read_set(&projection),
            &observed(13),
            AuthorizationMode::Strict,
            QueueDeadline::Pending,
        )
        .expect("the projection is live");
    assert_eq!(
        report.decision,
        AuthorizationDecision::Reject {
            reason: oxkv_projection::AUTHORIZATION_STALE_FACTS
        }
    );
    assert_eq!(
        report.decision.receipt_state(),
        Some(ReceiptState::Rejected)
    );
}

#[test]
fn a_write_inside_the_bound_is_allowed() {
    let set = fact_set(2);
    let projection = loaded_projection(&set, 1, 10);
    let report = projection
        .authorize(
            &read_set(&projection),
            &observed(12),
            AuthorizationMode::Default,
            QueueDeadline::Reached,
        )
        .expect("the projection is live");
    assert_eq!(report.decision, AuthorizationDecision::Allow);
    assert!(report.decision.is_allowed());
}

#[test]
fn a_same_fact_policy_change_does_not_invalidate() {
    let before = fact_set(2);
    let after = before.clone();
    let decision = evaluate_invalidation(&before, &after, PolicyChange::PredicateChanged);
    assert!(!decision.invalidates());
    assert_eq!(decision.cause(), None);

    let mut projection = loaded_projection(&before, 1, 10);
    let generation = projection.epoch();
    projection
        .rebind_policy(policy_two())
        .expect("a live projection rebinds");
    assert!(projection.is_valid());
    assert_eq!(projection.epoch(), generation);
    assert_eq!(projection.policy_version(), policy_two());
    assert!(projection.fact(&fact_id()).expect("live").is_some());
}

#[test]
fn a_fact_set_change_invalidates_and_forces_rebuild() {
    let before = fact_set(2);
    let mut after = before.clone();
    after
        .insert(FactDeclaration::new(
            FactId::new("account_tier"),
            CanonicalType::U64,
            FactOwner::Shard(ShardId::new(vec![3])),
            4,
            FactVersionDistance::new(0),
        ))
        .expect("the new fact is unique");
    let decision = evaluate_invalidation(&before, &after, PolicyChange::None);
    assert!(decision.invalidates());
    assert_eq!(decision.cause(), Some(InvalidationCause::FactSetChanged));

    let mut projection = loaded_projection(&before, 1, 10);
    let record = projection
        .invalidate(InvalidationCause::FactSetChanged)
        .expect("a live projection invalidates");
    assert!(!projection.is_valid());
    assert_eq!(
        projection.invalidation_cause(),
        Some(InvalidationCause::FactSetChanged)
    );
    assert_eq!(
        record.new_epoch.generation(),
        ProjectionEpoch::new(record.previous_epoch.generation().get() + 1)
    );
    assert_eq!(
        projection.watermarks().unwrap_err(),
        ProjectionError::EpochInvalid
    );

    let mut rebuild_set = before.clone();
    rebuild_set
        .insert(FactDeclaration::new(
            FactId::new("account_tier"),
            CanonicalType::U64,
            FactOwner::Shard(ShardId::new(vec![3])),
            4,
            FactVersionDistance::new(0),
        ))
        .expect("the new fact is unique");
    let rebuild_epoch = projection.epoch();
    let snapshot = ProjectionSnapshot::new(
        rebuild_epoch,
        policy_one(),
        rebuild_set.digest(),
        watermark_map(11),
        vec![stored("alice", 11)],
    );
    assert_eq!(
        projection
            .rebuild(rebuild_set, &snapshot)
            .expect("the rebuild applies"),
        ApplyOutcome::Applied
    );
    assert!(projection.is_valid());
    assert_eq!(
        projection.watermarks().expect("live").get(&source()),
        Some(FactVersion::new(11))
    );
}

#[test]
fn schema_ownership_and_revocation_changes_invalidate() {
    let before = fact_set(2);

    let mut schema_set = FactSet::new();
    schema_set
        .insert(FactDeclaration::new(
            fact_id(),
            CanonicalType::U64,
            owner(),
            4,
            FactVersionDistance::new(2),
        ))
        .expect("the schema fact is unique");
    assert_eq!(
        classify_fact_set_change(&before, &schema_set),
        Some(InvalidationCause::SchemaChanged)
    );

    let mut ownership = FactSet::new();
    ownership
        .insert(FactDeclaration::new(
            fact_id(),
            CanonicalType::Text,
            FactOwner::Shard(ShardId::new(vec![9])),
            4,
            FactVersionDistance::new(2),
        ))
        .expect("the ownership fact is unique");
    assert_eq!(
        classify_fact_set_change(&before, &ownership),
        Some(InvalidationCause::OwnershipChanged)
    );

    let mut revoked = before.clone();
    revoked.revoke(&fact_id()).expect("the fact is declared");
    assert_eq!(
        classify_fact_set_change(&before, &revoked),
        Some(InvalidationCause::Revocation)
    );
}

#[test]
fn a_rebuild_reaches_the_same_watermark_as_an_uninterrupted_feed() {
    let set = fact_set(8);
    let mut projection = Projection::new(set.clone(), epoch_state(1, policy_one()));
    let base = ProjectionSnapshot::new(
        ProjectionEpoch::new(1),
        policy_one(),
        set.digest(),
        ProjectionWatermarks::new(),
        Vec::new(),
    );
    projection
        .apply_snapshot(&base)
        .expect("the base snapshot applies");
    let first = ProjectionDiff::new(
        ProjectionEpoch::new(1),
        1,
        source(),
        FactVersion::new(1),
        vec![FactChange::upsert(
            fact_id(),
            FactVersion::new(1),
            FactValue::Text("alice".to_owned()),
        )],
    );
    let second = ProjectionDiff::new(
        ProjectionEpoch::new(1),
        2,
        source(),
        FactVersion::new(2),
        vec![FactChange::upsert(
            fact_id(),
            FactVersion::new(2),
            FactValue::Text("bob".to_owned()),
        )],
    );
    projection
        .apply_diff(&first)
        .expect("the first diff applies");
    projection
        .apply_diff(&second)
        .expect("the second diff applies");

    let rebuilt = loaded_projection(&set, 1, 2);
    assert_eq!(
        rebuilt.watermarks().expect("live").get(&source()),
        Some(FactVersion::new(2))
    );
    assert_eq!(
        rebuilt.cursor().expect("live"),
        projection.cursor().expect("live")
    );
}

#[test]
fn a_feed_resumes_after_a_sequence_and_falls_back_on_an_epoch_mismatch() {
    let set = fact_set(8);
    let base = ProjectionSnapshot::new(
        ProjectionEpoch::new(1),
        policy_one(),
        set.digest(),
        ProjectionWatermarks::new(),
        Vec::new(),
    );
    let first = ProjectionDiff::new(
        ProjectionEpoch::new(1),
        1,
        source(),
        FactVersion::new(1),
        vec![FactChange::upsert(
            fact_id(),
            FactVersion::new(1),
            FactValue::Text("alice".to_owned()),
        )],
    );
    let second = ProjectionDiff::new(
        ProjectionEpoch::new(1),
        2,
        source(),
        FactVersion::new(2),
        vec![FactChange::upsert(
            fact_id(),
            FactVersion::new(2),
            FactValue::Text("bob".to_owned()),
        )],
    );
    let feed = ProjectionFeed::new(base, vec![first, second]).expect("the feed is ordered");
    let cursor = ProjectionFeedCursor::new(
        ProjectionCursor::new(ProjectionWatermarks::new(), ProjectionEpoch::new(1)),
        1,
    );
    match feed.resume(&cursor) {
        FeedResume::Diffs(diffs) => {
            assert_eq!(diffs.len(), 1);
            assert_eq!(diffs[0].sequence(), 2);
        }
        FeedResume::FreshSnapshot(_) => panic!("the feed resumed from a fresh snapshot"),
    }

    let stale = ProjectionFeedCursor::new(
        ProjectionCursor::new(ProjectionWatermarks::new(), ProjectionEpoch::new(2)),
        0,
    );
    assert!(feed.resume(&stale).is_fresh_snapshot());
}

#[test]
fn apply_is_idempotent_and_rejects_a_version_regression() {
    let set = fact_set(8);
    let mut projection = loaded_projection(&set, 1, 10);
    let diff = ProjectionDiff::new(
        ProjectionEpoch::new(1),
        1,
        source(),
        FactVersion::new(11),
        vec![FactChange::upsert(
            fact_id(),
            FactVersion::new(11),
            FactValue::Text("bob".to_owned()),
        )],
    );
    assert_eq!(
        projection.apply_diff(&diff).expect("the first apply works"),
        ApplyOutcome::Applied
    );
    assert_eq!(
        projection
            .apply_diff(&diff)
            .expect("the repeat apply works"),
        ApplyOutcome::Duplicate
    );
    assert_eq!(
        projection.watermarks().expect("live").get(&source()),
        Some(FactVersion::new(11))
    );

    let regression = ProjectionDiff::new(
        ProjectionEpoch::new(1),
        2,
        source(),
        FactVersion::new(9),
        vec![FactChange::upsert(
            fact_id(),
            FactVersion::new(9),
            FactValue::Text("carol".to_owned()),
        )],
    );
    let error = projection
        .apply_diff(&regression)
        .expect_err("the regression fails");
    assert_eq!(error.code(), "projection.watermark_regression");
    assert_eq!(
        projection.watermarks().expect("live").get(&source()),
        Some(FactVersion::new(11))
    );
}

#[test]
fn a_fact_version_regression_fails_with_watermark_regression() {
    let set = fact_set(8);
    let mut projection = loaded_projection(&set, 1, 10);
    let diff = ProjectionDiff::new(
        ProjectionEpoch::new(1),
        1,
        source(),
        FactVersion::new(12),
        vec![FactChange::upsert(
            fact_id(),
            FactVersion::new(9),
            FactValue::Text("old".to_owned()),
        )],
    );
    let error = projection
        .apply_diff(&diff)
        .expect_err("the fact version moved backward");
    assert_eq!(error.code(), "projection.watermark_regression");
}

#[test]
fn a_checksum_failure_fails_closed() {
    let set = fact_set(8);
    let mut projection = loaded_projection(&set, 1, 10);
    let diff = ProjectionDiff::with_checksum(
        ProjectionEpoch::new(1),
        1,
        source(),
        FactVersion::new(11),
        vec![FactChange::upsert(
            fact_id(),
            FactVersion::new(11),
            FactValue::Text("bob".to_owned()),
        )],
        oxkv_contracts::version::Hash32::from_bytes([0; 32]),
    );
    let error = projection
        .apply_diff(&diff)
        .expect_err("the checksum fails");
    assert_eq!(error.code(), "projection.checksum_failed");
    assert_eq!(
        projection.watermarks().expect("live").get(&source()),
        Some(FactVersion::new(10))
    );
}

#[test]
fn a_snapshot_checksum_failure_fails_closed() {
    let set = fact_set(8);
    let mut projection = loaded_projection(&set, 1, 10);
    let corrupt = ProjectionSnapshot::with_checksum(
        ProjectionEpoch::new(1),
        policy_one(),
        set.digest(),
        watermark_map(10),
        vec![stored("corrupt", 10)],
        oxkv_contracts::version::Hash32::from_bytes([0; 32]),
    );
    let error = projection
        .apply_snapshot(&corrupt)
        .expect_err("the checksum fails");
    assert_eq!(error.code(), "projection.checksum_failed");
    assert_eq!(
        projection
            .fact(&fact_id())
            .expect("live")
            .map(StoredFact::value),
        Some(&FactValue::Text("alice".to_owned()))
    );
}

#[test]
fn a_client_write_to_a_projection_is_rejected() {
    let set = fact_set(2);
    let mut projection = loaded_projection(&set, 1, 10);
    let error = projection
        .apply_client_write()
        .expect_err("the client write fails");
    assert_eq!(error, ProjectionError::ClientWriteRejected);
    assert_eq!(error.code(), "projection.client_write_rejected");
    assert_eq!(error.receipt_state(), Some(ReceiptState::Rejected));
}

#[test]
fn the_read_set_limits_report_fan_in_and_read_set_errors() {
    let mut set = FactSet::new();
    let first = FactDeclaration::new(
        FactId::new("account_owner"),
        CanonicalType::Text,
        FactOwner::Shard(ShardId::new(vec![1])),
        1,
        FactVersionDistance::new(2),
    );
    let second = FactDeclaration::new(
        FactId::new("account_tier"),
        CanonicalType::U64,
        FactOwner::Shard(ShardId::new(vec![2])),
        1,
        FactVersionDistance::new(2),
    );
    set.insert(first).expect("unique");
    set.insert(second).expect("unique");
    let projection = Projection::new(set, epoch_state(1, policy_one()));

    let fan_in = projection
        .read_set(
            &[FactId::new("account_owner"), FactId::new("account_tier")],
            ReadSetLimits::new(4, 4),
        )
        .expect_err("two sources exceed the declared fan-in");
    assert_eq!(fan_in.code(), "projection.fan_in_exceeded");

    let read_set = projection
        .read_set(
            &[FactId::new("account_owner"), FactId::new("account_tier")],
            ReadSetLimits::new(1, 4),
        )
        .expect_err("two facts exceed the read-set cap");
    assert_eq!(read_set.code(), "projection.read_set_exceeded");
}

#[test]
fn a_cursor_past_its_epoch_returns_epoch_invalid() {
    let set = fact_set(2);
    let projection = loaded_projection(&set, 1, 10);
    let cursor = projection.cursor().expect("live");
    assert_eq!(validate_cursor(&cursor, ProjectionEpoch::new(1)), Ok(()));
    let error = validate_cursor(&cursor, ProjectionEpoch::new(2))
        .expect_err("the cursor epoch is older than the retained epoch");
    assert_eq!(error.code(), "projection.epoch_invalid");
}

#[test]
fn a_canary_and_control_cohort_serve_different_policy_versions() {
    let set = fact_set(2);
    let control = PolicyCohort::new("control", policy_one(), ProjectionEpoch::new(1));
    let canary = PolicyCohort::new("canary", policy_two(), ProjectionEpoch::new(2));
    let mut control_projection = Projection::for_cohort(set.clone(), &control);
    let mut canary_projection = Projection::for_cohort(set.clone(), &canary);
    let control_snapshot = ProjectionSnapshot::new(
        control.epoch_generation(),
        control.policy_version(),
        set.digest(),
        watermark_map(10),
        vec![stored("alice", 10)],
    );
    let canary_snapshot = ProjectionSnapshot::new(
        canary.epoch_generation(),
        canary.policy_version(),
        set.digest(),
        watermark_map(10),
        vec![stored("alice", 10)],
    );
    control_projection
        .apply_snapshot(&control_snapshot)
        .expect("the control snapshot applies");
    canary_projection
        .apply_snapshot(&canary_snapshot)
        .expect("the canary snapshot applies");

    assert_ne!(control_projection.epoch(), canary_projection.epoch());
    assert_ne!(
        control_projection.epoch_state().identity_digest(),
        canary_projection.epoch_state().identity_digest()
    );

    let control_verdict = control_projection
        .verdict(
            RuleId::new("accounts.owner"),
            Decision::Allow,
            None,
            &read_set(&control_projection),
        )
        .expect("the control verdict is built");
    let canary_verdict = canary_projection
        .verdict(
            RuleId::new("accounts.owner"),
            Decision::Allow,
            None,
            &read_set(&canary_projection),
        )
        .expect("the canary verdict is built");
    assert_eq!(control_verdict.policy_version(), policy_one());
    assert_eq!(canary_verdict.policy_version(), policy_two());
    assert_eq!(control_verdict.projection_epoch(), ProjectionEpoch::new(1));
    assert_eq!(canary_verdict.projection_epoch(), ProjectionEpoch::new(2));
}

#[test]
fn a_verdict_records_the_projection_identity_and_fact_versions() {
    let set = fact_set(2);
    let projection = loaded_projection(&set, 3, 10);
    let verdict: Verdict = projection
        .verdict(
            RuleId::new("accounts.owner"),
            Decision::Deny,
            Some(oxkv_projection::AUTHORIZATION_STALE_FACTS),
            &read_set(&projection),
        )
        .expect("the verdict is built");
    assert_eq!(verdict.policy_version(), policy_one());
    assert_eq!(verdict.projection_epoch(), ProjectionEpoch::new(3));
    assert_eq!(
        verdict.watermarks().get(&source()),
        Some(FactVersion::new(10))
    );
    assert_eq!(verdict.facts_read().len(), 1);
    assert_eq!(verdict.facts_read()[0].fact_version(), FactVersion::new(10));
    assert_eq!(verdict.rule().as_str(), "accounts.owner");
    assert_eq!(verdict.decision(), Decision::Deny);
    assert_eq!(
        verdict.reason(),
        Some(oxkv_projection::AUTHORIZATION_STALE_FACTS)
    );
    assert_eq!(verdict.fact_set_digest(), set.digest());
}

#[test]
fn a_lag_observation_keeps_time_out_of_the_decision() {
    let set = fact_set(2);
    let projection = loaded_projection(&set, 1, 10);
    let reports = projection
        .staleness(&read_set(&projection), &observed(13))
        .expect("the projection is live");
    let observation = reports[0].to_monitoring(120);
    assert_eq!(observation.elapsed_ticks, 120);
    assert_eq!(
        observation.version_distance,
        Some(FactVersionDistance::new(3))
    );
    let report = projection
        .authorize(
            &read_set(&projection),
            &observed(13),
            AuthorizationMode::Default,
            QueueDeadline::Pending,
        )
        .expect("the projection is live");
    assert!(report.decision.is_queued());
}
