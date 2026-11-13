//! Tests for the consumer internal diagnostic.

use oxkv_consumer::ConsumerError;
use oxkv_contracts::InternalDiagnostic;

#[test]
fn a_consumer_state_error_names_the_dead_letter_path() {
    let error = ConsumerError::NotPending { key: vec![1] };
    assert_eq!(error.code(), "internal.invariant_violation");
    assert_eq!(
        error.diagnostic(),
        Some(InternalDiagnostic::ConsumerDeadLetter)
    );
    assert_eq!(
        error
            .diagnostic()
            .expect("the consumer error has a diagnostic")
            .diagnostic_name(),
        "consumer_dead_letter"
    );
}

#[test]
fn a_public_consumer_error_has_no_internal_diagnostic() {
    let error = ConsumerError::MalformedPayload { field: "effect" };
    assert_eq!(error.code(), "event.poison");
    assert_eq!(error.diagnostic(), None);
}
