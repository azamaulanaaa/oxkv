//! Named hook errors from the canonical registry.

use core::fmt;

use oxkv_contracts::InternalDiagnostic;

use crate::batch::Key;
use crate::rule::RuleId;

/// One named error code for a hook failure.
///
/// Each code appears in the canonical registry in [`oxkv_contracts::error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HookErrorCode {
    /// A rule reads a field or key that the envelope does not carry.
    PolicyUnknownField,
    /// A step or event count budget is exhausted.
    PolicyBudgetExhausted,
    /// A put value exceeds the value budget.
    WriteValueTooLarge,
    /// A batch exceeds the operation budget.
    WriteBatchTooLarge,
    /// A notification payload exceeds the event budget.
    EventPayloadTooLarge,
    /// A pipeline invariant failed.
    InternalInvariantViolation,
}

impl HookErrorCode {
    /// Every hook error code.
    pub const ALL: [Self; 6] = [
        Self::PolicyUnknownField,
        Self::PolicyBudgetExhausted,
        Self::WriteValueTooLarge,
        Self::WriteBatchTooLarge,
        Self::EventPayloadTooLarge,
        Self::InternalInvariantViolation,
    ];

    /// Returns the canonical registry code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PolicyUnknownField => "policy.unknown_field",
            Self::PolicyBudgetExhausted => "policy.budget_exhausted",
            Self::WriteValueTooLarge => "write.value_too_large",
            Self::WriteBatchTooLarge => "write.batch_too_large",
            Self::EventPayloadTooLarge => "event.payload_too_large",
            Self::InternalInvariantViolation => "internal.invariant_violation",
        }
    }

    /// Reads one hook error code from its registry text.
    #[must_use]
    pub fn from_registry_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == code)
    }
}

impl fmt::Display for HookErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A text message with a fixed byte limit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundedText(String);

impl BoundedText {
    /// The maximum message length in bytes.
    const MAX_BYTES: usize = 256;

    fn new(text: impl Into<String>) -> Self {
        let mut text = text.into();
        if text.len() > Self::MAX_BYTES {
            let mut end = Self::MAX_BYTES;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        Self(text)
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// One failure from a hook or from the pipeline.
///
/// The error carries one named code. The pipeline adds the rule id, the
/// operation index, and the key when they are absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookError {
    code: HookErrorCode,
    rule_id: Option<RuleId>,
    op_index: Option<u32>,
    key: Option<Key>,
    path: Option<BoundedText>,
    message: BoundedText,
}

impl HookError {
    /// Creates a hook error with one named code.
    #[must_use]
    pub fn new(code: HookErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            rule_id: None,
            op_index: None,
            key: None,
            path: None,
            message: BoundedText::new(message),
        }
    }

    /// Creates a `policy.unknown_field` error.
    #[must_use]
    pub fn unknown_field(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(HookErrorCode::PolicyUnknownField, message).with_path(path)
    }

    /// Creates a `policy.budget_exhausted` error.
    #[must_use]
    pub fn budget_exhausted(message: impl Into<String>) -> Self {
        Self::new(HookErrorCode::PolicyBudgetExhausted, message)
    }

    /// Creates a `write.value_too_large` error.
    #[must_use]
    pub fn value_too_large(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(HookErrorCode::WriteValueTooLarge, message).with_path(path)
    }

    /// Creates a `write.batch_too_large` error.
    #[must_use]
    pub fn batch_too_large(message: impl Into<String>) -> Self {
        Self::new(HookErrorCode::WriteBatchTooLarge, message)
    }

    /// Creates an `event.payload_too_large` error.
    #[must_use]
    pub fn event_payload_too_large(message: impl Into<String>) -> Self {
        Self::new(HookErrorCode::EventPayloadTooLarge, message)
    }

    /// Creates an `internal.invariant_violation` error.
    #[must_use]
    pub fn invariant(message: impl Into<String>) -> Self {
        Self::new(HookErrorCode::InternalInvariantViolation, message)
    }

    /// Adds the value path to the error.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(BoundedText::new(path));
        self
    }

    /// Adds the rule id to the error.
    #[must_use]
    pub fn with_rule_id(mut self, rule_id: RuleId) -> Self {
        self.rule_id = Some(rule_id);
        self
    }

    /// Adds the operation index to the error.
    #[must_use]
    pub const fn with_op_index(mut self, op_index: u32) -> Self {
        self.op_index = Some(op_index);
        self
    }

    /// Adds the key to the error.
    #[must_use]
    pub fn with_key(mut self, key: Key) -> Self {
        self.key = Some(key);
        self
    }

    /// Adds context that the error does not carry.
    #[must_use]
    pub(crate) fn attach(mut self, rule_id: &RuleId, op_index: u32, key: &Key) -> Self {
        self.rule_id.get_or_insert_with(|| rule_id.clone());
        self.op_index.get_or_insert(op_index);
        self.key.get_or_insert_with(|| key.clone());
        self
    }

    /// Returns the named error code.
    #[must_use]
    pub const fn code(&self) -> HookErrorCode {
        self.code
    }

    /// Returns the canonical registry code.
    #[must_use]
    pub const fn code_str(&self) -> &'static str {
        self.code.as_str()
    }

    /// Returns the operator diagnostic for an internal invariant violation.
    ///
    /// The diagnostic is for operators and logs. It is not a wire value. The
    /// public code stays `internal.invariant_violation`.
    #[must_use]
    pub const fn diagnostic(&self) -> Option<InternalDiagnostic> {
        match self.code {
            HookErrorCode::InternalInvariantViolation => Some(InternalDiagnostic::HookPipeline),
            HookErrorCode::PolicyUnknownField
            | HookErrorCode::PolicyBudgetExhausted
            | HookErrorCode::WriteValueTooLarge
            | HookErrorCode::WriteBatchTooLarge
            | HookErrorCode::EventPayloadTooLarge => None,
        }
    }

    /// Returns the rule id when the error names one.
    #[must_use]
    pub fn rule_id(&self) -> Option<&RuleId> {
        self.rule_id.as_ref()
    }

    /// Returns the operation index when the error names one.
    #[must_use]
    pub const fn op_index(&self) -> Option<u32> {
        self.op_index
    }

    /// Returns the key when the error names one.
    #[must_use]
    pub fn key(&self) -> Option<&Key> {
        self.key.as_ref()
    }

    /// Returns the value path when the error names one.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        self.path.as_ref().map(BoundedText::as_str)
    }

    /// Returns the bounded message.
    #[must_use]
    pub fn message(&self) -> &str {
        self.message.as_str()
    }
}

impl fmt::Display for HookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "The hook failed with {}.", self.code)?;
        if let Some(rule_id) = &self.rule_id {
            write!(formatter, " The rule is {rule_id}.")?;
        }
        if let Some(op_index) = self.op_index {
            write!(formatter, " The operation index is {op_index}.")?;
        }
        write!(formatter, " {}", self.message.as_str())
    }
}

impl std::error::Error for HookError {}

/// The reason that a pipeline registration failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildErrorReason {
    /// The rule id is empty.
    EmptyRuleId,
    /// The rule id appears more than one time in one phase.
    DuplicateRuleId,
}

impl fmt::Display for BuildErrorReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRuleId => formatter.write_str("The rule id is empty."),
            Self::DuplicateRuleId => formatter.write_str("The rule id is registered two times."),
        }
    }
}

/// One failure from pipeline registration.
///
/// The error always maps to `internal.invariant_violation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineBuildError {
    rule_id: RuleId,
    reason: BuildErrorReason,
}

impl PipelineBuildError {
    /// Creates an error for an empty rule id.
    pub(crate) fn empty(rule_id: RuleId) -> Self {
        Self {
            rule_id,
            reason: BuildErrorReason::EmptyRuleId,
        }
    }

    /// Creates an error for a duplicate rule id.
    pub(crate) fn duplicate(rule_id: RuleId) -> Self {
        Self {
            rule_id,
            reason: BuildErrorReason::DuplicateRuleId,
        }
    }

    /// Returns the rejected rule id.
    #[must_use]
    pub const fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    /// Returns the named error code.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn code(&self) -> HookErrorCode {
        HookErrorCode::InternalInvariantViolation
    }

    /// Returns the canonical registry code.
    #[must_use]
    pub const fn code_str(&self) -> &'static str {
        self.code().as_str()
    }

    /// Returns the operator diagnostic for the failed pipeline check.
    ///
    /// The diagnostic is for operators and logs. It is not a wire value. The
    /// public code stays `internal.invariant_violation`.
    #[must_use]
    pub const fn diagnostic(&self) -> InternalDiagnostic {
        match self.reason {
            BuildErrorReason::EmptyRuleId | BuildErrorReason::DuplicateRuleId => {
                InternalDiagnostic::HookPipeline
            }
        }
    }
}

impl fmt::Display for PipelineBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "The pipeline rejected the rule {} with {}. {}",
            self.rule_id,
            self.code_str(),
            self.reason
        )
    }
}

impl std::error::Error for PipelineBuildError {}
