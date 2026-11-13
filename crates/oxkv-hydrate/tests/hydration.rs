//! End-to-end hydration, recovery, budget, and replica tests.

use std::collections::BTreeSet;

use oxkv_contracts::version::{
    CommitVersion, LeaseTerm, PageSequence, PolicyVersion, PrefixBounds, ScopeVersionVector,
    ShardId, VersionVector,
};
use oxkv_hydrate::{
    DiffFrame, DurableStore, FrameOutcome, GenerationId, GenerationReader, HydrationError,
    HydrationRequest, Hydrator, Journal, MemoryStore, MemoryTransport, Phase, Replica, ScopeBudget,
    SealedSnapshot, SnapshotDraft, SnapshotPage, StartOutcome, StepOutcome,
};

const SHARD: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishMode {
    Normal,
    Before,
    After,
}

#[derive(Debug, Clone)]
struct CrashStore {
    inner: MemoryStore,
    fail_at: Option<u64>,
    ops: u64,
    publish_mode: PublishMode,
}

impl CrashStore {
    fn new() -> Self {
        Self {
            inner: MemoryStore::new(),
            fail_at: None,
            ops: 0,
            publish_mode: PublishMode::Normal,
        }
    }

    fn fail_at(index: u64) -> Self {
        let mut store = Self::new();
        store.fail_at = Some(index);
        store
    }

    fn operations(&self) -> u64 {
        self.ops
    }

    fn hit(&mut self) -> Result<(), HydrationError> {
        self.ops = self.ops.saturating_add(1);
        if self.fail_at == Some(self.ops) {
            return Err(HydrationError::storage("injected crash"));
        }
        Ok(())
    }
}

impl GenerationReader for CrashStore {
    fn published_generation(&self) -> Option<GenerationId> {
        self.inner.published_generation()
    }

    fn generation(&self, id: GenerationId) -> Option<&oxkv_hydrate::Generation> {
        self.inner.generation(id)
    }
}

impl DurableStore for CrashStore {
    fn load_journal(&self) -> Result<Option<Journal>, HydrationError> {
        self.inner.load_journal()
    }

    fn save_journal(&mut self, journal: &Journal) -> Result<(), HydrationError> {
        self.hit()?;
        self.inner.save_journal(journal)
    }

    fn begin_generation(
        &mut self,
        id: GenerationId,
        descriptor: &oxkv_hydrate::SnapshotDescriptor,
    ) -> Result<(), HydrationError> {
        self.hit()?;
        self.inner.begin_generation(id, descriptor)
    }

    fn discard_generation(&mut self, id: GenerationId) -> Result<(), HydrationError> {
        self.hit()?;
        self.inner.discard_generation(id)
    }

    fn write_page(
        &mut self,
        id: GenerationId,
        raw: &[u8],
        page: &SnapshotPage,
    ) -> Result<(), HydrationError> {
        self.hit()?;
        self.inner.write_page(id, raw, page)
    }

    fn complete_generation(&mut self, id: GenerationId) -> Result<(), HydrationError> {
        self.hit()?;
        self.inner.complete_generation(id)
    }

    fn apply_diff(&mut self, id: GenerationId, frame: &DiffFrame) -> Result<(), HydrationError> {
        self.hit()?;
        self.inner.apply_diff(id, frame)
    }

    fn publish(&mut self, id: GenerationId) -> Result<(), HydrationError> {
        self.ops = self.ops.saturating_add(1);
        let fail = self.fail_at == Some(self.ops);
        match self.publish_mode {
            PublishMode::After => {
                self.inner.publish(id)?;
                Err(HydrationError::storage("injected crash after publish"))
            }
            PublishMode::Before => Err(HydrationError::storage("injected crash before publish")),
            PublishMode::Normal => {
                if fail {
                    Err(HydrationError::storage("injected crash before publish"))
                } else {
                    self.inner.publish(id)
                }
            }
        }
    }

    fn next_generation_id(&self) -> GenerationId {
        self.inner.next_generation_id()
    }

    fn generation_ids(&self) -> Vec<GenerationId> {
        self.inner.generation_ids()
    }
}

fn shard() -> ShardId {
    ShardId::new(vec![SHARD])
}

fn bounds() -> PrefixBounds {
    PrefixBounds::new(vec![b'a'], vec![b'z'])
}

fn vector(commit: u64) -> ScopeVersionVector {
    ScopeVersionVector::from_vector(VersionVector::from_entries([(
        shard(),
        CommitVersion::new(commit),
    )]))
}

fn draft(commit: u64, lease: u64, policy: u8) -> SnapshotDraft {
    let mut draft = SnapshotDraft::sample(bounds(), LeaseTerm::new(lease));
    draft.commit_version = CommitVersion::new(commit);
    draft.version_vector = vector(commit);
    draft.policy_version = PolicyVersion::from_bytes([policy; 32]);
    draft
}

fn page(sequence: u64, commit: u64, bounds: PrefixBounds, rows: &[(&[u8], &[u8])]) -> SnapshotPage {
    let records = rows
        .iter()
        .map(|(key, value)| ((*key).to_vec(), (*value).to_vec()))
        .collect::<Vec<_>>();
    SnapshotPage::record_page(
        sequence,
        shard(),
        CommitVersion::new(commit),
        bounds,
        &records,
    )
}

fn snapshot_two_pages(commit: u64, lease: u64, policy: u8) -> SealedSnapshot {
    let first = page(
        0,
        commit,
        PrefixBounds::new(vec![b'a'], vec![b'm']),
        &[(b"alpha", b"one"), (b"bravo", b"two")],
    );
    let second = page(
        1,
        commit,
        PrefixBounds::new(vec![b'm'], vec![b'z']),
        &[(b"xray", b"three"), (b"yankee", b"four")],
    );
    draft(commit, lease, policy)
        .seal(vec![first, second])
        .expect("the two-page snapshot seals")
}

fn snapshot_one_page(commit: u64, lease: u64, policy: u8) -> SealedSnapshot {
    snapshot_rows(
        commit,
        lease,
        policy,
        &[(b"alpha", b"one"), (b"bravo", b"two")],
    )
}

fn snapshot_rows(commit: u64, lease: u64, policy: u8, rows: &[(&[u8], &[u8])]) -> SealedSnapshot {
    let first = page(0, commit, bounds(), rows);
    draft(commit, lease, policy)
        .seal(vec![first])
        .expect("the one-page snapshot seals")
}

fn run(
    store: &mut impl DurableStore,
    snapshot: &SealedSnapshot,
    request: &HydrationRequest,
) -> Result<oxkv_hydrate::HydrationReport, HydrationError> {
    let mut transport = MemoryTransport::from_snapshot(snapshot);
    let mut hydrator = Hydrator::new(store, &mut transport);
    hydrator.start(request)?;
    hydrator.run()
}

fn rows_of(snapshot: &SealedSnapshot) -> BTreeSet<Vec<u8>> {
    snapshot
        .pages
        .iter()
        .flat_map(|page| page.records().expect("the records decode"))
        .map(|(key, _)| key)
        .collect()
}

#[test]
fn a_crash_at_every_durable_boundary_converges() {
    let snapshot = snapshot_two_pages(1, 1, 2);
    let request = HydrationRequest::from_snapshot(&snapshot, 0);
    let mut clean = CrashStore::new();
    let report = run(&mut clean, &snapshot, &request).expect("the clean run completes");
    let clean_hash = report.state_hash.expect("the clean run publishes");
    let total = clean.operations();
    assert!(total >= 6, "the clean run must cross several boundaries");

    for index in 1..=total {
        let mut store = CrashStore::fail_at(index);
        let mut transport = MemoryTransport::from_snapshot(&snapshot);
        let mut hydrator = Hydrator::new(&mut store, &mut transport);
        let result = match hydrator.start(&request) {
            Ok(_) => hydrator.run(),
            Err(error) => Err(error),
        };
        assert!(result.is_err(), "the crash at operation {index} must fail");

        store.fail_at = None;
        let mut transport = MemoryTransport::from_snapshot(&snapshot);
        let mut hydrator = Hydrator::new(&mut store, &mut transport);
        let outcome = hydrator
            .start(&request)
            .expect("the restart starts or resumes");
        assert!(matches!(
            outcome,
            StartOutcome::Started | StartOutcome::Resumed
        ));
        let report = hydrator.run().expect("the restart converges after a crash");
        assert_eq!(
            report.state_hash,
            Some(clean_hash),
            "the crash at operation {index} diverged"
        );
        assert_eq!(report.progress.phase, Phase::Published);
        let published = store
            .published_generation()
            .expect("one generation is published");
        let journal = store
            .load_journal()
            .expect("the journal reads")
            .expect("the journal exists");
        assert_eq!(journal.phase, Phase::Published);
        assert_eq!(journal.swap_commit, Some(published));
    }
}

#[test]
fn a_crash_before_the_swap_publishes_one_generation() {
    let snapshot = snapshot_two_pages(1, 1, 2);
    let request = HydrationRequest::from_snapshot(&snapshot, 0);
    let mut store = CrashStore::new();
    store.publish_mode = PublishMode::Before;
    let mut transport = MemoryTransport::from_snapshot(&snapshot);
    let mut hydrator = Hydrator::new(&mut store, &mut transport);
    hydrator.start(&request).expect("the start succeeds");
    let error = hydrator.run().expect_err("the swap fails");
    assert_eq!(error.code(), "hydrate.storage_failed");
    assert_eq!(store.published_generation(), None);

    store.publish_mode = PublishMode::Normal;
    let report = run(&mut store, &snapshot, &request).expect("the restart publishes");
    assert_eq!(report.progress.phase, Phase::Published);
    assert_eq!(
        store
            .load_journal()
            .expect("the journal reads")
            .expect("the journal exists")
            .swap_commit,
        store.published_generation()
    );
}

#[test]
fn a_crash_after_the_swap_leaves_one_published_generation() {
    let snapshot = snapshot_two_pages(1, 1, 2);
    let request = HydrationRequest::from_snapshot(&snapshot, 0);
    let mut store = CrashStore::new();
    store.publish_mode = PublishMode::After;
    let mut transport = MemoryTransport::from_snapshot(&snapshot);
    let mut hydrator = Hydrator::new(&mut store, &mut transport);
    hydrator.start(&request).expect("the start succeeds");
    let error = hydrator.run().expect_err("the swap reports a crash");
    assert_eq!(error.code(), "hydrate.storage_failed");
    let published = store
        .published_generation()
        .expect("the pointer moved before the crash");
    assert_eq!(
        store
            .load_journal()
            .expect("the journal reads")
            .expect("the journal exists")
            .phase,
        Phase::Swap
    );

    store.publish_mode = PublishMode::Normal;
    let report = run(&mut store, &snapshot, &request).expect("the restart publishes");
    assert_eq!(report.progress.phase, Phase::Published);
    assert_eq!(store.published_generation(), Some(published));
    assert_eq!(
        store
            .load_journal()
            .expect("the journal reads")
            .expect("the journal exists")
            .swap_commit,
        Some(published)
    );
}

#[test]
fn a_reader_loop_sees_one_version_step_and_no_hole() {
    let old = snapshot_one_page(1, 1, 2);
    let new = snapshot_rows(2, 1, 2, &[(b"alpha", b"three"), (b"charlie", b"four")]);
    let mut store = MemoryStore::new();
    let old_request = HydrationRequest::from_snapshot(&old, 0);
    run(&mut store, &old, &old_request).expect("the old generation publishes");
    let old_rows = rows_of(&old);

    let new_request = HydrationRequest::from_snapshot(&new, 0);
    let mut transport = MemoryTransport::from_snapshot(&new);
    let mut hydrator = Hydrator::new(&mut store, &mut transport);
    hydrator
        .start(&new_request)
        .expect("the new hydration starts");

    let mut last_version = 0_u64;
    loop {
        let outcome = hydrator.step().expect("the step succeeds");
        let replica = Replica::new(hydrator.store());
        let version = replica
            .version()
            .expect("the replica is hydrated")
            .get(&shard())
            .expect("the shard is present")
            .get();
        assert!(version >= last_version, "the served version regressed");
        assert!(version - last_version <= 1, "the reader skipped a version");
        last_version = version;
        let keys = [b"alpha" as &[u8], b"bravo", b"charlie"]
            .iter()
            .filter(|key| replica.read(key).expect("the read succeeds").is_some())
            .map(|key| (*key).to_vec())
            .collect::<BTreeSet<_>>();
        let new_rows = rows_of(&new);
        assert!(
            keys == old_rows || keys == new_rows,
            "the reader saw a mixture at version {version}"
        );
        if matches!(outcome, StepOutcome::Published { .. }) {
            break;
        }
    }
    assert_eq!(last_version, 2);
    let replica = Replica::new(&store);
    assert_eq!(
        replica.read(b"charlie").expect("the new read succeeds"),
        Some(b"four".to_vec())
    );
    assert_eq!(replica.read(b"bravo").expect("the new read succeeds"), None);
}

#[test]
fn a_duplicate_frame_changes_no_state_and_a_gap_requests_the_missing_sequence() {
    let snapshot = snapshot_one_page(1, 1, 2);
    let request = HydrationRequest::with_target(snapshot.descriptor(), vector(4), 3);
    let mut store = MemoryStore::new();
    let mut transport = MemoryTransport::from_snapshot(&snapshot);
    let mut hydrator = Hydrator::new(&mut store, &mut transport);
    hydrator.start(&request).expect("the hydration starts");

    let deferred = hydrator
        .queue_frame(frame(1, 2, b"xray", Some(b"one")))
        .expect("the frame queues");
    assert_eq!(deferred, FrameOutcome::Deferred);
    let report = hydrator.run().expect("the snapshot completes");
    assert_eq!(
        report.progress.phase,
        Phase::Tail,
        "the hydrator waits for the tail"
    );
    let journal = hydrator.journal().expect("the journal exists");
    assert_eq!(journal.applied_watermark, 1);
    assert_eq!(
        hydrator
            .store()
            .generation(journal.generation)
            .expect("the new generation exists")
            .read(b"xray"),
        Some(b"one".as_slice())
    );

    let before = generation_hash(hydrator.store());
    let duplicate = hydrator
        .queue_frame(frame(1, 2, b"xray", Some(b"corrupt")))
        .expect("the duplicate is discarded");
    assert_eq!(duplicate, FrameOutcome::Duplicate { watermark: 1 });
    assert_eq!(generation_hash(hydrator.store()), before);

    let missing = hydrator
        .queue_frame(frame(3, 4, b"zulu", Some(b"three")))
        .expect("the gap is reported");
    assert_eq!(missing, FrameOutcome::Missing { next_sequence: 2 });
    let applied = hydrator
        .queue_frame(frame(2, 3, b"yankee", Some(b"two")))
        .expect("the gap fills");
    assert!(matches!(applied, FrameOutcome::Published { .. }));
    let report = hydrator.run().expect("the generation publishes");
    assert_eq!(report.progress.phase, Phase::Published);

    let generation = store.published().expect("the generation is published");
    assert_eq!(generation.read(b"xray"), Some(b"one".as_slice()));
    assert_eq!(generation.read(b"yankee"), Some(b"two".as_slice()));
    assert_eq!(generation.read(b"zulu"), Some(b"three".as_slice()));
    assert_eq!(
        generation.version_vector().get(&shard()),
        Some(CommitVersion::new(4))
    );
}

fn frame(sequence: u64, commit: u64, key: &[u8], value: Option<&[u8]>) -> DiffFrame {
    DiffFrame::new(
        sequence,
        shard(),
        CommitVersion::new(commit),
        key.to_vec(),
        value.map(Vec::from),
    )
}

fn generation_hash(store: &impl DurableStore) -> [u8; 32] {
    let journal = store
        .load_journal()
        .expect("the journal reads")
        .expect("the journal exists");
    store
        .generation(journal.generation)
        .expect("the new generation exists")
        .state_hash()
}

#[test]
fn a_corrupt_page_is_refetched_and_never_served() {
    let snapshot = snapshot_two_pages(1, 1, 2);
    let request = HydrationRequest::from_snapshot(&snapshot, 0);
    let mut store = MemoryStore::new();
    let mut transport = MemoryTransport::from_snapshot(&snapshot);
    transport.corrupt_once(PageSequence::new(0));
    let mut hydrator = Hydrator::new(&mut store, &mut transport);
    hydrator.start(&request).expect("the hydration starts");
    let report = hydrator.run().expect("the refetch succeeds");
    assert_eq!(report.progress.phase, Phase::Published);
    assert_eq!(transport.fetch_count(PageSequence::new(0)), 2);
    let generation = store.published().expect("the generation is published");
    let stored = generation
        .pages
        .get(&PageSequence::new(0))
        .expect("page zero is stored");
    assert_eq!(stored.raw, snapshot.pages[0].encode());

    let mut store = MemoryStore::new();
    let mut transport = MemoryTransport::from_snapshot(&snapshot);
    transport.corrupt_always(PageSequence::new(0));
    let mut hydrator = Hydrator::new(&mut store, &mut transport);
    hydrator.start(&request).expect("the hydration starts");
    let error = hydrator.run().expect_err("the corruption persists");
    assert_eq!(error.code(), "format.corrupt_page");
    assert_eq!(store.published_generation(), None);
    assert_eq!(
        store.journal().expect("the journal exists").phase,
        Phase::Transfer
    );
}

#[test]
fn an_out_of_bounds_page_is_rejected_during_hydration() {
    let snapshot = snapshot_one_page(1, 1, 2);
    let request = HydrationRequest::from_snapshot(&snapshot, 0);
    let outside = SnapshotPage::record_page(
        0,
        shard(),
        CommitVersion::new(1),
        PrefixBounds::new(vec![b'A'], vec![b'z']),
        &[],
    );
    let mut transport = MemoryTransport::new();
    transport.insert(PageSequence::new(0), outside.encode());
    let mut store = MemoryStore::new();
    let mut hydrator = Hydrator::new(&mut store, &mut transport);
    hydrator.start(&request).expect("the hydration starts");
    let error = hydrator
        .run()
        .expect_err("the out-of-bounds page is rejected");
    assert_eq!(error.code(), "format.page_out_of_bounds");
    assert_eq!(store.published_generation(), None);
}

#[test]
fn a_restart_resumes_at_the_last_verified_page() {
    let snapshot = snapshot_two_pages(1, 1, 2);
    let request = HydrationRequest::from_snapshot(&snapshot, 0);
    let mut store = MemoryStore::new();
    let mut transport = MemoryTransport::from_snapshot(&snapshot);
    {
        let mut hydrator = Hydrator::new(&mut store, &mut transport);
        hydrator.start(&request).expect("the hydration starts");
        hydrator.step().expect("page zero verifies");
        hydrator.step().expect("page one verifies");
    }
    let mut hydrator = Hydrator::new(&mut store, &mut transport);
    let outcome = hydrator.start(&request).expect("the restart resumes");
    assert_eq!(outcome, StartOutcome::Resumed);
    assert_eq!(
        hydrator
            .journal()
            .expect("the journal exists")
            .first_unverified(),
        PageSequence::new(2)
    );
    hydrator.run().expect("the hydration completes");
    assert_eq!(transport.fetch_count(PageSequence::new(0)), 1);
    assert_eq!(transport.fetch_count(PageSequence::new(1)), 1);
}

#[test]
fn a_cold_replica_reports_progress() {
    let snapshot = snapshot_two_pages(1, 1, 2);
    let request = HydrationRequest::from_snapshot(&snapshot, 0);
    let mut store = MemoryStore::new();
    {
        let replica = Replica::new(&store);
        let error = replica.read(b"alpha").expect_err("the replica is cold");
        assert_eq!(error.code(), "read.not_hydrated");
        match error {
            HydrationError::NotHydrated { progress, .. } => {
                assert_eq!(progress.phase, Phase::Cold);
                assert!(progress.is_cold());
            }
            other => panic!("unexpected error {other}"),
        }
    }
    let mut transport = MemoryTransport::from_snapshot(&snapshot);
    let mut hydrator = Hydrator::new(&mut store, &mut transport);
    hydrator.start(&request).expect("the hydration starts");
    hydrator.step().expect("one page verifies");
    let replica = Replica::new(hydrator.store());
    let error = replica.read(b"alpha").expect_err("the replica is cold");
    assert_eq!(error.code(), "read.not_hydrated");
    match error {
        HydrationError::NotHydrated { progress, .. } => {
            assert_eq!(progress.phase, Phase::Transfer);
            assert_eq!(progress.verified_pages, 1);
            assert_eq!(progress.total_pages, 2);
            assert_eq!(progress.published_vector, None);
        }
        other => panic!("unexpected error {other}"),
    }
    hydrator.run().expect("the hydration completes");
    assert_eq!(
        Replica::new(hydrator.store())
            .read(b"alpha")
            .expect("the replica is warm"),
        Some(b"one".to_vec())
    );
}

#[test]
fn a_policy_change_restarts_hydration_without_mixing_identities() {
    let first = snapshot_two_pages(1, 1, 2);
    let first_request = HydrationRequest::from_snapshot(&first, 0);
    let mut store = MemoryStore::new();
    run(&mut store, &first, &first_request).expect("the first snapshot publishes");

    let second = snapshot_rows(2, 2, 9, &[(b"delta", b"five"), (b"echo", b"six")]);
    let second_request = HydrationRequest::from_snapshot(&second, 0);
    let mut transport = MemoryTransport::from_snapshot(&second);
    let mut hydrator = Hydrator::new(&mut store, &mut transport);
    let outcome = hydrator
        .start(&second_request)
        .expect("the new hydration starts");
    assert_eq!(outcome, StartOutcome::Started);
    hydrator.step().expect("one new page verifies");

    let replica = Replica::new(hydrator.store());
    assert_eq!(
        replica
            .read(b"alpha")
            .expect("the old generation is readable"),
        Some(b"one".to_vec()),
        "the old generation must serve during the new hydration"
    );

    hydrator.run().expect("the new snapshot publishes");
    let generation = hydrator
        .store()
        .published()
        .expect("the new generation is published");
    assert_eq!(generation.read(b"alpha"), None);
    assert_eq!(generation.read(b"bravo"), None);
    assert_eq!(generation.read(b"delta"), Some(b"five".as_slice()));
    assert_eq!(generation.read(b"echo"), Some(b"six".as_slice()));
    assert_eq!(
        generation.descriptor.policy_version,
        PolicyVersion::from_bytes([9; 32])
    );
    let journal = hydrator
        .store()
        .load_journal()
        .expect("the journal reads")
        .expect("the journal exists");
    assert_eq!(
        journal.descriptor.policy_version,
        PolicyVersion::from_bytes([9; 32])
    );
}

#[test]
fn a_sealed_snapshot_survives_a_lease_change_during_hydration() {
    let snapshot = snapshot_two_pages(1, 1, 2);
    let resealed = snapshot.reseal(LeaseTerm::new(9));
    let request = HydrationRequest::from_snapshot(&resealed, 0);
    let mut store = MemoryStore::new();
    let report = run(&mut store, &resealed, &request).expect("the resealed snapshot hydrates");
    let mut original_store = MemoryStore::new();
    let original_request = HydrationRequest::from_snapshot(&snapshot, 0);
    let original = run(&mut original_store, &snapshot, &original_request)
        .expect("the original snapshot hydrates");
    assert_eq!(report.state_hash, original.state_hash);
    assert_eq!(
        resealed.header.content_digest,
        snapshot.header.content_digest
    );
    assert_eq!(report.progress.phase, Phase::Published);
}

#[test]
fn a_scope_over_the_hydration_budget_fails_at_subscribe() {
    let snapshot = snapshot_one_page(1, 1, 2);
    let descriptor = snapshot.descriptor();
    assert!(descriptor.snapshot_bytes > 0);
    let budget = ScopeBudget::new(descriptor.snapshot_bytes / 2, u64::MAX);
    let mut store = MemoryStore::new();
    let mut transport = MemoryTransport::from_snapshot(&snapshot);
    let mut hydrator = Hydrator::new(&mut store, &mut transport).with_budget(budget);
    let error = hydrator
        .start(&HydrationRequest::from_snapshot(&snapshot, 0))
        .expect_err("the budget rejects the scope");
    assert_eq!(error.code(), "scope.hydration_budget_exceeded");
    assert_eq!(store.generation_count(), 0);
    assert!(store.journal().is_none());

    let overhead = ScopeBudget::new(u64::MAX, 1);
    let mut store = MemoryStore::new();
    let mut transport = MemoryTransport::from_snapshot(&snapshot);
    let mut hydrator = Hydrator::new(&mut store, &mut transport).with_budget(overhead);
    assert_eq!(
        hydrator
            .start(&HydrationRequest::from_snapshot(&snapshot, 0))
            .expect_err("the overhead limit rejects the scope")
            .code(),
        "scope.hydration_budget_exceeded"
    );
}

#[test]
fn a_one_gibibyte_scope_models_under_a_bounded_page_budget() {
    let page_count = 1024_u64;
    let logical_page_bytes = 1024 * 1024_u64;
    let pages = (0..page_count)
        .map(|sequence| {
            SnapshotPage::synthetic(
                sequence,
                shard(),
                CommitVersion::new(1),
                bounds(),
                logical_page_bytes,
            )
        })
        .collect::<Vec<_>>();
    let snapshot = draft(1, 1, 2)
        .seal(pages)
        .expect("the modeled snapshot seals");
    let descriptor = snapshot.descriptor();
    assert_eq!(descriptor.snapshot_bytes, page_count * logical_page_bytes);
    let page_budget = 2 * 1024 * 1024_u64;
    let mut store = MemoryStore::new();
    let mut transport = MemoryTransport::from_snapshot(&snapshot);
    let mut hydrator = Hydrator::new(&mut store, &mut transport)
        .with_page_budget(page_budget)
        .with_budget(ScopeBudget::new(
            2 * 1024 * 1024 * 1024,
            2 * 1024 * 1024 * 1024,
        ));
    hydrator
        .start(&HydrationRequest::from_snapshot(&snapshot, 0))
        .expect("the modeled scope starts");
    let report = hydrator.run().expect("the modeled scope hydrates");
    assert_eq!(
        report.progress.verified_bytes,
        page_count * logical_page_bytes
    );
    assert_eq!(report.progress.total_pages, page_count);
    assert!(hydrator.peak_page_bytes() <= page_budget);
    let generation = store.published().expect("the generation is published");
    assert!(
        generation.resident_bytes() < 10 * 1024 * 1024,
        "the adapter must not allocate the modeled snapshot size"
    );
}
