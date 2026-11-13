//! Production signing and durable witness outbox tests.

#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
use oxkv_contracts::version::{
    Hash32, KeyVersion, ManifestDigest, MapVersion, PolicyVersion, ProjectionEpoch,
    ProjectionWatermarks, ShardId, Signature,
};
use oxkv_security::audit::{CheckpointSigner, StructuralSigner};
use oxkv_security::secret::KeyIdentifier;
use oxkv_security::witness::WitnessQueue;
use oxkv_write::{
    AllowAll, Authority, AuthorityConfig, AuthorityInitError, ClientId, FileWitnessOutbox, Key,
    Mutation, WriteRequest,
};

/// A signer that reports the production kind.
#[derive(Debug)]
struct ProductionTestSigner {
    key_id: KeyIdentifier,
}

impl ProductionTestSigner {
    /// Creates a production test signer.
    fn new(key_id: KeyIdentifier) -> Self {
        Self { key_id }
    }
}

impl CheckpointSigner for ProductionTestSigner {
    fn key_id(&self) -> &KeyIdentifier {
        &self.key_id
    }

    fn sign(&self, checkpoint_digest: Hash32) -> Signature {
        let mut bytes = [0_u8; 64];
        bytes[..32].copy_from_slice(checkpoint_digest.as_bytes());
        bytes[32..].copy_from_slice(checkpoint_digest.as_bytes());
        Signature::from_bytes(bytes)
    }
}

/// A temporary directory that removes itself.
struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    /// Creates one empty temporary directory.
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("oxkv-write-{name}-{}-{unique}", std::process::id()));
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

fn checkpoint_key() -> KeyIdentifier {
    KeyIdentifier::new("checkpoint-key-1")
}

fn config(mode: oxkv_write::AuthorityMode) -> AuthorityConfig {
    AuthorityConfig::with_mode(
        shard(),
        PolicyVersion::from_bytes([4; 32]),
        ManifestDigest::from_bytes([5; 32]),
        MapVersion::new(6),
        ProjectionEpoch::new(7),
        ProjectionWatermarks::new(),
        checkpoint_key(),
        mode,
    )
}

fn production_config() -> AuthorityConfig {
    config(oxkv_write::AuthorityMode::Production)
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

fn signer() -> Arc<dyn CheckpointSigner> {
    Arc::new(ProductionTestSigner::new(checkpoint_key()))
}

#[test]
fn production_mode_rejects_a_structural_signer() {
    let structural = Arc::new(StructuralSigner::new(checkpoint_key()));
    let error = Authority::open(production_config(), structural, WitnessQueue::new())
        .expect_err("production mode rejects the structural signer");

    assert_eq!(error, AuthorityInitError::StructuralSignerInProduction);
    assert_eq!(error.code(), "authority.structural_signer_in_production");

    let local = Authority::local(production_config());
    assert_eq!(
        local.expect_err("the local constructor rejects production mode"),
        AuthorityInitError::StructuralSignerInProduction
    );
}

#[test]
fn production_mode_accepts_a_real_signer() {
    let authority = Authority::open(production_config(), signer(), WitnessQueue::new())
        .expect("production mode accepts the real signer");

    assert_eq!(authority.shard_id(), &shard());
}

#[test]
fn the_durable_outbox_survives_restart() {
    let directory = TempDirectory::new("restart");
    let outbox = FileWitnessOutbox::open(directory.path()).expect("the outbox opens");
    {
        let mut authority = Authority::open(production_config(), signer(), outbox)
            .expect("the production authority starts");
        let outcome = authority
            .submit(&request(1, 0), &AllowAll, &[])
            .expect("the write commits");

        assert_eq!(outcome.state().name(), "Committed");
        assert_eq!(authority.witness().pending_len(), 1);
        assert!(authority.witness().last_error().is_none());
    }

    let mut reopened = FileWitnessOutbox::open(directory.path()).expect("the outbox reopens");
    assert_eq!(reopened.pending_len(), 1);

    let mut witness = WitnessQueue::new();
    let delivered = reopened
        .drain_to(&mut witness)
        .expect("the outbox delivers the checkpoint");

    assert_eq!(delivered, 1);
    assert_eq!(witness.pending_len(), 1);
    assert_eq!(witness.deliver_all().len(), 1);
    assert_eq!(reopened.pending_len(), 0);
}

#[test]
fn the_witness_receives_the_checkpoint_after_commit() {
    let directory = TempDirectory::new("delivery");
    let outbox = FileWitnessOutbox::open(directory.path()).expect("the outbox opens");
    let mut authority = Authority::open(production_config(), signer(), outbox)
        .expect("the production authority starts");

    let outcome = authority
        .submit(&request(1, 0), &AllowAll, &[])
        .expect("the write commits");
    assert_eq!(outcome.state().name(), "Committed");
    assert_eq!(authority.witness().pending_len(), 1);

    let mut witness = WitnessQueue::new();
    let delivered = authority
        .witness_mut()
        .drain_to(&mut witness)
        .expect("the outbox delivers the checkpoint");

    assert_eq!(delivered, 1);
    assert_eq!(witness.pending_len(), 1);
    let checkpoints = witness.deliver_all();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(
        checkpoints[0].checkpoint_hash,
        authority
            .last_audit_checkpoint()
            .expect("the chain has a checkpoint")
            .checkpoint_hash
    );
}

#[test]
fn the_commit_path_does_not_wait_for_the_witness() {
    let directory = TempDirectory::new("non-blocking");
    let outbox = FileWitnessOutbox::open(directory.path()).expect("the outbox opens");
    let mut authority = Authority::open(production_config(), signer(), outbox)
        .expect("the production authority starts");
    let witness = WitnessQueue::new();

    for id in 1..=3 {
        let outcome = authority
            .submit(&request(id, u64::from(id) - 1), &AllowAll, &[])
            .expect("the write commits");
        assert_eq!(outcome.state().name(), "Committed");
        assert_eq!(authority.witness().pending_len(), usize::from(id));
        assert_eq!(witness.pending_len(), 0);
        assert_eq!(witness.delivered_len(), 0);
    }

    assert_eq!(authority.witness().pending_len(), 3);
    assert_eq!(witness.delivered_len(), 0);
}
