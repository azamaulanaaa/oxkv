//! Tests for the RPC descriptor internal diagnostic.

use oxkv_contracts::InternalDiagnostic;
use oxkv_rpc::DescriptorError;

#[test]
fn a_descriptor_error_names_the_rpc_descriptor() {
    let error = DescriptorError::NoMethods;
    assert_eq!(error.code(), "manifest.duplicate_name");
    assert_eq!(error.diagnostic(), InternalDiagnostic::RpcDescriptor);
    assert_eq!(error.diagnostic().diagnostic_name(), "rpc_descriptor");
}
