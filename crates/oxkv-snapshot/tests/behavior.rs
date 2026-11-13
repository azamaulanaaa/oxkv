//! Behavior tests for the snapshot contract.

mod support;

use oxkv_contracts::version::{CommitVersion, LeaseTerm, PrefixBounds, ShardId};
use oxkv_snapshot::{
    Header, Record, ScopeProjectionBuilder, ScopeSnapshot, ShardSnapshot, SnapshotError,
    forbidden_namespace, page_id_for,
};

fn sample_params_with_page_size(page_size: u64) -> oxkv_snapshot::ShardSnapshotParams {
    let mut params = support::sample_params();
    params.page_size = page_size;
    params
}

#[test]
fn the_header_is_plain_text() {
    let bytes = support::sample_snapshot();
    let header = Header::parse(&bytes).expect("the header parses");
    let text =
        String::from_utf8(bytes[..header.text_length].to_vec()).expect("the header is ASCII text");
    assert!(text.starts_with("oxkv-snapshot/v1\n"));
    assert!(text.contains("format_version=00001.00000\n"));
    assert!(text.contains("artifact=ShardSnapshot\n"));
    assert!(text.contains("required_features=0000000000000007\n"));
    assert!(text.ends_with("header_end\n"));
}

#[test]
fn a_missing_provenance_field_is_rejected() {
    let bytes = support::rename_field(
        &support::sample_snapshot(),
        "manifest_digest",
        "manifest_digesx",
    );
    let error = ShardSnapshot::read(&bytes).expect_err("the missing field is rejected");
    assert_eq!("format.missing_provenance", error.code());
    assert!(matches!(
        error,
        SnapshotError::MissingProvenance {
            field: oxkv_contracts::version::ProvenanceField::ManifestDigest
        }
    ));
}

#[test]
fn an_unknown_skippable_field_is_ignored() {
    let bytes = support::rename_field(
        &support::sample_snapshot(),
        "policy_version",
        "x-policy_versi",
    );
    let snapshot = ShardSnapshot::read(&bytes).expect("the unknown field is skipped");
    assert_eq!(3, snapshot.records().count());
}

#[test]
fn the_directory_integrity_is_checked() {
    let bytes = support::sample_snapshot();
    let header = Header::parse(&bytes).expect("the header parses");
    let start = usize::try_from(header.directory_offset).expect("the offset fits");
    let length = usize::try_from(header.directory_length).expect("the length fits");
    let mut mutated = bytes.clone();
    mutated[start + length - 1] ^= 0x01;
    let error = ShardSnapshot::read(&mutated).expect_err("the directory is checked");
    assert_eq!("format.corrupt_page", error.code());
}

#[test]
fn an_oversized_directory_count_is_rejected_before_allocation() {
    let bytes = support::sample_snapshot();
    let header = Header::parse(&bytes).expect("the header parses");
    let start = usize::try_from(header.directory_offset).expect("the offset fits");
    let mut mutated = bytes.clone();
    mutated[start + 8..start + 12].copy_from_slice(&1_000_000_u32.to_be_bytes());
    let error = ShardSnapshot::read(&mutated).expect_err("the count is over the directory bytes");
    assert_eq!("format.corrupt_page", error.code());
}

#[test]
fn the_content_digest_is_checked_before_the_seal() {
    let bytes = support::sample_snapshot();
    let range = support::field_value_range(&bytes, "content_digest");
    let mut digest_broken = bytes.clone();
    digest_broken[range].fill(b'0');
    let error = ShardSnapshot::read(&digest_broken).expect_err("the digest is checked");
    assert_eq!("format.corrupt_page", error.code());

    let seal_range = support::field_value_range(&bytes, "content_seal");
    let digest_start = seal_range.start + "digest:".len();
    let mut seal_broken = bytes.clone();
    seal_broken[digest_start] = if seal_broken[digest_start] == b'0' {
        b'1'
    } else {
        b'0'
    };
    let error = ShardSnapshot::read(&seal_broken).expect_err("the seal is checked");
    assert_eq!("replication.stale_epoch", error.code());

    let mut both_broken = digest_broken;
    both_broken[digest_start] = if both_broken[digest_start] == b'0' {
        b'1'
    } else {
        b'0'
    };
    let error = ShardSnapshot::read(&both_broken).expect_err("both are broken");
    assert_eq!(
        "format.corrupt_page",
        error.code(),
        "the content digest must be checked before the seal"
    );
}

#[test]
fn an_empty_snapshot_round_trips() {
    let bytes = ShardSnapshot::build(&support::sample_params(), &[]).expect("the snapshot builds");
    let snapshot = ShardSnapshot::read(&bytes).expect("the empty snapshot reads");
    assert_eq!(0, snapshot.header().page_count);
    assert_eq!(0, snapshot.records().count());
    assert_eq!(0, snapshot.pages().len());
}

#[test]
fn page_splitting_is_deterministic() {
    let params = sample_params_with_page_size(16);
    let records = support::sample_records();
    let first = ShardSnapshot::build(&params, &records).expect("the snapshot builds");
    let second = ShardSnapshot::build(&params, &records).expect("the snapshot builds again");
    assert_eq!(first, second);
    let snapshot = ShardSnapshot::read(&first).expect("the snapshot reads");
    assert_eq!(3, snapshot.header().page_count);
    let sequences: Vec<u64> = snapshot
        .pages()
        .iter()
        .map(|page| page.page_sequence.get())
        .collect();
    assert_eq!(vec![1, 2, 3], sequences);
    assert_eq!(3, snapshot.records().count());
}

#[test]
fn a_record_larger_than_the_page_budget_is_rejected() {
    let params = sample_params_with_page_size(8);
    let error = ShardSnapshot::build(&params, &support::sample_records())
        .expect_err("the record is over the page budget");
    assert_eq!("format.page_too_large", error.code());
}

#[test]
fn unsorted_records_are_rejected() {
    let records = vec![
        Record::new(b"b".to_vec(), b"2".to_vec()),
        Record::new(b"a".to_vec(), b"1".to_vec()),
    ];
    let error = ShardSnapshot::build(&support::sample_params(), &records)
        .expect_err("unsorted records are rejected");
    assert_eq!("format.corrupt_page", error.code());
}

#[test]
fn duplicate_records_are_rejected() {
    let records = vec![
        Record::new(b"a".to_vec(), b"1".to_vec()),
        Record::new(b"a".to_vec(), b"2".to_vec()),
    ];
    let error = ShardSnapshot::build(&support::sample_params(), &records)
        .expect_err("duplicate records are rejected");
    assert_eq!("format.corrupt_page", error.code());
    assert!(matches!(error, SnapshotError::DuplicateKey { .. }));
}

#[test]
fn a_key_outside_the_prefix_bounds_is_rejected_at_build() {
    let records = vec![Record::new(b"z".to_vec(), b"1".to_vec())];
    let error = ShardSnapshot::build(&support::sample_params(), &records)
        .expect_err("the key is outside the bounds");
    assert_eq!("format.page_out_of_bounds", error.code());
}

#[test]
fn the_content_digest_ignores_the_lease_term() {
    let mut first_params = support::sample_params();
    first_params.lease_term = LeaseTerm::new(3);
    let mut second_params = support::sample_params();
    second_params.lease_term = LeaseTerm::new(4);
    let records = support::sample_records();
    let first = ShardSnapshot::build(&first_params, &records).expect("the snapshot builds");
    let second = ShardSnapshot::build(&second_params, &records).expect("the snapshot builds");
    let first_header = Header::parse(&first).expect("the header parses");
    let second_header = Header::parse(&second).expect("the header parses");
    assert_eq!(first_header.content_digest, second_header.content_digest);
    assert_ne!(first, second);
}

#[test]
fn the_page_identity_is_stable() {
    let shard = ShardId::new(vec![1]);
    let commit = CommitVersion::new(9);
    let first = page_id_for(
        &shard,
        commit,
        oxkv_contracts::version::PageSequence::new(1),
        b"a",
        b"z",
    );
    let second = page_id_for(
        &shard,
        commit,
        oxkv_contracts::version::PageSequence::new(1),
        b"a",
        b"z",
    );
    let next = page_id_for(
        &shard,
        commit,
        oxkv_contracts::version::PageSequence::new(2),
        b"a",
        b"z",
    );
    assert_eq!(first, second);
    assert_ne!(first, next);
}

#[test]
fn the_scope_projection_is_deterministic() {
    let first = build_scope(&[0, 1, 2]);
    let second = build_scope(&[2, 0, 1]);
    assert_eq!(first, second);
    let snapshot = ScopeSnapshot::read(&first).expect("the scope reads");
    assert_eq!(3, snapshot.records().count());
    assert!(matches!(
        snapshot.header().identity,
        oxkv_snapshot::HeaderIdentity::Scope(_)
    ));
    for record in snapshot.records() {
        assert_eq!(None, forbidden_namespace(&record.key));
    }
}

#[test]
fn the_scope_snapshot_excludes_forbidden_namespaces() {
    let mut builder = ScopeProjectionBuilder::new(support::sample_scope_params());
    let error = builder
        .insert(
            ShardId::new(vec![1]),
            CommitVersion::new(9),
            Record::new(vec![0xff, 0x01], b"receipt".to_vec()),
        )
        .expect_err("the receipt namespace is rejected");
    assert_eq!("format.corrupt_page", error.code());
    assert!(matches!(
        error,
        SnapshotError::ForbiddenNamespace { id: 0x01 }
    ));
}

#[test]
fn the_scope_builder_rejects_an_unknown_shard() {
    let mut builder = ScopeProjectionBuilder::new(support::sample_scope_params());
    let error = builder
        .insert(
            ShardId::new(vec![9]),
            CommitVersion::new(1),
            Record::new(b"a1".to_vec(), b"one".to_vec()),
        )
        .expect_err("the shard is not in the version vector");
    assert_eq!("format.corrupt_page", error.code());
}

#[test]
fn the_scope_builder_rejects_a_wrong_commit_version() {
    let mut builder = ScopeProjectionBuilder::new(support::sample_scope_params());
    let error = builder
        .insert(
            ShardId::new(vec![1]),
            CommitVersion::new(8),
            Record::new(b"a1".to_vec(), b"one".to_vec()),
        )
        .expect_err("the commit version is wrong");
    assert_eq!("format.corrupt_page", error.code());
}

#[test]
fn a_scope_snapshot_does_not_read_as_a_shard_snapshot() {
    let bytes = build_scope(&[0, 1, 2]);
    let error = ShardSnapshot::read(&bytes).expect_err("the artifact is a scope snapshot");
    assert_eq!("format.unsupported_version", error.code());
}

#[test]
fn the_first_page_entry_matches_the_page() {
    let bytes = support::sample_snapshot();
    let (page_id, offset, length) = support::first_page(&bytes);
    let snapshot = ShardSnapshot::read(&bytes).expect("the snapshot reads");
    let page = &snapshot.pages()[0];
    assert_eq!(page_id, page.page_id);
    assert_eq!(
        u64::try_from(page.encode().expect("the page encodes").len()).unwrap(),
        length
    );
    assert!(offset > 0);
}

fn build_scope(order: &[usize]) -> Vec<u8> {
    let mut builder = ScopeProjectionBuilder::new(support::sample_scope_params());
    let rows = [
        (
            ShardId::new(vec![1]),
            CommitVersion::new(9),
            Record::new(b"a1".to_vec(), b"one".to_vec()),
        ),
        (
            ShardId::new(vec![1]),
            CommitVersion::new(9),
            Record::new(vec![0xff, 0x04, b'f', 0x01], b"fact".to_vec()),
        ),
        (
            ShardId::new(vec![2]),
            CommitVersion::new(7),
            Record::new(b"b1".to_vec(), b"two".to_vec()),
        ),
    ];
    for index in order {
        let (shard_id, commit_version, record) = rows[*index].clone();
        builder
            .insert(shard_id, commit_version, record)
            .expect("the row is admitted");
    }
    builder.build().expect("the scope builds")
}

#[test]
fn the_test_prefix_bounds_stay_valid() {
    let bounds = PrefixBounds::new(b"a".to_vec(), b"z".to_vec());
    assert!(bounds.lower() < bounds.upper());
}
