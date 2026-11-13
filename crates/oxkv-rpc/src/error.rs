//! Named protocol errors and the transport error mapping.
//!
//! Every protocol error maps to one code in the canonical error registry.
//! The RPC layer adds three codes: `rpc.unknown_method`,
//! `rpc.descriptor_mismatch`, and `rpc.codec_unsupported`. The sync errors pass
//! through without a change of meaning.

use std::fmt;

use oxkv_contracts::version::{DescriptorDigest, ManifestDigest, SyncWireVersion};
use oxkv_contracts::{ErrorCode, ErrorSpec, RetryClass};

fn registered(name: &'static str) -> ErrorCode {
    ErrorCode::parse(name).expect("The protocol error code is registered.")
}

/// One named protocol error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    /// The method path is not in the generated surface.
    #[error("The method is unknown: {method}.")]
    UnknownMethod {
        /// The unknown method path.
        method: String,
    },
    /// The client and server descriptor digests differ.
    #[error("The descriptor digest does not match.")]
    DescriptorMismatch {
        /// The server descriptor digest.
        expected: DescriptorDigest,
        /// The client descriptor digest.
        found: DescriptorDigest,
    },
    /// The codec is not supported.
    #[error("The codec is not supported: {codec}.")]
    CodecUnsupported {
        /// The requested codec name.
        codec: String,
    },
    /// A data call arrived before `SessionHello`.
    #[error("The session did not negotiate.")]
    NegotiationRequired,
    /// The manifest digest does not match.
    #[error("The manifest digest does not match.")]
    IncompatibleManifest {
        /// The server manifest digest.
        expected: ManifestDigest,
        /// The client manifest digest.
        found: ManifestDigest,
    },
    /// The sync wire version is not supported.
    #[error("The sync wire version is not supported: {found}.")]
    UnsupportedSyncVersion {
        /// The server sync wire version.
        expected: SyncWireVersion,
        /// The client sync wire version.
        found: SyncWireVersion,
    },
    /// The session already negotiated.
    #[error("The session already negotiated.")]
    AlreadyNegotiated,
    /// A transport failure.
    #[error("The transport failed.")]
    Transport {
        /// The transport failure class.
        failure: TransportFailure,
    },
    /// A handler returned a named error.
    #[error("The handler returned the error code {code}.")]
    Handler {
        /// The named error code.
        code: ErrorCode,
        /// A non-contractual diagnostic detail.
        detail: String,
    },
    /// A handler broke the streaming shape of one method.
    #[error("The handler broke the method contract: {detail}.")]
    HandlerViolation {
        /// A non-contractual diagnostic detail.
        detail: String,
    },
    /// The envelope is not valid for the receiver.
    #[error("The envelope is not valid for this direction.")]
    InvalidEnvelope,
    /// A remote error code is not in the local registry.
    #[error("The remote error code is not registered: {raw}.")]
    UnknownRemoteCode {
        /// The raw remote code. The value is preserved without change.
        raw: String,
    },
}

impl ProtocolError {
    /// Creates a handler error from one registered code.
    #[must_use]
    pub fn handler(code: ErrorCode) -> Self {
        Self::Handler {
            code,
            detail: String::new(),
        }
    }

    /// Creates a handler error with a diagnostic detail.
    #[must_use]
    pub fn handler_with_detail(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self::Handler {
            code,
            detail: detail.into(),
        }
    }

    /// Creates a transport protocol error.
    #[must_use]
    pub const fn transport(failure: TransportFailure) -> Self {
        Self::Transport { failure }
    }

    /// Creates a client error from a raw remote code.
    ///
    /// A known code maps to [`ProtocolError::Handler`]. An unknown code maps to
    /// [`ProtocolError::UnknownRemoteCode`] and keeps the raw text.
    #[must_use]
    pub fn from_wire_code(raw: &str) -> Self {
        ErrorCode::parse(raw).map_or_else(
            || Self::UnknownRemoteCode {
                raw: raw.to_owned(),
            },
            Self::handler,
        )
    }

    /// Returns the canonical error code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::UnknownMethod { .. } => registered("rpc.unknown_method"),
            Self::DescriptorMismatch { .. } => registered("rpc.descriptor_mismatch"),
            Self::CodecUnsupported { .. } => registered("rpc.codec_unsupported"),
            Self::NegotiationRequired => registered("sync.negotiation_required"),
            Self::IncompatibleManifest { .. } => registered("sync.incompatible_manifest"),
            Self::UnsupportedSyncVersion { .. } => registered("sync.unsupported_version"),
            Self::AlreadyNegotiated
            | Self::HandlerViolation { .. }
            | Self::InvalidEnvelope
            | Self::UnknownRemoteCode { .. } => registered("internal.invariant_violation"),
            Self::Transport { failure } => failure.code(),
            Self::Handler { code, .. } => *code,
        }
    }

    /// Returns the canonical error registry row.
    #[must_use]
    pub fn spec(&self) -> &'static ErrorSpec {
        self.code().spec()
    }

    /// Returns the retry class.
    #[must_use]
    pub fn retry(&self) -> RetryClass {
        self.code().retry()
    }

    /// Returns true when the error ends the call.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.code().is_terminal()
    }
}

/// The failure class of one transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportFailure {
    /// The connection closed before the stream finished.
    Closed,
    /// A frame is corrupt.
    CorruptFrame,
    /// The peer broke the transport protocol.
    Protocol,
    /// The consumer did not keep up.
    Backpressure,
}

impl TransportFailure {
    /// Returns the failure name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::CorruptFrame => "corrupt_frame",
            Self::Protocol => "protocol",
            Self::Backpressure => "backpressure",
        }
    }

    /// Returns the named protocol error code for this failure.
    #[must_use]
    pub fn code(self) -> ErrorCode {
        match self {
            Self::Closed => registered("sync.shard_unavailable"),
            Self::CorruptFrame | Self::Protocol => registered("sync.unsupported_version"),
            Self::Backpressure => registered("consumer.too_slow"),
        }
    }
}

impl fmt::Display for TransportFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A transport-level error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("The transport failed ({failure}). {detail}")]
pub struct TransportError {
    failure: TransportFailure,
    detail: String,
}

impl TransportError {
    /// Creates a transport error.
    #[must_use]
    pub const fn new(failure: TransportFailure) -> Self {
        Self {
            failure,
            detail: String::new(),
        }
    }

    /// Adds a non-contractual diagnostic detail.
    #[must_use]
    pub fn with_detail(failure: TransportFailure, detail: impl Into<String>) -> Self {
        Self {
            failure,
            detail: detail.into(),
        }
    }

    /// Returns the failure class.
    #[must_use]
    pub const fn failure(&self) -> TransportFailure {
        self.failure
    }

    /// Returns the diagnostic detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Returns the named protocol error code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.failure.code()
    }

    /// Maps the transport error to a named protocol error.
    #[must_use]
    pub fn into_protocol_error(self) -> ProtocolError {
        ProtocolError::transport(self.failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_protocol_error_maps_to_a_registered_code() {
        let errors = [
            ProtocolError::UnknownMethod {
                method: "data/missing".to_owned(),
            },
            ProtocolError::DescriptorMismatch {
                expected: DescriptorDigest::from_bytes([1; 32]),
                found: DescriptorDigest::from_bytes([2; 32]),
            },
            ProtocolError::CodecUnsupported {
                codec: "cbor".to_owned(),
            },
            ProtocolError::NegotiationRequired,
            ProtocolError::IncompatibleManifest {
                expected: ManifestDigest::from_bytes([1; 32]),
                found: ManifestDigest::from_bytes([2; 32]),
            },
            ProtocolError::UnsupportedSyncVersion {
                expected: SyncWireVersion::new(1),
                found: SyncWireVersion::new(9),
            },
            ProtocolError::AlreadyNegotiated,
            ProtocolError::HandlerViolation {
                detail: "the unary method returned two messages".to_owned(),
            },
            ProtocolError::InvalidEnvelope,
            ProtocolError::UnknownRemoteCode {
                raw: "rpc.from_the_future".to_owned(),
            },
        ];
        for error in errors {
            assert!(
                ErrorCode::parse(error.code().as_str()).is_some(),
                "the protocol error {} is not registered",
                error.code().as_str()
            );
        }
    }

    #[test]
    fn every_transport_failure_maps_to_a_named_protocol_error() {
        for failure in [
            TransportFailure::Closed,
            TransportFailure::CorruptFrame,
            TransportFailure::Protocol,
            TransportFailure::Backpressure,
        ] {
            let error = TransportError::new(failure);
            assert_eq!(error.code().as_str(), failure.code().as_str());
            assert_eq!(
                error.into_protocol_error(),
                ProtocolError::Transport { failure }
            );
        }
    }

    #[test]
    fn a_known_remote_code_maps_to_a_handler_error() {
        let error = ProtocolError::from_wire_code("projection.lagging");
        assert_eq!(error.code().as_str(), "projection.lagging");
        assert!(!error.is_terminal());
    }

    #[test]
    fn an_unknown_remote_code_preserves_the_raw_text() {
        let error = ProtocolError::from_wire_code("rpc.from_the_future");
        assert_eq!(
            error,
            ProtocolError::UnknownRemoteCode {
                raw: "rpc.from_the_future".to_owned()
            }
        );
        assert_eq!(error.code().as_str(), "internal.invariant_violation");
    }
}
