//! Durable authority state.
//!
//! The state file holds the mutable state of one authority. It covers the key
//! records, the receipt ledger, the commit version, the audit records, the
//! signed checkpoints, and the chain tip.
//!
//! # File format
//!
//! The file has a magic value, a format version, a payload length, the
//! payload, and a `BLAKE3` integrity value. The save path writes a temporary
//! file and renames it over the state file. A reader rejects a missing,
//! truncated, corrupt, or unknown-version file with a named error.
//!
//! This module is native only. It uses the file system.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use oxkv_contracts::error::lookup;
use oxkv_contracts::receipt::{
    AuditReference, AuditSequence, IdempotencyKey, ReceiptEnvelope, ReceiptId, ReceiptOutcome,
    ReceiptState, RequestId,
};
use oxkv_contracts::version::{
    CommitVersion, FactSourceId, FactVersion, Hash32, KeyVersion, ManifestDigest, MapVersion,
    PolicyVersion, ProjectionEpoch, ShardId, Signature,
};
use oxkv_security::audit::{
    AuditCheckpoint, AuditDecision, AuditEntry, AuditRecord, AuditRowDecision, RuleIdentity,
};
use oxkv_security::auth::PrincipalId;
use oxkv_security::secret::KeyIdentifier;

use crate::receipt::{ReceiptLedger, StoredReceipt};
use crate::record::{KeyRecord, KeyValue};
use crate::request::{ClientId, Key};
use crate::util::{push_bytes, push_u64};

/// The state file name inside a data directory.
pub const STATE_FILE: &str = "authority.state";

const MAGIC: &[u8; 8] = b"OXKVAUTH";
const FORMAT_VERSION: u32 = 1;
const HEADER_LEN: usize = 8 + 4 + 8;
const CHECKSUM_LEN: usize = 32;
const TAG_ABSENT: u8 = 0;
const TAG_PRESENT: u8 = 1;
const VALUE_TOMBSTONE: u8 = 0;
const VALUE_LIVE: u8 = 1;
const QUEUED_REASON: &str = "projection.lagging";

/// An error from a durable authority state operation.
#[derive(Debug, thiserror::Error)]
pub enum AuthorityStateError {
    /// The state file does not exist.
    #[error("The state file is missing: {path}.")]
    MissingState {
        /// The expected state file path.
        path: PathBuf,
    },
    /// A file operation failed.
    #[error("The state file operation failed for {path}: {source}.")]
    Io {
        /// The affected path.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: io::Error,
    },
    /// The file magic value is unknown.
    #[error("The state file has an unknown format.")]
    UnknownFormat,
    /// The format version is unknown.
    #[error("The state format version {version} is not supported.")]
    UnsupportedVersion {
        /// The version from the file header.
        version: u32,
    },
    /// The file is shorter than its header or payload length.
    #[error("The state file is truncated.")]
    Truncated,
    /// The integrity value does not match the payload.
    #[error("The state file integrity value does not match.")]
    IntegrityMismatch,
    /// The payload is malformed or internally inconsistent.
    #[error("The state file is corrupt: {reason}.")]
    Corrupt {
        /// A short reason for the rejection.
        reason: &'static str,
    },
}

/// The mutable state of one authority.
pub(crate) struct AuthorityState {
    pub(crate) shard_id: ShardId,
    pub(crate) records: BTreeMap<Key, KeyRecord>,
    pub(crate) receipts: ReceiptLedger,
    pub(crate) commit_version: CommitVersion,
    pub(crate) audit_records: Vec<AuditRecord>,
    pub(crate) checkpoints: Vec<AuditCheckpoint>,
    pub(crate) tip: Hash32,
}

impl AuthorityState {
    /// Writes the state to one directory.
    pub(crate) fn save(&self, directory: &Path) -> Result<(), AuthorityStateError> {
        fs::create_dir_all(directory).map_err(|source| AuthorityStateError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = directory.join(STATE_FILE);
        let temporary = path.with_file_name(format!("{STATE_FILE}.tmp"));
        let bytes = self.encode();
        let mut file = fs::File::create(&temporary).map_err(|source| AuthorityStateError::Io {
            path: temporary.clone(),
            source,
        })?;
        file.write_all(&bytes)
            .map_err(|source| AuthorityStateError::Io {
                path: temporary.clone(),
                source,
            })?;
        file.sync_all().map_err(|source| AuthorityStateError::Io {
            path: temporary.clone(),
            source,
        })?;
        fs::rename(&temporary, &path).map_err(|source| AuthorityStateError::Io {
            path: path.clone(),
            source,
        })
    }

    /// Reads the state from one directory.
    pub(crate) fn load(directory: &Path) -> Result<Self, AuthorityStateError> {
        let path = directory.join(STATE_FILE);
        let bytes = fs::read(&path).map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                AuthorityStateError::MissingState { path: path.clone() }
            } else {
                AuthorityStateError::Io {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        Self::decode(&bytes)
    }

    /// Encodes the state in the canonical file format.
    fn encode(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        push_bytes(&mut payload, self.shard_id.as_bytes());
        push_u64(
            &mut payload,
            u64::try_from(self.records.len()).unwrap_or(u64::MAX),
        );
        for (key, record) in &self.records {
            encode_record(&mut payload, key, record);
        }
        push_u64(
            &mut payload,
            u64::try_from(self.receipts.len()).unwrap_or(u64::MAX),
        );
        for ((client_id, idempotency_key), stored) in self.receipts.iter() {
            encode_stored_receipt(&mut payload, client_id, idempotency_key, stored);
        }
        push_u64(&mut payload, self.commit_version.get());
        push_u64(
            &mut payload,
            u64::try_from(self.audit_records.len()).unwrap_or(u64::MAX),
        );
        for record in &self.audit_records {
            encode_audit_record(&mut payload, record);
        }
        push_u64(
            &mut payload,
            u64::try_from(self.checkpoints.len()).unwrap_or(u64::MAX),
        );
        for checkpoint in &self.checkpoints {
            encode_checkpoint(&mut payload, checkpoint);
        }
        payload.extend_from_slice(self.tip.as_bytes());
        seal(&payload)
    }

    /// Decodes the state from the canonical file format.
    fn decode(bytes: &[u8]) -> Result<Self, AuthorityStateError> {
        if bytes.len() < HEADER_LEN + CHECKSUM_LEN {
            return Err(AuthorityStateError::Truncated);
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(AuthorityStateError::UnknownFormat);
        }
        let version = u32::from_be_bytes(
            bytes[MAGIC.len()..MAGIC.len() + 4]
                .try_into()
                .map_err(|_| AuthorityStateError::Truncated)?,
        );
        if version != FORMAT_VERSION {
            return Err(AuthorityStateError::UnsupportedVersion { version });
        }
        let length = u64::from_be_bytes(
            bytes[MAGIC.len() + 4..HEADER_LEN]
                .try_into()
                .map_err(|_| AuthorityStateError::Truncated)?,
        );
        let length = usize::try_from(length).map_err(|_| AuthorityStateError::Truncated)?;
        let payload_end = HEADER_LEN
            .checked_add(length)
            .ok_or(AuthorityStateError::Truncated)?;
        let expected_end = payload_end
            .checked_add(CHECKSUM_LEN)
            .ok_or(AuthorityStateError::Truncated)?;
        if bytes.len() < expected_end {
            return Err(AuthorityStateError::Truncated);
        }
        if bytes.len() > expected_end {
            return Err(AuthorityStateError::Corrupt {
                reason: "the file has trailing bytes",
            });
        }
        let payload = &bytes[HEADER_LEN..payload_end];
        if Hash32::of(payload).as_bytes() != &bytes[payload_end..] {
            return Err(AuthorityStateError::IntegrityMismatch);
        }

        let mut reader = Reader::new(payload);
        let shard_id = ShardId::new(reader.read_bytes()?.to_vec());
        let records = decode_records(&mut reader)?;
        let receipts = decode_receipts(&mut reader)?;
        let commit_version = CommitVersion::new(reader.read_u64()?);
        let audit_records = decode_audit_records(&mut reader)?;
        let checkpoints = decode_checkpoints(&mut reader)?;
        let tip = reader.read_hash32()?;
        reader.finish()?;

        Ok(Self {
            shard_id,
            records,
            receipts,
            commit_version,
            audit_records,
            checkpoints,
            tip,
        })
    }
}

/// Decodes the key records.
fn decode_records(
    reader: &mut Reader<'_>,
) -> Result<BTreeMap<Key, KeyRecord>, AuthorityStateError> {
    let count = reader.read_count()?;
    let mut records = BTreeMap::new();
    for _ in 0..count {
        let key = Key::new(reader.read_bytes()?.to_vec());
        let key_version = KeyVersion::new(reader.read_u64()?);
        let value = match reader.read_u8()? {
            VALUE_TOMBSTONE => KeyValue::Tombstone,
            VALUE_LIVE => KeyValue::Value(reader.read_bytes()?.to_vec()),
            _ => {
                return Err(AuthorityStateError::Corrupt {
                    reason: "a key value tag is unknown",
                });
            }
        };
        if records
            .insert(key, KeyRecord::new(key_version, value))
            .is_some()
        {
            return Err(AuthorityStateError::Corrupt {
                reason: "a key record occurs more than once",
            });
        }
    }
    Ok(records)
}

/// Decodes the receipt ledger.
fn decode_receipts(reader: &mut Reader<'_>) -> Result<ReceiptLedger, AuthorityStateError> {
    let count = reader.read_count()?;
    let mut receipts = ReceiptLedger::new();
    for _ in 0..count {
        let client_id = ClientId::new(reader.read_bytes()?.to_vec());
        let idempotency_key = IdempotencyKey::new(reader.read_bytes()?.to_vec());
        let body_digest = reader.read_hash32()?;
        let envelope = decode_envelope(reader)?;
        if envelope.idempotency_key() != &idempotency_key {
            return Err(AuthorityStateError::Corrupt {
                reason: "a receipt key does not match its ledger key",
            });
        }
        let result = if envelope.state().is_terminal() {
            receipts.insert_terminal(&client_id, body_digest, envelope)
        } else {
            receipts.queue(&client_id, body_digest, envelope)
        };
        result.map_err(|_| AuthorityStateError::Corrupt {
            reason: "a receipt ledger entry is invalid",
        })?;
    }
    Ok(receipts)
}

/// Decodes the audit records.
fn decode_audit_records(reader: &mut Reader<'_>) -> Result<Vec<AuditRecord>, AuthorityStateError> {
    let count = reader.read_count()?;
    let mut records = Vec::new();
    for _ in 0..count {
        records.push(decode_audit_record(reader)?);
    }
    Ok(records)
}

/// Decodes the signed checkpoints.
fn decode_checkpoints(
    reader: &mut Reader<'_>,
) -> Result<Vec<AuditCheckpoint>, AuthorityStateError> {
    let count = reader.read_count()?;
    let mut checkpoints = Vec::new();
    for _ in 0..count {
        checkpoints.push(decode_checkpoint(reader)?);
    }
    Ok(checkpoints)
}

/// Appends one key record.
fn encode_record(output: &mut Vec<u8>, key: &Key, record: &KeyRecord) {
    push_bytes(output, key.as_bytes());
    push_u64(output, record.key_version().get());
    match record.value() {
        KeyValue::Tombstone => output.push(VALUE_TOMBSTONE),
        KeyValue::Value(value) => {
            output.push(VALUE_LIVE);
            push_bytes(output, value);
        }
    }
}

/// Appends one stored receipt and its ledger key.
fn encode_stored_receipt(
    output: &mut Vec<u8>,
    client_id: &ClientId,
    idempotency_key: &IdempotencyKey,
    stored: &StoredReceipt,
) {
    push_bytes(output, client_id.as_bytes());
    push_bytes(output, idempotency_key.as_bytes());
    output.extend_from_slice(stored.body_digest().as_bytes());
    encode_envelope(output, stored.envelope());
}

/// Appends one receipt envelope.
fn encode_envelope(output: &mut Vec<u8>, envelope: &ReceiptEnvelope) {
    output.extend_from_slice(envelope.receipt_id().as_bytes());
    output.extend_from_slice(envelope.request_id().as_bytes());
    push_bytes(output, envelope.idempotency_key().as_bytes());
    push_bytes(output, envelope.audit().shard_id.as_bytes());
    push_u64(output, envelope.audit().audit_sequence.get());
    output.extend_from_slice(envelope.audit().record_hash.as_bytes());
    output.extend_from_slice(envelope.policy_version().as_bytes());
    output.extend_from_slice(envelope.manifest_digest().as_bytes());
    output.push(envelope.state().wire_tag());
    push_optional_u64(output, envelope.commit_version().map(CommitVersion::get));
    push_optional_u64(output, envelope.current_key_version().map(KeyVersion::get));
    push_optional_text(output, envelope.reason_code());
}

/// Decodes one receipt envelope.
fn decode_envelope(reader: &mut Reader<'_>) -> Result<ReceiptEnvelope, AuthorityStateError> {
    let receipt_id = ReceiptId::from_bytes(reader.read_array32()?);
    let request_id = RequestId::from_bytes(reader.read_array32()?);
    let idempotency_key = IdempotencyKey::new(reader.read_bytes()?.to_vec());
    let audit_shard = ShardId::new(reader.read_bytes()?.to_vec());
    let audit_sequence = AuditSequence::new(reader.read_u64()?);
    let audit_hash = reader.read_hash32()?;
    let policy_version = PolicyVersion::from_bytes(reader.read_array32()?);
    let manifest_digest = ManifestDigest::from_bytes(reader.read_array32()?);
    let state_tag = reader.read_u8()?;
    let state = ReceiptState::from_wire_tag(state_tag).ok_or(AuthorityStateError::Corrupt {
        reason: "a receipt state tag is unknown",
    })?;
    let commit_version = reader.read_optional_u64()?.map(CommitVersion::new);
    let current_key_version = reader.read_optional_u64()?.map(KeyVersion::new);
    let reason = reader.read_optional_reason()?;

    if state == ReceiptState::Queued && reason.is_none() {
        return Err(AuthorityStateError::Corrupt {
            reason: "a queued receipt has no reason",
        });
    }
    let constructor_reason = if state == ReceiptState::Queued {
        reason.ok_or(AuthorityStateError::Corrupt {
            reason: "a queued receipt has no reason",
        })?
    } else {
        QUEUED_REASON
    };
    let mut envelope = ReceiptEnvelope::queued(
        receipt_id,
        request_id,
        idempotency_key,
        AuditReference::new(audit_shard, audit_sequence, audit_hash),
        policy_version,
        manifest_digest,
        constructor_reason,
    )
    .map_err(|_| AuthorityStateError::Corrupt {
        reason: "a receipt reason is not registered",
    })?;

    match state {
        ReceiptState::Queued => {
            if commit_version.is_some() || current_key_version.is_some() {
                return Err(AuthorityStateError::Corrupt {
                    reason: "a queued receipt has a terminal field",
                });
            }
        }
        ReceiptState::Committed => {
            if reason.is_some() || current_key_version.is_some() {
                return Err(AuthorityStateError::Corrupt {
                    reason: "a committed receipt has an invalid field",
                });
            }
            let commit_version = commit_version.ok_or(AuthorityStateError::Corrupt {
                reason: "a committed receipt has no commit version",
            })?;
            envelope
                .close(ReceiptOutcome::Committed { commit_version })
                .map_err(|_| AuthorityStateError::Corrupt {
                    reason: "a committed receipt does not close",
                })?;
        }
        ReceiptState::Rejected => {
            let reason = reason.ok_or(AuthorityStateError::Corrupt {
                reason: "a rejected receipt has no reason",
            })?;
            if commit_version.is_some() || current_key_version.is_some() {
                return Err(AuthorityStateError::Corrupt {
                    reason: "a rejected receipt has an invalid field",
                });
            }
            envelope
                .close(ReceiptOutcome::Rejected { reason })
                .map_err(|_| AuthorityStateError::Corrupt {
                    reason: "a rejected receipt does not close",
                })?;
        }
        ReceiptState::Conflict => {
            let reason = reason.ok_or(AuthorityStateError::Corrupt {
                reason: "a conflict receipt has no reason",
            })?;
            let current_key_version = current_key_version.ok_or(AuthorityStateError::Corrupt {
                reason: "a conflict receipt has no current key version",
            })?;
            if commit_version.is_some() {
                return Err(AuthorityStateError::Corrupt {
                    reason: "a conflict receipt has an invalid field",
                });
            }
            envelope
                .close(ReceiptOutcome::Conflict {
                    current_key_version,
                    reason,
                })
                .map_err(|_| AuthorityStateError::Corrupt {
                    reason: "a conflict receipt does not close",
                })?;
        }
    }
    Ok(envelope)
}

/// Appends one audit record.
fn encode_audit_record(output: &mut Vec<u8>, record: &AuditRecord) {
    push_u64(output, record.audit_sequence.get());
    encode_audit_entry(output, &record.entry);
    output.extend_from_slice(record.previous_hash.as_bytes());
    output.extend_from_slice(record.record_hash.as_bytes());
}

/// Appends one audit entry.
fn encode_audit_entry(output: &mut Vec<u8>, entry: &AuditEntry) {
    output.extend_from_slice(entry.request_id.as_bytes());
    push_bytes(output, entry.idempotency_key.as_bytes());
    push_bytes(output, entry.principal.as_bytes());
    push_bytes(output, entry.shard_id.as_bytes());
    push_u64(output, entry.base_version.get());
    push_u64(
        output,
        u64::try_from(entry.base_versions.len()).unwrap_or(u64::MAX),
    );
    for (key, base_version) in &entry.base_versions {
        push_bytes(output, key);
        push_optional_u64(output, base_version.map(KeyVersion::get));
    }
    output.extend_from_slice(entry.policy_version.as_bytes());
    output.extend_from_slice(entry.manifest_digest.as_bytes());
    push_u64(output, entry.map_version.get());
    push_u64(output, entry.projection_epoch.get());
    push_u64(
        output,
        u64::try_from(entry.fact_versions.len()).unwrap_or(u64::MAX),
    );
    for (source, fact_version) in &entry.fact_versions {
        push_bytes(output, source.as_str().as_bytes());
        push_u64(output, fact_version.get());
    }
    push_bytes(output, entry.rule_identity.as_str().as_bytes());
    push_u64(
        output,
        u64::try_from(entry.decisions.len()).unwrap_or(u64::MAX),
    );
    for decision in &entry.decisions {
        push_bytes(output, &decision.key);
        push_bytes(output, decision.rule_identity.as_str().as_bytes());
        output.push(u8::from(decision.allowed));
        push_optional_text(output, decision.reason_code);
    }
    output.push(entry.decision.wire_tag());
    push_optional_text(output, entry.reason_code);
    push_optional_hash(output, entry.receipt_id.as_ref().map(ReceiptId::as_bytes));
    output.extend_from_slice(entry.input_digest.as_bytes());
}

/// Decodes one audit record.
fn decode_audit_record(reader: &mut Reader<'_>) -> Result<AuditRecord, AuthorityStateError> {
    let audit_sequence = AuditSequence::new(reader.read_u64()?);
    let entry = decode_audit_entry(reader)?;
    let previous_hash = reader.read_hash32()?;
    let record_hash = reader.read_hash32()?;
    Ok(AuditRecord {
        audit_sequence,
        entry,
        previous_hash,
        record_hash,
    })
}

/// Decodes one audit entry.
fn decode_audit_entry(reader: &mut Reader<'_>) -> Result<AuditEntry, AuthorityStateError> {
    let request_id = RequestId::from_bytes(reader.read_array32()?);
    let idempotency_key = IdempotencyKey::new(reader.read_bytes()?.to_vec());
    let principal = PrincipalId::from_bytes(reader.read_bytes()?.to_vec());
    let shard_id = ShardId::new(reader.read_bytes()?.to_vec());
    let base_version = KeyVersion::new(reader.read_u64()?);
    let base_count = reader.read_count()?;
    let mut base_versions = Vec::new();
    for _ in 0..base_count {
        let key = reader.read_bytes()?.to_vec();
        let version = reader.read_optional_u64()?.map(KeyVersion::new);
        base_versions.push((key, version));
    }
    let policy_version = PolicyVersion::from_bytes(reader.read_array32()?);
    let manifest_digest = ManifestDigest::from_bytes(reader.read_array32()?);
    let map_version = MapVersion::new(reader.read_u64()?);
    let projection_epoch = ProjectionEpoch::new(reader.read_u64()?);
    let fact_count = reader.read_count()?;
    let mut fact_versions = BTreeMap::new();
    for _ in 0..fact_count {
        let source = FactSourceId::new(reader.read_text()?);
        let fact_version = FactVersion::new(reader.read_u64()?);
        fact_versions.insert(source, fact_version);
    }
    let rule_identity = RuleIdentity::new(reader.read_text()?);
    let decision_count = reader.read_count()?;
    let mut decisions = Vec::new();
    for _ in 0..decision_count {
        let key = reader.read_bytes()?.to_vec();
        let rule_identity = RuleIdentity::new(reader.read_text()?);
        let allowed = match reader.read_u8()? {
            0 => false,
            1 => true,
            _ => {
                return Err(AuthorityStateError::Corrupt {
                    reason: "a row decision flag is unknown",
                });
            }
        };
        let reason_code = reader.read_optional_reason()?;
        decisions.push(AuditRowDecision {
            key,
            rule_identity,
            allowed,
            reason_code,
        });
    }
    let decision_tag = reader.read_u8()?;
    let decision =
        AuditDecision::from_wire_tag(decision_tag).ok_or(AuthorityStateError::Corrupt {
            reason: "an audit decision tag is unknown",
        })?;
    let reason_code = reader.read_optional_reason()?;
    let receipt_id = reader.read_optional_hash()?.map(ReceiptId::from_bytes);
    let input_digest = reader.read_hash32()?;

    Ok(AuditEntry {
        request_id,
        idempotency_key,
        principal,
        shard_id,
        base_version,
        base_versions,
        policy_version,
        manifest_digest,
        map_version,
        projection_epoch,
        fact_versions,
        rule_identity,
        decisions,
        decision,
        reason_code,
        receipt_id,
        input_digest,
    })
}

/// Appends one signed checkpoint.
fn encode_checkpoint(output: &mut Vec<u8>, checkpoint: &AuditCheckpoint) {
    push_bytes(output, checkpoint.shard_id.as_bytes());
    push_u64(output, checkpoint.audit_sequence.get());
    output.extend_from_slice(checkpoint.record_hash.as_bytes());
    output.extend_from_slice(checkpoint.previous_checkpoint_hash.as_bytes());
    push_bytes(output, checkpoint.key_id.as_str().as_bytes());
    output.extend_from_slice(checkpoint.signature.as_bytes());
    output.extend_from_slice(checkpoint.checkpoint_hash.as_bytes());
}

/// Decodes one signed checkpoint.
fn decode_checkpoint(reader: &mut Reader<'_>) -> Result<AuditCheckpoint, AuthorityStateError> {
    let shard_id = ShardId::new(reader.read_bytes()?.to_vec());
    let audit_sequence = AuditSequence::new(reader.read_u64()?);
    let record_hash = reader.read_hash32()?;
    let previous_checkpoint_hash = reader.read_hash32()?;
    let key_id = KeyIdentifier::new(reader.read_text()?);
    let signature = Signature::from_bytes(reader.read_array64()?);
    let checkpoint_hash = reader.read_hash32()?;
    Ok(AuditCheckpoint {
        shard_id,
        audit_sequence,
        record_hash,
        previous_checkpoint_hash,
        key_id,
        signature,
        checkpoint_hash,
    })
}

/// Appends one optional unsigned integer.
fn push_optional_u64(output: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            output.push(TAG_PRESENT);
            push_u64(output, value);
        }
        None => output.push(TAG_ABSENT),
    }
}

/// Appends one optional text value.
fn push_optional_text(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            output.push(TAG_PRESENT);
            push_bytes(output, value.as_bytes());
        }
        None => output.push(TAG_ABSENT),
    }
}

/// Appends one optional hash value.
fn push_optional_hash(output: &mut Vec<u8>, value: Option<&[u8; 32]>) {
    match value {
        Some(value) => {
            output.push(TAG_PRESENT);
            output.extend_from_slice(value);
        }
        None => output.push(TAG_ABSENT),
    }
}

/// Adds the header and the integrity value to a payload.
fn seal(payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len() + CHECKSUM_LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    bytes.extend_from_slice(
        &u64::try_from(payload.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(Hash32::of(payload).as_bytes());
    bytes
}

/// A bounds-checked reader for one state payload.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    /// Creates a reader.
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// Returns the number of unread bytes.
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    /// Reads one byte range.
    fn take(&mut self, length: usize) -> Result<&'a [u8], AuthorityStateError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(AuthorityStateError::Truncated)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(AuthorityStateError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    /// Reads one byte.
    fn read_u8(&mut self) -> Result<u8, AuthorityStateError> {
        Ok(self.take(1)?[0])
    }

    /// Reads one little-endian `u64`.
    fn read_u64(&mut self) -> Result<u64, AuthorityStateError> {
        let mut value = [0_u8; 8];
        value.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(value))
    }

    /// Reads one item count.
    fn read_count(&mut self) -> Result<u64, AuthorityStateError> {
        let count = self.read_u64()?;
        if count > u64::try_from(self.remaining()).unwrap_or(u64::MAX) {
            return Err(AuthorityStateError::Truncated);
        }
        Ok(count)
    }

    /// Reads one length-prefixed byte field.
    fn read_bytes(&mut self) -> Result<&'a [u8], AuthorityStateError> {
        let length = self.read_u64()?;
        let length = usize::try_from(length).map_err(|_| AuthorityStateError::Truncated)?;
        self.take(length)
    }

    /// Reads one length-prefixed UTF-8 text field.
    fn read_text(&mut self) -> Result<&'a str, AuthorityStateError> {
        core::str::from_utf8(self.read_bytes()?).map_err(|_| AuthorityStateError::Corrupt {
            reason: "a text field is not UTF-8",
        })
    }

    /// Reads one 32-byte value.
    fn read_array32(&mut self) -> Result<[u8; 32], AuthorityStateError> {
        let mut value = [0_u8; 32];
        value.copy_from_slice(self.take(32)?);
        Ok(value)
    }

    /// Reads one 64-byte value.
    fn read_array64(&mut self) -> Result<[u8; 64], AuthorityStateError> {
        let mut value = [0_u8; 64];
        value.copy_from_slice(self.take(64)?);
        Ok(value)
    }

    /// Reads one hash value.
    fn read_hash32(&mut self) -> Result<Hash32, AuthorityStateError> {
        Ok(Hash32::from_bytes(self.read_array32()?))
    }

    /// Reads one optional unsigned integer.
    fn read_optional_u64(&mut self) -> Result<Option<u64>, AuthorityStateError> {
        match self.read_u8()? {
            TAG_ABSENT => Ok(None),
            TAG_PRESENT => Ok(Some(self.read_u64()?)),
            _ => Err(AuthorityStateError::Corrupt {
                reason: "an optional value tag is unknown",
            }),
        }
    }

    /// Reads one optional registered reason code.
    fn read_optional_reason(&mut self) -> Result<Option<&'static str>, AuthorityStateError> {
        match self.read_u8()? {
            TAG_ABSENT => Ok(None),
            TAG_PRESENT => {
                let text = self.read_text()?;
                let spec = lookup(text).ok_or(AuthorityStateError::Corrupt {
                    reason: "a reason code is not registered",
                })?;
                Ok(Some(spec.code))
            }
            _ => Err(AuthorityStateError::Corrupt {
                reason: "an optional value tag is unknown",
            }),
        }
    }

    /// Reads one optional hash value.
    fn read_optional_hash(&mut self) -> Result<Option<[u8; 32]>, AuthorityStateError> {
        match self.read_u8()? {
            TAG_ABSENT => Ok(None),
            TAG_PRESENT => Ok(Some(self.read_array32()?)),
            _ => Err(AuthorityStateError::Corrupt {
                reason: "an optional value tag is unknown",
            }),
        }
    }

    /// Rejects trailing bytes.
    fn finish(self) -> Result<(), AuthorityStateError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(AuthorityStateError::Corrupt {
                reason: "the payload has trailing bytes",
            })
        }
    }
}
