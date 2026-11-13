//! Principal authentication.
//!
//! Authentication runs before authorization. The rule language does not
//! authenticate a principal. The authority supplies one [`Authenticator`].

use crate::request::WriteRequest;

/// An authenticated principal identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Principal(Vec<u8>);

impl Principal {
    /// Creates a principal identity from raw bytes.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Returns the raw identity bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// An authentication failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("The request failed authentication with {code}.")]
pub struct AuthError {
    code: &'static str,
}

impl AuthError {
    /// The request has no valid authentication.
    pub const UNAUTHENTICATED: Self = Self {
        code: "auth.unauthenticated",
    };

    /// The credential is past its allowed age.
    pub const STALE_CREDENTIALS: Self = Self {
        code: "auth.stale_credentials",
    };

    /// The request reuses a nonce with a different body.
    pub const REPLAYED_REQUEST: Self = Self {
        code: "auth.replayed_request",
    };

    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

/// Authenticates one write request.
///
/// The authority calls this trait before any other write check. A request
/// with no principal receives no receipt.
pub trait Authenticator {
    /// Returns the authenticated principal.
    ///
    /// # Errors
    ///
    /// Returns an [`AuthError`] when the request is not authenticated.
    fn authenticate(&self, request: &WriteRequest) -> Result<Principal, AuthError>;
}
