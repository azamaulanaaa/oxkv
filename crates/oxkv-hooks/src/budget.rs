//! The bounded resource meter for one hook run.

/// The resource bounds for one pipeline run.
///
/// The step budget counts one unit for each hook evaluation. The event budget
/// bounds the number of post-write notifications. The byte budgets bound one
/// accepted value and one notification payload. The batch budget bounds the
/// number of operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookBudget {
    /// The maximum number of hook evaluations in one run.
    pub steps: u32,
    /// The maximum number of notifications in one commit.
    pub events: u32,
    /// The maximum payload length of one notification.
    pub event_bytes: u32,
    /// The maximum value length of one put operation.
    pub value_bytes: u32,
    /// The maximum number of operations in one batch.
    pub batch_ops: u32,
}

impl HookBudget {
    /// Creates a budget.
    #[must_use]
    pub const fn new(
        steps: u32,
        events: u32,
        event_bytes: u32,
        value_bytes: u32,
        batch_ops: u32,
    ) -> Self {
        Self {
            steps,
            events,
            event_bytes,
            value_bytes,
            batch_ops,
        }
    }

    /// Returns the default budget.
    ///
    /// The default allows 4096 steps, 1024 events, a 64 KiB event payload, a
    /// 1 MiB value, and 1024 operations.
    #[must_use]
    pub const fn standard() -> Self {
        Self::new(4096, 1024, 65_536, 1_048_576, 1024)
    }
}

impl Default for HookBudget {
    fn default() -> Self {
        Self::standard()
    }
}
