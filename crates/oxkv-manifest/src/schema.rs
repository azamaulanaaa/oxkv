//! The typed manifest v1 schema.
//!
//! The schema declares user tables by name and key schema. It does not declare
//! an arbitrary byte prefix and it does not declare placement. Rule bodies live
//! in a separate policy catalog. The manifest declares rule identifiers and
//! selectors only.

use core::fmt;
use std::collections::BTreeMap;

use oxkv_contracts::error::RetryClass;
use oxkv_contracts::version::ManifestDigest;

use crate::error::ManifestError;
use crate::json::CanonicalValue;

/// The manifest v1 schema version.
pub const SCHEMA_VERSION: &str = "oxkv/manifest/v1";

/// The static cost budget for one rule.
pub const RULE_COST_BUDGET: i64 = 1024;

/// The required digest algorithm in manifest v1.
pub const DIGEST_ALGORITHM: &str = "blake3";

/// Generates a text enum with one canonical name per value.
macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        $name:ident {
            $($variant:ident => $text:literal),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $(
                /// One declared value.
                $variant,
            )*
        }

        impl $name {
            /// Every value of the enum.
            pub const ALL: &'static [Self] = &[
                $(Self::$variant,)*
            ];

            /// Returns the canonical text.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)*
                }
            }

            /// Parses one canonical text.
            #[must_use]
            pub fn parse(text: &str) -> Option<Self> {
                match text {
                    $($text => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

text_enum!(
    /// The type of one key component.
    ComponentType {
        Text => "text",
        Bytes => "bytes",
        U64 => "u64",
        I64 => "i64",
        Bool => "bool",
    }
);

text_enum!(
    /// The canonical type of one field or fact.
    FieldType {
        Text => "text",
        Bytes => "bytes",
        U64 => "u64",
        I64 => "i64",
        Bool => "bool",
    }
);

text_enum!(
    /// The storage encoding of one field.
    Encoding {
        Utf8 => "utf8",
        Raw => "raw",
        U64Le => "u64_le",
        I64Le => "i64_le",
        BoolByte => "bool_byte",
    }
);

text_enum!(
    /// The effect of one rule.
    RuleEffect {
        Allow => "allow",
        Deny => "deny",
    }
);

text_enum!(
    /// The scope that one rule serves.
    RuleScope {
        Read => "read",
        Write => "write",
        Both => "both",
    }
);

text_enum!(
    /// One retention class from the retention registry.
    RetentionClass {
        Receipt => "receipt_retention",
        Event => "event_retention",
        Audit => "audit_retention",
        PolicyVersion => "policy_version_retention",
        ManifestRevision => "manifest_revision_retention",
        ReadPin => "read_pin_retention",
        SyncCursor => "sync_cursor_retention",
        SearchPin => "search_pin_retention",
        Projection => "projection_retention",
        Tombstone => "tombstone_retention",
        BackupPin => "backup_pin_retention",
        ConsumerCompletion => "consumer_completion_retention",
    }
);

text_enum!(
    /// The audit visibility of one rule or artifact.
    AuditVisibility {
        None => "none",
        Operator => "operator",
        Public => "public",
    }
);

text_enum!(
    /// The audit export class.
    ExportClass {
        Disabled => "disabled",
        OperationalFeed => "operational_feed",
        SignedExport => "signed_export",
    }
);

text_enum!(
    /// The streaming shape of one operation.
    StreamingShape {
        Unary => "unary",
        ServerStream => "server_stream",
        ClientStream => "client_stream",
        Bidi => "bidi",
    }
);

text_enum!(
    /// The authentication class of one operation.
    AuthClass {
        None => "none",
        Session => "session",
        Operator => "operator",
        Admin => "admin",
    }
);

text_enum!(
    /// The kind of one control operation.
    ControlKind {
        Backup => "backup",
        Restore => "restore",
        Migrate => "migrate",
        RuleActivate => "rule_activate",
        RuleRollback => "rule_rollback",
        DlqRedrive => "dlq_redrive",
        Quota => "quota",
        AuditExport => "audit_export",
    }
);

/// The schema block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    /// The manifest schema version.
    pub version: String,
    /// The monotonic manifest revision.
    pub revision: i64,
    /// The digest of the canonical manifest bytes. The digest excludes itself.
    pub digest: Option<ManifestDigest>,
}

/// One key schema component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyComponent {
    /// The component name.
    pub name: String,
    /// The component type.
    pub component_type: ComponentType,
}

/// One table field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The field name.
    pub name: String,
    /// The canonical field type.
    pub value_type: FieldType,
    /// The storage encoding.
    pub encoding: Encoding,
    /// True when a filter can use the field.
    pub filterable: bool,
    /// True when a query can sort on the field.
    pub sortable: bool,
    /// True when an index covers the field.
    pub indexed: bool,
}

/// The static limits of one table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableLimits {
    /// The maximum key length in bytes.
    pub max_key_bytes: i64,
    /// The maximum value length in bytes.
    pub max_value_bytes: i64,
}

/// One user table declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The table name.
    pub name: String,
    /// The ordered key schema.
    pub key_schema: Vec<KeyComponent>,
    /// The declared fields.
    pub fields: Vec<Field>,
    /// The static limits.
    pub limits: TableLimits,
}

impl Table {
    /// Returns true when the table declares the field or key component.
    #[must_use]
    pub fn has_field(&self, name: &str) -> bool {
        self.key_schema
            .iter()
            .any(|component| component.name == name)
            || self.fields.iter().any(|field| field.name == name)
    }
}

/// One index declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Index {
    /// The index name.
    pub name: String,
    /// The target table.
    pub table: String,
    /// The ordered field list.
    pub fields: Vec<String>,
    /// The tokenizer identity for a text index.
    pub tokenizer: Option<String>,
    /// True when the index stores positions.
    pub positions: bool,
    /// True when the index serves a filter.
    pub filter: bool,
    /// True when the index serves search.
    pub search: bool,
    /// The scorer identity.
    pub scorer: Option<String>,
    /// The overhead budget in bytes.
    pub overhead_budget: i64,
}

/// One rule declaration without a rule body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The rule identifier.
    pub name: String,
    /// The selector path as `table.field`.
    pub selector: String,
    /// The rule effect.
    pub effect: RuleEffect,
    /// The rule scope.
    pub scope: RuleScope,
    /// The static cost bound.
    pub cost_bound: i64,
    /// True when the rule uses strict authorization.
    pub strict: bool,
    /// The retention class.
    pub retention: RetentionClass,
    /// The audit visibility.
    pub audit: AuditVisibility,
    /// The fact read set.
    pub facts: Vec<String>,
}

/// The rule section. The policy catalog holds the rule bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSet {
    /// The policy catalog location. It is not a policy version.
    pub catalog: Option<String>,
    /// The rule declarations.
    pub declarations: Vec<Rule>,
}

/// One fact declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// The fact identity.
    pub name: String,
    /// The canonical fact type.
    pub value_type: FieldType,
    /// The logical owner of the fact.
    pub owner: String,
    /// The fan-in cap.
    pub fan_in_cap: i64,
    /// The staleness bound as a version distance.
    pub staleness_bound: i64,
}

/// One operation declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    /// The operation name.
    pub name: String,
    /// The target table.
    pub target: String,
    /// The request field paths.
    pub request: Vec<String>,
    /// The response field paths.
    pub response: Vec<String>,
    /// The streaming shape.
    pub streaming: StreamingShape,
    /// The declared error codes.
    pub errors: Vec<String>,
    /// The authentication class.
    pub auth: AuthClass,
}

/// One event declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The event kind.
    pub kind: String,
    /// The source table.
    pub table: String,
    /// The payload field paths.
    pub payload: Vec<String>,
    /// The required effect field paths.
    pub required: Vec<String>,
    /// The retention class.
    pub retention: RetentionClass,
}

/// The receipt section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipts {
    /// The receipt retention class.
    pub retention: RetentionClass,
    /// The declared reason codes.
    pub reason_codes: Vec<String>,
    /// The declared retry classes.
    pub retry_classes: Vec<RetryClass>,
}

/// The audit section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audit {
    /// The audit visibility.
    pub visibility: AuditVisibility,
    /// The audit retention class.
    pub retention: RetentionClass,
    /// The export class.
    pub export: ExportClass,
}

/// The projection section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    /// The epoch rule.
    pub epoch_rule: String,
    /// The watermark rule.
    pub watermark_rule: String,
    /// The invalidation causes.
    pub invalidation: Vec<String>,
}

/// The codec section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Codecs {
    /// The format versions.
    pub format_versions: Vec<String>,
    /// The tokenizer versions.
    pub tokenizer_versions: Vec<String>,
    /// The digest algorithm.
    pub digest_algorithm: String,
}

/// One control operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlOperation {
    /// The control operation name.
    pub name: String,
    /// The control kind.
    pub kind: ControlKind,
    /// The declared error codes.
    pub errors: Vec<String>,
    /// The authentication class.
    pub auth: AuthClass,
}

/// One manifest v1 document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The schema block.
    pub schema: Schema,
    /// The user tables.
    pub tables: Vec<Table>,
    /// The indexes.
    pub indexes: Vec<Index>,
    /// The rule section.
    pub rules: RuleSet,
    /// The facts.
    pub facts: Vec<Fact>,
    /// The operations.
    pub operations: Vec<Operation>,
    /// The events.
    pub events: Vec<Event>,
    /// The receipt section.
    pub receipts: Receipts,
    /// The audit section.
    pub audit: Audit,
    /// The projection section.
    pub projection: Projection,
    /// The codec section.
    pub codecs: Codecs,
    /// The control operations.
    pub control: Vec<ControlOperation>,
}

/// A reader for one canonical object.
struct Reader {
    /// The object path.
    path: String,
    /// The unread fields.
    fields: BTreeMap<String, CanonicalValue>,
}

impl Reader {
    /// Creates a reader.
    fn new(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let fields = match value {
            CanonicalValue::Object(fields) => fields,
            other => {
                return Err(ManifestError::TypeMismatch {
                    path: path.to_owned(),
                    expected: "an object",
                    found: other.kind(),
                });
            }
        };
        Ok(Self {
            path: path.to_owned(),
            fields,
        })
    }

    /// Takes one required field.
    fn take(&mut self, key: &str) -> Result<CanonicalValue, ManifestError> {
        self.fields
            .remove(key)
            .ok_or_else(|| ManifestError::MissingField {
                path: self.path.clone(),
                field: key.to_owned(),
            })
    }

    /// Takes one optional field.
    fn take_optional(&mut self, key: &str) -> Option<CanonicalValue> {
        self.fields.remove(key)
    }

    /// Rejects every unread field.
    fn finish(self) -> Result<(), ManifestError> {
        if let Some((field, _)) = self.fields.into_iter().next() {
            return Err(ManifestError::UnknownField {
                path: self.path,
                field,
            });
        }
        Ok(())
    }
}

/// Builds one canonical object.
fn object(entries: Vec<(&str, CanonicalValue)>) -> CanonicalValue {
    CanonicalValue::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

/// Builds one canonical string.
fn string(value: impl Into<String>) -> CanonicalValue {
    CanonicalValue::String(value.into())
}

/// Builds one canonical array.
fn array(values: impl IntoIterator<Item = CanonicalValue>) -> CanonicalValue {
    CanonicalValue::Array(values.into_iter().collect())
}

/// Builds one canonical integer.
fn integer(value: i64) -> CanonicalValue {
    CanonicalValue::Integer(value)
}

/// Builds one canonical boolean.
fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::Bool(value)
}

/// Returns a type mismatch.
fn type_mismatch(value: &CanonicalValue, path: &str, expected: &'static str) -> ManifestError {
    ManifestError::TypeMismatch {
        path: path.to_owned(),
        expected,
        found: value.kind(),
    }
}

/// Reads one object.
fn expect_object(value: CanonicalValue, path: &str) -> Result<Reader, ManifestError> {
    Reader::new(value, path)
}

/// Reads one string.
fn expect_string(value: &CanonicalValue, path: &str) -> Result<String, ManifestError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| type_mismatch(value, path, "a string"))
}

/// Reads one integer.
fn expect_i64(value: &CanonicalValue, path: &str) -> Result<i64, ManifestError> {
    value
        .as_i64()
        .ok_or_else(|| type_mismatch(value, path, "an integer"))
}

/// Reads one boolean.
fn expect_bool(value: &CanonicalValue, path: &str) -> Result<bool, ManifestError> {
    value
        .as_bool()
        .ok_or_else(|| type_mismatch(value, path, "a boolean"))
}

/// Reads one array.
fn expect_array<'a>(
    value: &'a CanonicalValue,
    path: &str,
) -> Result<&'a [CanonicalValue], ManifestError> {
    value
        .as_array()
        .ok_or_else(|| type_mismatch(value, path, "an array"))
}

/// Reads one text enum.
fn expect_enum<T>(
    value: &CanonicalValue,
    path: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T, ManifestError> {
    let text = expect_string(value, path)?;
    parse(&text).ok_or_else(|| ManifestError::InvalidValue {
        path: path.to_owned(),
        value: text,
    })
}

/// Returns the canonical name of one retry class.
#[must_use]
pub fn retry_class_name(retry: RetryClass) -> &'static str {
    retry.name()
}

/// Parses one retry class from its canonical name.
#[must_use]
pub fn retry_class_parse(text: &str) -> Option<RetryClass> {
    match text {
        "Never" => Some(RetryClass::Never),
        "Refetch" => Some(RetryClass::Refetch),
        "Regenerate" => Some(RetryClass::Regenerate),
        "After fix" => Some(RetryClass::AfterFix),
        "After approval" => Some(RetryClass::AfterApproval),
        "After rollout" => Some(RetryClass::AfterRollout),
        "After re-read" => Some(RetryClass::AfterReRead),
        "Resubmit" => Some(RetryClass::Resubmit),
        "After drain" => Some(RetryClass::AfterDrain),
        "Backoff" => Some(RetryClass::Backoff),
        "After rebuild" => Some(RetryClass::AfterRebuild),
        "After catch-up" => Some(RetryClass::AfterCatchUp),
        "Snapshot fallback" => Some(RetryClass::SnapshotFallback),
        "New cursor" => Some(RetryClass::NewCursor),
        "After auth" => Some(RetryClass::AfterAuth),
        "New pin" => Some(RetryClass::NewPin),
        "New query" => Some(RetryClass::NewQuery),
        "Narrow query" => Some(RetryClass::NarrowQuery),
        "Rebuild" => Some(RetryClass::Rebuild),
        "After re-acquire" => Some(RetryClass::AfterReAcquire),
        "After hint" => Some(RetryClass::AfterHint),
        "Operator action" => Some(RetryClass::OperatorAction),
        "Continue with cursor" => Some(RetryClass::ContinueWithCursor),
        "Snapshot backfill" => Some(RetryClass::SnapshotBackfill),
        "Dead-letter" => Some(RetryClass::DeadLetter),
        "After refresh" => Some(RetryClass::AfterRefresh),
        "Restart backup" => Some(RetryClass::RestartBackup),
        "Restart restore" => Some(RetryClass::RestartRestore),
        "Resume migration" => Some(RetryClass::ResumeMigration),
        "After rebase" => Some(RetryClass::AfterRebase),
        "After quota" => Some(RetryClass::AfterQuota),
        "Regenerate client" => Some(RetryClass::RegenerateClient),
        "Reconnect" => Some(RetryClass::Reconnect),
        "After maintenance" => Some(RetryClass::AfterMaintenance),
        "After change" => Some(RetryClass::AfterChange),
        "After policy sync" => Some(RetryClass::AfterPolicySync),
        _ => None,
    }
}

/// Parses one manifest digest from lowercase hexadecimal text.
fn expect_digest(value: &CanonicalValue, path: &str) -> Result<ManifestDigest, ManifestError> {
    let text = expect_string(value, path)?;
    let bytes = hex_decode(&text).ok_or_else(|| ManifestError::InvalidValue {
        path: path.to_owned(),
        value: text.clone(),
    })?;
    Ok(ManifestDigest::from_bytes(bytes))
}

/// Decodes 32 bytes of lowercase hexadecimal text.
fn hex_decode(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let high = hex_nibble(text.as_bytes()[index * 2])?;
        let low = hex_nibble(text.as_bytes()[index * 2 + 1])?;
        *byte = (high << 4) | low;
    }
    Some(bytes)
}

/// Returns one hexadecimal nibble.
fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

impl Schema {
    /// Encodes the schema block.
    fn to_value(&self, include_digest: bool) -> CanonicalValue {
        let mut entries = vec![
            ("revision", integer(self.revision)),
            ("version", string(&self.version)),
        ];
        if include_digest && let Some(digest) = self.digest {
            entries.push(("digest", string(digest.to_hex())));
        }
        object(entries)
    }

    /// Decodes the schema block.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let version = expect_string(&reader.take("version")?, &format!("{path}.version"))?;
        let revision = expect_i64(&reader.take("revision")?, &format!("{path}.revision"))?;
        let digest = match reader.take_optional("digest") {
            Some(value) => Some(expect_digest(&value, &format!("{path}.digest"))?),
            None => None,
        };
        reader.finish()?;
        Ok(Self {
            version,
            revision,
            digest,
        })
    }
}

impl KeyComponent {
    /// Encodes one key component.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("name", string(&self.name)),
            ("type", string(self.component_type.as_str())),
        ])
    }

    /// Decodes one key component.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let name = expect_string(&reader.take("name")?, &format!("{path}.name"))?;
        let component_type = expect_enum(
            &reader.take("type")?,
            &format!("{path}.type"),
            ComponentType::parse,
        )?;
        reader.finish()?;
        Ok(Self {
            name,
            component_type,
        })
    }
}

impl Field {
    /// Encodes one field.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("encoding", string(self.encoding.as_str())),
            ("filterable", boolean(self.filterable)),
            ("indexed", boolean(self.indexed)),
            ("name", string(&self.name)),
            ("sortable", boolean(self.sortable)),
            ("type", string(self.value_type.as_str())),
        ])
    }

    /// Decodes one field.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let name = expect_string(&reader.take("name")?, &format!("{path}.name"))?;
        let value_type = expect_enum(
            &reader.take("type")?,
            &format!("{path}.type"),
            FieldType::parse,
        )?;
        let encoding = expect_enum(
            &reader.take("encoding")?,
            &format!("{path}.encoding"),
            Encoding::parse,
        )?;
        let filterable = expect_bool(&reader.take("filterable")?, &format!("{path}.filterable"))?;
        let sortable = expect_bool(&reader.take("sortable")?, &format!("{path}.sortable"))?;
        let indexed = expect_bool(&reader.take("indexed")?, &format!("{path}.indexed"))?;
        reader.finish()?;
        Ok(Self {
            name,
            value_type,
            encoding,
            filterable,
            sortable,
            indexed,
        })
    }
}

impl TableLimits {
    /// Encodes the limits.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("max_key_bytes", integer(self.max_key_bytes)),
            ("max_value_bytes", integer(self.max_value_bytes)),
        ])
    }

    /// Decodes the limits.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let max_key_bytes = expect_i64(
            &reader.take("max_key_bytes")?,
            &format!("{path}.max_key_bytes"),
        )?;
        let max_value_bytes = expect_i64(
            &reader.take("max_value_bytes")?,
            &format!("{path}.max_value_bytes"),
        )?;
        reader.finish()?;
        Ok(Self {
            max_key_bytes,
            max_value_bytes,
        })
    }
}

impl Table {
    /// Encodes one table.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("fields", array(self.fields.iter().map(Field::to_value))),
            (
                "key_schema",
                array(self.key_schema.iter().map(KeyComponent::to_value)),
            ),
            ("limits", self.limits.to_value()),
            ("name", string(&self.name)),
        ])
    }

    /// Decodes one table.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let name = expect_string(&reader.take("name")?, &format!("{path}.name"))?;
        let key_schema = match reader.take_optional("key_schema") {
            Some(value) => {
                let items = expect_array(&value, &format!("{path}.key_schema"))?;
                let mut components = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    components.push(KeyComponent::from_value(
                        item.clone(),
                        &format!("{path}.key_schema[{index}]"),
                    )?);
                }
                if components.is_empty() {
                    return Err(ManifestError::KeySchemaMissing { table: name });
                }
                components
            }
            None => return Err(ManifestError::KeySchemaMissing { table: name }),
        };
        let fields_value = reader.take("fields")?;
        let field_items = expect_array(&fields_value, &format!("{path}.fields"))?;
        let mut fields = Vec::with_capacity(field_items.len());
        for (index, item) in field_items.iter().enumerate() {
            fields.push(Field::from_value(
                item.clone(),
                &format!("{path}.fields[{index}]"),
            )?);
        }
        let limits = TableLimits::from_value(reader.take("limits")?, &format!("{path}.limits"))?;
        reader.finish()?;
        Ok(Self {
            name,
            key_schema,
            fields,
            limits,
        })
    }
}

impl Index {
    /// Encodes one index.
    fn to_value(&self) -> CanonicalValue {
        let mut entries = vec![
            ("fields", array(self.fields.iter().map(string))),
            ("filter", boolean(self.filter)),
            ("name", string(&self.name)),
            ("overhead_budget", integer(self.overhead_budget)),
            ("positions", boolean(self.positions)),
            ("search", boolean(self.search)),
            ("table", string(&self.table)),
        ];
        if let Some(tokenizer) = &self.tokenizer {
            entries.push(("tokenizer", string(tokenizer)));
        }
        if let Some(scorer) = &self.scorer {
            entries.push(("scorer", string(scorer)));
        }
        object(entries)
    }

    /// Decodes one index.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let name = expect_string(&reader.take("name")?, &format!("{path}.name"))?;
        let table = expect_string(&reader.take("table")?, &format!("{path}.table"))?;
        let fields_value = reader.take("fields")?;
        let field_items = expect_array(&fields_value, &format!("{path}.fields"))?;
        let mut fields = Vec::with_capacity(field_items.len());
        for (index, item) in field_items.iter().enumerate() {
            fields.push(expect_string(item, &format!("{path}.fields[{index}]"))?);
        }
        let tokenizer = match reader.take_optional("tokenizer") {
            Some(value) => Some(expect_string(&value, &format!("{path}.tokenizer"))?),
            None => None,
        };
        let positions = expect_bool(&reader.take("positions")?, &format!("{path}.positions"))?;
        let filter = expect_bool(&reader.take("filter")?, &format!("{path}.filter"))?;
        let search = expect_bool(&reader.take("search")?, &format!("{path}.search"))?;
        let scorer = match reader.take_optional("scorer") {
            Some(value) => Some(expect_string(&value, &format!("{path}.scorer"))?),
            None => None,
        };
        let overhead_budget = expect_i64(
            &reader.take("overhead_budget")?,
            &format!("{path}.overhead_budget"),
        )?;
        reader.finish()?;
        Ok(Self {
            name,
            table,
            fields,
            tokenizer,
            positions,
            filter,
            search,
            scorer,
            overhead_budget,
        })
    }
}

impl Rule {
    /// Encodes one rule.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("audit", string(self.audit.as_str())),
            ("cost_bound", integer(self.cost_bound)),
            ("effect", string(self.effect.as_str())),
            ("facts", array(self.facts.iter().map(string))),
            ("name", string(&self.name)),
            ("retention", string(self.retention.as_str())),
            ("scope", string(self.scope.as_str())),
            ("selector", string(&self.selector)),
            ("strict", boolean(self.strict)),
        ])
    }

    /// Decodes one rule.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let name = expect_string(&reader.take("name")?, &format!("{path}.name"))?;
        let selector = expect_string(&reader.take("selector")?, &format!("{path}.selector"))?;
        let effect = expect_enum(
            &reader.take("effect")?,
            &format!("{path}.effect"),
            RuleEffect::parse,
        )?;
        let scope = expect_enum(
            &reader.take("scope")?,
            &format!("{path}.scope"),
            RuleScope::parse,
        )?;
        let cost_bound = expect_i64(&reader.take("cost_bound")?, &format!("{path}.cost_bound"))?;
        let strict = expect_bool(&reader.take("strict")?, &format!("{path}.strict"))?;
        let retention = expect_enum(
            &reader.take("retention")?,
            &format!("{path}.retention"),
            RetentionClass::parse,
        )?;
        let audit = expect_enum(
            &reader.take("audit")?,
            &format!("{path}.audit"),
            AuditVisibility::parse,
        )?;
        let facts_value = reader.take("facts")?;
        let fact_items = expect_array(&facts_value, &format!("{path}.facts"))?;
        let mut facts = Vec::with_capacity(fact_items.len());
        for (index, item) in fact_items.iter().enumerate() {
            facts.push(expect_string(item, &format!("{path}.facts[{index}]"))?);
        }
        reader.finish()?;
        Ok(Self {
            name,
            selector,
            effect,
            scope,
            cost_bound,
            strict,
            retention,
            audit,
            facts,
        })
    }
}

impl RuleSet {
    /// Encodes the rule section.
    fn to_value(&self) -> CanonicalValue {
        let mut entries = vec![(
            "declarations",
            array(self.declarations.iter().map(Rule::to_value)),
        )];
        if let Some(catalog) = &self.catalog {
            entries.push(("catalog", string(catalog)));
        }
        object(entries)
    }

    /// Decodes the rule section.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let catalog = match reader.take_optional("catalog") {
            Some(value) => Some(expect_string(&value, &format!("{path}.catalog"))?),
            None => None,
        };
        let declarations_value = reader.take("declarations")?;
        let declaration_items = expect_array(&declarations_value, &format!("{path}.declarations"))?;
        let mut declarations = Vec::with_capacity(declaration_items.len());
        for (index, item) in declaration_items.iter().enumerate() {
            declarations.push(Rule::from_value(
                item.clone(),
                &format!("{path}.declarations[{index}]"),
            )?);
        }
        reader.finish()?;
        Ok(Self {
            catalog,
            declarations,
        })
    }
}

impl Fact {
    /// Encodes one fact.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("fan_in_cap", integer(self.fan_in_cap)),
            ("name", string(&self.name)),
            ("owner", string(&self.owner)),
            ("staleness_bound", integer(self.staleness_bound)),
            ("type", string(self.value_type.as_str())),
        ])
    }

    /// Decodes one fact.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let name = expect_string(&reader.take("name")?, &format!("{path}.name"))?;
        let value_type = expect_enum(
            &reader.take("type")?,
            &format!("{path}.type"),
            FieldType::parse,
        )?;
        let owner = expect_string(&reader.take("owner")?, &format!("{path}.owner"))?;
        let fan_in_cap = expect_i64(&reader.take("fan_in_cap")?, &format!("{path}.fan_in_cap"))?;
        let staleness_bound = expect_i64(
            &reader.take("staleness_bound")?,
            &format!("{path}.staleness_bound"),
        )?;
        reader.finish()?;
        Ok(Self {
            name,
            value_type,
            owner,
            fan_in_cap,
            staleness_bound,
        })
    }
}

impl Operation {
    /// Encodes one operation.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("auth", string(self.auth.as_str())),
            ("errors", array(self.errors.iter().map(string))),
            ("name", string(&self.name)),
            ("request", array(self.request.iter().map(string))),
            ("response", array(self.response.iter().map(string))),
            ("streaming", string(self.streaming.as_str())),
            ("target", string(&self.target)),
        ])
    }

    /// Decodes one operation.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let name = expect_string(&reader.take("name")?, &format!("{path}.name"))?;
        let target = expect_string(&reader.take("target")?, &format!("{path}.target"))?;
        let request_value = reader.take("request")?;
        let request_items = expect_array(&request_value, &format!("{path}.request"))?;
        let mut request = Vec::with_capacity(request_items.len());
        for (index, item) in request_items.iter().enumerate() {
            request.push(expect_string(item, &format!("{path}.request[{index}]"))?);
        }
        let response_value = reader.take("response")?;
        let response_items = expect_array(&response_value, &format!("{path}.response"))?;
        let mut response = Vec::with_capacity(response_items.len());
        for (index, item) in response_items.iter().enumerate() {
            response.push(expect_string(item, &format!("{path}.response[{index}]"))?);
        }
        let streaming = expect_enum(
            &reader.take("streaming")?,
            &format!("{path}.streaming"),
            StreamingShape::parse,
        )?;
        let errors_value = reader.take("errors")?;
        let error_items = expect_array(&errors_value, &format!("{path}.errors"))?;
        let mut errors = Vec::with_capacity(error_items.len());
        for (index, item) in error_items.iter().enumerate() {
            errors.push(expect_string(item, &format!("{path}.errors[{index}]"))?);
        }
        let auth = expect_enum(
            &reader.take("auth")?,
            &format!("{path}.auth"),
            AuthClass::parse,
        )?;
        reader.finish()?;
        Ok(Self {
            name,
            target,
            request,
            response,
            streaming,
            errors,
            auth,
        })
    }
}

impl Event {
    /// Encodes one event.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("kind", string(&self.kind)),
            ("payload", array(self.payload.iter().map(string))),
            ("required", array(self.required.iter().map(string))),
            ("retention", string(self.retention.as_str())),
            ("table", string(&self.table)),
        ])
    }

    /// Decodes one event.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let kind = expect_string(&reader.take("kind")?, &format!("{path}.kind"))?;
        let table = expect_string(&reader.take("table")?, &format!("{path}.table"))?;
        let payload_value = reader.take("payload")?;
        let payload_items = expect_array(&payload_value, &format!("{path}.payload"))?;
        let mut payload = Vec::with_capacity(payload_items.len());
        for (index, item) in payload_items.iter().enumerate() {
            payload.push(expect_string(item, &format!("{path}.payload[{index}]"))?);
        }
        let required_value = reader.take("required")?;
        let required_items = expect_array(&required_value, &format!("{path}.required"))?;
        let mut required = Vec::with_capacity(required_items.len());
        for (index, item) in required_items.iter().enumerate() {
            required.push(expect_string(item, &format!("{path}.required[{index}]"))?);
        }
        let retention = expect_enum(
            &reader.take("retention")?,
            &format!("{path}.retention"),
            RetentionClass::parse,
        )?;
        reader.finish()?;
        Ok(Self {
            kind,
            table,
            payload,
            required,
            retention,
        })
    }
}

impl Receipts {
    /// Encodes the receipt section.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("reason_codes", array(self.reason_codes.iter().map(string))),
            ("retention", string(self.retention.as_str())),
            (
                "retry_classes",
                array(
                    self.retry_classes
                        .iter()
                        .map(|retry| string(retry_class_name(*retry))),
                ),
            ),
        ])
    }

    /// Decodes the receipt section.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let retention = expect_enum(
            &reader.take("retention")?,
            &format!("{path}.retention"),
            RetentionClass::parse,
        )?;
        let reason_value = reader.take("reason_codes")?;
        let reason_items = expect_array(&reason_value, &format!("{path}.reason_codes"))?;
        let mut reason_codes = Vec::with_capacity(reason_items.len());
        for (index, item) in reason_items.iter().enumerate() {
            reason_codes.push(expect_string(
                item,
                &format!("{path}.reason_codes[{index}]"),
            )?);
        }
        let retry_value = reader.take("retry_classes")?;
        let retry_items = expect_array(&retry_value, &format!("{path}.retry_classes"))?;
        let mut retry_classes = Vec::with_capacity(retry_items.len());
        for (index, item) in retry_items.iter().enumerate() {
            retry_classes.push(expect_enum(
                item,
                &format!("{path}.retry_classes[{index}]"),
                retry_class_parse,
            )?);
        }
        reader.finish()?;
        Ok(Self {
            retention,
            reason_codes,
            retry_classes,
        })
    }
}

impl Audit {
    /// Encodes the audit section.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("export", string(self.export.as_str())),
            ("retention", string(self.retention.as_str())),
            ("visibility", string(self.visibility.as_str())),
        ])
    }

    /// Decodes the audit section.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let visibility = expect_enum(
            &reader.take("visibility")?,
            &format!("{path}.visibility"),
            AuditVisibility::parse,
        )?;
        let retention = expect_enum(
            &reader.take("retention")?,
            &format!("{path}.retention"),
            RetentionClass::parse,
        )?;
        let export = expect_enum(
            &reader.take("export")?,
            &format!("{path}.export"),
            ExportClass::parse,
        )?;
        reader.finish()?;
        Ok(Self {
            visibility,
            retention,
            export,
        })
    }
}

impl Projection {
    /// Encodes the projection section.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("epoch_rule", string(&self.epoch_rule)),
            ("invalidation", array(self.invalidation.iter().map(string))),
            ("watermark_rule", string(&self.watermark_rule)),
        ])
    }

    /// Decodes the projection section.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let epoch_rule = expect_string(&reader.take("epoch_rule")?, &format!("{path}.epoch_rule"))?;
        let watermark_rule = expect_string(
            &reader.take("watermark_rule")?,
            &format!("{path}.watermark_rule"),
        )?;
        let invalidation_value = reader.take("invalidation")?;
        let invalidation_items =
            expect_array(&invalidation_value, &format!("{path}.invalidation"))?;
        let mut invalidation = Vec::with_capacity(invalidation_items.len());
        for (index, item) in invalidation_items.iter().enumerate() {
            invalidation.push(expect_string(
                item,
                &format!("{path}.invalidation[{index}]"),
            )?);
        }
        reader.finish()?;
        Ok(Self {
            epoch_rule,
            watermark_rule,
            invalidation,
        })
    }
}

impl Codecs {
    /// Encodes the codec section.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("digest_algorithm", string(&self.digest_algorithm)),
            (
                "format_versions",
                array(self.format_versions.iter().map(string)),
            ),
            (
                "tokenizer_versions",
                array(self.tokenizer_versions.iter().map(string)),
            ),
        ])
    }

    /// Decodes the codec section.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let digest_algorithm = expect_string(
            &reader.take("digest_algorithm")?,
            &format!("{path}.digest_algorithm"),
        )?;
        let format_value = reader.take("format_versions")?;
        let format_items = expect_array(&format_value, &format!("{path}.format_versions"))?;
        let mut format_versions = Vec::with_capacity(format_items.len());
        for (index, item) in format_items.iter().enumerate() {
            format_versions.push(expect_string(
                item,
                &format!("{path}.format_versions[{index}]"),
            )?);
        }
        let tokenizer_value = reader.take("tokenizer_versions")?;
        let tokenizer_items =
            expect_array(&tokenizer_value, &format!("{path}.tokenizer_versions"))?;
        let mut tokenizer_versions = Vec::with_capacity(tokenizer_items.len());
        for (index, item) in tokenizer_items.iter().enumerate() {
            tokenizer_versions.push(expect_string(
                item,
                &format!("{path}.tokenizer_versions[{index}]"),
            )?);
        }
        reader.finish()?;
        Ok(Self {
            format_versions,
            tokenizer_versions,
            digest_algorithm,
        })
    }
}

impl ControlOperation {
    /// Encodes one control operation.
    fn to_value(&self) -> CanonicalValue {
        object(vec![
            ("auth", string(self.auth.as_str())),
            ("errors", array(self.errors.iter().map(string))),
            ("kind", string(self.kind.as_str())),
            ("name", string(&self.name)),
        ])
    }

    /// Decodes one control operation.
    fn from_value(value: CanonicalValue, path: &str) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, path)?;
        let name = expect_string(&reader.take("name")?, &format!("{path}.name"))?;
        let kind = expect_enum(
            &reader.take("kind")?,
            &format!("{path}.kind"),
            ControlKind::parse,
        )?;
        let errors_value = reader.take("errors")?;
        let error_items = expect_array(&errors_value, &format!("{path}.errors"))?;
        let mut errors = Vec::with_capacity(error_items.len());
        for (index, item) in error_items.iter().enumerate() {
            errors.push(expect_string(item, &format!("{path}.errors[{index}]"))?);
        }
        let auth = expect_enum(
            &reader.take("auth")?,
            &format!("{path}.auth"),
            AuthClass::parse,
        )?;
        reader.finish()?;
        Ok(Self {
            name,
            kind,
            errors,
            auth,
        })
    }
}

impl Manifest {
    /// Encodes the manifest as one canonical value.
    ///
    /// The value includes the digest only when `include_digest` is true and the
    /// schema holds a digest.
    pub(crate) fn to_value(&self, include_digest: bool) -> CanonicalValue {
        object(vec![
            ("audit", self.audit.to_value()),
            ("codecs", self.codecs.to_value()),
            (
                "control",
                array(self.control.iter().map(ControlOperation::to_value)),
            ),
            ("events", array(self.events.iter().map(Event::to_value))),
            ("facts", array(self.facts.iter().map(Fact::to_value))),
            ("indexes", array(self.indexes.iter().map(Index::to_value))),
            (
                "operations",
                array(self.operations.iter().map(Operation::to_value)),
            ),
            ("projection", self.projection.to_value()),
            ("receipts", self.receipts.to_value()),
            ("rules", self.rules.to_value()),
            ("schema", self.schema.to_value(include_digest)),
            ("tables", array(self.tables.iter().map(Table::to_value))),
        ])
    }

    /// Decodes one manifest from one canonical value.
    ///
    /// # Errors
    ///
    /// Returns a [`ManifestError`] for an unknown field, a wrong type, or a
    /// missing required field.
    pub(crate) fn from_value(value: CanonicalValue) -> Result<Self, ManifestError> {
        let mut reader = expect_object(value, "manifest")?;
        let schema = Schema::from_value(reader.take("schema")?, "manifest.schema")?;
        let tables_value = reader.take("tables")?;
        let table_items = expect_array(&tables_value, "manifest.tables")?;
        let mut tables = Vec::with_capacity(table_items.len());
        for (index, item) in table_items.iter().enumerate() {
            tables.push(Table::from_value(
                item.clone(),
                &format!("manifest.tables[{index}]"),
            )?);
        }
        let indexes_value = reader.take("indexes")?;
        let index_items = expect_array(&indexes_value, "manifest.indexes")?;
        let mut indexes = Vec::with_capacity(index_items.len());
        for (index, item) in index_items.iter().enumerate() {
            indexes.push(Index::from_value(
                item.clone(),
                &format!("manifest.indexes[{index}]"),
            )?);
        }
        let rules = RuleSet::from_value(reader.take("rules")?, "manifest.rules")?;
        let facts_value = reader.take("facts")?;
        let fact_items = expect_array(&facts_value, "manifest.facts")?;
        let mut facts = Vec::with_capacity(fact_items.len());
        for (index, item) in fact_items.iter().enumerate() {
            facts.push(Fact::from_value(
                item.clone(),
                &format!("manifest.facts[{index}]"),
            )?);
        }
        let operations_value = reader.take("operations")?;
        let operation_items = expect_array(&operations_value, "manifest.operations")?;
        let mut operations = Vec::with_capacity(operation_items.len());
        for (index, item) in operation_items.iter().enumerate() {
            operations.push(Operation::from_value(
                item.clone(),
                &format!("manifest.operations[{index}]"),
            )?);
        }
        let events_value = reader.take("events")?;
        let event_items = expect_array(&events_value, "manifest.events")?;
        let mut events = Vec::with_capacity(event_items.len());
        for (index, item) in event_items.iter().enumerate() {
            events.push(Event::from_value(
                item.clone(),
                &format!("manifest.events[{index}]"),
            )?);
        }
        let receipts = Receipts::from_value(reader.take("receipts")?, "manifest.receipts")?;
        let audit = Audit::from_value(reader.take("audit")?, "manifest.audit")?;
        let projection = Projection::from_value(reader.take("projection")?, "manifest.projection")?;
        let codecs = Codecs::from_value(reader.take("codecs")?, "manifest.codecs")?;
        let control_value = reader.take("control")?;
        let control_items = expect_array(&control_value, "manifest.control")?;
        let mut control = Vec::with_capacity(control_items.len());
        for (index, item) in control_items.iter().enumerate() {
            control.push(ControlOperation::from_value(
                item.clone(),
                &format!("manifest.control[{index}]"),
            )?);
        }
        reader.finish()?;
        Ok(Self {
            schema,
            tables,
            indexes,
            rules,
            facts,
            operations,
            events,
            receipts,
            audit,
            projection,
            codecs,
            control,
        })
    }
}
