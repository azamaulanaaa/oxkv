//! Golden vector runner for the snapshot contract.

mod support;

use oxkv_contracts::version::LeaseTerm;
use oxkv_snapshot::{Header, ScopeSnapshot, ShardSnapshot, SnapshotError, reseal};
use serde_json::Value;
use support::{GoldenCase, hex, load};

fn expected_str<'a>(case: &'a GoldenCase, field: &str) -> &'a str {
    case.expected[field]
        .as_str()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

fn expected_u64(case: &GoldenCase, field: &str) -> u64 {
    case.expected[field]
        .as_u64()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

fn expected_bool(case: &GoldenCase, field: &str) -> bool {
    case.expected[field]
        .as_bool()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

fn input_u64(case: &GoldenCase, field: &str) -> u64 {
    case.input[field]
        .as_u64()
        .unwrap_or_else(|| panic!("the case {} has no {field} input", case.name))
}

#[test]
fn valid_v1_vector_is_pinned() {
    let file = load(include_str!("vectors/valid-v1.json"));
    assert_eq!(file.contract, "snapshot-page/v1");
    let case = &file.cases[0];
    let bytes = support::sample_snapshot();
    assert_eq!(
        expected_str(case, "snapshot_hex"),
        hex(&bytes),
        "the v1 snapshot bytes changed"
    );
    assert_eq!(
        support::unhex(expected_str(case, "snapshot_hex")),
        bytes,
        "the pinned bytes decode"
    );
    let snapshot = ShardSnapshot::read(&bytes).expect("the sample reads");
    assert_eq!(
        expected_str(case, "content_digest"),
        snapshot.content_digest().to_hex()
    );
    assert_eq!(
        expected_u64(case, "page_count"),
        snapshot.header().page_count
    );
    assert_eq!(
        expected_u64(case, "record_count"),
        u64::try_from(snapshot.records().count()).expect("the count fits")
    );
}

#[test]
fn v2_major_vector_is_rejected() {
    let file = load(include_str!("vectors/v2-major.json"));
    assert_eq!(file.contract, "snapshot-page/v1");
    let case = &file.cases[0];
    let first_line = case.input["first_line"]
        .as_str()
        .expect("the case has a first line");
    let format_version = case.input["format_version"]
        .as_str()
        .expect("the case has a format version");
    let bytes = support::sample_snapshot();
    let bytes = support::replace_first_line(&bytes, first_line);
    let bytes = support::replace_field(&bytes, "format_version", format_version);
    let error = ShardSnapshot::read(&bytes).expect_err("a v2 major snapshot is rejected");
    assert_eq!(expected_str(case, "code"), error.code());
    assert!(matches!(
        error,
        SnapshotError::UnsupportedVersion { major: 2, .. }
    ));
}

#[test]
fn one_byte_page_corruption_vector_is_rejected() {
    let file = load(include_str!("vectors/page-corruption.json"));
    assert_eq!(file.contract, "snapshot-page/v1");
    let case = &file.cases[0];
    let page_index = usize::try_from(input_u64(case, "page_index")).expect("the index fits");
    let byte_from_end = usize::try_from(input_u64(case, "byte_from_end")).expect("the offset fits");
    let (page_id, page_offset, bytes) =
        support::corrupt_page(&support::sample_snapshot(), page_index, byte_from_end);
    let error = ShardSnapshot::read(&bytes).expect_err("a corrupt page is rejected");
    assert_eq!(expected_str(case, "code"), error.code());
    match error {
        SnapshotError::CorruptPage {
            page_id: found_id,
            offset,
            ..
        } => {
            assert_eq!(page_id, found_id);
            assert_eq!(page_offset, offset);
        }
        other => panic!("the error is not CorruptPage: {other}"),
    }
}

#[test]
fn page_out_of_bounds_vector_is_rejected() {
    let file = load(include_str!("vectors/page-out-of-bounds.json"));
    assert_eq!(file.contract, "snapshot-page/v1");
    let case = &file.cases[0];
    let field = case.input["field"].as_str().expect("the case has a field");
    let value = case.input["value_hex"]
        .as_str()
        .expect("the case has a value");
    let bytes = support::replace_field(&support::sample_snapshot(), field, value);
    let error = ShardSnapshot::read(&bytes).expect_err("an out-of-bounds page is rejected");
    assert_eq!(expected_str(case, "code"), error.code());
    assert!(matches!(error, SnapshotError::PageOutOfBounds { .. }));
}

#[test]
fn truncation_vector_is_rejected_at_every_offset() {
    let file = load(include_str!("vectors/truncation.json"));
    assert_eq!(file.contract, "snapshot-page/v1");
    let case = &file.cases[0];
    let bytes = support::sample_snapshot();
    let offsets = case.expected["offsets"]
        .as_array()
        .expect("the case lists offsets");
    for value in offsets {
        let offset = usize::try_from(value.as_u64().expect("the offset is a number"))
            .expect("the offset fits");
        let error = ShardSnapshot::read(&bytes[..offset]).expect_err("a cut snapshot is rejected");
        assert_eq!("format.truncated_snapshot", error.code());
        assert_eq!(
            u64::try_from(offset).expect("the offset fits"),
            error.offset()
        );
    }
    for offset in 0..bytes.len() {
        let error = ShardSnapshot::read(&bytes[..offset]).expect_err("a cut snapshot is rejected");
        assert_eq!(
            "format.truncated_snapshot",
            error.code(),
            "the cut at byte {offset} did not report a truncation"
        );
        assert_eq!(
            u64::try_from(offset).expect("the offset fits"),
            error.offset(),
            "the cut at byte {offset} reported the wrong offset"
        );
    }
}

#[test]
fn unknown_required_feature_vector_is_rejected() {
    let file = load(include_str!("vectors/unknown-required-feature.json"));
    assert_eq!(file.contract, "snapshot-page/v1");
    let case = &file.cases[0];
    let field = case.input["field"].as_str().expect("the case has a field");
    let value = case.input["value"].as_str().expect("the case has a value");
    let bytes = support::replace_field(&support::sample_snapshot(), field, value);
    let error = ShardSnapshot::read(&bytes).expect_err("an unknown feature is rejected");
    assert_eq!(expected_str(case, "code"), error.code());
    match error {
        SnapshotError::MissingRequiredFeature { bit, .. } => {
            assert_eq!(expected_u64(case, "bit"), u64::from(bit));
        }
        other => panic!("the error is not MissingRequiredFeature: {other}"),
    }
}

#[test]
fn lease_change_vector_keeps_the_content_digest() {
    let file = load(include_str!("vectors/lease-change.json"));
    assert_eq!(file.contract, "snapshot-page/v1");
    let case = &file.cases[0];
    let new_lease = LeaseTerm::new(input_u64(case, "new_lease_term"));
    let base = support::sample_snapshot();
    let resealed = reseal(&base, new_lease, None).expect("the snapshot reseals");
    let base_header = Header::parse(&base).expect("the base header parses");
    let resealed_header = Header::parse(&resealed).expect("the resealed header parses");
    assert!(expected_bool(case, "same_content_digest"));
    assert_eq!(base_header.content_digest, resealed_header.content_digest);
    assert_eq!(
        base_header.content_seal.content_digest(),
        resealed_header.content_seal.content_digest()
    );
    assert_eq!(new_lease, resealed_header.lease_term);
    assert_eq!(
        &base[base_header.text_length..],
        &resealed[resealed_header.text_length..],
        "the body changed across a lease term"
    );
    assert!(base != resealed, "the headers must differ");
    assert!(ShardSnapshot::read(&base).is_ok());
    assert!(ShardSnapshot::read(&resealed).is_ok());
}

#[test]
fn native_and_wasm_identity_vector_is_pinned() {
    let file = load(include_str!("vectors/native-wasm.json"));
    assert_eq!(file.contract, "snapshot-page/v1");
    let case = &file.cases[0];
    let valid = load(include_str!("vectors/valid-v1.json"));
    let expected_hex = valid.cases[0].expected["snapshot_hex"]
        .as_str()
        .expect("the valid vector pins the bytes")
        .to_owned();
    assert_eq!(expected_str(case, "snapshot_hex"), expected_hex);
    assert_eq!(
        expected_str(case, "content_digest"),
        valid.cases[0].expected["content_digest"]
            .as_str()
            .expect("the valid vector pins the digest")
    );
    let bytes = support::sample_snapshot();
    assert_eq!(hex(&bytes), expected_hex);
    let scope = support::sample_scope_snapshot();
    let scope = ScopeSnapshot::read(&scope).expect("the scope sample reads");
    assert!(scope.records().count() > 0);
}

#[test]
fn every_golden_file_names_a_contract() {
    for text in [
        include_str!("vectors/valid-v1.json"),
        include_str!("vectors/v2-major.json"),
        include_str!("vectors/page-corruption.json"),
        include_str!("vectors/page-out-of-bounds.json"),
        include_str!("vectors/truncation.json"),
        include_str!("vectors/unknown-required-feature.json"),
        include_str!("vectors/lease-change.json"),
        include_str!("vectors/native-wasm.json"),
    ] {
        let file = load(text);
        assert_eq!(file.contract, "snapshot-page/v1");
        assert!(!file.cases.is_empty());
        let _: &Value = &file.cases[0].expected;
    }
}
