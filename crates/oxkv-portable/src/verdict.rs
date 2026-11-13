//! Deterministic write verdicts and receipt envelopes.
//!
//! A verdict depends only on the request facts. It does not depend on a clock,
//! entropy, an environment, a locale, a thread count, or a transport.
//!
//! This module uses the four receipt states from `oxkv-contracts`. A terminal
//! receipt cannot change state.

use oxkv_contracts::receipt::ReceiptState;

use crate::encode::{Decoder, EncodeError, Encoder};

const VERDICT_MAGIC: &[u8] = b"OXKV1";
const RECEIPT_MAGIC: &[u8] = b"OXKR1";
const RECEIPT_VERSION: &str = "oxkv/receipt/v1";

/// The facts that decide one write verdict.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerdictSpec {
    /// The base version from the request.
    pub base_version: u64,
    /// The current key version in the authority.
    pub current_key_version: u64,
    /// True when the policy admits the write.
    pub policy_allows: bool,
    /// True when a read fact is over its version-distance bound.
    pub stale_facts: bool,
    /// True when strict authorization is declared.
    pub strict: bool,
    /// True when the authority queue deadline passed.
    pub queue_deadline_passed: bool,
    /// The shard commit version for an admitted write.
    pub commit_version: u64,
}

/// One deterministic write verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    /// The receipt state.
    pub state: ReceiptState,
    /// The canonical error code for a non-committed verdict.
    pub code: Option<&'static str>,
    /// The shard commit version for a committed verdict.
    pub commit_version: Option<u64>,
    /// The current key version for a conflict verdict.
    pub current_key_version: Option<u64>,
}

impl Verdict {
    /// Encodes the verdict in canonical form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.raw(VERDICT_MAGIC);
        encoder.u8(self.state.wire_tag());
        match self.code {
            Some(code) => {
                encoder.u8(1);
                encoder.text(code);
            }
            None => encoder.u8(0),
        }
        encoder.optional_u64(self.commit_version);
        encoder.optional_u64(self.current_key_version);
        encoder.finish()
    }

    /// Decodes one canonical verdict.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] when the bytes are malformed.
    /// Returns [`VerdictError::UnknownState`] when the state tag is unknown.
    /// Returns [`VerdictError::UnknownCode`] when a code is not registered.
    pub fn decode(bytes: &[u8]) -> Result<Self, VerdictError> {
        let mut decoder = Decoder::new(bytes);
        if decoder.take(VERDICT_MAGIC.len())? != VERDICT_MAGIC {
            return Err(VerdictError::InvalidMagic);
        }
        let state = ReceiptState::from_wire_tag(decoder.u8()?).ok_or(VerdictError::UnknownState)?;
        let code = match decoder.u8()? {
            0 => None,
            1 => {
                let code = decoder.text()?;
                let code = oxkv_contracts::error::ERROR_REGISTRY
                    .iter()
                    .find(|spec| spec.code == code.as_str())
                    .map(|spec| spec.code)
                    .ok_or(VerdictError::UnknownCode)?;
                Some(code)
            }
            _ => return Err(VerdictError::InvalidMagic),
        };
        let commit_version = decoder.optional_u64()?;
        let current_key_version = decoder.optional_u64()?;
        decoder.finish()?;
        Ok(Self {
            state,
            code,
            commit_version,
            current_key_version,
        })
    }
}

/// An error from verdict decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VerdictError {
    /// The magic bytes are wrong.
    #[error("The verdict magic is wrong.")]
    InvalidMagic,
    /// The receipt state tag is unknown.
    #[error("The receipt state tag is unknown.")]
    UnknownState,
    /// The error code is not registered.
    #[error("The error code is not registered.")]
    UnknownCode,
    /// The bytes are truncated or malformed.
    #[error("The verdict bytes are malformed.")]
    Encode(#[from] EncodeError),
}

impl VerdictError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidMagic | Self::UnknownState | Self::UnknownCode => "receipt.state_invalid",
            Self::Encode(error) => error.code(),
        }
    }
}

/// Decides one write verdict from request facts only.
#[must_use]
pub fn decide(spec: &VerdictSpec) -> Verdict {
    if spec.base_version > spec.current_key_version {
        return Verdict {
            state: ReceiptState::Rejected,
            code: Some("write.stale_base_version"),
            commit_version: None,
            current_key_version: None,
        };
    }
    if spec.base_version < spec.current_key_version {
        return Verdict {
            state: ReceiptState::Conflict,
            code: Some("write.version_conflict"),
            commit_version: None,
            current_key_version: Some(spec.current_key_version),
        };
    }
    if spec.queue_deadline_passed {
        return Verdict {
            state: ReceiptState::Rejected,
            code: Some("write.queue_timeout"),
            commit_version: None,
            current_key_version: None,
        };
    }
    if spec.stale_facts {
        if spec.strict {
            return Verdict {
                state: ReceiptState::Rejected,
                code: Some("authorization.stale_facts"),
                commit_version: None,
                current_key_version: None,
            };
        }
        return Verdict {
            state: ReceiptState::Queued,
            code: Some("projection.lagging"),
            commit_version: None,
            current_key_version: None,
        };
    }
    if !spec.policy_allows {
        return Verdict {
            state: ReceiptState::Rejected,
            code: Some("auth.forbidden"),
            commit_version: None,
            current_key_version: None,
        };
    }
    Verdict {
        state: ReceiptState::Committed,
        code: None,
        commit_version: Some(spec.commit_version),
        current_key_version: None,
    }
}

/// One receipt input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptSpec {
    /// The verdict facts.
    pub verdict: VerdictSpec,
    /// The receipt identity.
    pub receipt_id: [u8; 32],
    /// The request identity.
    pub request_id: [u8; 32],
    /// The client idempotency key.
    pub idempotency_key: Vec<u8>,
    /// The digest of the request body.
    pub body_digest: [u8; 32],
    /// The pinned policy version.
    pub policy_version: [u8; 32],
    /// The manifest digest.
    pub manifest_digest: [u8; 32],
    /// The shard identifier.
    pub shard_id: Vec<u8>,
    /// The audit sequence for this receipt.
    pub audit_sequence: u64,
}

/// One canonical receipt.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The deterministic verdict.
    pub verdict: Verdict,
    /// The receipt identity.
    pub receipt_id: [u8; 32],
    /// The request identity.
    pub request_id: [u8; 32],
    /// The client idempotency key.
    pub idempotency_key: Vec<u8>,
    /// The digest of the request body.
    pub body_digest: [u8; 32],
    /// The pinned policy version.
    pub policy_version: [u8; 32],
    /// The manifest digest.
    pub manifest_digest: [u8; 32],
    /// The shard identifier.
    pub shard_id: Vec<u8>,
    /// The audit sequence for this receipt.
    pub audit_sequence: u64,
}

impl Receipt {
    /// Builds a receipt and decides its verdict.
    #[must_use]
    pub fn from_spec(spec: &ReceiptSpec) -> Self {
        Self {
            verdict: decide(&spec.verdict),
            receipt_id: spec.receipt_id,
            request_id: spec.request_id,
            idempotency_key: spec.idempotency_key.clone(),
            body_digest: spec.body_digest,
            policy_version: spec.policy_version,
            manifest_digest: spec.manifest_digest,
            shard_id: spec.shard_id.clone(),
            audit_sequence: spec.audit_sequence,
        }
    }

    /// Encodes the receipt envelope.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.raw(RECEIPT_MAGIC);
        encoder.text(RECEIPT_VERSION);
        encoder.raw(&self.receipt_id);
        encoder.raw(&self.request_id);
        encoder.bytes(&self.idempotency_key);
        encoder.raw(&self.body_digest);
        encoder.raw(&self.policy_version);
        encoder.raw(&self.manifest_digest);
        encoder.bytes(&self.shard_id);
        encoder.u64(self.audit_sequence);
        encoder.u8(self.verdict.state.wire_tag());
        match self.verdict.code {
            Some(code) => {
                encoder.u8(1);
                encoder.text(code);
            }
            None => encoder.u8(0),
        }
        encoder.optional_u64(self.verdict.commit_version);
        encoder.optional_u64(self.verdict.current_key_version);
        encoder.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::fn_params_excessive_bools)]
    fn spec(
        base_version: u64,
        current_key_version: u64,
        policy_allows: bool,
        stale_facts: bool,
        strict: bool,
        queue_deadline_passed: bool,
    ) -> VerdictSpec {
        VerdictSpec {
            base_version,
            current_key_version,
            policy_allows,
            stale_facts,
            strict,
            queue_deadline_passed,
            commit_version: 11,
        }
    }

    #[test]
    fn a_matching_base_and_allowed_policy_commits() {
        let verdict = decide(&spec(3, 3, true, false, false, false));
        assert_eq!(verdict.state, ReceiptState::Committed);
        assert_eq!(verdict.commit_version, Some(11));
    }

    #[test]
    fn a_lower_base_is_a_conflict() {
        let verdict = decide(&spec(2, 3, true, false, false, false));
        assert_eq!(verdict.state, ReceiptState::Conflict);
        assert_eq!(verdict.code, Some("write.version_conflict"));
        assert_eq!(verdict.current_key_version, Some(3));
    }

    #[test]
    fn a_higher_base_is_rejected() {
        let verdict = decide(&spec(4, 3, true, false, false, false));
        assert_eq!(verdict.state, ReceiptState::Rejected);
        assert_eq!(verdict.code, Some("write.stale_base_version"));
    }

    #[test]
    fn a_queue_deadline_rejects_the_write() {
        let verdict = decide(&spec(3, 3, true, false, false, true));
        assert_eq!(verdict.state, ReceiptState::Rejected);
        assert_eq!(verdict.code, Some("write.queue_timeout"));
    }

    #[test]
    fn stale_facts_queue_or_reject_by_mode() {
        let queued = decide(&spec(3, 3, true, true, false, false));
        assert_eq!(queued.state, ReceiptState::Queued);
        assert_eq!(queued.code, Some("projection.lagging"));
        let rejected = decide(&spec(3, 3, true, true, true, false));
        assert_eq!(rejected.state, ReceiptState::Rejected);
        assert_eq!(rejected.code, Some("authorization.stale_facts"));
    }

    #[test]
    fn a_denied_policy_is_rejected() {
        let verdict = decide(&spec(3, 3, false, false, false, false));
        assert_eq!(verdict.state, ReceiptState::Rejected);
        assert_eq!(verdict.code, Some("auth.forbidden"));
    }

    #[test]
    fn a_verdict_round_trips() {
        for verdict in [
            decide(&spec(3, 3, true, false, false, false)),
            decide(&spec(2, 3, true, false, false, false)),
            decide(&spec(4, 3, true, false, false, false)),
            decide(&spec(3, 3, true, true, false, false)),
        ] {
            assert_eq!(Verdict::decode(&verdict.encode()), Ok(verdict));
        }
    }

    #[test]
    fn a_receipt_carries_the_verdict_and_every_required_field() {
        let receipt = Receipt::from_spec(&ReceiptSpec {
            verdict: spec(3, 3, true, false, false, false),
            receipt_id: [1; 32],
            request_id: [2; 32],
            idempotency_key: vec![3],
            body_digest: [4; 32],
            policy_version: [5; 32],
            manifest_digest: [6; 32],
            shard_id: vec![7],
            audit_sequence: 8,
        });
        assert_eq!(receipt.verdict.state, ReceiptState::Committed);
        assert_eq!(receipt.encode().first(), Some(&b'O'));
    }
}
