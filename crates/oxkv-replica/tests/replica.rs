#![allow(missing_docs, unreachable_pub)]

mod support;

use oxkv_contracts::version::{
    CommitVersion, ContentDigest, KeyVersion, PageSequence, ScopeIdentity, TokenizerVersion,
};
use oxkv_replica::{
    DeviceBudget, DiffFrame, Ingest, Mirror, MirroredRecord, Pin, ReadRequest, Replica,
    ReplicaIndex, ReplicaStatus, ScopeFootprint, Scorer, SearchRequest, StalenessBound,
};
use support::{
    SCOPE, config, golden_rows, hex, key, page, row, scope, shard, snapshot, two_page_snapshot,
    two_row_snapshot, unhex, vector,
};

fn hydrated(commit_version: u64) -> Replica {
    let mut replica = Replica::new(config(u64::MAX / 4, u64::MAX / 4), scope());
    replica
        .hydrate(two_row_snapshot(commit_version))
        .expect("the snapshot hydrates");
    replica
}

fn read_request(replica: &Replica, name: &str, bound: StalenessBound) -> ReadRequest {
    ReadRequest::new(key(name), Pin::at_least(replica.applied_version()), bound)
}

fn search_request(replica: &Replica, text: &str, bound: StalenessBound) -> SearchRequest {
    SearchRequest::new(text, Pin::at_least(replica.applied_version()), bound, 10)
}

#[test]
fn an_offline_day_still_reads_and_searches_the_scope() {
    let mut replica = hydrated(5);
    replica.note_authority(&vector(5), 0);
    let status = replica.status(86_400);
    assert_eq!(status.lag_time, Some(86_400));
    assert_eq!(status.lag_versions, 0);

    let read = replica
        .get(&read_request(&replica, "alice", StalenessBound::Unbounded))
        .expect("the row is resident");
    assert_eq!(read.value, b"red green");

    let search = replica
        .search(&search_request(
            &replica,
            "green",
            StalenessBound::Unbounded,
        ))
        .expect("the index answers");
    assert_eq!(search.hits.len(), 2);
}

#[test]
fn no_out_of_scope_row_is_stored() {
    let mut mirror = Mirror::new(scope(), 1_000_000, 1);
    let out_of_scope = MirroredRecord::row(
        b"\x01orders\x00\x10eve".to_vec(),
        b"green".to_vec(),
        KeyVersion::new(1),
    );
    let error = mirror
        .ingest_page(page(1, 5, vec![out_of_scope]))
        .expect_err("the page is outside the scope");
    assert_eq!(error.code(), "replica.scope_violation");
    assert!(mirror.membership().is_empty());
    assert_eq!(mirror.resident_bytes(), 0);
}

#[test]
fn a_search_hit_point_reads_at_the_same_pin() {
    let replica = hydrated(5);
    let search = replica
        .search(&search_request(
            &replica,
            "green",
            StalenessBound::Unbounded,
        ))
        .expect("the index answers");
    for hit in &search.hits {
        let name = hit
            .key
            .rsplit(|byte| *byte == 0x10)
            .next()
            .unwrap_or_default();
        let name = String::from_utf8_lossy(name);
        let read = replica
            .get(&read_request(&replica, &name, StalenessBound::Unbounded))
            .expect("the hit is resident");
        assert_eq!(read.served, search.served);
    }
}

#[test]
fn the_applied_vector_never_decreases() {
    let mut replica = hydrated(5);
    let error = replica
        .hydrate(two_row_snapshot(4))
        .expect_err("the snapshot is older");
    assert_eq!(error.code(), "replica.version_regression");
    assert_eq!(
        replica.applied_version().get(&shard()),
        Some(CommitVersion::new(5))
    );

    replica.note_authority(&vector(7), 10);
    replica.note_authority(&vector(6), 20);
    assert_eq!(
        replica.authority_watermark().get(&shard()),
        Some(CommitVersion::new(7))
    );
}

#[test]
fn a_local_mirror_mutation_is_rejected() {
    let mut replica = hydrated(5);
    let before = replica.residency();
    let error = replica
        .apply_local_mutation(&key("alice"), b"mallory")
        .expect_err("the mirror is read-only");
    assert_eq!(error.code(), "replica.local_mutation_rejected");
    assert_eq!(replica.residency(), before);
    let read = replica
        .get(&read_request(&replica, "alice", StalenessBound::Unbounded))
        .expect("the row is unchanged");
    assert_eq!(read.value, b"red green");
}

#[test]
fn eviction_does_not_change_membership() {
    let mut mirror = Mirror::new(scope(), 1_000_000, 1);
    let first = page(
        1,
        5,
        vec![row("alice", "red green", 1), row("bob", "green blue", 1)],
    );
    let second = page(2, 6, vec![row("alice", "green", 2)]);
    assert_eq!(
        mirror.ingest_page(first).expect("the page ingests"),
        Ingest::Applied
    );
    assert_eq!(
        mirror.ingest_page(second).expect("the page ingests"),
        Ingest::Applied
    );
    let before = mirror.membership();
    let report = mirror.evict_superseded();
    assert_eq!(report.pages_removed, 1);
    assert_eq!(mirror.membership(), before);
    assert_eq!(mirror.page_count(), 1);
}

#[test]
fn a_cold_read_is_not_an_empty_scope() {
    let replica = Replica::new(config(1_000_000, 1_000_000), scope());
    let request = ReadRequest::new(
        key("alice"),
        Pin::at_least(vector(0)),
        StalenessBound::Unbounded,
    );
    let error = replica.get(&request).expect_err("the replica is cold");
    assert_eq!(error.code(), "read.not_hydrated");
    assert!(matches!(
        error,
        oxkv_replica::ReplicaError::NotHydrated { .. }
    ));
    let status = replica.status(0);
    assert_eq!(status.hydration, oxkv_replica::HydrationState::Cold);
}

#[test]
fn a_hydrated_replica_serves_the_old_generation_during_hydration() {
    let mut replica = hydrated(5);
    let next = two_row_snapshot(6);
    replica
        .begin_hydration(oxkv_replica::HydrationTarget::new(
            next.resident_bytes(),
            vector(6),
        ))
        .expect("the target fits");
    assert!(matches!(
        replica.hydration_state(),
        oxkv_replica::HydrationState::Hydrating { .. }
    ));
    let read = replica
        .get(&read_request(&replica, "alice", StalenessBound::Unbounded))
        .expect("the old generation serves");
    assert_eq!(read.value, b"red green");
    replica.abort_hydration();
}

#[test]
fn a_read_over_its_bound_returns_stale_and_no_data() {
    let mut replica = hydrated(5);
    replica.note_authority(&vector(10), 0);
    let error = replica
        .get(&read_request(
            &replica,
            "alice",
            StalenessBound::MaxVersions(3),
        ))
        .expect_err("the read is too stale");
    assert_eq!(error.code(), "read.stale");
    assert!(matches!(error, oxkv_replica::ReplicaError::Stale { .. }));
}

#[test]
fn a_tokenizer_mismatch_returns_the_named_error() {
    let replica = hydrated(5);
    let request = search_request(&replica, "green", StalenessBound::Unbounded)
        .with_tokenizer(TokenizerVersion::new(2));
    let error = replica.search(&request).expect_err("the tokenizer differs");
    assert_eq!(error.code(), "search.tokenizer_mismatch");
}

#[test]
fn a_flipped_index_byte_returns_index_mismatch() {
    let replica = hydrated(5);
    let mut bytes = replica
        .index_bytes()
        .expect("the index is published")
        .to_vec();
    let position = bytes.len() / 2;
    bytes[position] ^= 0x01;
    let error = ReplicaIndex::open(&bytes).expect_err("the digest differs");
    assert_eq!(error.code(), "search.index_mismatch");
}

#[test]
fn a_scope_over_the_device_budget_fails() {
    let snapshot = two_row_snapshot(5);
    let required = snapshot.resident_bytes();
    let budget = DeviceBudget::new(required.saturating_sub(1), required);
    let error = budget
        .admit(ScopeFootprint::new(required, 0))
        .expect_err("the scope is too large");
    assert_eq!(error.code(), "scope.device_budget_exceeded");

    let mut replica = Replica::new(config(required.saturating_sub(1), required), scope());
    let error = replica
        .hydrate(snapshot)
        .expect_err("the scope is too large");
    assert_eq!(error.code(), "scope.device_budget_exceeded");
    assert!(!replica.is_hydrated());
}

#[test]
fn a_hydration_peak_over_the_budget_fails() {
    let first = two_row_snapshot(5);
    let probe = hydrated(5);
    let used = probe.residency().total_bytes;
    let mut replica = Replica::new(config(used + 1, used + 1), scope());
    replica
        .hydrate(first.clone())
        .expect("the first generation fits");
    let second = snapshot(
        6,
        vec![
            page(1, 6, vec![row("alice", "red green", 2)]),
            page(2, 6, vec![row("bob", "green blue", 2)]),
        ],
    );
    let error = replica.hydrate(second).expect_err("the peak is too large");
    assert_eq!(error.code(), "scope.hydration_budget_exceeded");
    assert!(replica.is_hydrated());
    let read = replica
        .get(&read_request(&replica, "alice", StalenessBound::Unbounded))
        .expect("the served generation survives");
    assert_eq!(read.value, b"red green");
}

#[test]
fn one_cursor_advancer_exists_per_replica_identity() {
    let mut replica = hydrated(5);
    let advancer = replica
        .acquire_advancer(oxkv_replica::TabId::new("tab-a"))
        .expect("the lease is free");
    let error = replica
        .acquire_advancer(oxkv_replica::TabId::new("tab-b"))
        .expect_err("the lease is held");
    assert_eq!(error.code(), "replica.tab_lease_held");

    let diff = DiffFrame {
        shard_id: shard(),
        commit_version: CommitVersion::new(6),
        key: key("alice"),
        value: Some(b"green".to_vec()),
        key_version: KeyVersion::new(2),
        scope_identity: ScopeIdentity::from_bytes(SCOPE),
    };
    assert_eq!(
        replica
            .advance_cursor(&advancer, &diff)
            .expect("the frame applies"),
        Ingest::Applied
    );
    assert_eq!(
        replica
            .advance_cursor(&advancer, &diff)
            .expect("the duplicate is safe"),
        Ingest::Duplicate
    );
    replica
        .release_advancer(&advancer)
        .expect("the guard matches");
    let second = replica
        .acquire_advancer(oxkv_replica::TabId::new("tab-b"))
        .expect("the lease is free");
    replica
        .release_advancer(&second)
        .expect("the guard matches");
}

#[test]
fn a_replica_does_not_hold_an_authority_lease() {
    let replica = hydrated(5);
    assert!(replica.authority_lease().is_none());
    assert!(!replica.holds_authority_lease());
    let lease = oxkv_replica::LocalTabLease::new(replica.identity().clone());
    assert!(!lease.commits_data());
    assert!(!lease.fences_authority_writes());
}

#[test]
fn the_outbox_stays_separate_from_the_mirror() {
    let mut replica = hydrated(5);
    let before = replica.residency();
    replica
        .enqueue_write(oxkv_replica::OutboxRecord::new(
            "write-1".to_owned(),
            KeyVersion::new(1),
            ScopeIdentity::from_bytes(SCOPE),
            ContentDigest::from_bytes([9; 32]),
            None,
        ))
        .expect("the outbox accepts the write");
    assert_eq!(replica.outbox().len(), 1);
    assert_eq!(replica.residency().mirror_bytes, before.mirror_bytes);
    let read = replica
        .get(&read_request(&replica, "alice", StalenessBound::Unbounded))
        .expect("the mirror is unchanged");
    assert_eq!(read.value, b"red green");
}

#[test]
fn a_single_page_publish_of_a_two_page_snapshot_fails() {
    let mut snapshot = two_page_snapshot(5);
    snapshot.pages.truncate(1);
    let mut replica = Replica::new(config(u64::MAX / 4, u64::MAX / 4), scope());
    let error = replica
        .hydrate(snapshot)
        .expect_err("a partial generation must not publish");
    assert_eq!(error.code(), "replica.partial_generation");
    assert!(!replica.is_hydrated());
}

#[test]
fn a_crash_and_resume_publishes_one_complete_generation() {
    let snapshot = two_page_snapshot(5);
    let mut replica = Replica::new(config(u64::MAX / 4, u64::MAX / 4), scope());
    replica
        .begin_hydration(oxkv_replica::HydrationTarget::for_snapshot(&snapshot))
        .expect("the hydration starts");
    replica
        .hydrate_page(snapshot.pages[0].clone())
        .expect("the first page verifies");
    let journal = replica.journal().expect("the journal exists").clone();
    assert_eq!(journal.phase, oxkv_replica::HydrationPhase::Transfer);
    assert!(journal.is_verified(PageSequence::new(1)));

    let mut resumed = Replica::with_journal(config(u64::MAX / 4, u64::MAX / 4), scope(), journal);
    let report = resumed
        .hydrate(snapshot)
        .expect("the resumed hydration publishes");
    assert_eq!(report.page_count, 2);
    let journal = resumed.journal().expect("the journal exists");
    assert_eq!(journal.phase, oxkv_replica::HydrationPhase::Published);
    assert_eq!(journal.swap_commit, Some(report.generation));
    let read = resumed
        .get(&read_request(&resumed, "bob", StalenessBound::Unbounded))
        .expect("the resumed generation serves");
    assert_eq!(read.value, b"green blue");
}

#[test]
fn a_diff_gap_is_rejected_before_apply() {
    let mut mirror = Mirror::new(scope(), 1_000_000, 1);
    let first = DiffFrame {
        shard_id: shard(),
        commit_version: CommitVersion::new(3),
        key: key("alice"),
        value: Some(b"red green".to_vec()),
        key_version: KeyVersion::new(1),
        scope_identity: ScopeIdentity::from_bytes(SCOPE),
    };
    assert_eq!(
        mirror.apply_diff(&first).expect("the first frame applies"),
        Ingest::Applied
    );
    let gap = DiffFrame {
        shard_id: shard(),
        commit_version: CommitVersion::new(5),
        key: key("bob"),
        value: Some(b"green blue".to_vec()),
        key_version: KeyVersion::new(1),
        scope_identity: ScopeIdentity::from_bytes(SCOPE),
    };
    let error = mirror.apply_diff(&gap).expect_err("the gap must not apply");
    assert_eq!(error.code(), "sync.gap_too_large");
    assert!(matches!(error, oxkv_replica::ReplicaError::DiffGap { .. }));
    assert!(mirror.row(&key("bob")).is_none());
    assert_eq!(mirror.applied().get(&shard()), Some(CommitVersion::new(3)));
}

#[test]
fn a_page_with_a_changed_record_fails_its_checksum() {
    let mut page = page(1, 5, vec![row("alice", "red green", 1)]);
    page.records[0].value = Some(b"mallory".to_vec());
    let mut replica = Replica::new(config(u64::MAX / 4, u64::MAX / 4), scope());
    let error = replica
        .hydrate(snapshot(5, vec![page]))
        .expect_err("the page is corrupt");
    assert_eq!(error.code(), "format.corrupt_page");
    assert!(!replica.is_hydrated());
}

#[test]
fn the_status_reports_the_applied_version_and_lag() {
    let mut replica = hydrated(5);
    replica.note_authority(&vector(9), 0);
    let status: ReplicaStatus = replica.status(30);
    assert_eq!(status.applied.get(&shard()), Some(CommitVersion::new(5)));
    assert_eq!(
        status.authority_watermark.get(&shard()),
        Some(CommitVersion::new(9))
    );
    assert_eq!(status.lag_versions, 4);
    assert_eq!(status.lag_time, Some(30));
}

#[test]
fn a_native_rebuild_produces_the_same_index_bytes() {
    let rows = golden_rows();
    let scope = scope();
    let forward = ReplicaIndex::build(
        &scope,
        rows.iter().map(|(key, entry)| (key.as_slice(), entry)),
        Scorer::DEFAULT,
    );
    let backward = ReplicaIndex::build(
        &scope,
        rows.iter()
            .rev()
            .map(|(key, entry)| (key.as_slice(), entry)),
        Scorer::DEFAULT,
    );
    assert_eq!(forward.bytes(), backward.bytes());
    assert_eq!(forward.content_digest(), backward.content_digest());
    assert_eq!(forward.document_count(), 3);
}

#[test]
fn the_golden_index_vector_is_pinned() {
    let file = support::load(include_str!("vectors/replica.json"));
    assert_eq!(file.contract, "replica-index/v1");
    let case = file.cases.first().expect("the file has a case");
    assert_eq!(case.name, "three_documents");
    let expected = &case.expected;
    let rows = golden_rows();
    let index = ReplicaIndex::build(
        &scope(),
        rows.iter().map(|(key, entry)| (key.as_slice(), entry)),
        Scorer::DEFAULT,
    );
    assert_eq!(
        hex(index.bytes()),
        expected["index_bytes_hex"]
            .as_str()
            .expect("the case has index bytes")
    );
    assert_eq!(
        index.content_digest().to_hex(),
        expected["index_digest_hex"]
            .as_str()
            .expect("the case has a digest")
    );
    assert_eq!(
        u64::from(index.index_version().get()),
        expected["index_version"]
            .as_u64()
            .expect("the case has an index version")
    );
    assert_eq!(
        u64::from(index.tokenizer_version().revision()),
        expected["tokenizer_version"]
            .as_u64()
            .expect("the case has a tokenizer version")
    );
    let bytes = unhex(
        expected["index_bytes_hex"]
            .as_str()
            .expect("the case has index bytes"),
    );
    let opened = ReplicaIndex::open(&bytes).expect("the golden index opens");
    assert_eq!(opened.bytes(), index.bytes());
    let hits = opened.search(&scope(), "green", 10);
    let expected_hits = expected["hits"].as_array().expect("the case has hits");
    assert_eq!(hits.len(), expected_hits.len());
    for (hit, expected_hit) in hits.iter().zip(expected_hits) {
        assert_eq!(
            hex(&hit.key),
            expected_hit["key_hex"].as_str().expect("the hit has a key")
        );
        assert_eq!(
            hit.score,
            expected_hit["score"].as_u64().expect("the hit has a score")
        );
    }
}

#[test]
fn a_page_sequence_does_not_change_page_identity() {
    let first = page(1, 5, vec![row("alice", "red green", 1)]);
    let second = page(1, 5, vec![row("alice", "red green", 1)]);
    assert_eq!(first.header.page_id, second.header.page_id);
    assert_eq!(first.header.page_sequence, PageSequence::new(1));
}
