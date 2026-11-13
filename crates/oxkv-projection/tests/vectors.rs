//! Golden vectors for the projection contract.

use oxkv_contracts::version::{
    FactSourceId, FactVersion, PolicyVersion, ProjectionEpoch, ProjectionWatermarks, ShardId,
};
use oxkv_projection::{
    CanonicalEncode, CanonicalType, FactChange, FactDeclaration, FactId, FactOwner, FactSet,
    FactValue, FactVersionDistance, ProjectionDiff, ProjectionSnapshot, StalenessReport,
    StoredFact, hex,
};
use serde_json::Value;

/// One golden file.
struct GoldenFile {
    contract: String,
    cases: Vec<GoldenCase>,
}

/// One golden case.
struct GoldenCase {
    name: String,
    expected: Value,
}

/// Loads one golden file.
fn load(text: &str) -> GoldenFile {
    let value: Value = serde_json::from_str(text).expect("the golden file is valid JSON");
    let contract = value["contract"]
        .as_str()
        .expect("the golden file names a contract")
        .to_owned();
    let cases = value["cases"]
        .as_array()
        .expect("the golden file has a case list")
        .iter()
        .map(|case| GoldenCase {
            name: case["name"]
                .as_str()
                .expect("the case names a vector")
                .to_owned(),
            expected: case["expected"].clone(),
        })
        .collect();
    GoldenFile { contract, cases }
}

fn case<'a>(file: &'a GoldenFile, name: &str) -> &'a GoldenCase {
    file.cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("the golden case {name} is absent"))
}

fn text<'a>(case: &'a GoldenCase, field: &str) -> &'a str {
    case.expected[field]
        .as_str()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

fn u64_value(case: &GoldenCase, field: &str) -> u64 {
    case.expected[field]
        .as_u64()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

fn boolean(case: &GoldenCase, field: &str) -> bool {
    case.expected[field]
        .as_bool()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

fn declaration() -> FactDeclaration {
    FactDeclaration::new(
        FactId::new("account_owner"),
        CanonicalType::Text,
        FactOwner::Shard(ShardId::new(vec![2])),
        4,
        FactVersionDistance::new(2),
    )
}

fn fact_set() -> FactSet {
    let tier = FactDeclaration::new(
        FactId::new("account_tier"),
        CanonicalType::U64,
        FactOwner::Source(FactSourceId::new("catalog")),
        8,
        FactVersionDistance::new(0),
    );
    FactSet::from_declarations([declaration(), tier]).expect("the fact set is valid")
}

fn source() -> FactSourceId {
    declaration().source_id()
}

fn snapshot() -> ProjectionSnapshot {
    let mut watermarks = ProjectionWatermarks::new();
    watermarks.set(source(), FactVersion::new(12));
    ProjectionSnapshot::new(
        ProjectionEpoch::new(7),
        PolicyVersion::from_bytes([3; 32]),
        fact_set().digest(),
        watermarks,
        vec![StoredFact::new(
            FactId::new("account_owner"),
            source(),
            FactVersion::new(12),
            FactValue::Text("alice".to_owned()),
        )],
    )
}

fn diff() -> ProjectionDiff {
    ProjectionDiff::new(
        ProjectionEpoch::new(7),
        3,
        source(),
        FactVersion::new(13),
        vec![FactChange::upsert(
            FactId::new("account_owner"),
            FactVersion::new(13),
            FactValue::Text("bob".to_owned()),
        )],
    )
}

#[test]
fn projection_golden_vectors_are_pinned() {
    let file = load(include_str!("vectors/projection.json"));
    assert_eq!(file.contract, "projection/v1");

    let declaration_case = case(&file, "fact_declaration_text");
    assert_eq!(
        text(declaration_case, "canonical_hex"),
        hex(&declaration().canonical_bytes())
    );

    let fact_set_case = case(&file, "fact_set_two");
    assert_eq!(
        text(fact_set_case, "digest_hex"),
        fact_set().digest().to_hex()
    );

    let snapshot_case = case(&file, "snapshot_seven");
    assert_eq!(
        text(snapshot_case, "canonical_hex"),
        hex(&snapshot().canonical_bytes())
    );
    assert_eq!(
        text(snapshot_case, "checksum_hex"),
        snapshot().checksum().to_hex()
    );

    let diff_case = case(&file, "diff_upsert");
    assert_eq!(
        text(diff_case, "canonical_hex"),
        hex(&diff().canonical_bytes())
    );
    assert_eq!(text(diff_case, "checksum_hex"), diff().checksum().to_hex());
}

#[test]
fn watermark_comparison_golden_vectors_are_pinned() {
    let file = load(include_str!("vectors/projection.json"));
    let watermark = FactVersion::new(12);

    let same = case(&file, "watermark_same");
    let distance = FactVersionDistance::between(watermark, FactVersion::new(12))
        .expect("the same version has a distance");
    assert_eq!(text(same, "outcome"), "accept");
    assert_eq!(u64_value(same, "distance"), distance.get());

    let ahead = case(&file, "watermark_ahead");
    let distance = FactVersionDistance::between(watermark, FactVersion::new(13))
        .expect("the newer version has a distance");
    assert_eq!(text(ahead, "outcome"), "accept");
    assert_eq!(u64_value(ahead, "distance"), distance.get());

    let behind = case(&file, "watermark_behind");
    assert_eq!(text(behind, "outcome"), "regression");
    assert_eq!(
        FactVersionDistance::between(watermark, FactVersion::new(11)),
        None
    );
}

#[test]
fn staleness_golden_vectors_are_pinned() {
    let file = load(include_str!("vectors/projection.json"));
    let bound = FactVersionDistance::new(2);

    let within = case(&file, "staleness_within");
    let report = StalenessReport::new(
        source(),
        FactVersion::new(10),
        Some(FactVersion::new(12)),
        bound,
    );
    assert_eq!(boolean(within, "within_bound"), report.within_bound());
    assert_eq!(
        u64_value(within, "distance"),
        report.distance().expect("the distance is known").get()
    );

    let over = case(&file, "staleness_over");
    let report = StalenessReport::new(
        source(),
        FactVersion::new(10),
        Some(FactVersion::new(13)),
        bound,
    );
    assert_eq!(boolean(over, "within_bound"), report.within_bound());
    assert_eq!(
        u64_value(over, "distance"),
        report.distance().expect("the distance is known").get()
    );

    let unknown = case(&file, "staleness_unknown");
    let report = StalenessReport::new(source(), FactVersion::new(10), None, bound);
    assert_eq!(boolean(unknown, "within_bound"), report.within_bound());
    assert_eq!(report.distance(), None);
}

#[test]
fn every_golden_case_is_named() {
    let file = load(include_str!("vectors/projection.json"));
    assert!(!file.cases.is_empty());
    for case in &file.cases {
        assert_ne!(case.name, "");
        assert!(!case.expected.is_null());
    }
}
