//! Durable authority state tests.

#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use oxkv_contracts::receipt::{IdempotencyKey, ReceiptState, RequestId};
use oxkv_contracts::version::{
    KeyVersion, ManifestDigest, MapVersion, PolicyVersion, ProjectionEpoch, ProjectionWatermarks,
    ShardId,
};
use oxkv_security::audit::{CheckpointSigner, StructuralSigner};
use oxkv_security::secret::KeyIdentifier;
use oxkv_write::{
    AllowAll, Authority, AuthorityConfig, AuthorityStateError, ClientId, FileWitnessOutbox, Key,
    Mutation, STATE_FILE, WriteRequest,
};

/// A temporary directory that removes itself.
struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    /// Creates one empty temporary directory.
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "oxkv-write-state-{name}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("the temporary directory is created");
        Self { path }
    }

    /// Returns the directory path.
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn shard() -> ShardId {
    ShardId::new(vec![1])
}

fn config() -> AuthorityConfig {
    AuthorityConfig::new(
        shard(),
        PolicyVersion::from_bytes([4; 32]),
        ManifestDigest::from_bytes([5; 32]),
        MapVersion::new(6),
        ProjectionEpoch::new(7),
        ProjectionWatermarks::new(),
        KeyIdentifier::new("checkpoint-key-1"),
    )
}

fn open(directory: &Path) -> Authority<FileWitnessOutbox> {
    let signer: Arc<dyn CheckpointSigner> = Arc::new(StructuralSigner::new(KeyIdentifier::new(
        "checkpoint-key-1",
    )));
    let outbox = FileWitnessOutbox::open(directory.join("witness")).expect("the outbox opens");
    Authority::open(config(), signer, outbox).expect("the local authority starts")
}

fn request(id: u8, base: u64) -> WriteRequest {
    WriteRequest::new(
        RequestId::from_bytes([id; 32]),
        ClientId::new(vec![1]),
        IdempotencyKey::new(vec![id]),
        [Mutation::put(
            Key::new(b"account".to_vec()),
            shard(),
            KeyVersion::new(base),
            b"one".to_vec(),
        )],
    )
}

fn seed_state(directory: &Path) {
    let mut authority = open(directory);
    let outcome = authority
        .submit(&request(1, 0), &AllowAll, &[])
        .expect("the write commits");
    assert_eq!(outcome.state(), ReceiptState::Committed);
    authority
        .save_state(directory)
        .expect("the authority state saves");
}

#[test]
fn a_committed_write_survives_a_restart() {
    let directory = TempDirectory::new("restart");
    let request = request(1, 0);
    let (receipt, records, checkpoints) = {
        let mut authority = open(directory.path());
        let outcome = authority
            .submit(&request, &AllowAll, &[])
            .expect("the write commits");
        assert_eq!(outcome.state(), ReceiptState::Committed);
        assert_eq!(authority.commit_version().get(), 1);
        assert_eq!(
            authority.key_version(request.mutations()[0].key()),
            KeyVersion::new(1)
        );
        authority
            .save_state(directory.path())
            .expect("the authority state saves");
        (
            outcome.receipt().clone(),
            authority.audit_records().to_vec(),
            authority.audit_checkpoints().to_vec(),
        )
    };

    let mut reopened = open(directory.path());
    reopened
        .load_state(directory.path())
        .expect("the authority state loads");

    assert_eq!(
        reopened.receipt(request.client_id(), request.idempotency_key()),
        Some(&receipt)
    );
    assert_eq!(
        reopened
            .receipt(request.client_id(), request.idempotency_key())
            .expect("the receipt is present")
            .state(),
        ReceiptState::Committed
    );
    assert_eq!(reopened.audit_records(), records);
    assert_eq!(reopened.audit_checkpoints(), checkpoints);
    let report = reopened
        .verify_audit_chain()
        .expect("the audit chain verifies");
    assert_eq!(report.records, 1);
    assert_eq!(report.checkpoints, 1);
    assert_eq!(reopened.commit_version().get(), 1);
    assert_eq!(
        reopened.key_version(request.mutations()[0].key()),
        KeyVersion::new(1)
    );
    assert_eq!(
        reopened
            .record(request.mutations()[0].key())
            .and_then(oxkv_write::KeyRecord::as_value),
        Some(b"one".as_slice())
    );

    let retry = reopened
        .submit(&request, &AllowAll, &[])
        .expect("the retry returns the receipt");
    assert!(retry.is_duplicate());
    assert_eq!(retry.receipt(), &receipt);
}

#[test]
fn the_state_round_trips_a_commit_and_a_conflict() {
    let directory = TempDirectory::new("round-trip");
    let commit = request(1, 0);
    let conflict = request(2, 0);
    let (committed, rejected, records, checkpoints) = {
        let mut authority = open(directory.path());
        let committed = authority
            .submit(&commit, &AllowAll, &[])
            .expect("the write commits")
            .receipt()
            .clone();
        let rejected = authority
            .submit(&conflict, &AllowAll, &[])
            .expect("the conflict returns an outcome")
            .receipt()
            .clone();
        assert_eq!(rejected.state(), ReceiptState::Conflict);
        authority
            .save_state(directory.path())
            .expect("the authority state saves");
        (
            committed,
            rejected,
            authority.audit_records().to_vec(),
            authority.audit_checkpoints().to_vec(),
        )
    };

    let mut reopened = open(directory.path());
    reopened
        .load_state(directory.path())
        .expect("the authority state loads");

    assert_eq!(
        reopened.receipt(commit.client_id(), commit.idempotency_key()),
        Some(&committed)
    );
    assert_eq!(
        reopened.receipt(conflict.client_id(), conflict.idempotency_key()),
        Some(&rejected)
    );
    assert_eq!(
        reopened
            .receipt(conflict.client_id(), conflict.idempotency_key())
            .expect("the conflict receipt is present")
            .reason_code(),
        Some("write.version_conflict")
    );
    assert_eq!(reopened.audit_records(), records);
    assert_eq!(reopened.audit_checkpoints(), checkpoints);
    assert_eq!(reopened.commit_version().get(), 1);
    assert_eq!(
        reopened.key_version(commit.mutations()[0].key()),
        KeyVersion::new(1)
    );
    assert_eq!(reopened.receipts().len(), 2);
}

#[test]
fn a_corrupt_integrity_value_is_rejected() {
    let directory = TempDirectory::new("integrity");
    seed_state(directory.path());
    let path = directory.path().join(STATE_FILE);
    let mut bytes = std::fs::read(&path).expect("the state file is readable");
    let last = bytes.last_mut().expect("the state file is not empty");
    *last ^= 0xff;
    std::fs::write(&path, &bytes).expect("the state file is writable");

    let mut authority = open(directory.path());
    let error = authority
        .load_state(directory.path())
        .expect_err("the integrity value does not match");

    assert!(matches!(error, AuthorityStateError::IntegrityMismatch));
}

#[test]
fn a_truncated_state_file_is_rejected() {
    let directory = TempDirectory::new("truncated");
    seed_state(directory.path());
    let path = directory.path().join(STATE_FILE);
    let bytes = std::fs::read(&path).expect("the state file is readable");
    std::fs::write(&path, &bytes[..bytes.len() - 1]).expect("the state file is writable");

    let mut authority = open(directory.path());
    let error = authority
        .load_state(directory.path())
        .expect_err("the state file is truncated");

    assert!(matches!(error, AuthorityStateError::Truncated));
}

#[test]
fn an_unknown_format_version_is_rejected() {
    let directory = TempDirectory::new("version");
    seed_state(directory.path());
    let path = directory.path().join(STATE_FILE);
    let mut bytes = std::fs::read(&path).expect("the state file is readable");
    bytes[8..12].copy_from_slice(&99_u32.to_be_bytes());
    std::fs::write(&path, &bytes).expect("the state file is writable");

    let mut authority = open(directory.path());
    let error = authority
        .load_state(directory.path())
        .expect_err("the format version is unknown");

    assert!(matches!(
        error,
        AuthorityStateError::UnsupportedVersion { version: 99 }
    ));
}

#[test]
fn a_missing_state_file_is_rejected() {
    let directory = TempDirectory::new("missing");
    let mut authority = open(directory.path());
    let error = authority
        .load_state(directory.path())
        .expect_err("the state file is missing");

    assert!(matches!(error, AuthorityStateError::MissingState { .. }));
}

#[test]
fn a_second_save_replaces_the_state_file() {
    let directory = TempDirectory::new("replace");
    let first = request(1, 0);
    let second = request(2, 1);
    let mut authority = open(directory.path());
    authority
        .submit(&first, &AllowAll, &[])
        .expect("the first write commits");
    authority
        .save_state(directory.path())
        .expect("the first state saves");
    let temporary = directory.path().join(format!("{STATE_FILE}.tmp"));
    std::fs::write(&temporary, b"stale").expect("the stale temporary file is writable");

    authority
        .submit(&second, &AllowAll, &[])
        .expect("the second write commits");
    authority
        .save_state(directory.path())
        .expect("the second state saves");

    assert!(!temporary.exists());
    let mut reopened = open(directory.path());
    reopened
        .load_state(directory.path())
        .expect("the second state loads");
    assert_eq!(reopened.commit_version().get(), 2);
    assert_eq!(
        reopened.key_version(first.mutations()[0].key()),
        KeyVersion::new(2)
    );
    assert_eq!(reopened.receipts().len(), 2);
    assert!(
        reopened
            .receipt(first.client_id(), first.idempotency_key())
            .is_some()
    );
    assert!(
        reopened
            .receipt(second.client_id(), second.idempotency_key())
            .is_some()
    );
    assert!(reopened.verify_audit_chain().is_ok());
}
