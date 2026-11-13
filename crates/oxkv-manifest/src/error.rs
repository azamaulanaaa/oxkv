//! Named manifest errors.
//!
//! Each error has one cause, one canonical code, and one fixed shape. The code
//! is the contract. The message is for humans.

use core::fmt;

/// The canonical code for an unsupported schema major version.
pub const CODE_UNSUPPORTED_SCHEMA: &str = "manifest.unsupported_schema";
/// The canonical code for a key that the schema does not declare.
pub const CODE_UNKNOWN_FIELD: &str = "manifest.unknown_field";
/// The canonical code for two declarations with one name.
pub const CODE_DUPLICATE_NAME: &str = "manifest.duplicate_name";
/// The canonical code for one JSON object key that appears two times.
pub const CODE_DUPLICATE_KEY: &str = "manifest.duplicate_key";
/// The canonical code for a reserved user table name.
pub const CODE_RESERVED_TABLE_NAME: &str = "manifest.reserved_table_name";
/// The canonical code for a table without a key schema.
pub const CODE_KEY_SCHEMA_MISSING: &str = "manifest.key_schema_missing";
/// The canonical code for a number that is not an i64 integer.
pub const CODE_FLOAT_NOT_ALLOWED: &str = "manifest.float_not_allowed";
/// The canonical code for an index that names an undeclared field.
pub const CODE_INDEX_UNKNOWN_FIELD: &str = "manifest.index_unknown_field";
/// The canonical code for an index that serves no scope.
pub const CODE_INDEX_SCOPE_UNREACHABLE: &str = "manifest.index_scope_unreachable";
/// The canonical code for a rule path that does not resolve.
pub const CODE_RULE_UNKNOWN_PATH: &str = "manifest.rule_unknown_path";
/// The canonical code for a rule above the cost budget.
pub const CODE_RULE_UNBOUNDED: &str = "manifest.rule_unbounded";
/// The canonical code for an operation that can reach two shards.
pub const CODE_OPERATION_CROSS_SHARD: &str = "manifest.operation_cross_shard";
/// The canonical code for an operation that names an undeclared field.
pub const CODE_OPERATION_UNKNOWN_FIELD: &str = "manifest.operation_unknown_field";
/// The canonical code for an artifact that does not match its digest.
pub const CODE_ARTIFACT_DRIFT: &str = "manifest.artifact_drift";
/// The canonical code for a missing policy catalog.
pub const CODE_POLICY_CATALOG_MISSING: &str = "manifest.policy_catalog_missing";
/// The canonical code for JSON text outside the canonical subset.
pub const CODE_JSON_SYNTAX: &str = "manifest.json_syntax";
/// The canonical code for a required field that is absent.
pub const CODE_MISSING_FIELD: &str = "manifest.missing_field";
/// The canonical code for a value with the wrong canonical type.
pub const CODE_TYPE_MISMATCH: &str = "manifest.type_mismatch";
/// The canonical code for a text value outside its declared enum.
pub const CODE_INVALID_VALUE: &str = "manifest.invalid_value";

/// One manifest validation or parse error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// The JSON text is not the canonical subset.
    JsonSyntax {
        /// The parser detail.
        detail: String,
    },
    /// One JSON object holds the same key two times.
    DuplicateKey {
        /// The duplicate key.
        key: String,
    },
    /// A number is a float or is outside the i64 range.
    FloatNotAllowed {
        /// The JSON path of the number.
        path: String,
    },
    /// The schema major version is not 1.
    UnsupportedSchema {
        /// The rejected schema text.
        schema: String,
    },
    /// An object holds a key that the schema does not declare.
    UnknownField {
        /// The object path.
        path: String,
        /// The undeclared key.
        field: String,
    },
    /// Two declarations share a name.
    DuplicateName {
        /// The declaration kind.
        kind: &'static str,
        /// The duplicate name.
        name: String,
    },
    /// A user table name is reserved.
    ReservedTableName {
        /// The reserved name.
        name: String,
    },
    /// A table has no key schema.
    KeySchemaMissing {
        /// The table name.
        table: String,
    },
    /// An index names an undeclared field.
    IndexUnknownField {
        /// The index name.
        index: String,
        /// The undeclared field.
        field: String,
    },
    /// An index serves no declared scope.
    IndexScopeUnreachable {
        /// The index name.
        index: String,
    },
    /// A rule path does not resolve.
    RuleUnknownPath {
        /// The rule name.
        rule: String,
        /// The unknown path.
        path: String,
    },
    /// A rule exceeds the cost budget.
    RuleUnbounded {
        /// The rule name.
        rule: String,
        /// The declared cost bound.
        cost: i64,
        /// The allowed budget.
        budget: i64,
    },
    /// An operation can require two shards.
    OperationCrossShard {
        /// The operation name.
        operation: String,
        /// The field path.
        field: String,
    },
    /// An operation names an undeclared field.
    OperationUnknownField {
        /// The operation name.
        operation: String,
        /// The undeclared field.
        field: String,
    },
    /// An artifact does not match its digest.
    ArtifactDrift {
        /// The expected digest text.
        expected: String,
        /// The found digest text.
        found: String,
    },
    /// The rules declare no policy catalog.
    PolicyCatalogMissing,
    /// A required field is absent.
    MissingField {
        /// The object path.
        path: String,
        /// The missing field.
        field: String,
    },
    /// A value has the wrong canonical type.
    TypeMismatch {
        /// The JSON path.
        path: String,
        /// The expected type.
        expected: &'static str,
        /// The found type.
        found: &'static str,
    },
    /// A text value is not a member of its enum.
    InvalidValue {
        /// The JSON path.
        path: String,
        /// The rejected value.
        value: String,
    },
}

impl ManifestError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::JsonSyntax { .. } => CODE_JSON_SYNTAX,
            Self::UnknownField { .. } => CODE_UNKNOWN_FIELD,
            Self::MissingField { .. } => CODE_MISSING_FIELD,
            Self::TypeMismatch { .. } => CODE_TYPE_MISMATCH,
            Self::InvalidValue { .. } => CODE_INVALID_VALUE,
            Self::DuplicateKey { .. } => CODE_DUPLICATE_KEY,
            Self::DuplicateName { .. } => CODE_DUPLICATE_NAME,
            Self::FloatNotAllowed { .. } => CODE_FLOAT_NOT_ALLOWED,
            Self::UnsupportedSchema { .. } => CODE_UNSUPPORTED_SCHEMA,
            Self::ReservedTableName { .. } => CODE_RESERVED_TABLE_NAME,
            Self::KeySchemaMissing { .. } => CODE_KEY_SCHEMA_MISSING,
            Self::IndexUnknownField { .. } => CODE_INDEX_UNKNOWN_FIELD,
            Self::IndexScopeUnreachable { .. } => CODE_INDEX_SCOPE_UNREACHABLE,
            Self::RuleUnknownPath { .. } => CODE_RULE_UNKNOWN_PATH,
            Self::RuleUnbounded { .. } => CODE_RULE_UNBOUNDED,
            Self::OperationCrossShard { .. } => CODE_OPERATION_CROSS_SHARD,
            Self::OperationUnknownField { .. } => CODE_OPERATION_UNKNOWN_FIELD,
            Self::ArtifactDrift { .. } => CODE_ARTIFACT_DRIFT,
            Self::PolicyCatalogMissing => CODE_POLICY_CATALOG_MISSING,
        }
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::JsonSyntax { detail } => format!("The manifest JSON is not valid: {detail}."),
            Self::DuplicateKey { key } => {
                format!("The JSON object holds the key {key} more than one time.")
            }
            Self::FloatNotAllowed { path } => {
                format!("The value at {path} is not an i64 integer.")
            }
            Self::UnsupportedSchema { schema } => {
                format!("The manifest schema {schema} is not supported.")
            }
            Self::UnknownField { path, field } => {
                format!("The field {field} at {path} is not declared.")
            }
            Self::DuplicateName { kind, name } => {
                format!("The {kind} name {name} appears more than one time.")
            }
            Self::ReservedTableName { name } => {
                format!("The user table name {name} is reserved.")
            }
            Self::KeySchemaMissing { table } => {
                format!("The table {table} has no key schema.")
            }
            Self::IndexUnknownField { index, field } => {
                format!("The index {index} names the undeclared field {field}.")
            }
            Self::IndexScopeUnreachable { index } => {
                format!("The index {index} serves no filter or search scope.")
            }
            Self::RuleUnknownPath { rule, path } => {
                format!("The rule {rule} names the unknown path {path}.")
            }
            Self::RuleUnbounded { rule, cost, budget } => {
                format!("The rule {rule} has cost {cost} above the budget {budget}.")
            }
            Self::OperationCrossShard { operation, field } => {
                format!("The operation {operation} names the field {field} on another table.")
            }
            Self::OperationUnknownField { operation, field } => {
                format!("The operation {operation} names the undeclared field {field}.")
            }
            Self::ArtifactDrift { expected, found } => {
                format!(
                    "The artifact digest {found} does not match the expected digest {expected}."
                )
            }
            Self::PolicyCatalogMissing => {
                "The manifest declares rules but no policy catalog.".to_owned()
            }
            Self::MissingField { path, field } => {
                format!("The field {field} at {path} is required.")
            }
            Self::TypeMismatch {
                path,
                expected,
                found,
            } => format!("The value at {path} must be {expected}, not {found}."),
            Self::InvalidValue { path, value } => {
                format!("The value {value} at {path} is not a declared name.")
            }
        };
        formatter.write_str(&message)
    }
}

impl std::error::Error for ManifestError {}
