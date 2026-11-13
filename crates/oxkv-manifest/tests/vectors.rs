//! Golden vectors for manifest v1.
//!
//! The vectors are append-only for one schema version. A later change adds a
//! case. It does not change a released case.

use oxkv_manifest::{
    CODE_ARTIFACT_DRIFT, CODE_DUPLICATE_NAME, CODE_FLOAT_NOT_ALLOWED, CODE_INDEX_SCOPE_UNREACHABLE,
    CODE_INDEX_UNKNOWN_FIELD, CODE_KEY_SCHEMA_MISSING, CODE_OPERATION_CROSS_SHARD,
    CODE_OPERATION_UNKNOWN_FIELD, CODE_POLICY_CATALOG_MISSING, CODE_RESERVED_TABLE_NAME,
    CODE_RULE_UNBOUNDED, CODE_RULE_UNKNOWN_PATH, CODE_UNKNOWN_FIELD, CODE_UNSUPPORTED_SCHEMA,
    Manifest, ManifestError, compile,
};

/// The full manifest fixture.
const FULL_MANIFEST: &str = include_str!("vectors/full-manifest.json");
/// The expected canonical bytes.
const CANONICAL: &str = include_str!("vectors/full-manifest.canonical.json");
/// The expected manifest digest.
const MANIFEST_DIGEST: &str = include_str!("vectors/full-manifest.digest.txt");
/// The expected descriptor bytes.
const DESCRIPTOR: &str = include_str!("vectors/descriptor.canonical.json");
/// The expected descriptor digest.
const DESCRIPTOR_DIGEST: &str = include_str!("vectors/descriptor.digest.txt");

fn full_manifest() -> Manifest {
    let manifest = Manifest::parse(FULL_MANIFEST).expect("the full manifest parses");
    manifest.validate().expect("the full manifest validates");
    manifest
}

fn fixture(name: &str) -> &'static str {
    match name {
        "unknown-field" => include_str!("vectors/errors/unknown-field.json"),
        "unsupported-schema" => include_str!("vectors/errors/unsupported-schema.json"),
        "duplicate-key" => include_str!("vectors/errors/duplicate-key.json"),
        "duplicate-name" => include_str!("vectors/errors/duplicate-name.json"),
        "reserved-table-name" => include_str!("vectors/errors/reserved-table-name.json"),
        "missing-key-schema" => include_str!("vectors/errors/missing-key-schema.json"),
        "float" => include_str!("vectors/errors/float.json"),
        "index-unknown-field" => include_str!("vectors/errors/index-unknown-field.json"),
        "index-scope-unreachable" => {
            include_str!("vectors/errors/index-scope-unreachable.json")
        }
        "rule-unknown-path" => include_str!("vectors/errors/rule-unknown-path.json"),
        "rule-unbounded" => include_str!("vectors/errors/rule-unbounded.json"),
        "operation-unknown-field" => {
            include_str!("vectors/errors/operation-unknown-field.json")
        }
        "operation-cross-shard" => include_str!("vectors/errors/operation-cross-shard.json"),
        "artifact-drift" => include_str!("vectors/errors/artifact-drift.json"),
        "policy-catalog-missing" => {
            include_str!("vectors/errors/policy-catalog-missing.json")
        }
        other => panic!("the fixture {other} is unknown"),
    }
}

fn fixture_error(name: &str) -> ManifestError {
    let text = fixture(name);
    match Manifest::parse(text) {
        Ok(manifest) => manifest
            .validate()
            .and_then(|()| manifest.verify_digest())
            .expect_err("the error fixture must fail"),
        Err(error) => error,
    }
}

#[test]
fn the_full_manifest_has_the_declared_shape() {
    let manifest = full_manifest();
    assert_eq!(manifest.tables.len(), 3);
    assert_eq!(manifest.indexes.len(), 4);
    assert_eq!(manifest.rules.declarations.len(), 5);
    assert_eq!(manifest.operations.len(), 3);
}

#[test]
fn the_full_manifest_canonical_bytes_are_pinned() {
    let manifest = full_manifest();
    assert_eq!(manifest.canonical_text(), CANONICAL);
    assert_eq!(manifest.canonical_bytes(), CANONICAL.as_bytes());
}

#[test]
fn the_full_manifest_digest_is_pinned() {
    let manifest = full_manifest();
    assert_eq!(manifest.digest().to_hex(), MANIFEST_DIGEST.trim());
    manifest.verify_digest().expect("the stored digest matches");
    assert_eq!(
        manifest.schema.digest.expect("the fixture is sealed"),
        manifest.digest()
    );
}

#[test]
fn the_canonical_bytes_round_trip() {
    let manifest = full_manifest();
    let reparsed = Manifest::parse(&manifest.canonical_text()).expect("the canonical text parses");
    assert_eq!(reparsed, manifest);
    assert_eq!(reparsed.canonical_text(), manifest.canonical_text());
    assert_eq!(reparsed.digest(), manifest.digest());
}

#[test]
fn a_rule_body_change_does_not_change_the_digest() {
    let body_v1 = include_str!("vectors/rule-body-v1.txt");
    let body_v2 = include_str!("vectors/rule-body-v2.txt");
    assert_ne!(body_v1, body_v2, "the two rule bodies must differ");

    let manifest = full_manifest();
    assert_eq!(manifest.digest().to_hex(), MANIFEST_DIGEST.trim());
    let canonical = manifest.canonical_text();
    assert!(
        !canonical.contains(body_v1.trim()),
        "the manifest embeds the first rule body"
    );
    assert!(
        !canonical.contains(body_v2.trim()),
        "the manifest embeds the second rule body"
    );
    assert!(
        !canonical.contains("policy_version"),
        "the manifest names the active policy version"
    );
}

#[test]
fn the_descriptor_bytes_are_byte_identical() {
    let first = full_manifest();
    let second = Manifest::parse(FULL_MANIFEST).expect("the second parse succeeds");
    second.validate().expect("the second manifest validates");

    let descriptor_first = compile(&first).expect("the first compile succeeds");
    let descriptor_second = compile(&second).expect("the second compile succeeds");

    assert_eq!(descriptor_first.bytes(), descriptor_second.bytes());
    assert_eq!(descriptor_first.text(), DESCRIPTOR);
    assert_eq!(descriptor_first.manifest_digest(), first.digest());
    assert_eq!(descriptor_first.digest().to_hex(), DESCRIPTOR_DIGEST.trim());
    assert_eq!(descriptor_second.digest(), descriptor_first.digest());
}

#[test]
fn an_unsealed_manifest_fails_validation() {
    let mut unsealed = full_manifest();
    unsealed.schema.digest = None;
    let error = unsealed.validate().expect_err("the digest is required");
    assert_eq!(error.code(), CODE_ARTIFACT_DRIFT);
    assert!(compile(&unsealed).is_err());
}

#[test]
fn a_search_index_requires_a_tokenizer_a_scorer_and_integer_parameters() {
    let mut manifest = full_manifest();
    let index = manifest
        .indexes
        .iter_mut()
        .find(|index| index.search)
        .expect("the fixture has a search index");
    index.scorer = None;
    index.scorer_parameters.clear();
    let error = manifest.validate().expect_err("the scorer is required");
    assert_eq!(error.code(), CODE_UNKNOWN_FIELD);

    let mut manifest = full_manifest();
    let index = manifest
        .indexes
        .iter_mut()
        .find(|index| index.search)
        .expect("the fixture has a search index");
    index.tokenizer = None;
    let error = manifest.validate().expect_err("the tokenizer is required");
    assert_eq!(error.code(), CODE_UNKNOWN_FIELD);

    let mut manifest = full_manifest();
    let index = manifest
        .indexes
        .iter_mut()
        .find(|index| index.search)
        .expect("the fixture has a search index");
    index.scorer_parameters.clear();
    let error = manifest
        .validate()
        .expect_err("the integer parameters are required");
    assert_eq!(error.code(), CODE_UNKNOWN_FIELD);
}

#[test]
fn a_projection_invalidation_cause_is_closed() {
    let text = FULL_MANIFEST.replace("\"fact-set\"", "\"every_policy_predicate_change\"");
    let error = Manifest::parse(&text).expect_err("the cause is not declared");
    assert_eq!(error.code(), CODE_UNKNOWN_FIELD);
}

#[test]
fn a_fact_owner_must_name_a_table_or_a_fact() {
    let mut manifest = full_manifest();
    manifest.facts[0].owner = "missing_owner".to_owned();
    let error = manifest.validate().expect_err("the owner is not declared");
    assert_eq!(error.code(), CODE_UNKNOWN_FIELD);
}

#[test]
fn every_named_error_fixture_matches_its_code() {
    let cases = [
        ("unknown-field", CODE_UNKNOWN_FIELD),
        ("unsupported-schema", CODE_UNSUPPORTED_SCHEMA),
        ("duplicate-key", CODE_DUPLICATE_NAME),
        ("duplicate-name", CODE_DUPLICATE_NAME),
        ("reserved-table-name", CODE_RESERVED_TABLE_NAME),
        ("missing-key-schema", CODE_KEY_SCHEMA_MISSING),
        ("float", CODE_FLOAT_NOT_ALLOWED),
        ("index-unknown-field", CODE_INDEX_UNKNOWN_FIELD),
        ("index-scope-unreachable", CODE_INDEX_SCOPE_UNREACHABLE),
        ("rule-unknown-path", CODE_RULE_UNKNOWN_PATH),
        ("rule-unbounded", CODE_RULE_UNBOUNDED),
        ("operation-unknown-field", CODE_OPERATION_UNKNOWN_FIELD),
        ("operation-cross-shard", CODE_OPERATION_CROSS_SHARD),
        ("artifact-drift", CODE_ARTIFACT_DRIFT),
        ("policy-catalog-missing", CODE_POLICY_CATALOG_MISSING),
    ];
    for (name, expected) in cases {
        let error = fixture_error(name);
        assert_eq!(
            error.code(),
            expected,
            "the fixture {name} is wrong: {error}"
        );
    }
}

#[test]
fn a_receipt_that_names_an_unknown_code_fails_validation() {
    let text = FULL_MANIFEST.replace("\"write.queue_timeout\"", "\"write.not_a_code\"");
    assert_ne!(text, FULL_MANIFEST, "the fixture has the known code");
    let manifest = Manifest::parse(&text).expect("the manifest parses");
    let error = manifest
        .validate()
        .expect_err("the unknown receipt code must fail");
    assert_eq!(error.code(), CODE_UNKNOWN_FIELD);
    assert!(
        error.to_string().contains("write.not_a_code"),
        "the error does not name the unknown code: {error}"
    );
}

#[test]
fn a_float_fails_before_schema_checks() {
    let error = Manifest::parse(fixture("float")).expect_err("the float is rejected");
    assert_eq!(error.code(), CODE_FLOAT_NOT_ALLOWED);
}

#[test]
fn a_duplicate_key_fails_before_schema_checks() {
    let error = Manifest::parse(fixture("duplicate-key")).expect_err("the duplicate is rejected");
    assert_eq!(error.code(), CODE_DUPLICATE_NAME);
}
