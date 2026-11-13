//! Reserved system identifiers and keyspace validation.
//!
//! A system key uses the [`SYSTEM_MARKER`] byte and a one-byte system
//! identifier. [`SYSTEM_NAMESPACES`] holds the registered identifiers.
//! [`validate_system_id`] rejects an unknown identifier.
//!
//! This module defines the namespace constants and validation only. Full key
//! encoding belongs to the key encoding essay.

use core::fmt;

/// The key encoding version byte.
pub const KEY_ENCODING_VERSION: u8 = 0x01;

/// The system key marker.
pub const SYSTEM_MARKER: u8 = 0xFF;

/// The receipt system identifier.
pub const SYSTEM_ID_RECEIPT: u8 = 0x01;
/// The event system identifier.
pub const SYSTEM_ID_EVENT: u8 = 0x02;
/// The audit system identifier.
pub const SYSTEM_ID_AUDIT: u8 = 0x03;
/// The projection system identifier.
pub const SYSTEM_ID_PROJECTION: u8 = 0x04;
/// The index system identifier.
pub const SYSTEM_ID_INDEX: u8 = 0x05;
/// The lease system identifier.
pub const SYSTEM_ID_LEASE: u8 = 0x06;
/// The statistics system identifier.
pub const SYSTEM_ID_STATISTICS: u8 = 0x07;

/// The only top-level system table in 1.0.
pub const CATALOG_TABLE: &str = "_catalog";

/// The text component tag.
pub const USER_COMPONENT_TEXT: u8 = 0x10;
/// The bytes component tag.
pub const USER_COMPONENT_BYTES: u8 = 0x11;
/// The unsigned 64-bit component tag.
pub const USER_COMPONENT_U64: u8 = 0x20;
/// The signed 64-bit component tag.
pub const USER_COMPONENT_I64: u8 = 0x21;
/// The boolean component tag.
pub const USER_COMPONENT_BOOL: u8 = 0x30;

/// The commit domain that owns one system sub-namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommitDomain {
    /// The sub-namespace is valid in a data shard only.
    DataShard,
    /// The sub-namespace is valid in a data shard or the catalog shard.
    DataOrCatalogShard,
}

impl CommitDomain {
    /// Returns the canonical domain text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DataShard => "data shard",
            Self::DataOrCatalogShard => "data shard or catalog shard",
        }
    }

    /// Returns true when the domain permits the catalog shard.
    #[must_use]
    pub const fn allows_catalog_shard(self) -> bool {
        matches!(self, Self::DataOrCatalogShard)
    }
}

impl fmt::Display for CommitDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One registered system sub-namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SystemNamespace {
    /// The one-byte system identifier.
    pub id: u8,
    /// The canonical label.
    pub label: &'static str,
    /// The commit domain that owns the sub-namespace.
    pub commit_domain: CommitDomain,
    /// True when a user scope can see the sub-namespace. It is always false
    /// in 1.0.
    pub scope_visible: bool,
}

/// Every registered system sub-namespace.
pub static SYSTEM_NAMESPACES: &[SystemNamespace] = &[
    SystemNamespace {
        id: SYSTEM_ID_RECEIPT,
        label: "receipt",
        commit_domain: CommitDomain::DataOrCatalogShard,
        scope_visible: false,
    },
    SystemNamespace {
        id: SYSTEM_ID_EVENT,
        label: "event",
        commit_domain: CommitDomain::DataOrCatalogShard,
        scope_visible: false,
    },
    SystemNamespace {
        id: SYSTEM_ID_AUDIT,
        label: "audit",
        commit_domain: CommitDomain::DataOrCatalogShard,
        scope_visible: false,
    },
    SystemNamespace {
        id: SYSTEM_ID_PROJECTION,
        label: "projection",
        commit_domain: CommitDomain::DataShard,
        scope_visible: false,
    },
    SystemNamespace {
        id: SYSTEM_ID_INDEX,
        label: "index",
        commit_domain: CommitDomain::DataShard,
        scope_visible: false,
    },
    SystemNamespace {
        id: SYSTEM_ID_LEASE,
        label: "lease",
        commit_domain: CommitDomain::DataOrCatalogShard,
        scope_visible: false,
    },
    SystemNamespace {
        id: SYSTEM_ID_STATISTICS,
        label: "statistics",
        commit_domain: CommitDomain::DataShard,
        scope_visible: false,
    },
];

/// An error from keyspace validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyspaceError {
    /// The system identifier is not registered.
    #[error("The system identifier {id:#04x} is not registered.")]
    UnknownSystemId {
        /// The unknown identifier.
        id: u8,
    },
    /// The system identifier is not valid in this commit domain.
    #[error("The system identifier {id:#04x} is not valid in the {domain}.")]
    UnknownSystemIdInDomain {
        /// The rejected identifier.
        id: u8,
        /// The commit domain.
        domain: CommitDomain,
    },
    /// A user key uses the system marker as a component tag.
    #[error("A user key cannot use the system marker as a component tag.")]
    ReservedSystemMarker,
    /// A user table name is reserved.
    #[error("The user table name {name} is reserved.")]
    ReservedTableName {
        /// The reserved name.
        name: String,
    },
    /// A table name does not match the grammar.
    #[error("The table name {name} does not match the table name grammar.")]
    InvalidTableName {
        /// The invalid name.
        name: String,
    },
    /// A user component tag is unknown.
    #[error("The user component tag {tag:#04x} is unknown.")]
    InvalidComponentTag {
        /// The unknown tag.
        tag: u8,
    },
}

impl KeyspaceError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownSystemId { .. } | Self::UnknownSystemIdInDomain { .. } => {
                "key.unknown_system_id"
            }
            Self::ReservedSystemMarker => "key.reserved_system_marker",
            Self::ReservedTableName { .. } => "manifest.reserved_table_name",
            Self::InvalidTableName { .. } | Self::InvalidComponentTag { .. } => "key.malformed",
        }
    }
}

/// The type tag of one user key component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserComponentTag {
    /// A UTF-8 text component.
    Text,
    /// An opaque bytes component.
    Bytes,
    /// An unsigned 64-bit component.
    U64,
    /// A signed 64-bit component.
    I64,
    /// A boolean component.
    Bool,
}

impl UserComponentTag {
    /// Returns the stable wire tag.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Text => USER_COMPONENT_TEXT,
            Self::Bytes => USER_COMPONENT_BYTES,
            Self::U64 => USER_COMPONENT_U64,
            Self::I64 => USER_COMPONENT_I64,
            Self::Bool => USER_COMPONENT_BOOL,
        }
    }

    /// Reads a component tag.
    ///
    /// # Errors
    ///
    /// Returns [`KeyspaceError::ReservedSystemMarker`] for `0xFF`. Returns
    /// [`KeyspaceError::InvalidComponentTag`] for an unknown tag.
    pub const fn from_tag(tag: u8) -> Result<Self, KeyspaceError> {
        match tag {
            SYSTEM_MARKER => Err(KeyspaceError::ReservedSystemMarker),
            USER_COMPONENT_TEXT => Ok(Self::Text),
            USER_COMPONENT_BYTES => Ok(Self::Bytes),
            USER_COMPONENT_U64 => Ok(Self::U64),
            USER_COMPONENT_I64 => Ok(Self::I64),
            USER_COMPONENT_BOOL => Ok(Self::Bool),
            _ => Err(KeyspaceError::InvalidComponentTag { tag }),
        }
    }
}

/// A table name. It is a valid user table name or the catalog table name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TableName(String);

impl TableName {
    /// Parses a table name.
    ///
    /// The value `_catalog` is the only valid system table name. Any other
    /// name that starts with `_` is reserved.
    ///
    /// # Errors
    ///
    /// Returns [`KeyspaceError::ReservedTableName`] for a reserved name.
    /// Returns [`KeyspaceError::InvalidTableName`] for a grammar mismatch.
    pub fn parse(name: &str) -> Result<Self, KeyspaceError> {
        if name == CATALOG_TABLE {
            return Ok(Self(name.to_owned()));
        }
        validate_user_table_name(name)?;
        Ok(Self(name.to_owned()))
    }

    /// Parses a user table name. The catalog table name is not allowed.
    ///
    /// # Errors
    ///
    /// Returns [`KeyspaceError::ReservedTableName`] when the name starts with
    /// `_`. Returns [`KeyspaceError::InvalidTableName`] for a grammar
    /// mismatch.
    pub fn parse_user(name: &str) -> Result<Self, KeyspaceError> {
        validate_user_table_name(name)?;
        Ok(Self(name.to_owned()))
    }

    /// Creates the catalog table name.
    #[must_use]
    pub fn catalog() -> Self {
        Self(CATALOG_TABLE.to_owned())
    }

    /// Returns the table name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns true when the table is the catalog table.
    #[must_use]
    pub fn is_catalog(&self) -> bool {
        self.0 == CATALOG_TABLE
    }
}

impl fmt::Display for TableName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validates a user table name.
///
/// The grammar is `[a-z][a-z0-9_]*`. A user table name does not start with
/// `_`.
///
/// # Errors
///
/// Returns [`KeyspaceError::ReservedTableName`] for a name that starts with
/// `_`. Returns [`KeyspaceError::InvalidTableName`] for a grammar mismatch.
pub fn validate_user_table_name(name: &str) -> Result<(), KeyspaceError> {
    if name.starts_with('_') {
        return Err(KeyspaceError::ReservedTableName {
            name: name.to_owned(),
        });
    }
    let bytes = name.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let valid = if index == 0 {
            byte.is_ascii_lowercase()
        } else {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
        };
        if !valid {
            return Err(KeyspaceError::InvalidTableName {
                name: name.to_owned(),
            });
        }
        index += 1;
    }
    if bytes.is_empty() {
        return Err(KeyspaceError::InvalidTableName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// Returns one registered system sub-namespace.
#[must_use]
pub fn system_namespace(id: u8) -> Option<&'static SystemNamespace> {
    SYSTEM_NAMESPACES
        .iter()
        .find(|namespace| namespace.id == id)
}

/// Returns one system sub-namespace for a commit domain.
#[must_use]
pub fn system_namespace_in(domain: CommitDomain, id: u8) -> Option<&'static SystemNamespace> {
    system_namespace(id).filter(|namespace| match domain {
        CommitDomain::DataShard => true,
        CommitDomain::DataOrCatalogShard => namespace.commit_domain.allows_catalog_shard(),
    })
}

/// Validates one system identifier for a data shard.
///
/// # Errors
///
/// Returns [`KeyspaceError::UnknownSystemId`] when the identifier is not
/// registered.
pub fn validate_system_id(id: u8) -> Result<&'static SystemNamespace, KeyspaceError> {
    validate_system_id_in(CommitDomain::DataShard, id)
}

/// Validates one system identifier for a commit domain.
///
/// # Errors
///
/// Returns [`KeyspaceError::UnknownSystemId`] when the identifier is not
/// registered. Returns [`KeyspaceError::UnknownSystemIdInDomain`] when the
/// identifier is registered but not valid in the domain.
pub fn validate_system_id_in(
    domain: CommitDomain,
    id: u8,
) -> Result<&'static SystemNamespace, KeyspaceError> {
    let namespace = system_namespace(id).ok_or(KeyspaceError::UnknownSystemId { id })?;
    let allowed = match domain {
        CommitDomain::DataShard => true,
        CommitDomain::DataOrCatalogShard => namespace.commit_domain.allows_catalog_shard(),
    };
    if allowed {
        Ok(namespace)
    } else {
        Err(KeyspaceError::UnknownSystemIdInDomain { id, domain })
    }
}

/// Validates one user key component tag.
///
/// # Errors
///
/// Returns [`KeyspaceError::ReservedSystemMarker`] for `0xFF`. Returns
/// [`KeyspaceError::InvalidComponentTag`] for an unknown tag.
pub const fn validate_user_component_tag(tag: u8) -> Result<UserComponentTag, KeyspaceError> {
    UserComponentTag::from_tag(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_has_every_system_identifier() {
        assert_eq!(SYSTEM_NAMESPACES.len(), 7);
        let expected = [
            (SYSTEM_ID_RECEIPT, "receipt"),
            (SYSTEM_ID_EVENT, "event"),
            (SYSTEM_ID_AUDIT, "audit"),
            (SYSTEM_ID_PROJECTION, "projection"),
            (SYSTEM_ID_INDEX, "index"),
            (SYSTEM_ID_LEASE, "lease"),
            (SYSTEM_ID_STATISTICS, "statistics"),
        ];
        for (id, label) in expected {
            let namespace = system_namespace(id).expect("the identifier is registered");
            assert_eq!(namespace.label, label);
            assert!(!namespace.scope_visible);
        }
    }

    #[test]
    fn an_unknown_system_id_fails() {
        assert!(system_namespace(0x00).is_none());
        assert!(system_namespace(0x08).is_none());
        assert_eq!(
            validate_system_id(0x08),
            Err(KeyspaceError::UnknownSystemId { id: 0x08 })
        );
        assert_eq!(
            validate_system_id(0x08).unwrap_err().code(),
            "key.unknown_system_id"
        );
    }

    #[test]
    fn the_catalog_shard_accepts_only_registered_catalog_identifiers() {
        for id in [
            SYSTEM_ID_RECEIPT,
            SYSTEM_ID_EVENT,
            SYSTEM_ID_AUDIT,
            SYSTEM_ID_LEASE,
        ] {
            assert!(validate_system_id_in(CommitDomain::DataOrCatalogShard, id).is_ok());
        }
        for id in [SYSTEM_ID_PROJECTION, SYSTEM_ID_INDEX, SYSTEM_ID_STATISTICS] {
            assert_eq!(
                validate_system_id_in(CommitDomain::DataOrCatalogShard, id),
                Err(KeyspaceError::UnknownSystemIdInDomain {
                    id,
                    domain: CommitDomain::DataOrCatalogShard
                })
            );
        }
    }

    #[test]
    fn a_user_table_name_uses_the_grammar() {
        assert!(TableName::parse_user("accounts").is_ok());
        assert!(TableName::parse_user("ledger_entry").is_ok());
        assert!(TableName::parse_user("a1").is_ok());
        assert!(TableName::parse_user("").is_err());
        assert!(TableName::parse_user("Accounts").is_err());
        assert!(TableName::parse_user("1accounts").is_err());
        assert!(TableName::parse_user("account-name").is_err());
    }

    #[test]
    fn a_reserved_table_name_fails() {
        assert_eq!(
            TableName::parse_user("_receipt"),
            Err(KeyspaceError::ReservedTableName {
                name: "_receipt".to_owned()
            })
        );
        assert_eq!(
            TableName::parse_user("_receipt").unwrap_err().code(),
            "manifest.reserved_table_name"
        );
        assert!(TableName::parse("_catalog").is_ok());
        assert!(TableName::parse("_catalog").unwrap().is_catalog());
        assert!(TableName::parse("_other").is_err());
    }

    #[test]
    fn the_system_marker_is_not_a_user_component_tag() {
        assert_eq!(
            validate_user_component_tag(SYSTEM_MARKER),
            Err(KeyspaceError::ReservedSystemMarker)
        );
        assert_eq!(
            validate_user_component_tag(SYSTEM_MARKER)
                .unwrap_err()
                .code(),
            "key.reserved_system_marker"
        );
        assert_eq!(
            validate_user_component_tag(0x99),
            Err(KeyspaceError::InvalidComponentTag { tag: 0x99 })
        );
    }

    #[test]
    fn every_user_component_tag_round_trips() {
        for tag in [
            UserComponentTag::Text,
            UserComponentTag::Bytes,
            UserComponentTag::U64,
            UserComponentTag::I64,
            UserComponentTag::Bool,
        ] {
            assert_eq!(UserComponentTag::from_tag(tag.as_u8()), Ok(tag));
        }
    }
}
