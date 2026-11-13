//! Tests for the operator diagnostics behind `internal.invariant_violation`.

use oxkv_contracts::error::{ERROR_REGISTRY, ErrorKind, WireShape, lookup};
use oxkv_contracts::internal::InternalDiagnostic;

#[test]
fn every_diagnostic_keeps_the_public_code() {
    for diagnostic in InternalDiagnostic::ALL {
        assert_eq!(diagnostic.code(), "internal.invariant_violation");
        let spec = lookup(diagnostic.code()).expect("the public code is registered");
        assert_eq!(spec.kind, ErrorKind::Internal);
        assert_eq!(spec.wire_shape, WireShape::Code);
    }
}

#[test]
fn every_diagnostic_has_a_distinct_operator_name() {
    for (index, diagnostic) in InternalDiagnostic::ALL.iter().enumerate() {
        let name = diagnostic.diagnostic_name();
        assert_ne!(name, "");
        for other in &InternalDiagnostic::ALL[index + 1..] {
            assert_ne!(name, other.diagnostic_name());
        }
    }
}

#[test]
fn a_diagnostic_name_is_not_a_wire_code() {
    for diagnostic in InternalDiagnostic::ALL {
        assert!(
            lookup(diagnostic.diagnostic_name()).is_none(),
            "the diagnostic {} is a wire code",
            diagnostic.diagnostic_name()
        );
    }
}

#[test]
fn the_diagnostic_names_are_pinned() {
    let names: Vec<&str> = InternalDiagnostic::ALL
        .iter()
        .map(|diagnostic| diagnostic.diagnostic_name())
        .collect();
    assert_eq!(
        names,
        [
            "portable_codec",
            "rpc_descriptor",
            "consumer_dead_letter",
            "scheduler_transition",
            "hook_pipeline",
        ]
    );
}

#[test]
fn the_display_is_the_operator_name() {
    for diagnostic in InternalDiagnostic::ALL {
        assert_eq!(diagnostic.to_string(), diagnostic.diagnostic_name());
    }
}

#[test]
fn the_registry_keeps_one_internal_invariant_code() {
    let codes: Vec<&str> = ERROR_REGISTRY
        .iter()
        .filter(|spec| spec.code == "internal.invariant_violation")
        .map(|spec| spec.code)
        .collect();
    assert_eq!(codes, ["internal.invariant_violation"]);
}
