//! Parsing, canonical bytes, and digest for the manifest.
//!
//! The digest is BLAKE3 over the canonical JSON bytes with the `schema.digest`
//! field absent. The active policy version is not a manifest field.

use oxkv_contracts::version::ManifestDigest;

use crate::error::ManifestError;
use crate::json;
use crate::schema::Manifest;

impl Manifest {
    /// Parses one manifest from canonical JSON text.
    ///
    /// # Errors
    ///
    /// Returns a [`ManifestError`] for malformed JSON, a duplicate key, a
    /// float, or a schema mismatch.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        Self::from_value(json::parse(text)?)
    }

    /// Returns the canonical JSON bytes.
    ///
    /// The bytes include the digest when the schema holds one.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.to_value(true).to_bytes()
    }

    /// Returns the canonical JSON text.
    ///
    /// The text includes the digest when the schema holds one.
    ///
    /// # Panics
    ///
    /// Panics only if the internal canonical bytes are not UTF-8. The encoder
    /// always writes valid UTF-8.
    #[must_use]
    pub fn canonical_text(&self) -> String {
        let bytes = self.canonical_bytes();
        String::from_utf8(bytes).expect("the canonical JSON bytes are valid UTF-8")
    }

    /// Returns the digest of the canonical JSON bytes.
    ///
    /// The digest excludes the `schema.digest` field. A rule body change does
    /// not change this value because the manifest holds no rule body.
    #[must_use]
    pub fn digest(&self) -> ManifestDigest {
        let bytes = self.to_value(false).to_bytes();
        ManifestDigest::from_bytes(*blake3::hash(&bytes).as_bytes())
    }

    /// Returns a copy with the digest set.
    #[must_use]
    pub fn with_digest(mut self) -> Self {
        let digest = self.digest();
        self.schema.digest = Some(digest);
        self
    }

    /// Checks the stored digest against the canonical bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::ArtifactDrift`] when the digest is absent or
    /// does not match.
    pub fn verify_digest(&self) -> Result<(), ManifestError> {
        let computed = self.digest();
        match self.schema.digest {
            Some(stored) if stored == computed => Ok(()),
            Some(stored) => Err(ManifestError::ArtifactDrift {
                expected: computed.to_hex(),
                found: stored.to_hex(),
            }),
            None => Err(ManifestError::ArtifactDrift {
                expected: computed.to_hex(),
                found: "none".to_owned(),
            }),
        }
    }
}
