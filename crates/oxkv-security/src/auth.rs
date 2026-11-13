//! Credential authentication and revocation.
//!
//! Authentication runs before authorization. The caller injects the logical
//! time. The crate does not read a clock.

use core::fmt;
use std::collections::BTreeMap;

use crate::error::SecurityError;

/// A logical time value.
///
/// The value is an injected tick. It is not a wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LogicalTime(u64);

impl LogicalTime {
    /// Creates a logical time value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Adds a bound without overflow.
    #[must_use]
    pub const fn saturating_add(self, value: u64) -> Self {
        Self(self.0.saturating_add(value))
    }
}

impl fmt::Display for LogicalTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

macro_rules! identifier {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Vec<u8>);

        impl $name {
            /// Creates an identifier from raw bytes.
            #[must_use]
            pub fn from_bytes(bytes: Vec<u8>) -> Self {
                Self(bytes)
            }

            /// Returns the raw identifier bytes.
            #[must_use]
            pub fn as_bytes(&self) -> &[u8] {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&hex(&self.0))
                    .finish()
            }
        }
    };
}

identifier!(
    /// A credential identity.
    CredentialId
);
identifier!(
    /// A principal identity.
    PrincipalId
);

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// An authenticated subject.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Subject {
    principal: PrincipalId,
    credential: CredentialId,
}

impl Subject {
    /// Creates a subject.
    #[must_use]
    pub const fn new(principal: PrincipalId, credential: CredentialId) -> Self {
        Self {
            principal,
            credential,
        }
    }

    /// Returns the principal identity.
    #[must_use]
    pub const fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    /// Returns the credential identity.
    #[must_use]
    pub const fn credential(&self) -> &CredentialId {
        &self.credential
    }
}

/// An unverified credential proof.
///
/// The proof carries an identifier only. The device key stays outside the
/// scope database.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CredentialProof {
    credential_id: CredentialId,
}

impl CredentialProof {
    /// Creates a proof from a credential identity.
    #[must_use]
    pub const fn new(credential_id: CredentialId) -> Self {
        Self { credential_id }
    }

    /// Returns the credential identity.
    #[must_use]
    pub const fn credential_id(&self) -> &CredentialId {
        &self.credential_id
    }
}

/// The revocation staleness bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RevocationPolicy {
    staleness_bound: u64,
}

impl RevocationPolicy {
    /// Creates a policy with a staleness bound.
    #[must_use]
    pub const fn new(staleness_bound: u64) -> Self {
        Self { staleness_bound }
    }

    /// Returns the staleness bound.
    #[must_use]
    pub const fn staleness_bound(self) -> u64 {
        self.staleness_bound
    }
}

/// A verifier that turns a credential proof into a subject.
pub trait CredentialVerifier {
    /// Verifies one credential proof at one logical time.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::Unauthenticated`] for an unknown credential.
    /// Returns [`SecurityError::StaleCredentials`] after the revocation bound.
    fn authenticate(
        &self,
        proof: &CredentialProof,
        now: LogicalTime,
    ) -> Result<Subject, SecurityError>;
}

/// An in-memory credential directory with a signed revocation view.
#[derive(Debug, Clone, Default)]
pub struct CredentialDirectory {
    principals: BTreeMap<CredentialId, PrincipalId>,
    revocations: BTreeMap<CredentialId, LogicalTime>,
    revocation_policy: RevocationPolicy,
}

impl CredentialDirectory {
    /// Creates an empty directory.
    #[must_use]
    pub fn new(revocation_policy: RevocationPolicy) -> Self {
        Self {
            principals: BTreeMap::new(),
            revocations: BTreeMap::new(),
            revocation_policy,
        }
    }

    /// Adds one credential.
    ///
    /// The function returns the previous principal for the credential.
    pub fn insert(
        &mut self,
        credential: CredentialId,
        principal: PrincipalId,
    ) -> Option<PrincipalId> {
        self.principals.insert(credential, principal)
    }

    /// Revokes one credential at one logical time.
    pub fn revoke(&mut self, credential: CredentialId, revoked_at: LogicalTime) {
        self.revocations.insert(credential, revoked_at);
    }

    /// Removes one revocation.
    ///
    /// The function returns the previous revocation time.
    pub fn unrevoke(&mut self, credential: &CredentialId) -> Option<LogicalTime> {
        self.revocations.remove(credential)
    }

    /// Returns the revocation policy.
    #[must_use]
    pub const fn revocation_policy(&self) -> RevocationPolicy {
        self.revocation_policy
    }

    /// Returns true when a credential is revoked at or before the time.
    #[must_use]
    pub fn is_revoked(&self, credential: &CredentialId, now: LogicalTime) -> bool {
        self.revocations
            .get(credential)
            .is_some_and(|revoked_at| now >= *revoked_at)
    }
}

impl CredentialVerifier for CredentialDirectory {
    fn authenticate(
        &self,
        proof: &CredentialProof,
        now: LogicalTime,
    ) -> Result<Subject, SecurityError> {
        let principal = self
            .principals
            .get(proof.credential_id())
            .ok_or(SecurityError::Unauthenticated)?;
        if let Some(revoked_at) = self.revocations.get(proof.credential_id()) {
            let deadline = revoked_at.saturating_add(self.revocation_policy.staleness_bound());
            if now > deadline {
                return Err(SecurityError::StaleCredentials);
            }
        }
        Ok(Subject::new(
            principal.clone(),
            proof.credential_id().clone(),
        ))
    }
}
