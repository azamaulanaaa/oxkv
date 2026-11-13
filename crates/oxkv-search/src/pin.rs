//! Search pins, query identities, and pin retention.
//!
//! A search pin is one version vector plus one query fingerprint. A search and
//! a get that use the same pin read the same committed version.

use blake3::Hasher;

use oxkv_contracts::cursor::QueryFingerprint;
use oxkv_contracts::version::ScopeVersionVector;

use crate::error::SearchError;
use crate::hit::QueryOrder;

/// The domain tag for a search query fingerprint.
pub const QUERY_FINGERPRINT_DOMAIN: &[u8] = b"oxkv/search-query/v1";

/// The canonical identity of one query.
///
/// The fingerprint covers the query bytes, the scope bytes, the order, and the
/// declared sort bytes. A change to any part produces a new fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryIdentity {
    query: Vec<u8>,
    scope: Vec<u8>,
    order: QueryOrder,
    sort: Vec<u8>,
}

impl QueryIdentity {
    /// Creates a query identity from canonical bytes.
    #[must_use]
    pub fn new(
        query: impl Into<Vec<u8>>,
        scope: impl Into<Vec<u8>>,
        order: QueryOrder,
        sort: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            query: query.into(),
            scope: scope.into(),
            order,
            sort: sort.into(),
        }
    }

    /// Returns the canonical query bytes.
    #[must_use]
    pub fn query(&self) -> &[u8] {
        &self.query
    }

    /// Returns the canonical scope bytes.
    #[must_use]
    pub fn scope(&self) -> &[u8] {
        &self.scope
    }

    /// Returns the query order.
    #[must_use]
    pub const fn order(&self) -> QueryOrder {
        self.order
    }

    /// Returns the canonical sort bytes.
    #[must_use]
    pub fn sort(&self) -> &[u8] {
        &self.sort
    }

    /// Computes the query fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> QueryFingerprint {
        let mut hasher = Hasher::new();
        hasher.update(QUERY_FINGERPRINT_DOMAIN);
        hasher.update(&[0]);
        update_part(&mut hasher, &self.query);
        update_part(&mut hasher, &self.scope);
        hasher.update(&[self.order.tag()]);
        update_part(&mut hasher, &self.sort);
        QueryFingerprint::from_bytes(*hasher.finalize().as_bytes())
    }
}

fn update_part(hasher: &mut Hasher, bytes: &[u8]) {
    hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

/// A search pin.
///
/// The pin binds one version vector and one query fingerprint. A search cursor
/// and a point get use the same pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPin {
    version_vector: ScopeVersionVector,
    query_fingerprint: QueryFingerprint,
}

impl SearchPin {
    /// Creates a search pin.
    #[must_use]
    pub const fn new(
        version_vector: ScopeVersionVector,
        query_fingerprint: QueryFingerprint,
    ) -> Self {
        Self {
            version_vector,
            query_fingerprint,
        }
    }

    /// Creates a search pin from a version vector and a query identity.
    #[must_use]
    pub fn from_identity(version_vector: ScopeVersionVector, identity: &QueryIdentity) -> Self {
        Self::new(version_vector, identity.fingerprint())
    }

    /// Returns the pinned version vector.
    #[must_use]
    pub const fn version_vector(&self) -> &ScopeVersionVector {
        &self.version_vector
    }

    /// Returns the query fingerprint.
    #[must_use]
    pub const fn query_fingerprint(&self) -> QueryFingerprint {
        self.query_fingerprint
    }

    /// Returns true when the pin is at or above the oldest readable vector.
    #[must_use]
    pub fn is_readable_at(&self, oldest_readable: &ScopeVersionVector) -> bool {
        self.version_vector
            .as_vector()
            .entails(oldest_readable.as_vector())
    }

    /// Checks the pin against a retention floor.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::PinExpired`] when the pin is below the floor.
    pub fn check(&self, retention: &PinRetention) -> Result<(), SearchError> {
        if self.is_readable_at(retention.oldest_readable()) {
            Ok(())
        } else {
            Err(SearchError::PinExpired {
                requested: self.version_vector.clone(),
                oldest_readable: retention.oldest_readable().clone(),
            })
        }
    }
}

/// The oldest readable version vector for one pin.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PinRetention {
    oldest_readable: ScopeVersionVector,
}

impl PinRetention {
    /// Creates a retention floor.
    #[must_use]
    pub const fn new(oldest_readable: ScopeVersionVector) -> Self {
        Self { oldest_readable }
    }

    /// Creates a retention floor with no lower bound.
    #[must_use]
    pub fn unbounded() -> Self {
        Self::new(ScopeVersionVector::new())
    }

    /// Returns the oldest readable version vector.
    #[must_use]
    pub const fn oldest_readable(&self) -> &ScopeVersionVector {
        &self.oldest_readable
    }

    /// Returns true when the floor has no entries.
    #[must_use]
    pub fn is_unbounded(&self) -> bool {
        self.oldest_readable.is_empty()
    }
}
