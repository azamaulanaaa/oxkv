//! Operator diagnostics for internal invariant violations.
//!
//! The wire contract has one code for every internal invariant failure:
//! `internal.invariant_violation`. The wire code does not name the failed
//! check. [`InternalDiagnostic`] names the failed check for operators and
//! logs.
//!
//! The diagnostic is not a wire value. Do not send it in a receipt, an error
//! envelope, or a protocol frame. The public code stays
//! `internal.invariant_violation`.

use core::fmt;

/// The cause of one internal invariant violation.
///
/// Each variant names one grouped internal validation path. The diagnostic is
/// for operators and logs. It is not an error code and it does not enter the
/// wire. Use [`InternalDiagnostic::code`] to get the public code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InternalDiagnostic {
    /// The portable semantic core, storage adapter, or parity check failed.
    PortableCodec,
    /// The RPC descriptor validation failed.
    RpcDescriptor,
    /// The consumer dead-letter or state check failed.
    ConsumerDeadLetter,
    /// The scheduler state transition check failed.
    SchedulerTransition,
    /// The hook pipeline check failed.
    HookPipeline,
}

impl InternalDiagnostic {
    /// Every internal diagnostic.
    pub const ALL: &'static [Self] = &[
        Self::PortableCodec,
        Self::RpcDescriptor,
        Self::ConsumerDeadLetter,
        Self::SchedulerTransition,
        Self::HookPipeline,
    ];

    /// Returns the operator name of the failed check.
    ///
    /// The name is stable text for logs. It is not a wire code.
    #[must_use]
    pub const fn diagnostic_name(self) -> &'static str {
        match self {
            Self::PortableCodec => "portable_codec",
            Self::RpcDescriptor => "rpc_descriptor",
            Self::ConsumerDeadLetter => "consumer_dead_letter",
            Self::SchedulerTransition => "scheduler_transition",
            Self::HookPipeline => "hook_pipeline",
        }
    }

    /// Returns the public wire code.
    ///
    /// The value is always `internal.invariant_violation`. The diagnostic
    /// does not add a new code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::PortableCodec
            | Self::RpcDescriptor
            | Self::ConsumerDeadLetter
            | Self::SchedulerTransition
            | Self::HookPipeline => "internal.invariant_violation",
        }
    }
}

impl fmt::Display for InternalDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_name())
    }
}
