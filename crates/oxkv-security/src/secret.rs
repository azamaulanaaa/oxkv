//! Secret wrappers and key identifiers.
//!
//! A [`Secret`] never prints its bytes. A [`SecretReference`] names a key
//! identifier only. The manifest and the policy use references, not key
//! material.

use core::fmt;

/// Opaque secret bytes.
///
/// The `Debug` and `Display` output is always `[REDACTED]`. Use
/// [`Secret::expose_secret`] at the one point that needs the bytes.
pub struct Secret(Vec<u8>);

impl Secret {
    /// Creates a secret from raw bytes.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Returns the secret bytes.
    ///
    /// The caller must not put the result in a log, an error, an audit record,
    /// a trace, or an event.
    #[must_use]
    pub fn expose_secret(&self) -> &[u8] {
        &self.0
    }

    /// Returns the number of secret bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns true when the secret has no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        constant_time_eq(&self.0, &other.0)
    }
}

impl Eq for Secret {}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// A stable key identifier.
///
/// The value names a key. It is not key material.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyIdentifier(String);

impl KeyIdentifier {
    /// Creates a key identifier.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KeyIdentifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A reference to key material that stays outside the store.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretReference {
    key_id: KeyIdentifier,
}

impl SecretReference {
    /// Creates a reference from a key identifier.
    #[must_use]
    pub const fn new(key_id: KeyIdentifier) -> Self {
        Self { key_id }
    }

    /// Returns the key identifier.
    #[must_use]
    pub const fn key_id(&self) -> &KeyIdentifier {
        &self.key_id
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    let mut index = 0;
    while index < left.len() {
        difference |= left[index] ^ right[index];
        index += 1;
    }
    difference == 0
}
