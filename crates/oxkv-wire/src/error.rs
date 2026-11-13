//! The frozen sync error vocabulary.

use core::fmt;

use oxkv_contracts::error::{ErrorSpec, lookup};

/// One frozen sync wire error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ErrorCode {
    /// The frame or sync wire version is not supported.
    UnsupportedVersion,
    /// A required capability is unknown.
    UnknownCapability,
    /// The authority rejected the scope.
    ScopeRejected,
    /// The cursor is older than the retention window.
    CursorExpired,
    /// The cursor is malformed or tampered.
    CursorInvalid,
    /// The cursor belongs to another cursor type.
    CursorTypeMismatch,
    /// The resume gap is larger than the authority can serve.
    GapTooLarge,
    /// No snapshot is available for the fallback path.
    SnapshotUnavailable,
    /// The client exceeded a rate limit.
    RateLimited,
    /// The client is not authenticated.
    Unauthorized,
    /// A required shard is not available.
    ShardUnavailable,
    /// The call arrived before `SessionHello`.
    NegotiationRequired,
    /// The client manifest is not compatible with the authority.
    IncompatibleManifest,
    /// The consumer is too slow to keep up.
    ConsumerTooSlow,
    /// The RPC method is unknown.
    RpcUnknownMethod,
    /// The RPC descriptor does not match.
    RpcDescriptorMismatch,
    /// The RPC codec is not supported.
    RpcCodecUnsupported,
    /// An internal invariant did not hold.
    InternalInvariantViolation,
}

impl ErrorCode {
    /// Every sync wire error code.
    pub const ALL: &'static [Self] = &[
        Self::UnsupportedVersion,
        Self::UnknownCapability,
        Self::ScopeRejected,
        Self::CursorExpired,
        Self::CursorInvalid,
        Self::CursorTypeMismatch,
        Self::GapTooLarge,
        Self::SnapshotUnavailable,
        Self::RateLimited,
        Self::Unauthorized,
        Self::ShardUnavailable,
        Self::NegotiationRequired,
        Self::IncompatibleManifest,
        Self::ConsumerTooSlow,
        Self::RpcUnknownMethod,
        Self::RpcDescriptorMismatch,
        Self::RpcCodecUnsupported,
        Self::InternalInvariantViolation,
    ];

    /// Returns the registered error code text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedVersion => "sync.unsupported_version",
            Self::UnknownCapability => "sync.unknown_capability",
            Self::ScopeRejected => "sync.scope_rejected",
            Self::CursorExpired => "sync.cursor_expired",
            Self::CursorInvalid => "sync.cursor_invalid",
            Self::CursorTypeMismatch => "sync.cursor_type_mismatch",
            Self::GapTooLarge => "sync.gap_too_large",
            Self::SnapshotUnavailable => "sync.snapshot_unavailable",
            Self::RateLimited => "sync.rate_limited",
            Self::Unauthorized => "sync.unauthorized",
            Self::ShardUnavailable => "sync.shard_unavailable",
            Self::NegotiationRequired => "sync.negotiation_required",
            Self::IncompatibleManifest => "sync.incompatible_manifest",
            Self::ConsumerTooSlow => "consumer.too_slow",
            Self::RpcUnknownMethod => "rpc.unknown_method",
            Self::RpcDescriptorMismatch => "rpc.descriptor_mismatch",
            Self::RpcCodecUnsupported => "rpc.codec_unsupported",
            Self::InternalInvariantViolation => "internal.invariant_violation",
        }
    }

    /// Returns the registry entry for this code.
    #[must_use]
    pub fn spec(self) -> Option<&'static ErrorSpec> {
        lookup(self.as_str())
    }

    /// Returns true when the client may retry the call.
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::ScopeRejected
                | Self::CursorExpired
                | Self::CursorInvalid
                | Self::CursorTypeMismatch
                | Self::GapTooLarge
                | Self::SnapshotUnavailable
                | Self::RateLimited
                | Self::ShardUnavailable
                | Self::ConsumerTooSlow
        )
    }

    /// Returns true when the client must resynchronize.
    #[must_use]
    pub const fn requires_resync(self) -> bool {
        matches!(
            self,
            Self::CursorExpired
                | Self::CursorInvalid
                | Self::CursorTypeMismatch
                | Self::GapTooLarge
        )
    }

    /// Returns true when a cursor remains valid after this error.
    #[must_use]
    pub const fn cursor_stays_valid(self) -> bool {
        !self.requires_resync() && !matches!(self, Self::NegotiationRequired)
    }

    /// Returns true when the error offers a snapshot fallback.
    #[must_use]
    pub const fn offers_snapshot(self) -> bool {
        matches!(
            self,
            Self::CursorExpired
                | Self::CursorInvalid
                | Self::CursorTypeMismatch
                | Self::GapTooLarge
                | Self::SnapshotUnavailable
        )
    }

    /// Reads a code from its canonical text.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == code)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
