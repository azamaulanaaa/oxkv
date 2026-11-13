//! The generated control namespace.
//!
//! Control operations use the same generator, error mapping, and
//! authentication as data operations. They differ only in their namespace and
//! their declared auth class. The manifest declares every operation. No
//! control method is hand-written.

/// The namespace of every control operation.
pub const CONTROL_NAMESPACE: &str = "control";

/// The path of every generated control operation.
pub const CONTROL_OPERATION_PATHS: [&str; 8] = [
    "control/audit_export",
    "control/backup",
    "control/dlq_redrive",
    "control/migrate",
    "control/quota",
    "control/restore",
    "control/rule_activate",
    "control/rule_rollback",
];
