//! Golden vector runner for the contract registry.
//!
//! Every vector file is append-only for one contract version. A later essay
//! adds a case to the matching file. A released case does not change.

mod support;

use oxkv_contracts::cursor::{CursorExpiry, CursorToken, CursorType};
use oxkv_contracts::error::{ERROR_REGISTRY, lookup};
use oxkv_contracts::keyspace::SYSTEM_NAMESPACES;
use oxkv_contracts::receipt::ReceiptState;
use oxkv_contracts::version::{ArtifactKind, VersionAxis};

fn assert_contract(file: &support::GoldenFile, expected: &str) {
    assert_eq!(file.contract, expected);
}

fn expected_str<'a>(case: &'a support::GoldenCase, field: &str) -> &'a str {
    case.expected[field]
        .as_str()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

fn expected_bool(case: &support::GoldenCase, field: &str) -> bool {
    case.expected[field]
        .as_bool()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

fn expected_u64(case: &support::GoldenCase, field: &str) -> u64 {
    case.expected[field]
        .as_u64()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

#[test]
fn error_registry_golden_vector_is_pinned() {
    let file = support::load(include_str!("vectors/error-registry.json"));
    assert_contract(&file, "receipt-error/v1");
    assert_eq!(file.cases.len(), ERROR_REGISTRY.len());
    for case in &file.cases {
        let spec = lookup(&case.name)
            .unwrap_or_else(|| panic!("the golden code {} is not registered", case.name));
        assert_eq!(expected_str(case, "kind"), spec.kind.name());
        assert_eq!(
            case.expected["receipt"].as_str(),
            spec.receipt.map(ReceiptState::name)
        );
        assert_eq!(expected_str(case, "retry"), spec.retry.name());
        assert_eq!(expected_bool(case, "terminal"), spec.terminal);
    }
}

#[test]
fn cursor_golden_vectors_are_pinned() {
    let file = support::load(include_str!("vectors/cursors.json"));
    assert_contract(&file, "cursor/v1");
    assert_eq!(file.cases.len(), CursorType::ALL.len());
    let tokens = support::cursor_tokens();
    for case in &file.cases {
        let cursor_type = CursorType::ALL
            .iter()
            .copied()
            .find(|cursor_type| cursor_type.name() == case.name)
            .unwrap_or_else(|| panic!("the golden cursor {} is unknown", case.name));
        let token = tokens
            .iter()
            .find(|(kind, _)| *kind == cursor_type)
            .map(|(_, token)| token)
            .expect("the cursor type has a sample token");
        assert_eq!(
            expected_str(case, "token_hex"),
            support::hex(token),
            "the token bytes changed for {}",
            case.name
        );
        assert_eq!(
            expected_str(case, "expiry"),
            sample_expiry(cursor_type).name()
        );
        assert_eq!(expected_str(case, "error"), cursor_type.expiry_error());
        support::decode_for(cursor_type, token).expect("the token decodes as its own type");
    }
}

fn sample_expiry(cursor_type: CursorType) -> CursorExpiry {
    match cursor_type {
        CursorType::Key => support::key_cursor().expiry_rule(),
        CursorType::Sync => support::sync_cursor().expiry_rule(),
        CursorType::Hydration => support::hydration_cursor().expiry_rule(),
        CursorType::Receipt => support::receipt_cursor().expiry_rule(),
        CursorType::Event => support::event_cursor().expiry_rule(),
        CursorType::Search => support::search_cursor().expiry_rule(),
        CursorType::Projection => support::projection_cursor().expiry_rule(),
        CursorType::Cost => support::cost_cursor().expiry_rule(),
    }
}

#[test]
fn a_cursor_golden_token_fails_in_another_type() {
    let tokens = support::cursor_tokens();
    for (source_type, token) in &tokens {
        for target_type in CursorType::ALL {
            if target_type == source_type {
                continue;
            }
            assert_eq!(
                support::decode_for(*target_type, token),
                Err(oxkv_contracts::CursorError::TypeMismatch {
                    expected: *target_type,
                    found: *source_type
                })
            );
        }
    }
}

#[test]
fn receipt_golden_vector_is_pinned() {
    let file = support::load(include_str!("vectors/receipts.json"));
    assert_contract(&file, "receipt-error/v1");
    assert_eq!(file.cases.len(), ReceiptState::ALL.len());
    for case in &file.cases {
        let state = ReceiptState::from_name(&case.name)
            .unwrap_or_else(|| panic!("the golden receipt state {} is unknown", case.name));
        assert_eq!(expected_u64(case, "wire_tag"), u64::from(state.wire_tag()));
        assert_eq!(expected_bool(case, "terminal"), state.is_terminal());
    }
}

#[test]
fn version_axis_golden_vector_is_pinned() {
    let file = support::load(include_str!("vectors/versions.json"));
    assert_contract(&file, "version-axes/v1");
    assert_eq!(file.cases.len(), VersionAxis::ALL.len());
    for case in &file.cases {
        let axis = VersionAxis::ALL
            .iter()
            .copied()
            .find(|axis| axis.name() == case.name)
            .unwrap_or_else(|| panic!("the golden version axis {} is unknown", case.name));
        assert_eq!(expected_str(case, "group"), axis.group().name());
    }
}

#[test]
fn keyspace_golden_vector_is_pinned() {
    let file = support::load(include_str!("vectors/keyspace.json"));
    assert_contract(&file, "key-encoding/v1");
    assert_eq!(file.cases.len(), SYSTEM_NAMESPACES.len());
    for case in &file.cases {
        let namespace = SYSTEM_NAMESPACES
            .iter()
            .find(|namespace| namespace.label == case.name)
            .unwrap_or_else(|| panic!("the golden system id {} is unknown", case.name));
        assert_eq!(expected_u64(case, "id"), u64::from(namespace.id));
        assert_eq!(
            expected_str(case, "commit_domain"),
            namespace.commit_domain.name()
        );
        assert_eq!(
            expected_bool(case, "scope_visible"),
            namespace.scope_visible
        );
    }
}

#[test]
fn provenance_golden_vector_is_pinned() {
    let file = support::load(include_str!("vectors/provenance.json"));
    assert_contract(&file, "snapshot-page/v1");
    assert_eq!(file.cases.len(), ArtifactKind::ALL.len());
    for case in &file.cases {
        let kind = ArtifactKind::ALL
            .iter()
            .copied()
            .find(|kind| kind.name() == case.name)
            .unwrap_or_else(|| panic!("the golden artifact {} is unknown", case.name));
        let expected = case.expected["required_fields"]
            .as_array()
            .expect("the case lists the required fields");
        let required: Vec<&str> = kind
            .required_fields()
            .iter()
            .map(|field| field.name())
            .collect();
        let actual: Vec<&str> = expected
            .iter()
            .map(|field| field.as_str().expect("a field name is text"))
            .collect();
        assert_eq!(required, actual);
    }
}

#[test]
fn every_golden_file_names_a_contract() {
    for text in [
        include_str!("vectors/error-registry.json"),
        include_str!("vectors/cursors.json"),
        include_str!("vectors/receipts.json"),
        include_str!("vectors/versions.json"),
        include_str!("vectors/keyspace.json"),
        include_str!("vectors/provenance.json"),
    ] {
        let file = support::load(text);
        assert_ne!(file.contract, "");
        assert_ne!(file.cases.len(), 0);
    }
}
