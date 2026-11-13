#![allow(missing_docs, unreachable_pub)]

//! Durable journal adapter tests.

#[allow(dead_code)]
mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use oxkv_contracts::version::PageSequence;
use oxkv_replica::{
    FileJournalStore, HydrationJournalStore, HydrationPhase, HydrationTarget, Pin, ReadRequest,
    Replica, ReplicaError, StalenessBound,
};
use support::{config, key, scope, two_page_snapshot};

/// A temporary directory that removes itself.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "oxkv-replica-journal-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("the temporary directory is created");
        Self { path }
    }

    fn journal(&self) -> PathBuf {
        self.path.join("hydration.journal")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn budget() -> u64 {
    u64::MAX / 4
}

#[test]
fn a_second_replica_resumes_from_the_durable_journal() {
    let directory = TempDir::new();
    let snapshot = two_page_snapshot(5);
    let mut store = FileJournalStore::new(directory.journal());
    let mut first = Replica::new(config(budget(), budget()), scope());
    first
        .begin_hydration(HydrationTarget::for_snapshot(&snapshot))
        .expect("the hydration starts");
    first
        .hydrate_page(snapshot.pages[0].clone())
        .expect("the first page verifies");
    first
        .persist_journal(&mut store)
        .expect("the journal persists");

    drop(first);
    assert!(store.path().exists(), "the journal file is durable");

    let mut resumed =
        Replica::resume(config(budget(), budget()), scope(), &store).expect("the journal loads");
    let journal = resumed.journal().expect("the journal exists");
    assert_eq!(journal.phase, HydrationPhase::Transfer);
    assert_eq!(journal.verified.len(), 1);
    assert!(journal.is_verified(PageSequence::new(1)));
    let generation = journal.generation;

    let report = resumed
        .hydrate(snapshot)
        .expect("the resumed hydration publishes");
    assert_eq!(report.generation, generation, "the generation resumes");
    assert_eq!(report.page_count, 2);
    let journal = resumed.journal().expect("the journal exists");
    assert_eq!(journal.phase, HydrationPhase::Published);
    assert_eq!(journal.swap_commit, Some(generation));
    assert!(journal.is_verified(PageSequence::new(1)));
    assert!(journal.is_verified(PageSequence::new(2)));
    resumed
        .persist_journal(&mut store)
        .expect("the published journal persists");

    let reloaded = FileJournalStore::new(directory.journal())
        .load_journal()
        .expect("the journal reads")
        .expect("the journal exists");
    assert_eq!(reloaded.phase, HydrationPhase::Published);
    assert_eq!(reloaded.swap_commit, Some(generation));
    assert!(reloaded.is_verified(PageSequence::new(1)));
    assert!(reloaded.is_verified(PageSequence::new(2)));

    let request = ReadRequest::new(
        key("bob"),
        Pin::at_least(resumed.applied_version()),
        StalenessBound::Unbounded,
    );
    let read = resumed.get(&request).expect("the resumed row serves");
    assert_eq!(read.value, b"green blue");
}

#[test]
fn a_corrupt_journal_fails_closed() {
    let directory = TempDir::new();
    let snapshot = two_page_snapshot(5);
    let mut store = FileJournalStore::new(directory.journal());
    let mut replica = Replica::new(config(budget(), budget()), scope());
    replica
        .begin_hydration(HydrationTarget::for_snapshot(&snapshot))
        .expect("the hydration starts");
    replica
        .persist_journal(&mut store)
        .expect("the journal persists");

    let mut bytes = std::fs::read(store.path()).expect("the journal reads");
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    std::fs::write(store.path(), &bytes).expect("the corrupt journal writes");

    let error = store.load_journal().expect_err("the journal is corrupt");
    assert!(matches!(error, ReplicaError::JournalCorrupt { .. }));
    assert_eq!(error.code(), "replication.journal_corrupt");
    assert_ne!(error.code(), "internal.invariant_violation");
    assert!(!error.is_retryable());

    let error = Replica::resume(config(budget(), budget()), scope(), &store)
        .expect_err("the resume fails closed");
    assert!(matches!(error, ReplicaError::JournalCorrupt { .. }));
    assert_eq!(error.code(), "replication.journal_corrupt");
}

#[test]
fn a_journal_io_failure_returns_the_named_code() {
    let directory = TempDir::new();
    let store = FileJournalStore::new(directory.path.clone());

    let error = store
        .load_journal()
        .expect_err("the directory is not a journal");
    assert!(matches!(error, ReplicaError::JournalIo { .. }));
    assert_eq!(error.code(), "replication.journal_io");
    assert!(error.is_retryable());
}
