//! Tests for the portable internal diagnostic.

use oxkv_contracts::InternalDiagnostic;
use oxkv_portable::{CoreError, ParityError, StorageError};

#[test]
fn the_core_incomplete_error_names_the_portable_codec() {
    let error = CoreError::Incomplete { slot: 3 };
    assert_eq!(error.code(), "internal.invariant_violation");
    assert_eq!(error.diagnostic(), Some(InternalDiagnostic::PortableCodec));
    assert_eq!(
        error
            .diagnostic()
            .expect("the core error has a diagnostic")
            .diagnostic_name(),
        "portable_codec"
    );
}

#[test]
fn the_storage_batch_error_names_the_portable_codec() {
    let error = StorageError::BatchNotAtomic;
    assert_eq!(error.code(), "internal.invariant_violation");
    assert_eq!(error.diagnostic(), Some(InternalDiagnostic::PortableCodec));
}

#[test]
fn a_parity_error_names_the_portable_codec() {
    let error = ParityError::Bytes;
    assert_eq!(error.code(), "internal.invariant_violation");
    assert_eq!(error.diagnostic(), InternalDiagnostic::PortableCodec);
    assert_eq!(error.diagnostic().diagnostic_name(), "portable_codec");
}

#[test]
fn a_public_portable_error_has_no_internal_diagnostic() {
    assert_eq!(StorageError::Unavailable.diagnostic(), None);
    assert_eq!(StorageError::Corrupt.diagnostic(), None);
}
