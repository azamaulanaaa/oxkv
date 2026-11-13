//! The transport seam.
//!
//! The in-process transport is the reference transport. Every other transport
//! must produce the same message sequence for the same input transcript. A
//! transport may change framing and lifecycle. It must not change ordering,
//! retry, or error meaning.

use crate::connect::{ConnectEnvelope, FrameError, decode_frame, encode_frame};
use crate::error::{ProtocolError, TransportError, TransportFailure};
use crate::protocol::ProtocolCore;
use crate::session::{NegotiatedSession, SessionHelloFacts};

/// A sans-IO transport for one call.
pub trait Transport {
    /// Sends one call envelope and returns the response frames.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] when the transport fails. A protocol error
    /// becomes an error end frame, not a transport error.
    fn round_trip(
        &mut self,
        request: &ConnectEnvelope,
    ) -> Result<Vec<ConnectEnvelope>, TransportError>;
}

/// The reference in-process transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InProcessTransport<H> {
    core: ProtocolCore<H>,
}

impl<H> InProcessTransport<H> {
    /// Creates an in-process transport.
    #[must_use]
    pub const fn new(core: ProtocolCore<H>) -> Self {
        Self { core }
    }

    /// Returns the protocol core.
    #[must_use]
    pub const fn core(&self) -> &ProtocolCore<H> {
        &self.core
    }

    /// Returns the mutable protocol core.
    #[must_use]
    pub const fn core_mut(&mut self) -> &mut ProtocolCore<H> {
        &mut self.core
    }

    /// Negotiates the session on the core.
    ///
    /// # Errors
    ///
    /// Returns the error from [`ProtocolCore::negotiate`].
    pub fn negotiate(
        &mut self,
        facts: &SessionHelloFacts,
    ) -> Result<NegotiatedSession, ProtocolError> {
        self.core.negotiate(facts)
    }
}

impl<H: crate::protocol::CallHandler> Transport for InProcessTransport<H> {
    fn round_trip(
        &mut self,
        request: &ConnectEnvelope,
    ) -> Result<Vec<ConnectEnvelope>, TransportError> {
        let (method, payload) = match request.body() {
            crate::connect::ConnectBody::Call { method, payload } => {
                (method.as_str(), payload.as_slice())
            }
            crate::connect::ConnectBody::Message { .. }
            | crate::connect::ConnectBody::End { .. } => {
                return Err(TransportError::with_detail(
                    TransportFailure::Protocol,
                    "The client sent a frame that is not a call.",
                ));
            }
        };
        let stream_id = request.stream_id();
        let mut frames = Vec::new();
        let mut sequence = 0_u64;
        match self.core.call(method, payload) {
            Ok(outcome) => {
                for message in outcome.into_messages() {
                    frames.push(ConnectEnvelope::message(sequence, stream_id, message));
                    sequence = sequence.saturating_add(1);
                }
                frames.push(ConnectEnvelope::end_success(sequence, stream_id));
            }
            Err(error) => {
                frames.push(ConnectEnvelope::end_error(
                    sequence,
                    stream_id,
                    error.code(),
                ));
            }
        }
        Ok(frames)
    }
}

/// A transport that encodes and decodes each frame.
///
/// The wrapper changes framing only. The decoded message sequence is equal to
/// the reference sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramedTransport<T> {
    inner: T,
}

impl<T> FramedTransport<T> {
    /// Creates a framed transport around another transport.
    #[must_use]
    pub const fn new(inner: T) -> Self {
        Self { inner }
    }

    /// Returns the inner transport.
    #[must_use]
    pub const fn inner(&self) -> &T {
        &self.inner
    }

    /// Returns the mutable inner transport.
    #[must_use]
    pub const fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T: Transport> Transport for FramedTransport<T> {
    fn round_trip(
        &mut self,
        request: &ConnectEnvelope,
    ) -> Result<Vec<ConnectEnvelope>, TransportError> {
        let decoded = decode_frame(&encode_frame(request)).map_err(|error| {
            TransportError::with_detail(FrameError::transport_failure(), error.to_string())
        })?;
        let response = self.inner.round_trip(&decoded)?;
        let mut frames = Vec::with_capacity(response.len());
        for frame in response {
            let decoded = decode_frame(&encode_frame(&frame)).map_err(|error| {
                TransportError::with_detail(FrameError::transport_failure(), error.to_string())
            })?;
            frames.push(decoded);
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect::ConnectCodec;
    use crate::generator::{GeneratedMethod, reference_surface};
    use crate::protocol::{CallHandler, ProtocolCore};
    use crate::session::SessionConfig;
    use oxkv_contracts::version::SyncWireVersion;

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct EchoHandler {
        seen: Vec<(String, Vec<u8>)>,
    }

    impl CallHandler for EchoHandler {
        fn handle(
            &mut self,
            method: &GeneratedMethod,
            request: &[u8],
        ) -> Result<Vec<Vec<u8>>, ProtocolError> {
            self.seen.push((method.path().to_owned(), request.to_vec()));
            if method.streaming() == crate::descriptor::StreamingShape::Unary {
                Ok(vec![request.to_vec()])
            } else {
                Ok(vec![request.to_vec(), request.to_vec()])
            }
        }
    }

    fn transport() -> InProcessTransport<EchoHandler> {
        let surface = reference_surface().expect("the surface compiles");
        let config = SessionConfig::new(
            surface.identity(),
            SyncWireVersion::new(1),
            vec![ConnectCodec::Json],
        );
        let mut transport =
            InProcessTransport::new(ProtocolCore::new(surface, config, EchoHandler::default()));
        let identity = transport.core().surface().identity();
        transport
            .negotiate(&SessionHelloFacts::new(
                identity.descriptor_digest(),
                identity.manifest_digest(),
                SyncWireVersion::new(1),
                ConnectCodec::Json,
            ))
            .expect("the facts are compatible");
        transport
    }

    #[test]
    fn the_reference_transport_preserves_a_policy_payload() {
        let policy = vec![0, 0xff, b'p', b'o', b'l', 0, 1, 2, 3];
        let mut transport = transport();
        let call = ConnectEnvelope::call(0, 7, "sync/subscribe", policy.clone());
        let frames = transport.round_trip(&call).expect("the call succeeds");
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].payload(), Some(policy.as_slice()));
        assert_eq!(frames[1].payload(), Some(policy.as_slice()));
        assert!(frames[2].is_end());
        assert!(frames[2].status().expect("an end frame").is_success());
        assert_eq!(transport.core().handler().seen[0].1, policy);
    }

    #[test]
    fn a_framed_transport_has_the_same_sequence() {
        let policy = vec![0, 0xff, 1, 2, 3];
        let call = ConnectEnvelope::call(0, 7, "sync/subscribe", policy);
        let reference = transport()
            .round_trip(&call)
            .expect("the reference call succeeds");
        let mut framed = FramedTransport::new(transport());
        let actual = framed.round_trip(&call).expect("the framed call succeeds");
        assert_eq!(actual, reference);
    }

    #[test]
    fn an_unnegotiated_call_ends_with_the_named_error() {
        let surface = reference_surface().expect("the surface compiles");
        let config = SessionConfig::new(
            surface.identity(),
            SyncWireVersion::new(1),
            vec![ConnectCodec::Json],
        );
        let mut transport =
            InProcessTransport::new(ProtocolCore::new(surface, config, EchoHandler::default()));
        let frames = transport
            .round_trip(&ConnectEnvelope::call(0, 1, "data/get", Vec::new()))
            .expect("the transport succeeds");
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0]
                .status()
                .expect("an end frame")
                .code()
                .map(oxkv_contracts::ErrorCode::as_str),
            Some("sync.negotiation_required")
        );
    }

    #[test]
    fn an_unknown_method_ends_with_the_named_error() {
        let mut transport = transport();
        let frames = transport
            .round_trip(&ConnectEnvelope::call(0, 1, "data/unknown", Vec::new()))
            .expect("the transport succeeds");
        assert_eq!(
            frames[0]
                .status()
                .expect("an end frame")
                .code()
                .map(oxkv_contracts::ErrorCode::as_str),
            Some("rpc.unknown_method")
        );
    }

    #[test]
    fn the_reference_transport_rejects_a_non_call_frame() {
        let mut transport = transport();
        let error = transport
            .round_trip(&ConnectEnvelope::message(0, 1, Vec::new()))
            .unwrap_err();
        assert_eq!(error.failure(), TransportFailure::Protocol);
        assert_eq!(
            error.into_protocol_error().code().as_str(),
            "sync.protocol_error"
        );
    }
}
