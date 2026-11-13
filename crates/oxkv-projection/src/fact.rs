//! Fact declarations, canonical types, and fact values.
//!
//! A fact declaration names the fact identity, its canonical type, its owner,
//! its fan-in cap, and its staleness bound. The bound is a version distance.
//! It is not a duration.

use std::collections::BTreeMap;

use oxkv_contracts::version::{FactSourceId, FactVersion, Hash32, ShardId};

use crate::codec::{CanonicalEncode, Encoder, hex};
use crate::error::ProjectionError;

/// A maximum fact version distance.
///
/// The distance is `source_version - projection_watermark`. A value of zero
/// means that the projection must hold the current source version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FactVersionDistance(u64);

impl FactVersionDistance {
    /// A zero distance.
    pub const ZERO: Self = Self(0);

    /// Creates a distance.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer distance.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns true when this distance is within a bound.
    #[must_use]
    pub const fn is_within(self, bound: Self) -> bool {
        self.0 <= bound.0
    }

    /// Returns the distance between a watermark and an observed version.
    ///
    /// The result is `None` when the observed version is behind the
    /// watermark.
    #[must_use]
    pub const fn between(watermark: FactVersion, observed: FactVersion) -> Option<Self> {
        if observed.get() >= watermark.get() {
            Some(Self(observed.get() - watermark.get()))
        } else {
            None
        }
    }
}

/// The identity of one fact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FactId(String);

impl FactId {
    /// Creates a fact identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for FactId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl CanonicalEncode for FactId {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.text(self.as_str());
        encoder.finish()
    }
}

/// The canonical type of one fact value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CanonicalType {
    /// A boolean.
    Bool,
    /// An unsigned 64-bit integer.
    U64,
    /// A signed 64-bit integer.
    I64,
    /// A UTF-8 text string.
    Text,
    /// An opaque byte string.
    Bytes,
}

impl CanonicalType {
    /// Returns the stable type tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Bool => 1,
            Self::U64 => 2,
            Self::I64 => 3,
            Self::Text => 4,
            Self::Bytes => 5,
        }
    }

    /// Returns the canonical type name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::U64 => "u64",
            Self::I64 => "i64",
            Self::Text => "text",
            Self::Bytes => "bytes",
        }
    }
}

/// One canonical fact value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FactValue {
    /// A boolean value.
    Bool(bool),
    /// An unsigned 64-bit integer value.
    U64(u64),
    /// A signed 64-bit integer value.
    I64(i64),
    /// A UTF-8 text value.
    Text(String),
    /// An opaque byte value.
    Bytes(Vec<u8>),
}

impl FactValue {
    /// Returns the canonical type of the value.
    #[must_use]
    pub const fn canonical_type(&self) -> CanonicalType {
        match self {
            Self::Bool(_) => CanonicalType::Bool,
            Self::U64(_) => CanonicalType::U64,
            Self::I64(_) => CanonicalType::I64,
            Self::Text(_) => CanonicalType::Text,
            Self::Bytes(_) => CanonicalType::Bytes,
        }
    }
}

impl CanonicalEncode for FactValue {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.u8(self.canonical_type().tag());
        match self {
            Self::Bool(value) => encoder.bool(*value),
            Self::U64(value) => encoder.u64(*value),
            Self::I64(value) => encoder.i64(*value),
            Self::Text(value) => encoder.text(value),
            Self::Bytes(value) => encoder.bytes(value),
        }
        encoder.finish()
    }
}

/// The owner of one fact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FactOwner {
    /// A data shard owns the fact.
    Shard(ShardId),
    /// A named subsystem owns the fact.
    Source(FactSourceId),
}

impl FactOwner {
    /// Returns the fact source identity used by watermarks.
    #[must_use]
    pub fn source_id(&self) -> FactSourceId {
        match self {
            Self::Shard(shard_id) => {
                FactSourceId::new(format!("shard:{}", hex(shard_id.as_bytes())))
            }
            Self::Source(source) => source.clone(),
        }
    }

    /// Returns the stable owner tag.
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self {
            Self::Shard(_) => 1,
            Self::Source(_) => 2,
        }
    }
}

impl CanonicalEncode for FactOwner {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.u8(self.tag());
        match self {
            Self::Shard(shard_id) => encoder.bytes(shard_id.as_bytes()),
            Self::Source(source) => encoder.text(source.as_str()),
        }
        encoder.finish()
    }
}

/// One declared fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactDeclaration {
    fact_id: FactId,
    canonical_type: CanonicalType,
    owner: FactOwner,
    fan_in_cap: u32,
    staleness_bound: FactVersionDistance,
    revoked: bool,
}

impl FactDeclaration {
    /// Creates a fact declaration.
    #[must_use]
    pub fn new(
        fact_id: FactId,
        canonical_type: CanonicalType,
        owner: FactOwner,
        fan_in_cap: u32,
        staleness_bound: FactVersionDistance,
    ) -> Self {
        Self {
            fact_id,
            canonical_type,
            owner,
            fan_in_cap,
            staleness_bound,
            revoked: false,
        }
    }

    /// Returns the fact identity.
    #[must_use]
    pub const fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    /// Returns the canonical type.
    #[must_use]
    pub const fn canonical_type(&self) -> CanonicalType {
        self.canonical_type
    }

    /// Returns the owner.
    #[must_use]
    pub const fn owner(&self) -> &FactOwner {
        &self.owner
    }

    /// Returns the fact source identity.
    #[must_use]
    pub fn source_id(&self) -> FactSourceId {
        self.owner.source_id()
    }

    /// Returns the fan-in cap.
    #[must_use]
    pub const fn fan_in_cap(&self) -> u32 {
        self.fan_in_cap
    }

    /// Returns the staleness bound.
    #[must_use]
    pub const fn staleness_bound(&self) -> FactVersionDistance {
        self.staleness_bound
    }

    /// Returns true when the declaration is revoked.
    #[must_use]
    pub const fn is_revoked(&self) -> bool {
        self.revoked
    }

    /// Returns a copy with the revocation flag set.
    #[must_use]
    pub const fn with_revoked(mut self, revoked: bool) -> Self {
        self.revoked = revoked;
        self
    }
}

impl CanonicalEncode for FactDeclaration {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.text(self.fact_id.as_str());
        encoder.u8(self.canonical_type.tag());
        encoder.bytes(&self.owner.canonical_bytes());
        encoder.u64(u64::from(self.fan_in_cap));
        encoder.u64(self.staleness_bound.get());
        encoder.bool(self.revoked);
        encoder.finish()
    }
}

/// A declared fact set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactSet {
    facts: BTreeMap<FactId, FactDeclaration>,
}

impl FactSet {
    /// Creates an empty fact set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a fact set from declarations.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::DuplicateFact`] when one identity appears
    /// more than one time.
    pub fn from_declarations(
        declarations: impl IntoIterator<Item = FactDeclaration>,
    ) -> Result<Self, ProjectionError> {
        let mut set = Self::new();
        for declaration in declarations {
            set.insert(declaration)?;
        }
        Ok(set)
    }

    /// Inserts one declaration.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::DuplicateFact`] when the identity is already
    /// declared.
    pub fn insert(&mut self, declaration: FactDeclaration) -> Result<(), ProjectionError> {
        let fact_id = declaration.fact_id.clone();
        if self.facts.contains_key(&fact_id) {
            return Err(ProjectionError::DuplicateFact { fact_id });
        }
        self.facts.insert(fact_id, declaration);
        Ok(())
    }

    /// Returns one declaration.
    #[must_use]
    pub fn get(&self, fact_id: &FactId) -> Option<&FactDeclaration> {
        self.facts.get(fact_id)
    }

    /// Returns the number of declarations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.facts.len()
    }

    /// Returns true when the set has no declarations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    /// Iterates over the declarations in canonical identity order.
    pub fn iter(&self) -> impl Iterator<Item = (&FactId, &FactDeclaration)> {
        self.facts.iter()
    }

    /// Revokes one fact.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::UnknownFact`] when the fact is absent.
    pub fn revoke(&mut self, fact_id: &FactId) -> Result<(), ProjectionError> {
        let declaration =
            self.facts
                .get_mut(fact_id)
                .ok_or_else(|| ProjectionError::UnknownFact {
                    fact_id: fact_id.clone(),
                })?;
        *declaration = declaration.clone().with_revoked(true);
        Ok(())
    }

    /// Returns the canonical fact set digest.
    #[must_use]
    pub fn digest(&self) -> Hash32 {
        Hash32::of(&self.canonical_bytes())
    }
}

impl CanonicalEncode for FactSet {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.u64(u64::try_from(self.facts.len()).unwrap_or(u64::MAX));
        for declaration in self.facts.values() {
            encoder.bytes(&declaration.canonical_bytes());
        }
        encoder.finish()
    }
}

/// One fact value inside a projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFact {
    fact_id: FactId,
    source: FactSourceId,
    fact_version: FactVersion,
    value: FactValue,
}

impl StoredFact {
    /// Creates a stored fact.
    #[must_use]
    pub const fn new(
        fact_id: FactId,
        source: FactSourceId,
        fact_version: FactVersion,
        value: FactValue,
    ) -> Self {
        Self {
            fact_id,
            source,
            fact_version,
            value,
        }
    }

    /// Returns the fact identity.
    #[must_use]
    pub const fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    /// Returns the fact source.
    #[must_use]
    pub const fn source(&self) -> &FactSourceId {
        &self.source
    }

    /// Returns the fact version.
    #[must_use]
    pub const fn fact_version(&self) -> FactVersion {
        self.fact_version
    }

    /// Returns the fact value.
    #[must_use]
    pub const fn value(&self) -> &FactValue {
        &self.value
    }

    /// Returns true when the value matches the declaration.
    #[must_use]
    pub fn matches(&self, declaration: &FactDeclaration) -> bool {
        self.source == declaration.source_id()
            && self.value.canonical_type() == declaration.canonical_type()
    }
}

impl CanonicalEncode for StoredFact {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.text(self.fact_id.as_str());
        encoder.text(self.source.as_str());
        encoder.u64(self.fact_version.get());
        encoder.bytes(&self.value.canonical_bytes());
        encoder.finish()
    }
}
