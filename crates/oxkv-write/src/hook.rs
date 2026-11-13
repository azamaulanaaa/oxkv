//! Commit hooks for index and event derivation.
//!
//! The authority calls a hook only when a request reaches the commit step. A
//! rejected request or a conflict never calls a hook. A hook returns effects
//! as values. The authority does not apply them to other subsystems.
//!
//! A hook implementation must be pure. It must not read a clock, draw
//! randomness, perform I/O, or keep state. A hook failure aborts the batch.

use crate::record::CommitView;

/// One effect that a commit hook derives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEffect {
    source: &'static str,
    key: Vec<u8>,
    payload: Vec<u8>,
}

impl HookEffect {
    /// Creates a hook effect.
    #[must_use]
    pub const fn new(source: &'static str, key: Vec<u8>, payload: Vec<u8>) -> Self {
        Self {
            source,
            key,
            payload,
        }
    }

    /// Returns the hook name.
    #[must_use]
    pub const fn source(&self) -> &'static str {
        self.source
    }

    /// Returns the effect key.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// Returns the effect payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// An error from a commit hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("The commit hook failed with {code}.")]
pub struct HookError {
    code: &'static str,
}

impl HookError {
    /// Creates a hook error from a registered error code.
    #[must_use]
    pub const fn new(code: &'static str) -> Self {
        Self { code }
    }

    /// Returns the error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

/// A pure hook that derives index entries or business events at commit.
pub trait CommitHook {
    /// Returns the stable hook name.
    fn name(&self) -> &'static str;

    /// Derives effects for one proposed commit.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] when the hook cannot derive the effects. The
    /// authority then commits nothing.
    fn derive(&self, view: &CommitView<'_>) -> Result<Vec<HookEffect>, HookError>;
}
