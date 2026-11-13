//! The neutral deterministic descriptor compiler.
//!
//! The compiler is pure. It reads one manifest value and writes canonical
//! descriptor bytes. It does not read the clock, the environment, or the
//! network. Two clean builds produce byte-identical bytes.

use core::fmt;

use oxkv_contracts::version::{DescriptorDigest, ManifestDigest};

use crate::error::ManifestError;
use crate::json::CanonicalValue;
use crate::schema::{DIGEST_ALGORITHM, Manifest, SCHEMA_VERSION};

/// The descriptor schema version.
pub const DESCRIPTOR_VERSION: &str = "descriptor/v1";

/// One compiled neutral descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// The canonical descriptor bytes.
    bytes: Vec<u8>,
    /// The digest of the canonical bytes.
    digest: DescriptorDigest,
    /// The manifest digest that the descriptor binds.
    manifest_digest: ManifestDigest,
}

impl Descriptor {
    /// Returns the canonical descriptor bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the canonical descriptor text.
    ///
    /// # Panics
    ///
    /// Panics only if the internal canonical bytes are not UTF-8. The encoder
    /// always writes valid UTF-8.
    #[must_use]
    pub fn text(&self) -> &str {
        core::str::from_utf8(&self.bytes).expect("the descriptor bytes are valid UTF-8")
    }

    /// Returns the descriptor digest.
    #[must_use]
    pub const fn digest(&self) -> DescriptorDigest {
        self.digest
    }

    /// Returns the bound manifest digest.
    #[must_use]
    pub const fn manifest_digest(&self) -> ManifestDigest {
        self.manifest_digest
    }
}

impl fmt::Display for Descriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.text())
    }
}

/// Compiles one manifest to a neutral descriptor.
///
/// The compiler validates the manifest first. It checks a stored digest when
/// one is present.
///
/// # Errors
///
/// Returns the first [`ManifestError`] from validation. Returns
/// [`ManifestError::ArtifactDrift`] when a stored digest does not match.
pub fn compile(manifest: &Manifest) -> Result<Descriptor, ManifestError> {
    manifest.validate()?;
    let manifest_digest = manifest.digest();
    if let Some(stored) = manifest.schema.digest
        && stored != manifest_digest
    {
        return Err(ManifestError::ArtifactDrift {
            expected: manifest_digest.to_hex(),
            found: stored.to_hex(),
        });
    }
    let value = descriptor_value(manifest, manifest_digest);
    let bytes = value.to_bytes();
    let digest = DescriptorDigest::from_bytes(*blake3::hash(&bytes).as_bytes());
    Ok(Descriptor {
        bytes,
        digest,
        manifest_digest,
    })
}

/// Builds the canonical descriptor value.
fn descriptor_value(manifest: &Manifest, manifest_digest: ManifestDigest) -> CanonicalValue {
    let CanonicalValue::Object(mut object) = manifest.to_value(false) else {
        return CanonicalValue::Object(std::collections::BTreeMap::new());
    };
    object.insert(
        "descriptor_version".to_owned(),
        CanonicalValue::String(DESCRIPTOR_VERSION.to_owned()),
    );
    object.insert(
        "manifest_digest".to_owned(),
        CanonicalValue::String(manifest_digest.to_hex()),
    );
    object.insert(
        "manifest_revision".to_owned(),
        CanonicalValue::Integer(manifest.schema.revision),
    );
    object.insert(
        "manifest_schema_version".to_owned(),
        CanonicalValue::String(SCHEMA_VERSION.to_owned()),
    );
    object.insert(
        "digest_algorithm".to_owned(),
        CanonicalValue::String(DIGEST_ALGORITHM.to_owned()),
    );
    CanonicalValue::Object(object)
}
