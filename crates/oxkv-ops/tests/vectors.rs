//! Golden vector tests for the operations contracts.

use std::collections::BTreeMap;

use oxkv_contracts::version::{
    CommitVersion, KeyEncodingVersion, LeaseTerm, ManifestDigest, ManifestRevision, MapDigest,
    MapVersion, PolicyVersion, ShardId, SnapshotPageVersion,
};
use oxkv_ops::artifact::{ArtifactMigrationPlan, ArtifactObject, LayoutChange, MigratableArtifact};
use oxkv_ops::backup::{BackupImage, BackupPlan, BackupVersions, QuiesceState, capture};
use oxkv_ops::error::OpsError;
use oxkv_ops::maintenance::Timestamp;
use oxkv_ops::migration::{
    MigrationPhase, MigrationStep, ShardFormatPlan, ShardMigration, ShardStore,
};
use oxkv_ops::quota::{
    OperationClass, QuotaLimits, QuotaPolicy, QuotaRequest, QuotaReserve, QuotaResource,
    QuotaUsage, RateLimitKey, RateLimiter, TokenBucket,
};
use oxkv_ops::receipts::SchedulerState;
use oxkv_ops::snapshot::SnapshotObject;
use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!("vectors/ops.json")).expect("the vector file is valid JSON")
}

fn shard(value: u64) -> ShardId {
    ShardId::new(value.to_be_bytes().to_vec())
}

fn snapshot(shard_value: u64, version: u64) -> SnapshotObject {
    let mut rows = BTreeMap::new();
    rows.insert(
        shard_value.to_be_bytes().to_vec(),
        version.to_be_bytes().to_vec(),
    );
    SnapshotObject::new(
        format!("snapshot-{shard_value}"),
        shard(shard_value),
        CommitVersion::new(version),
        SnapshotPageVersion::new(1, 0),
        KeyEncodingVersion::new(1),
        rows,
        BTreeMap::new(),
    )
}

fn plan() -> ShardFormatPlan {
    ShardFormatPlan::new(
        shard(1),
        SnapshotPageVersion::new(1, 0),
        SnapshotPageVersion::new(1, 1),
        LeaseTerm::new(1),
    )
    .expect("the plan is valid")
}

fn source() -> ShardStore {
    let mut rows = BTreeMap::new();
    rows.insert(b"a".to_vec(), b"1".to_vec());
    ShardStore::new(
        shard(1),
        LeaseTerm::new(1),
        SnapshotPageVersion::new(1, 0),
        KeyEncodingVersion::new(1),
        rows,
    )
}

#[test]
fn the_migration_step_vector_matches_the_framework() {
    let vectors = vectors();
    let expected = vectors["migration_steps"]
        .as_array()
        .expect("the steps are an array")
        .iter()
        .map(|value| value.as_str().expect("a step is text"))
        .collect::<Vec<_>>();
    let actual = MigrationStep::ALL.map(MigrationStep::name);
    assert_eq!(expected, actual);
    let mut migration = ShardMigration::begin(plan(), source(), SchedulerState::new())
        .expect("the migration starts");
    let mut seen = Vec::new();
    while let Some(step) = migration.step().expect("the step succeeds") {
        seen.push(step.name());
    }
    assert_eq!(seen, actual);
    assert_eq!(migration.phase(), MigrationPhase::Complete);
}

#[test]
fn the_rollback_vector_matches_the_framework() {
    let cases = vectors()["rollback"]
        .as_array()
        .expect("the rollback cases are an array")
        .clone();
    for case in cases {
        let first_write = case["first_new_format_write"]
            .as_bool()
            .expect("the flag is a bool");
        let allowed = case["allowed"].as_bool().expect("the result is a bool");
        let code = case["code"].as_str();
        let mut migration = ShardMigration::begin(plan(), source(), SchedulerState::new())
            .expect("the migration starts");
        migration
            .run_to_completion()
            .expect("the migration completes");
        if first_write {
            migration
                .record_new_format_write(b"b".to_vec(), b"2".to_vec())
                .expect("the write lands");
        }
        let result = migration.rollback();
        assert_eq!(result.is_ok(), allowed);
        assert_eq!(result.err().map(|error| error.code()), code);
    }
}

#[test]
fn the_error_code_vector_is_registered() {
    let codes = vectors()["error_codes"]
        .as_object()
        .expect("the error codes are an object")
        .clone();
    for (_, value) in codes {
        let code = value.as_str().expect("the code is text");
        assert!(
            oxkv_contracts::error::lookup(code).is_some(),
            "the code {code} is not registered"
        );
    }
}

#[test]
fn the_backup_vector_records_each_shard_version() {
    let entries = vectors()["backup_vector"]
        .as_array()
        .expect("the vector is an array")
        .clone();
    let mut image = BackupImage::new();
    for entry in &entries {
        let shard_value = entry["shard"].as_u64().expect("the shard is a number");
        let version = entry["version"].as_u64().expect("the version is a number");
        image.add_snapshot(snapshot(shard_value, version));
    }
    let plan = BackupPlan::new(
        "pin-vector",
        BackupVersions::new(
            ManifestRevision::new(1),
            SnapshotPageVersion::new(1, 0),
            KeyEncodingVersion::new(1),
            ManifestDigest::from_bytes([1; 32]),
            PolicyVersion::from_bytes([2; 32]),
            MapVersion::new(1),
            MapDigest::from_bytes([3; 32]),
        ),
    );
    let outcome = capture(&plan, QuiesceState::Writable, &image).expect("the backup captures");
    for entry in entries {
        let shard_value = entry["shard"].as_u64().expect("the shard is a number");
        let version = entry["version"].as_u64().expect("the version is a number");
        assert_eq!(
            outcome.manifest.version(&shard(shard_value)),
            Some(CommitVersion::new(version))
        );
    }
}

#[test]
fn the_quota_vector_matches_admission() {
    let cases = vectors()["quota"]
        .as_array()
        .expect("the quota cases are an array")
        .clone();
    for case in cases {
        let resource = case["resource"].as_str().expect("the resource is text");
        let limit = case["limit"].as_u64().expect("the limit is a number");
        let usage = case["usage"].as_u64().expect("the usage is a number");
        let request = case["request"].as_u64().expect("the request is a number");
        assert_eq!(resource, QuotaResource::StoredBytes.name());
        let policy = QuotaPolicy::new(
            QuotaLimits::new(limit, 1_000, 1_000, 1_000, 1_000, 1_000),
            QuotaReserve::default(),
        );
        let result = policy.check(
            &QuotaUsage::new(usage, 0, 0, 0, 0, 0),
            &QuotaRequest::new(request, 0, 0, 0, 0, 0),
        );
        assert_eq!(
            result.err().map(|error| error.code()),
            case["code"].as_str()
        );
    }
}

#[test]
fn the_rate_limit_vector_matches_the_bucket() {
    let case = &vectors()["rate_limit"];
    let capacity = case["capacity"].as_u64().expect("the capacity is a number");
    let refill = case["refill_per_second"]
        .as_u64()
        .expect("the refill is a number");
    let cost = case["cost"].as_u64().expect("the cost is a number");
    let acquires = case["acquires"]
        .as_u64()
        .expect("the acquire count is a number");
    let key = RateLimitKey::shard(shard(1), OperationClass::Write);
    let mut limiter = RateLimiter::new();
    limiter.configure(
        key.clone(),
        TokenBucket::new(capacity, refill, Timestamp::new(0)),
    );
    let mut last = Ok(());
    for _ in 0..acquires {
        last = limiter.admit(&key, Timestamp::new(0), cost);
    }
    assert_eq!(last.err().map(|error| error.code()), case["code"].as_str());
}

#[test]
fn the_artifact_vector_lists_every_layout_change() {
    let case = &vectors()["artifact"];
    let from = SnapshotPageVersion::new(
        u16::try_from(case["from"][0].as_u64().expect("the major is a number"))
            .expect("the major fits"),
        u16::try_from(case["from"][1].as_u64().expect("the minor is a number"))
            .expect("the minor fits"),
    );
    let to = SnapshotPageVersion::new(
        u16::try_from(case["to"][0].as_u64().expect("the major is a number"))
            .expect("the major fits"),
        u16::try_from(case["to"][1].as_u64().expect("the minor is a number"))
            .expect("the minor fits"),
    );
    let changes = case["changes"]
        .as_array()
        .expect("the changes are an array")
        .iter()
        .map(|change| {
            LayoutChange::new(
                change["field"].as_str().expect("the field is text"),
                change["before"].as_str().expect("the before is text"),
                change["after"].as_str().expect("the after is text"),
            )
        })
        .collect::<Vec<_>>();
    let plan =
        ArtifactMigrationPlan::new(from, to, changes.clone()).expect("the artifact plan is valid");
    let object = ArtifactObject::new(
        "snapshot-1",
        MigratableArtifact::Snapshot,
        from,
        KeyEncodingVersion::new(1),
        b"bytes".to_vec(),
    );
    let migrated = plan.migrate(&object).expect("the artifact migrates");
    assert_eq!(migrated.changes.len(), changes.len());
    assert_ne!(migrated.old_digest, migrated.new_digest);
}

#[test]
fn every_vector_error_code_matches_one_constructor() {
    let codes = vectors()["error_codes"]
        .as_object()
        .expect("the error codes are an object")
        .clone();
    assert_eq!(
        codes["backup_pin_lost"].as_str(),
        Some(
            OpsError::BackupPinLost {
                pin: "pin".to_owned()
            }
            .code()
        )
    );
    assert_eq!(
        codes["migration_incomplete"].as_str(),
        Some(OpsError::MigrationIncomplete { step: "step" }.code())
    );
}
