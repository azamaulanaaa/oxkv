//! Descriptor identity and the `SessionHello` compatibility check.
//!
//! The sync wire owns `SessionHello`. This module exposes only the facts that
//! the RPC layer checks: the descriptor digest, the manifest digest, the sync
//! wire version, and the codec. A session negotiates one time.
//!
//! A data call before negotiation returns `sync.negotiation_required`. A
//! descriptor digest mismatch returns `rpc.descriptor_mismatch`.

use oxkv_contracts::version::{
    DescriptorDigest, ManifestDigest, MapDigest, MapVersion, SyncWireVersion,
};

use crate::connect::ConnectCodec;
use crate::descriptor::DescriptorIdentity;
use crate::error::ProtocolError;

/// The `SessionHello` facts that the RPC layer checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionHelloFacts {
    descriptor_digest: DescriptorDigest,
    manifest_digest: ManifestDigest,
    map_version: MapVersion,
    map_digest: MapDigest,
    sync_wire_version: SyncWireVersion,
    codec: ConnectCodec,
}

impl SessionHelloFacts {
    /// Creates the compatibility facts.
    #[must_use]
    pub const fn new(
        descriptor_digest: DescriptorDigest,
        manifest_digest: ManifestDigest,
        sync_wire_version: SyncWireVersion,
        codec: ConnectCodec,
    ) -> Self {
        Self {
            descriptor_digest,
            manifest_digest,
            map_version: MapVersion::new(0),
            map_digest: MapDigest::from_bytes([0; 32]),
            sync_wire_version,
            codec,
        }
    }

    /// Sets the client shard map version and digest.
    #[must_use]
    pub const fn with_map(mut self, map_version: MapVersion, map_digest: MapDigest) -> Self {
        self.map_version = map_version;
        self.map_digest = map_digest;
        self
    }

    /// Returns the client shard map version.
    #[must_use]
    pub const fn map_version(self) -> MapVersion {
        self.map_version
    }

    /// Returns the client shard map digest.
    #[must_use]
    pub const fn map_digest(self) -> MapDigest {
        self.map_digest
    }

    /// Returns the client descriptor digest.
    #[must_use]
    pub const fn descriptor_digest(self) -> DescriptorDigest {
        self.descriptor_digest
    }

    /// Returns the client manifest digest.
    #[must_use]
    pub const fn manifest_digest(self) -> ManifestDigest {
        self.manifest_digest
    }

    /// Returns the client sync wire version.
    #[must_use]
    pub const fn sync_wire_version(self) -> SyncWireVersion {
        self.sync_wire_version
    }

    /// Returns the client codec.
    #[must_use]
    pub const fn codec(self) -> ConnectCodec {
        self.codec
    }
}

/// The server session configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    identity: DescriptorIdentity,
    map_version: MapVersion,
    map_digest: MapDigest,
    sync_wire_version: SyncWireVersion,
    codecs: Vec<ConnectCodec>,
}

impl SessionConfig {
    /// Creates a session configuration.
    #[must_use]
    pub fn new(
        identity: DescriptorIdentity,
        sync_wire_version: SyncWireVersion,
        codecs: Vec<ConnectCodec>,
    ) -> Self {
        Self {
            identity,
            map_version: MapVersion::new(0),
            map_digest: MapDigest::from_bytes([0; 32]),
            sync_wire_version,
            codecs,
        }
    }

    /// Sets the server shard map version and digest.
    #[must_use]
    pub const fn with_map(mut self, map_version: MapVersion, map_digest: MapDigest) -> Self {
        self.map_version = map_version;
        self.map_digest = map_digest;
        self
    }

    /// Returns the server shard map version.
    #[must_use]
    pub const fn map_version(&self) -> MapVersion {
        self.map_version
    }

    /// Returns the server shard map digest.
    #[must_use]
    pub const fn map_digest(&self) -> MapDigest {
        self.map_digest
    }

    /// Returns the descriptor identity.
    #[must_use]
    pub const fn identity(&self) -> DescriptorIdentity {
        self.identity
    }

    /// Returns the sync wire version.
    #[must_use]
    pub const fn sync_wire_version(&self) -> SyncWireVersion {
        self.sync_wire_version
    }

    /// Returns the supported codecs.
    #[must_use]
    pub fn codecs(&self) -> &[ConnectCodec] {
        &self.codecs
    }
}

/// The result of one successful negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NegotiatedSession {
    identity: DescriptorIdentity,
    sync_wire_version: SyncWireVersion,
    codec: ConnectCodec,
}

impl NegotiatedSession {
    /// Returns the descriptor identity.
    #[must_use]
    pub const fn identity(self) -> DescriptorIdentity {
        self.identity
    }

    /// Returns the sync wire version.
    #[must_use]
    pub const fn sync_wire_version(self) -> SyncWireVersion {
        self.sync_wire_version
    }

    /// Returns the negotiated codec.
    #[must_use]
    pub const fn codec(self) -> ConnectCodec {
        self.codec
    }
}

/// One RPC session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    config: SessionConfig,
    negotiated: Option<NegotiatedSession>,
}

impl Session {
    /// Creates an unnegotiated session.
    #[must_use]
    pub const fn new(config: SessionConfig) -> Self {
        Self {
            config,
            negotiated: None,
        }
    }

    /// Returns the server configuration.
    #[must_use]
    pub const fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Returns true after a successful negotiation.
    #[must_use]
    pub const fn is_negotiated(&self) -> bool {
        self.negotiated.is_some()
    }

    /// Returns the negotiated session.
    #[must_use]
    pub const fn negotiated(&self) -> Option<NegotiatedSession> {
        self.negotiated
    }

    /// Checks the `SessionHello` facts without a side effect.
    ///
    /// # Errors
    ///
    /// Returns `rpc.descriptor_mismatch` when the descriptor digest differs.
    /// Returns `sync.incompatible_manifest` when the manifest digest differs.
    /// Returns `sync.unsupported_version` when the sync version differs.
    /// Returns `rpc.codec_unsupported` when the codec is not advertised.
    pub fn check_compatibility(
        &self,
        facts: &SessionHelloFacts,
    ) -> Result<NegotiatedSession, ProtocolError> {
        if facts.descriptor_digest != self.config.identity.descriptor_digest() {
            return Err(ProtocolError::DescriptorMismatch {
                expected: self.config.identity.descriptor_digest(),
                found: facts.descriptor_digest,
            });
        }
        if facts.manifest_digest != self.config.identity.manifest_digest() {
            return Err(ProtocolError::IncompatibleManifest {
                expected: self.config.identity.manifest_digest(),
                found: facts.manifest_digest,
            });
        }
        if facts.map_version != self.config.map_version
            || facts.map_digest != self.config.map_digest
        {
            return Err(ProtocolError::MapMismatch {
                expected_version: self.config.map_version,
                found_version: facts.map_version,
                expected_digest: self.config.map_digest,
                found_digest: facts.map_digest,
            });
        }
        if facts.sync_wire_version != self.config.sync_wire_version {
            return Err(ProtocolError::UnsupportedSyncVersion {
                expected: self.config.sync_wire_version,
                found: facts.sync_wire_version,
            });
        }
        if !self.config.codecs.contains(&facts.codec) {
            return Err(ProtocolError::CodecUnsupported {
                codec: facts.codec.name().to_owned(),
            });
        }
        Ok(NegotiatedSession {
            identity: self.config.identity,
            sync_wire_version: self.config.sync_wire_version,
            codec: facts.codec,
        })
    }

    /// Negotiates one session.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::AlreadyNegotiated`] when the session already
    /// negotiated. Returns the error from [`Session::check_compatibility`] when
    /// the facts are not compatible.
    pub fn negotiate(
        &mut self,
        facts: &SessionHelloFacts,
    ) -> Result<NegotiatedSession, ProtocolError> {
        if self.negotiated.is_some() {
            return Err(ProtocolError::AlreadyNegotiated);
        }
        let negotiated = self.check_compatibility(facts)?;
        self.negotiated = Some(negotiated);
        Ok(negotiated)
    }

    /// Returns the negotiated session or `sync.negotiation_required`.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::NegotiationRequired`] before a successful
    /// negotiation.
    pub fn require_negotiated(&self) -> Result<NegotiatedSession, ProtocolError> {
        self.negotiated.ok_or(ProtocolError::NegotiationRequired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::version::DescriptorVersion;

    fn identity() -> DescriptorIdentity {
        DescriptorIdentity::new(
            DescriptorVersion::new(1),
            DescriptorDigest::from_bytes([1; 32]),
            ManifestDigest::from_bytes([2; 32]),
        )
    }

    fn facts() -> SessionHelloFacts {
        SessionHelloFacts::new(
            DescriptorDigest::from_bytes([1; 32]),
            ManifestDigest::from_bytes([2; 32]),
            SyncWireVersion::new(1),
            ConnectCodec::Json,
        )
    }

    fn session() -> Session {
        Session::new(SessionConfig::new(
            identity(),
            SyncWireVersion::new(1),
            vec![ConnectCodec::Json],
        ))
    }

    #[test]
    fn a_matching_hello_negotiates_one_time() {
        let mut session = session();
        let negotiated = session
            .negotiate(&facts())
            .expect("the facts are compatible");
        assert_eq!(negotiated.codec(), ConnectCodec::Json);
        assert!(session.is_negotiated());
        assert_eq!(
            session.negotiate(&facts()),
            Err(ProtocolError::AlreadyNegotiated)
        );
    }

    #[test]
    fn a_data_call_before_hello_requires_negotiation() {
        let session = session();
        assert_eq!(
            session.require_negotiated(),
            Err(ProtocolError::NegotiationRequired)
        );
        assert_eq!(
            session.require_negotiated().unwrap_err().code().as_str(),
            "sync.negotiation_required"
        );
    }

    #[test]
    fn a_descriptor_mismatch_has_the_named_error() {
        let session = session();
        let wrong = SessionHelloFacts::new(
            DescriptorDigest::from_bytes([9; 32]),
            ManifestDigest::from_bytes([2; 32]),
            SyncWireVersion::new(1),
            ConnectCodec::Json,
        );
        let error = session.check_compatibility(&wrong).unwrap_err();
        assert_eq!(error.code().as_str(), "rpc.descriptor_mismatch");
    }

    #[test]
    fn a_manifest_mismatch_has_the_named_error() {
        let session = session();
        let wrong = SessionHelloFacts::new(
            DescriptorDigest::from_bytes([1; 32]),
            ManifestDigest::from_bytes([9; 32]),
            SyncWireVersion::new(1),
            ConnectCodec::Json,
        );
        assert_eq!(
            session
                .check_compatibility(&wrong)
                .unwrap_err()
                .code()
                .as_str(),
            "sync.incompatible_manifest"
        );
    }

    #[test]
    fn a_map_digest_mismatch_has_the_named_error() {
        let session = session();
        let wrong = facts().with_map(MapVersion::new(1), MapDigest::from_bytes([9; 32]));
        let error = session.check_compatibility(&wrong).unwrap_err();
        assert_eq!(error.code().as_str(), "sync.map_mismatch");
        assert!(matches!(error, ProtocolError::MapMismatch { .. }));
    }

    #[test]
    fn an_unknown_codec_has_the_named_error() {
        let session = session();
        let wrong = SessionHelloFacts::new(
            DescriptorDigest::from_bytes([1; 32]),
            ManifestDigest::from_bytes([2; 32]),
            SyncWireVersion::new(1),
            ConnectCodec::Other("cbor"),
        );
        assert_eq!(
            session
                .check_compatibility(&wrong)
                .unwrap_err()
                .code()
                .as_str(),
            "rpc.codec_unsupported"
        );
    }

    #[test]
    fn an_unknown_sync_version_has_the_named_error() {
        let session = session();
        let wrong = SessionHelloFacts::new(
            DescriptorDigest::from_bytes([1; 32]),
            ManifestDigest::from_bytes([2; 32]),
            SyncWireVersion::new(9),
            ConnectCodec::Json,
        );
        assert_eq!(
            session
                .check_compatibility(&wrong)
                .unwrap_err()
                .code()
                .as_str(),
            "sync.unsupported_version"
        );
    }
}
