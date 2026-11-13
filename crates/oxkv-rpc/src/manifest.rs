//! Manifest declarations that compile to the neutral descriptor.
//!
//! The manifest is the single source of the RPC surface. This module defines
//! the declaration shape that the compiler consumes. It does not define a
//! manifest file format. A manifest compiler supplies the digest and the
//! declarations.
//!
//! [`reference_manifest`] returns the fixed declaration set for the generated
//! reference surface. The set covers data operations, sync operations, and the
//! eight control operations.

use oxkv_contracts::version::ManifestDigest;

use crate::descriptor::{AuthClass, FieldCardinality, Namespace, StreamingShape, WireType};

/// The fixed manifest digest of the reference declaration set.
///
/// The value is a test input. A production caller supplies the digest that its
/// manifest compiler emits.
pub const REFERENCE_MANIFEST_DIGEST: ManifestDigest = ManifestDigest::from_bytes([0x25; 32]);

/// One field declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    number: u32,
    name: String,
    cardinality: FieldCardinality,
    wire_type: WireType,
    message_type: Option<String>,
}

impl FieldDecl {
    /// Creates a scalar field declaration.
    #[must_use]
    pub fn new(
        number: u32,
        name: impl Into<String>,
        cardinality: FieldCardinality,
        wire_type: WireType,
    ) -> Self {
        Self {
            number,
            name: name.into(),
            cardinality,
            wire_type,
            message_type: None,
        }
    }

    /// Creates a message field declaration.
    #[must_use]
    pub fn message(
        number: u32,
        name: impl Into<String>,
        cardinality: FieldCardinality,
        message_type: impl Into<String>,
    ) -> Self {
        Self {
            number,
            name: name.into(),
            cardinality,
            wire_type: WireType::Message,
            message_type: Some(message_type.into()),
        }
    }

    /// Returns the field number.
    #[must_use]
    pub const fn number(&self) -> u32 {
        self.number
    }

    /// Returns the field name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the field cardinality.
    #[must_use]
    pub const fn cardinality(&self) -> FieldCardinality {
        self.cardinality
    }

    /// Returns the field wire type.
    #[must_use]
    pub const fn wire_type(&self) -> WireType {
        self.wire_type
    }

    /// Returns the message type name of a message field.
    #[must_use]
    pub fn message_type(&self) -> Option<&str> {
        self.message_type.as_deref()
    }
}

/// One message declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageDecl {
    name: String,
    fields: Vec<FieldDecl>,
}

impl MessageDecl {
    /// Creates a message declaration.
    #[must_use]
    pub fn new(name: impl Into<String>, fields: Vec<FieldDecl>) -> Self {
        Self {
            name: name.into(),
            fields,
        }
    }

    /// Returns the message name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the declared fields.
    #[must_use]
    pub fn fields(&self) -> &[FieldDecl] {
        &self.fields
    }
}

/// One operation declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationDecl {
    path: String,
    namespace: Namespace,
    request: String,
    response: String,
    streaming: StreamingShape,
    auth: AuthClass,
    errors: Vec<&'static str>,
}

impl OperationDecl {
    /// Creates an operation declaration.
    #[must_use]
    pub fn new(
        path: impl Into<String>,
        namespace: Namespace,
        request: impl Into<String>,
        response: impl Into<String>,
        streaming: StreamingShape,
        auth: AuthClass,
        errors: Vec<&'static str>,
    ) -> Self {
        Self {
            path: path.into(),
            namespace,
            request: request.into(),
            response: response.into(),
            streaming,
            auth,
            errors,
        }
    }

    /// Returns the method path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the method namespace.
    #[must_use]
    pub const fn namespace(&self) -> Namespace {
        self.namespace
    }

    /// Returns the request message name.
    #[must_use]
    pub fn request(&self) -> &str {
        &self.request
    }

    /// Returns the response message name.
    #[must_use]
    pub fn response(&self) -> &str {
        &self.response
    }

    /// Returns the streaming shape.
    #[must_use]
    pub const fn streaming(&self) -> StreamingShape {
        self.streaming
    }

    /// Returns the auth class.
    #[must_use]
    pub const fn auth(&self) -> AuthClass {
        self.auth
    }

    /// Returns the declared error codes.
    #[must_use]
    pub fn errors(&self) -> &[&'static str] {
        &self.errors
    }
}

/// One manifest declaration set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestSurface {
    manifest_digest: ManifestDigest,
    messages: Vec<MessageDecl>,
    operations: Vec<OperationDecl>,
}

impl ManifestSurface {
    /// Creates a manifest declaration set.
    #[must_use]
    pub fn new(
        manifest_digest: ManifestDigest,
        messages: Vec<MessageDecl>,
        operations: Vec<OperationDecl>,
    ) -> Self {
        Self {
            manifest_digest,
            messages,
            operations,
        }
    }

    /// Returns the manifest digest.
    #[must_use]
    pub const fn manifest_digest(&self) -> ManifestDigest {
        self.manifest_digest
    }

    /// Returns the message declarations.
    #[must_use]
    pub fn messages(&self) -> &[MessageDecl] {
        &self.messages
    }

    /// Returns the operation declarations.
    #[must_use]
    pub fn operations(&self) -> &[OperationDecl] {
        &self.operations
    }
}

/// Returns the fixed reference manifest declaration set.
///
/// The set has six data methods, four sync methods, and eight control methods.
/// Every control method is in the `control` namespace.
#[must_use]
pub fn reference_manifest() -> ManifestSurface {
    let mut messages = data_messages();
    messages.extend(sync_messages());
    messages.extend(control_messages());
    let mut operations = data_operations();
    operations.extend(sync_operations());
    operations.extend(control_operations());
    ManifestSurface::new(REFERENCE_MANIFEST_DIGEST, messages, operations)
}

macro_rules! message {
    ($name:literal, $(($number:literal, $field:literal, $cardinality:ident, $wire:ident)),* $(,)?) => {
        MessageDecl::new(
            $name,
            vec![
                $(FieldDecl::new(
                    $number,
                    $field,
                    FieldCardinality::$cardinality,
                    WireType::$wire,
                )),*
            ],
        )
    };
}

fn data_messages() -> Vec<MessageDecl> {
    vec![
        message!(
            "GetRequest",
            (1, "table", Required, String),
            (2, "key", Required, Bytes),
            (3, "read_pin", Optional, Bytes),
        ),
        message!(
            "GetResponse",
            (1, "value", Optional, Bytes),
            (2, "commit_version", Required, Uint64),
        ),
        message!(
            "PutRequest",
            (1, "table", Required, String),
            (2, "key", Required, Bytes),
            (3, "value", Required, Bytes),
            (4, "idempotency_key", Required, Bytes),
            (5, "base_version", Required, Uint64),
        ),
        message!(
            "PutResponse",
            (1, "receipt_id", Required, Bytes),
            (2, "receipt_state", Required, Uint32),
            (3, "commit_version", Optional, Uint64),
        ),
        message!(
            "DeleteRequest",
            (1, "table", Required, String),
            (2, "key", Required, Bytes),
            (3, "idempotency_key", Required, Bytes),
            (4, "base_version", Required, Uint64),
        ),
        message!(
            "DeleteResponse",
            (1, "receipt_id", Required, Bytes),
            (2, "receipt_state", Required, Uint32),
            (3, "commit_version", Optional, Uint64),
        ),
        message!(
            "QueryRequest",
            (1, "scope", Required, Bytes),
            (2, "cursor", Optional, Bytes),
            (3, "limit", Required, Uint32),
        ),
        message!(
            "QueryResponse",
            (1, "page", Required, Bytes),
            (2, "cursor", Optional, Bytes),
        ),
        message!(
            "SearchRequest",
            (1, "index", Required, String),
            (2, "text", Required, String),
            (3, "cursor", Optional, Bytes),
            (4, "limit", Required, Uint32),
        ),
        message!(
            "SearchResponse",
            (1, "page", Required, Bytes),
            (2, "cursor", Optional, Bytes),
        ),
        message!("ReceiptGetRequest", (1, "receipt_id", Required, Bytes),),
        message!(
            "ReceiptGetResponse",
            (1, "receipt_state", Required, Uint32),
            (2, "commit_version", Optional, Uint64),
            (3, "reason_code", Optional, String),
        ),
    ]
}

fn sync_messages() -> Vec<MessageDecl> {
    vec![
        message!(
            "SubscribeRequest",
            (1, "scope", Required, Bytes),
            (2, "policy_version", Required, Bytes),
            (3, "cursor", Optional, Bytes),
            (4, "scope_pin", Optional, Bytes),
        ),
        message!(
            "SubscribeResponse",
            (1, "subscription_id", Required, Bytes),
            (2, "cursor", Required, Bytes),
        ),
        message!(
            "ResumeRequest",
            (1, "subscription_id", Required, Bytes),
            (2, "cursor", Required, Bytes),
        ),
        message!("ResumeResponse", (1, "cursor", Required, Bytes),),
        message!(
            "SnapshotPage",
            (1, "page", Required, Bytes),
            (2, "page_sequence", Required, Uint64),
        ),
        message!("DiffFrame", (1, "frame", Required, Bytes),),
        message!("ReceiptFrame", (1, "receipt", Required, Bytes),),
    ]
}

fn control_messages() -> Vec<MessageDecl> {
    vec![
        message!(
            "BackupRequest",
            (1, "backup_id", Required, Bytes),
            (2, "retention_pin", Optional, Uint64),
        ),
        message!("BackupResponse", (1, "backup_pin", Required, Bytes),),
        message!(
            "RestoreRequest",
            (1, "backup_id", Required, Bytes),
            (2, "verify_only", Optional, Bool),
        ),
        message!("RestoreResponse", (1, "restored_epoch", Required, Uint64),),
        message!(
            "MigrateRequest",
            (1, "target_epoch", Required, Uint64),
            (2, "quiesce", Required, Bool),
        ),
        message!("MigrateResponse", (1, "migration_id", Required, Bytes),),
        message!(
            "RuleActivateRequest",
            (1, "rule_id", Required, String),
            (2, "policy_version", Required, Bytes),
        ),
        message!("RuleActivateResponse", (1, "receipt_id", Required, Bytes),),
        message!(
            "RuleRollbackRequest",
            (1, "rule_id", Required, String),
            (2, "policy_version", Required, Bytes),
        ),
        message!("RuleRollbackResponse", (1, "receipt_id", Required, Bytes),),
        message!(
            "DlqRedriveRequest",
            (1, "dead_letter_id", Required, Bytes),
            (2, "cursor", Optional, Bytes),
        ),
        message!("DlqRedriveResponse", (1, "cursor", Required, Bytes),),
        message!(
            "QuotaRequest",
            (1, "shard_id", Required, Bytes),
            (2, "quota_bytes", Required, Uint64),
        ),
        message!("QuotaResponse", (1, "quota_version", Required, Uint64),),
        message!(
            "AuditExportRequest",
            (1, "since_sequence", Optional, Uint64),
            (2, "shard_id", Optional, Bytes),
        ),
        message!("AuditExportResponse", (1, "page", Required, Bytes),),
    ]
}

fn data_operations() -> Vec<OperationDecl> {
    vec![
        OperationDecl::new(
            "data/get",
            Namespace::Data,
            "GetRequest",
            "GetResponse",
            StreamingShape::Unary,
            AuthClass::Authenticated,
            vec!["key.not_found", "read.pin_expired", "read.stale"],
        ),
        OperationDecl::new(
            "data/put",
            Namespace::Data,
            "PutRequest",
            "PutResponse",
            StreamingShape::Unary,
            AuthClass::Authenticated,
            vec![
                "auth.unauthenticated",
                "shard.cross_mutation_rejected",
                "write.idempotency_key_reuse",
                "write.missing_base_version",
                "write.version_conflict",
            ],
        ),
        OperationDecl::new(
            "data/delete",
            Namespace::Data,
            "DeleteRequest",
            "DeleteResponse",
            StreamingShape::Unary,
            AuthClass::Authenticated,
            vec![
                "auth.unauthenticated",
                "shard.cross_mutation_rejected",
                "write.idempotency_key_reuse",
                "write.missing_base_version",
                "write.version_conflict",
            ],
        ),
        OperationDecl::new(
            "data/query",
            Namespace::Data,
            "QueryRequest",
            "QueryResponse",
            StreamingShape::ServerStream,
            AuthClass::Authenticated,
            vec![
                "auth.unauthenticated",
                "query.budget_exceeded",
                "read.stale",
            ],
        ),
        OperationDecl::new(
            "data/search",
            Namespace::Data,
            "SearchRequest",
            "SearchResponse",
            StreamingShape::ServerStream,
            AuthClass::Authenticated,
            vec![
                "auth.unauthenticated",
                "search.cursor_invalid",
                "search.fanout_exceeded",
            ],
        ),
        OperationDecl::new(
            "data/receipt_get",
            Namespace::Data,
            "ReceiptGetRequest",
            "ReceiptGetResponse",
            StreamingShape::Unary,
            AuthClass::Authenticated,
            vec![
                "auth.unauthenticated",
                "receipt.expired",
                "receipt.not_found",
            ],
        ),
    ]
}

fn sync_operations() -> Vec<OperationDecl> {
    vec![
        OperationDecl::new(
            "sync/subscribe",
            Namespace::Sync,
            "SubscribeRequest",
            "SubscribeResponse",
            StreamingShape::ServerStream,
            AuthClass::Authenticated,
            vec![
                "auth.unauthenticated",
                "scope.device_budget_exceeded",
                "scope.over_limit",
                "sync.negotiation_required",
                "sync.scope_rejected",
                "sync.unauthorized",
            ],
        ),
        OperationDecl::new(
            "sync/resume",
            Namespace::Sync,
            "ResumeRequest",
            "ResumeResponse",
            StreamingShape::ServerStream,
            AuthClass::Authenticated,
            vec![
                "sync.cursor_expired",
                "sync.cursor_invalid",
                "sync.negotiation_required",
                "sync.unauthorized",
            ],
        ),
        OperationDecl::new(
            "sync/ack",
            Namespace::Sync,
            "ResumeRequest",
            "ResumeResponse",
            StreamingShape::Unary,
            AuthClass::Authenticated,
            vec!["sync.negotiation_required", "sync.unauthorized"],
        ),
        OperationDecl::new(
            "sync/receipts",
            Namespace::Sync,
            "ReceiptGetRequest",
            "ReceiptFrame",
            StreamingShape::ServerStream,
            AuthClass::Authenticated,
            vec![
                "receipt.expired",
                "receipt.not_found",
                "sync.negotiation_required",
            ],
        ),
    ]
}

fn control_operations() -> Vec<OperationDecl> {
    vec![
        OperationDecl::new(
            "control/backup",
            Namespace::Control,
            "BackupRequest",
            "BackupResponse",
            StreamingShape::Unary,
            AuthClass::Operator,
            vec!["auth.forbidden", "ops.backup_pin_lost", "ops.rate_limited"],
        ),
        OperationDecl::new(
            "control/restore",
            Namespace::Control,
            "RestoreRequest",
            "RestoreResponse",
            StreamingShape::Unary,
            AuthClass::Operator,
            vec![
                "auth.forbidden",
                "ops.downgrade_not_allowed",
                "ops.restore_checksum_mismatch",
            ],
        ),
        OperationDecl::new(
            "control/migrate",
            Namespace::Control,
            "MigrateRequest",
            "MigrateResponse",
            StreamingShape::Unary,
            AuthClass::Operator,
            vec![
                "auth.forbidden",
                "ops.downgrade_not_allowed",
                "ops.migration_incomplete",
                "shard.frozen_for_maintenance",
            ],
        ),
        OperationDecl::new(
            "control/rule_activate",
            Namespace::Control,
            "RuleActivateRequest",
            "RuleActivateResponse",
            StreamingShape::Unary,
            AuthClass::Operator,
            vec![
                "auth.forbidden",
                "receipt.state_invalid",
                "rule.approval_required",
                "rule.compile_failed",
            ],
        ),
        OperationDecl::new(
            "control/rule_rollback",
            Namespace::Control,
            "RuleRollbackRequest",
            "RuleRollbackResponse",
            StreamingShape::Unary,
            AuthClass::Operator,
            vec![
                "auth.forbidden",
                "receipt.state_invalid",
                "rule.approval_required",
                "rule.rollout_limit",
            ],
        ),
        OperationDecl::new(
            "control/dlq_redrive",
            Namespace::Control,
            "DlqRedriveRequest",
            "DlqRedriveResponse",
            StreamingShape::Unary,
            AuthClass::Operator,
            vec![
                "auth.forbidden",
                "consumer.cursor_expired",
                "consumer.dead_lettered",
            ],
        ),
        OperationDecl::new(
            "control/quota",
            Namespace::Control,
            "QuotaRequest",
            "QuotaResponse",
            StreamingShape::Unary,
            AuthClass::Operator,
            vec!["auth.forbidden", "ops.quota_exceeded", "ops.rate_limited"],
        ),
        OperationDecl::new(
            "control/audit_export",
            Namespace::Control,
            "AuditExportRequest",
            "AuditExportResponse",
            StreamingShape::ServerStream,
            AuthClass::Operator,
            vec!["audit.export_denied", "auth.forbidden", "ops.rate_limited"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_manifest_has_the_declared_surface() {
        let manifest = reference_manifest();
        assert_eq!(manifest.operations().len(), 18);
        assert_eq!(manifest.messages().len(), 35);
    }

    #[test]
    fn every_control_operation_uses_the_control_namespace() {
        let manifest = reference_manifest();
        let control: Vec<&OperationDecl> = manifest
            .operations()
            .iter()
            .filter(|operation| operation.namespace() == Namespace::Control)
            .collect();
        assert_eq!(control.len(), 8);
        for operation in control {
            assert!(operation.path().starts_with("control/"));
            assert_eq!(operation.auth(), AuthClass::Operator);
        }
    }

    #[test]
    fn a_scalar_field_has_no_message_type() {
        let field = FieldDecl::new(1, "value", FieldCardinality::Required, WireType::Bytes);
        assert_eq!(field.message_type(), None);
    }

    #[test]
    fn a_message_field_keeps_its_type() {
        let field = FieldDecl::message(1, "child", FieldCardinality::Optional, "Child");
        assert_eq!(field.wire_type(), WireType::Message);
        assert_eq!(field.message_type(), Some("Child"));
    }
}
