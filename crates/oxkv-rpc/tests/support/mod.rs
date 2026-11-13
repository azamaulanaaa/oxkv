#![allow(missing_docs)]

//! Shared support for the RPC golden vectors.

use std::collections::BTreeMap;

use oxkv_contracts::version::SyncWireVersion;
use oxkv_rpc::{
    CallHandler, ConnectCodec, DescriptorIdentity, GeneratedMethod, InProcessTransport,
    ProtocolCore, ProtocolError, SessionConfig, SessionHelloFacts,
};

/// A handler with one fixed response list per method path.
pub(crate) struct ScriptHandler {
    responses: BTreeMap<String, Vec<Vec<u8>>>,
    seen: Vec<(String, Vec<u8>)>,
}

impl ScriptHandler {
    pub(crate) fn new(responses: BTreeMap<String, Vec<Vec<u8>>>) -> Self {
        Self {
            responses,
            seen: Vec::new(),
        }
    }

    pub(crate) fn seen(&self) -> &[(String, Vec<u8>)] {
        &self.seen
    }
}

impl CallHandler for ScriptHandler {
    fn handle(
        &mut self,
        method: &GeneratedMethod,
        request: &[u8],
    ) -> Result<Vec<Vec<u8>>, ProtocolError> {
        self.seen.push((method.path().to_owned(), request.to_vec()));
        Ok(self
            .responses
            .get(method.path())
            .cloned()
            .unwrap_or_default())
    }
}

/// Builds the hello facts for one descriptor identity.
pub(crate) fn hello(identity: DescriptorIdentity, codec: ConnectCodec) -> SessionHelloFacts {
    SessionHelloFacts::new(
        identity.descriptor_digest(),
        identity.manifest_digest(),
        SyncWireVersion::new(1),
        codec,
    )
}

/// Builds the session configuration for one descriptor identity.
pub(crate) fn config(identity: DescriptorIdentity) -> SessionConfig {
    SessionConfig::new(
        identity,
        SyncWireVersion::new(1),
        vec![ConnectCodec::Json, ConnectCodec::Binary],
    )
}

/// Builds a negotiated reference transport around one handler.
pub(crate) fn negotiated_transport(handler: ScriptHandler) -> InProcessTransport<ScriptHandler> {
    let surface = oxkv_rpc::reference_surface().expect("the reference surface compiles");
    let identity = surface.identity();
    let mut transport =
        InProcessTransport::new(ProtocolCore::new(surface, config(identity), handler));
    transport
        .negotiate(&hello(identity, ConnectCodec::Json))
        .expect("the hello facts are compatible");
    transport
}
