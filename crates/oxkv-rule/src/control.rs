//! Control operations for rule activation and rollback.
//!
//! The control operations live in the `control` RPC namespace. They use the
//! same error registry and the same authentication class as a data operation.

use oxkv_contracts::error::{ErrorSpec, lookup};

use crate::error::RULE_ERROR_CODES;

/// The RPC control namespace.
pub const CONTROL_NAMESPACE: &str = "control";

/// The activation control method.
pub const RULE_ACTIVATE: &str = "control/rule_activate";

/// The rollback control method.
pub const RULE_ROLLBACK: &str = "control/rule_rollback";

/// The authentication class of one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthClass {
    /// The data-plane authentication class.
    Data,
}

impl AuthClass {
    /// Returns the canonical class name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Data => "data",
        }
    }
}

/// The authentication class that data operations use.
pub const DATA_AUTH_CLASS: AuthClass = AuthClass::Data;

/// One generated control operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlOperation {
    /// The full method name.
    pub name: &'static str,
    /// The authentication class.
    pub auth_class: AuthClass,
    /// The error map.
    pub errors: &'static [&'static str],
}

impl ControlOperation {
    /// Returns true when the method uses the control namespace.
    #[must_use]
    pub fn is_control_namespace(self) -> bool {
        self.name.starts_with("control/")
    }
}

/// The control operation table.
pub static CONTROL_OPERATIONS: &[ControlOperation] = &[
    ControlOperation {
        name: RULE_ACTIVATE,
        auth_class: DATA_AUTH_CLASS,
        errors: RULE_ERROR_CODES,
    },
    ControlOperation {
        name: RULE_ROLLBACK,
        auth_class: DATA_AUTH_CLASS,
        errors: RULE_ERROR_CODES,
    },
];

/// Returns the rule control error map.
#[must_use]
pub const fn error_map() -> &'static [&'static str] {
    RULE_ERROR_CODES
}

/// Looks up one error registry entry.
#[must_use]
pub fn error_spec(code: &str) -> Option<&'static ErrorSpec> {
    lookup(code)
}

/// Returns one control operation by name.
#[must_use]
pub fn operation(name: &str) -> Option<&'static ControlOperation> {
    CONTROL_OPERATIONS
        .iter()
        .find(|operation| operation.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_method_uses_the_control_namespace() {
        assert_eq!(CONTROL_OPERATIONS.len(), 2);
        for operation in CONTROL_OPERATIONS {
            assert!(operation.is_control_namespace());
        }
    }

    #[test]
    fn every_control_error_is_registered() {
        for operation in CONTROL_OPERATIONS {
            for code in operation.errors {
                assert!(
                    error_spec(code).is_some(),
                    "the error code {code} is not registered"
                );
            }
        }
    }

    #[test]
    fn a_control_operation_uses_the_data_auth_class() {
        for operation in CONTROL_OPERATIONS {
            assert_eq!(operation.auth_class, DATA_AUTH_CLASS);
        }
    }
}
