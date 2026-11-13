//! Golden vector runner for the read pin contract.
//!
//! Every vector file is append-only for one contract version. A later essay
//! adds a case to the matching file. A released case does not change.

mod support;

use oxkv_contracts::cursor::{
    QueryFingerprint, ScopeGeneration, SearchCursor, SortTuple, SyncCursor,
};
use oxkv_contracts::receipt::{
    AuditReference, AuditSequence, IdempotencyKey, ReceiptEnvelope, ReceiptId, ReceiptOutcome,
    RequestId,
};
use oxkv_contracts::version::{
    CommitVersion, Hash32, ManifestDigest, PolicyVersion, ScopeIdentity, SessionMinimum,
    VersionVector,
};
use oxkv_readpin::{
    AuthorityReadPin, AuthorityView, BoundRequest, CursorExpiryReason, DurationMillis,
    HydrationState, PinMode, ReadError, ReadOutcome, ReadRequest, ReadTarget, ReplicaLocalPin,
    ReplicaView, Residency, ResultMode, ResumeCursor, ResumeOutcome, SessionMode, SessionToken,
    StalenessBound, Timestamp, evaluate,
};

const T0: Timestamp = Timestamp::from_millis(1000);

fn golden() -> support::GoldenFile {
    let file = support::load(include_str!("vectors/readpin.json"));
    assert_eq!(file.contract, "read-pin/v1");
    file
}

fn local_target(vector: VersionVector) -> ReadTarget {
    ReadTarget::Local(ReplicaLocalPin::from_applied(vector))
}

fn replica(applied: VersionVector) -> ReplicaView {
    ReplicaView::new(applied, T0, HydrationState::Hydrated, Residency::Resident)
}

#[test]
fn a_24_hour_offline_session_reads_its_scope() {
    let file = golden();
    let case = support::case(&file, "24-hour offline read");
    let applied = support::input_vector(case, "applied");
    let last_contact = Timestamp::from_millis(
        case.input["last_contact_ms"]
            .as_u64()
            .expect("the case has a last contact"),
    );
    let now = Timestamp::from_millis(
        case.input["now_ms"]
            .as_u64()
            .expect("the case has a now time"),
    );
    let replica = ReplicaView::new(
        applied.clone(),
        last_contact,
        HydrationState::Hydrated,
        Residency::Resident,
    );
    let request = ReadRequest::new(
        ReadTarget::Local(replica.local_pin()),
        StalenessBound::UNBOUNDED,
    );
    let authority = AuthorityView::offline(&applied, now);
    let outcome = evaluate(&request, &SessionToken::empty(), &replica, &authority);
    let response = outcome.served().expect("the offline read is served");
    assert_eq!(response.served(), &support::expected_vector(case, "served"));
    assert_eq!(
        response.staleness().version_lag(),
        support::expected_u64(case, "version_lag")
    );
    assert_eq!(
        response.staleness().silence().get(),
        support::expected_u64(case, "silence_ms")
    );
    assert!(response.is_exact());
    assert_eq!(outcome.code(), None);
}

#[test]
fn a_7_day_offline_session_resumes_without_a_full_resync() {
    let file = golden();
    let case = support::case(&file, "7-day resume");
    let issued = Timestamp::from_millis(
        case.input["issued_ms"]
            .as_u64()
            .expect("the case has an issue time"),
    );
    let now = Timestamp::from_millis(
        case.input["now_ms"]
            .as_u64()
            .expect("the case has a now time"),
    );
    let retention = DurationMillis::from_millis(
        case.input["diff_retention_ms"]
            .as_u64()
            .expect("the case has a retention"),
    );
    let floor = support::scope_vector(&case.input["floor"]);
    let cursor = SyncCursor::new(
        ScopeGeneration::new(3),
        floor.clone(),
        floor.clone(),
        Hash32::from_bytes([7; 32]),
    );
    let resume = ResumeCursor::new(cursor, issued, retention);
    let outcome = resume.resume(now, &floor);
    assert!(outcome.is_resume());
    assert_eq!(
        outcome.error_code(),
        support::expected_opt_str(case, "error")
    );
    assert_eq!(
        outcome.offers_snapshot(),
        support::expected_bool(case, "snapshot_offered")
    );
}

#[test]
fn an_expired_authority_pin_returns_read_pin_expired() {
    let file = golden();
    let case = support::case(&file, "expired authority pin");
    let issued = Timestamp::from_millis(
        case.input["issued_ms"]
            .as_u64()
            .expect("the case has an issue time"),
    );
    let now = Timestamp::from_millis(
        case.input["now_ms"]
            .as_u64()
            .expect("the case has a now time"),
    );
    let retention = DurationMillis::from_millis(
        case.input["retention_ms"]
            .as_u64()
            .expect("the case has a retention"),
    );
    let pin = AuthorityReadPin::from_retention(
        ScopeIdentity::from_bytes([1; 32]),
        ManifestDigest::from_bytes([2; 32]),
        support::scope_vector(&case.input["pin"]),
        issued,
        retention,
    );
    let error = pin
        .check(now, &VersionVector::new())
        .expect_err("the pin expired");
    assert_eq!(error.code(), support::expected_str(case, "error"));
}

#[test]
fn an_expired_sync_cursor_offers_a_snapshot() {
    let file = golden();
    let case = support::case(&file, "expired sync cursor offers a snapshot");
    let issued = Timestamp::from_millis(
        case.input["issued_ms"]
            .as_u64()
            .expect("the case has an issue time"),
    );
    let now = Timestamp::from_millis(
        case.input["now_ms"]
            .as_u64()
            .expect("the case has a now time"),
    );
    let retention = DurationMillis::from_millis(
        case.input["diff_retention_ms"]
            .as_u64()
            .expect("the case has a retention"),
    );
    let floor = support::scope_vector(&case.input["floor"]);
    let cursor = SyncCursor::new(
        ScopeGeneration::new(3),
        floor.clone(),
        floor.clone(),
        Hash32::from_bytes([7; 32]),
    );
    let resume = ResumeCursor::new(cursor, issued, retention);
    let outcome = resume.resume(now, &floor);
    assert_eq!(
        outcome.error_code(),
        Some(support::expected_str(case, "error"))
    );
    assert_eq!(
        outcome.offers_snapshot(),
        support::expected_bool(case, "snapshot_offered")
    );
    let expired = match &outcome {
        ResumeOutcome::SnapshotRequired(expired) => expired,
        ResumeOutcome::Resume => panic!("the cursor must expire"),
    };
    assert!(matches!(expired.reason(), CursorExpiryReason::Age { .. }));
}

#[test]
fn a_session_version_minimum_applies_across_replicas() {
    let file = golden();
    let case = support::case(&file, "session vector minimum across replicas");
    let shard = support::shard_id(
        case.input["commit_shard"]
            .as_str()
            .expect("the case has a shard"),
    );
    let commit_version = CommitVersion::new(
        case.input["commit_version"]
            .as_u64()
            .expect("the case has a commit version"),
    );
    let mut session = SessionToken::empty();
    assert!(session.raise_to(shard.clone(), commit_version));

    let behind = VersionVector::from_entries([(
        shard.clone(),
        CommitVersion::new(commit_version.get() - 1),
    )]);
    let request = ReadRequest::new(local_target(behind.clone()), StalenessBound::UNBOUNDED)
        .with_mode(SessionMode::ReadYourWrites);
    let behind_replica = replica(behind.clone());
    let outcome = evaluate(
        &request,
        &session,
        &behind_replica,
        &AuthorityView::offline(&behind, T0),
    );
    assert_eq!(outcome.code(), Some(support::expected_str(case, "before")));
    assert!(outcome.served().is_none());

    let applied = VersionVector::from_entries([(shard.clone(), commit_version)]);
    let ahead_replica = replica(applied.clone());
    let outcome = evaluate(
        &request,
        &session,
        &ahead_replica,
        &AuthorityView::offline(&applied, T0),
    );
    let response = outcome.served().expect("the caught-up replica serves");
    assert_eq!(response.served(), &support::expected_vector(case, "after"));

    let fresher = VersionVector::from_entries([(
        shard.clone(),
        CommitVersion::new(commit_version.get() + 1),
    )]);
    let fresher_replica = replica(fresher.clone());
    let outcome = evaluate(
        &request,
        &session,
        &fresher_replica,
        &AuthorityView::offline(&fresher, T0),
    );
    let served = outcome.served().expect("the fresher replica serves");
    assert!(
        served
            .served()
            .get(&shard)
            .is_some_and(|version| version >= commit_version),
        "the served version is below the session minimum"
    );
}

#[test]
fn read_your_writes_blocks_or_returns_read_behind_commit() {
    let file = golden();
    let case = support::case(
        &file,
        "read-your-writes blocks or returns read.behind_commit",
    );
    let minimum = support::input_vector(case, "minimum");
    let applied = support::input_vector(case, "applied");
    let mut session = SessionToken::empty();
    session.raise(&minimum);
    let request = ReadRequest::new(local_target(applied.clone()), StalenessBound::UNBOUNDED)
        .with_mode(SessionMode::ReadYourWrites);
    let view = replica(applied.clone());
    let outcome = evaluate(
        &request,
        &session,
        &view,
        &AuthorityView::offline(&applied, T0),
    );
    assert_eq!(outcome.code(), Some(support::expected_str(case, "blocked")));
    assert!(outcome.served().is_none());
    let blocked = outcome.blocked().expect("the read is blocked");
    assert_eq!(blocked.code(), support::expected_str(case, "blocked"));
    let error = outcome.into_result().expect_err("the block is an error");
    assert_eq!(error.code(), support::expected_str(case, "error"));

    let bounded = ReadRequest::new(local_target(applied.clone()), StalenessBound::UNBOUNDED);
    let outcome = evaluate(
        &bounded,
        &session,
        &view,
        &AuthorityView::offline(&applied, T0),
    );
    assert_eq!(outcome.code(), Some(support::expected_str(case, "error")));
    assert!(outcome.served().is_none());
}

#[test]
fn a_read_over_its_staleness_bound_returns_read_stale_and_no_data() {
    let file = golden();
    let case = support::case(
        &file,
        "read over its staleness bound returns read.stale and no data",
    );
    let watermark = support::input_vector(case, "watermark");
    let applied = support::input_vector(case, "applied");
    let max_version_lag = case.input["max_version_lag"]
        .as_u64()
        .expect("the case has a version lag bound");
    let bound = StalenessBound::new(max_version_lag, DurationMillis::from_seconds(30));
    let request = ReadRequest::new(local_target(applied.clone()), bound);
    let view = replica(applied);
    let authority = AuthorityView::new(
        watermark,
        VersionVector::new(),
        T0.saturating_add(DurationMillis::from_seconds(1)),
    );
    let outcome = evaluate(&request, &SessionToken::empty(), &view, &authority);
    assert_eq!(outcome.code(), Some(support::expected_str(case, "error")));
    assert!(outcome.served().is_none());
    assert!(!support::expected_bool(case, "data"));
    let measured = outcome
        .error()
        .and_then(ReadError::measured)
        .expect("the stale read reports its measure");
    assert_eq!(
        measured.version_lag(),
        support::expected_u64(case, "version_lag")
    );
}

#[test]
fn a_read_at_an_exact_pin_returns_that_pin() {
    let file = golden();
    let case = support::case(&file, "read at an exact pin returns that pin");
    let pin = support::input_vector(case, "pin");
    let applied = support::input_vector(case, "applied");
    let request = ReadRequest::new(local_target(pin.clone()), StalenessBound::UNBOUNDED)
        .with_pin_mode(PinMode::Exactly);
    let view = replica(applied.clone());
    let outcome = evaluate(
        &request,
        &SessionToken::empty(),
        &view,
        &AuthorityView::offline(&applied, T0),
    );
    let response = outcome.served().expect("the exact pin is served");
    assert_eq!(response.served(), &pin);
    assert_eq!(response.served(), &support::expected_vector(case, "served"));
    assert_eq!(
        format!("{:?}", response.pin_mode()),
        support::expected_str(case, "pin_mode")
    );
    assert!(response.is_exact());
}

#[test]
fn a_read_at_an_exact_pin_returns_a_named_error() {
    let file = golden();
    let case = support::case(&file, "read at an exact pin returns a named error");
    let pin = support::input_vector(case, "pin");
    let applied = support::input_vector(case, "applied");
    let resident = case.input["resident"]
        .as_bool()
        .expect("the case has a residency flag");
    let residency = if resident {
        Residency::Resident
    } else {
        Residency::NotResident
    };
    let view = ReplicaView::new(applied, T0, HydrationState::Hydrated, residency);
    let request = ReadRequest::new(local_target(pin), StalenessBound::UNBOUNDED)
        .with_pin_mode(PinMode::Exactly)
        .with_residency_requirement(true);
    let outcome = evaluate(
        &request,
        &SessionToken::empty(),
        &view,
        &AuthorityView::offline(view.applied(), T0),
    );
    assert_eq!(outcome.code(), Some(support::expected_str(case, "error")));
    assert!(outcome.served().is_none());
}

#[test]
fn a_scalar_session_minimum_cannot_be_constructed() {
    let file = golden();
    let case = support::case(&file, "scalar session minimum cannot be constructed");
    let constructor: fn(&SessionMinimum) -> SessionToken = SessionToken::new;
    let token = constructor(&SessionMinimum::default());
    assert!(token.minimum().is_empty());
    assert!(!support::expected_bool(case, "constructible"));
    assert_eq!(support::expected_str(case, "doctest"), "compile_fail");
    let source = include_str!("../src/lib.rs");
    assert!(
        source.contains("```compile_fail"),
        "the compile-fail doctest is absent"
    );
    assert!(
        source.contains("SessionToken::new(CommitVersion::new(7))"),
        "the compile-fail doctest does not test a scalar minimum"
    );
}

#[test]
fn a_session_token_has_canonical_bytes() {
    let file = golden();
    let case = support::case(&file, "session token bytes are canonical");
    let token = SessionToken::new(&SessionMinimum::from_vector(support::input_vector(
        case, "minimum",
    )));
    let bytes = token.to_bytes();
    assert_eq!(support::hex(&bytes), support::expected_str(case, "hex"));
    assert_eq!(SessionToken::from_bytes(&bytes), Ok(token));
}

#[test]
fn a_cold_replica_returns_read_not_hydrated() {
    let applied = VersionVector::from_entries([(support::shard_id("01"), CommitVersion::new(18))]);
    let view = ReplicaView::new(
        applied.clone(),
        T0,
        HydrationState::Cold {
            verified_pages: 2,
            target_pages: 10,
        },
        Residency::Resident,
    );
    let request = ReadRequest::new(local_target(applied.clone()), StalenessBound::UNBOUNDED);
    let outcome = evaluate(
        &request,
        &SessionToken::empty(),
        &view,
        &AuthorityView::offline(&applied, T0),
    );
    assert_eq!(outcome.code(), Some("read.not_hydrated"));
    assert!(outcome.served().is_none());
    assert!(matches!(
        outcome.error(),
        Some(ReadError::NotHydrated {
            verified_pages: 2,
            target_pages: 10
        })
    ));
}

#[test]
fn an_unreadable_authority_pin_returns_read_pin_unreadable() {
    let pin = AuthorityReadPin::from_retention(
        ScopeIdentity::from_bytes([1; 32]),
        ManifestDigest::from_bytes([2; 32]),
        support::scope_vector(&serde_json::json!({"01": 18})),
        T0,
        DurationMillis::from_hours(1),
    );
    let floor = VersionVector::from_entries([(support::shard_id("01"), CommitVersion::new(20))]);
    let error = pin
        .check(T0, &floor)
        .expect_err("the pin is below the floor");
    assert_eq!(error.code(), "read.pin_unreadable");
}

#[test]
fn a_local_pin_stays_readable_offline() {
    let applied = VersionVector::from_entries([(support::shard_id("01"), CommitVersion::new(18))]);
    let local = ReplicaLocalPin::from_applied(applied.clone());
    assert!(local.is_readable_offline());
    assert!(!local.expires_with_authority_retention());
    let view = replica(applied.clone());
    let now = T0.saturating_add(DurationMillis::from_days(30));
    let request = ReadRequest::new(ReadTarget::Local(local), StalenessBound::UNBOUNDED);
    let outcome = evaluate(
        &request,
        &SessionToken::empty(),
        &view,
        &AuthorityView::offline(&applied, now),
    );
    let response = outcome.served().expect("the local pin serves offline");
    assert_eq!(
        response.staleness().silence(),
        DurationMillis::from_days(30)
    );
}

#[test]
fn receipt_retention_is_separate_from_read_pins() {
    let pin = AuthorityReadPin::from_retention(
        ScopeIdentity::from_bytes([1; 32]),
        ManifestDigest::from_bytes([2; 32]),
        support::scope_vector(&serde_json::json!({"01": 18})),
        T0,
        DurationMillis::from_hours(1),
    );
    let receipt = oxkv_readpin::ReceiptRetention::new(DurationMillis::from_days(30));
    let after_two_hours = T0.saturating_add(DurationMillis::from_hours(2));
    assert_eq!(
        pin.check(after_two_hours, &VersionVector::new())
            .expect_err("the pin expired")
            .code(),
        "read.pin_expired"
    );
    assert!(receipt.check(T0, after_two_hours).is_ok());

    let after_forty_days = T0.saturating_add(DurationMillis::from_days(40));
    let expired = receipt
        .check(T0, after_forty_days)
        .expect_err("the receipt expired");
    assert_eq!(expired.code(), "receipt.expired");

    let local = ReplicaLocalPin::from_applied(VersionVector::from_entries([(
        support::shard_id("01"),
        CommitVersion::new(18),
    )]));
    assert!(local.is_readable_offline());
}

#[test]
fn search_and_read_share_one_pin() {
    let applied = VersionVector::from_entries([(support::shard_id("01"), CommitVersion::new(18))]);
    let view = replica(applied.clone());
    let request = ReadRequest::new(local_target(applied.clone()), StalenessBound::UNBOUNDED);
    let outcome = evaluate(
        &request,
        &SessionToken::empty(),
        &view,
        &AuthorityView::offline(&applied, T0),
    );
    let response = outcome.served().expect("the read is served");
    let cursor = SearchCursor::new(
        response.search_version_vector(),
        QueryFingerprint::from_bytes([3; 32]),
        SortTuple::new(Vec::new()),
    );
    assert_eq!(cursor.pin.as_vector(), response.served());
}

#[test]
fn the_default_read_mode_is_bounded_staleness() {
    assert_eq!(SessionMode::default(), SessionMode::BoundedStaleness);
    assert_eq!(PinMode::default(), PinMode::AtLeast);
    assert_eq!(ResultMode::default(), ResultMode::Exact);
}

#[test]
fn a_narrow_request_never_widens_the_scope_default() {
    let scope = StalenessBound::new(10, DurationMillis::from_seconds(5));
    let narrowed = BoundRequest::Narrow(StalenessBound::new(20, DurationMillis::from_seconds(1)))
        .resolve(scope);
    assert_eq!(
        narrowed,
        StalenessBound::new(10, DurationMillis::from_seconds(1))
    );
    let widened = BoundRequest::Override(StalenessBound::new(20, DurationMillis::from_seconds(6)))
        .resolve(scope);
    assert_eq!(
        widened,
        StalenessBound::new(20, DurationMillis::from_seconds(6))
    );
    assert_eq!(BoundRequest::ScopeDefault.resolve(scope), scope);
    assert!(BoundRequest::Unbounded.resolve(scope).is_unbounded());
}

#[test]
fn a_replica_never_regresses_its_high_water_mark() {
    let shard = support::shard_id("01");
    let mut view = ReplicaView::new(
        VersionVector::from_entries([(shard.clone(), CommitVersion::new(4))]),
        T0,
        HydrationState::Hydrated,
        Residency::Resident,
    );
    let newer = VersionVector::from_entries([(shard.clone(), CommitVersion::new(9))]);
    assert!(view.apply(&newer));
    assert_eq!(view.high_water(), &newer);
    let older = VersionVector::from_entries([(shard.clone(), CommitVersion::new(7))]);
    assert!(!view.apply(&older));
    assert_eq!(view.applied(), &newer);
    let restored = ReplicaView::new(
        view.high_water().clone(),
        T0,
        HydrationState::Hydrated,
        Residency::Resident,
    );
    assert!(restored.meets(&newer));
}

#[test]
fn a_coverage_read_reports_its_missing_shards() {
    let applied = VersionVector::from_entries([(support::shard_id("01"), CommitVersion::new(5))]);
    let pin = VersionVector::from_entries([
        (support::shard_id("01"), CommitVersion::new(5)),
        (support::shard_id("02"), CommitVersion::new(5)),
    ]);
    let view = replica(applied.clone());
    let request = ReadRequest::new(local_target(pin.clone()), StalenessBound::UNBOUNDED)
        .with_result_mode(ResultMode::Coverage);
    let outcome = evaluate(
        &request,
        &SessionToken::empty(),
        &view,
        &AuthorityView::offline(&applied, T0),
    );
    let response = outcome.served().expect("the coverage read is served");
    let coverage = response.coverage().expect("the result has a descriptor");
    assert_eq!(coverage.answered(), &[support::shard_id("01")]);
    assert_eq!(coverage.missing(), &[support::shard_id("02")]);
    assert!(!coverage.is_complete());
    assert!(!response.is_exact());
}

#[test]
fn an_exact_read_with_one_missing_shard_returns_an_error() {
    let applied = VersionVector::from_entries([(support::shard_id("01"), CommitVersion::new(5))]);
    let pin = VersionVector::from_entries([
        (support::shard_id("01"), CommitVersion::new(5)),
        (support::shard_id("02"), CommitVersion::new(5)),
    ]);
    let view = replica(applied.clone());
    let request = ReadRequest::new(local_target(pin), StalenessBound::UNBOUNDED);
    let outcome = evaluate(
        &request,
        &SessionToken::empty(),
        &view,
        &AuthorityView::offline(&applied, T0),
    );
    assert_eq!(outcome.code(), Some("read.behind_commit"));
    assert!(matches!(outcome, ReadOutcome::Failed(_)));
}

#[test]
fn read_your_writes_raises_the_minimum_to_the_receipt_commit_version() {
    let shard = support::shard_id("01");
    let commit_version = CommitVersion::new(5);
    let mut receipt = ReceiptEnvelope::queued(
        ReceiptId::from_bytes([1; 32]),
        RequestId::from_bytes([2; 32]),
        IdempotencyKey::new(vec![9, 9]),
        AuditReference::new(
            shard.clone(),
            AuditSequence::new(7),
            Hash32::from_bytes([3; 32]),
        ),
        PolicyVersion::from_bytes([4; 32]),
        ManifestDigest::from_bytes([5; 32]),
        "projection.lagging",
    );
    receipt
        .close(ReceiptOutcome::Committed { commit_version })
        .expect("the receipt closes");
    let mut session = SessionToken::empty();
    assert!(session.raise_to_receipt(&receipt));
    assert_eq!(session.minimum().get(&shard), Some(commit_version));

    let behind = VersionVector::from_entries([(
        shard.clone(),
        CommitVersion::new(commit_version.get() - 1),
    )]);
    let request = ReadRequest::new(local_target(behind.clone()), StalenessBound::UNBOUNDED)
        .with_mode(SessionMode::ReadYourWrites);
    let view = replica(behind.clone());
    let outcome = evaluate(
        &request,
        &session,
        &view,
        &AuthorityView::offline(&behind, T0),
    );
    assert_eq!(outcome.code(), Some("read.behind_commit"));

    let applied = VersionVector::from_entries([(shard, commit_version)]);
    let mut view = replica(applied.clone());
    view.apply(&applied);
    let outcome = evaluate(
        &request,
        &session,
        &view,
        &AuthorityView::offline(&applied, T0),
    );
    let response = outcome.served().expect("the write is visible");
    assert_eq!(response.served(), &applied);
}

#[test]
fn an_expired_authority_pin_fails_the_read() {
    let now = T0.saturating_add(DurationMillis::from_hours(2));
    let watermark =
        VersionVector::from_entries([(support::shard_id("01"), CommitVersion::new(18))]);
    let pin = AuthorityReadPin::from_retention(
        ScopeIdentity::from_bytes([1; 32]),
        ManifestDigest::from_bytes([2; 32]),
        support::scope_vector(&serde_json::json!({"01": 18})),
        T0,
        DurationMillis::from_hours(1),
    );
    let request = ReadRequest::new(ReadTarget::Authority(pin), StalenessBound::UNBOUNDED);
    let view = replica(watermark.clone());
    let outcome = evaluate(
        &request,
        &SessionToken::empty(),
        &view,
        &AuthorityView::new(watermark, VersionVector::new(), now),
    );
    assert_eq!(outcome.code(), Some("read.pin_expired"));
    assert!(outcome.served().is_none());
}

#[test]
fn an_exact_pin_does_not_serve_above_the_pin() {
    let shard = support::shard_id("01");
    let applied = VersionVector::from_entries([(shard.clone(), CommitVersion::new(20))]);
    let pin = VersionVector::from_entries([(shard, CommitVersion::new(18))]);
    let request = ReadRequest::new(local_target(pin.clone()), StalenessBound::UNBOUNDED)
        .with_pin_mode(PinMode::Exactly);
    let view = replica(applied.clone());
    let outcome = evaluate(
        &request,
        &SessionToken::empty(),
        &view,
        &AuthorityView::offline(&applied, T0),
    );
    let response = outcome.served().expect("the pin is retained");
    assert_eq!(response.served(), &pin);
    assert_ne!(response.served(), &applied);
}

#[test]
fn a_session_minimum_outside_the_pin_does_not_block() {
    let shard_one = support::shard_id("01");
    let shard_two = support::shard_id("02");
    let mut session = SessionToken::empty();
    assert!(session.raise_to(shard_one.clone(), CommitVersion::new(5)));
    assert!(session.raise_to(shard_two, CommitVersion::new(9)));
    let applied = VersionVector::from_entries([(shard_one, CommitVersion::new(5))]);
    let request = ReadRequest::new(local_target(applied.clone()), StalenessBound::UNBOUNDED)
        .with_mode(SessionMode::ReadYourWrites);
    let view = replica(applied.clone());
    let outcome = evaluate(
        &request,
        &session,
        &view,
        &AuthorityView::offline(&applied, T0),
    );
    let response = outcome
        .served()
        .expect("the out-of-pin shard does not block");
    assert_eq!(response.served(), &applied);
}
