//! Control operations as audited records.
//!
//! Every control operation uses the `control` namespace. A control action is
//! audited. A control action never impersonates a data write.

use oxkv_contracts::version::{ManifestDigest, MapVersion, ShardId};

/// One control operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlOperation {
    /// Capture a backup.
    Backup,
    /// Restore a backup.
    Restore,
    /// Run a migration.
    Migrate,
    /// Activate a rule version.
    RuleActivate,
    /// Roll a rule back.
    RuleRollback,
    /// Redrive a dead-letter record.
    DlqRedrive,
    /// Change a quota.
    Quota,
    /// Export the audit chain.
    AuditExport,
}

impl ControlOperation {
    /// Every control operation.
    pub const ALL: [Self; 8] = [
        Self::Backup,
        Self::Restore,
        Self::Migrate,
        Self::RuleActivate,
        Self::RuleRollback,
        Self::DlqRedrive,
        Self::Quota,
        Self::AuditExport,
    ];

    /// Returns the canonical operation name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::Restore => "restore",
            Self::Migrate => "migrate",
            Self::RuleActivate => "rule_activate",
            Self::RuleRollback => "rule_rollback",
            Self::DlqRedrive => "dlq_redrive",
            Self::Quota => "quota",
            Self::AuditExport => "audit_export",
        }
    }

    /// Returns the control namespace.
    #[must_use]
    pub const fn namespace() -> &'static str {
        "control"
    }

    /// Returns the full control method path.
    #[must_use]
    pub fn method(self) -> String {
        format!("{}/{}", Self::namespace(), self.name())
    }

    /// Returns true when the operation is a data write.
    ///
    /// A control operation never writes data outside a receipt. This method is
    /// always false.
    #[must_use]
    pub const fn is_data_write() -> bool {
        false
    }

    /// Returns true when the operation requires an audit record.
    #[must_use]
    pub const fn requires_audit() -> bool {
        true
    }
}

impl core::fmt::Display for ControlOperation {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The version that one control action names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ControlVersion {
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The shard map version.
    pub map_version: MapVersion,
}

impl ControlVersion {
    /// Creates a control version.
    #[must_use]
    pub const fn new(manifest_digest: ManifestDigest, map_version: MapVersion) -> Self {
        Self {
            manifest_digest,
            map_version,
        }
    }
}

/// The result of one control action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlResult {
    /// The action was accepted.
    Accepted,
    /// The action was refused with a named code.
    Rejected {
        /// The named error code.
        code: &'static str,
    },
}

impl ControlResult {
    /// Returns the result label.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected { .. } => "rejected",
        }
    }
}

/// One audit record for a control action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlAuditRecord {
    /// The control operation.
    pub operation: ControlOperation,
    /// The actor principal.
    pub actor: String,
    /// The target shard, when the action is shard-scoped.
    pub shard_id: Option<ShardId>,
    /// The version that the action names.
    pub version: ControlVersion,
    /// The action result.
    pub result: ControlResult,
}

impl ControlAuditRecord {
    /// Creates one control audit record.
    #[must_use]
    pub fn new(
        operation: ControlOperation,
        actor: impl Into<String>,
        shard_id: Option<ShardId>,
        version: ControlVersion,
        result: ControlResult,
    ) -> Self {
        Self {
            operation,
            actor: actor.into(),
            shard_id,
            version,
            result,
        }
    }
}

/// Builds one control audit record.
#[must_use]
pub fn audit_control(
    operation: ControlOperation,
    actor: impl Into<String>,
    shard_id: Option<ShardId>,
    version: ControlVersion,
    result: ControlResult,
) -> ControlAuditRecord {
    ControlAuditRecord::new(operation, actor, shard_id, version, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_operation_uses_the_control_namespace() {
        for operation in ControlOperation::ALL {
            assert_eq!(operation.method(), format!("control/{}", operation.name()));
            assert!(!ControlOperation::is_data_write());
            assert!(ControlOperation::requires_audit());
        }
        assert_eq!(ControlOperation::namespace(), "control");
    }

    #[test]
    fn a_control_action_is_audited_with_every_required_field() {
        let record = audit_control(
            ControlOperation::Backup,
            "operator",
            Some(ShardId::new(vec![1])),
            ControlVersion::new(ManifestDigest::from_bytes([1; 32]), MapVersion::new(2)),
            ControlResult::Accepted,
        );
        assert_eq!(record.actor, "operator");
        assert!(record.shard_id.is_some());
        assert_eq!(record.version.map_version, MapVersion::new(2));
        assert_eq!(record.result.name(), "accepted");
    }

    #[test]
    fn a_rejected_control_action_names_its_code() {
        let result = ControlResult::Rejected {
            code: "ops.quota_exceeded",
        };
        assert_eq!(result.name(), "rejected");
    }
}
