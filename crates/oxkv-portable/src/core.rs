//! The one semantic core.
//!
//! The core applies one operation and returns one observation. It reads time
//! and entropy through [`Ports`]. It reads and writes data through the
//! [`Storage`] adapter. It sends opaque frames through the [`Transport`]
//! adapter.
//!
//! This module has no target conditional. It has no direct clock, randomness,
//! environment, locale, filesystem, network, or thread call. It uses no
//! floating point.

use oxkv_contracts::InternalDiagnostic;
use oxkv_contracts::cursor::{CursorDirection, CursorToken, KeyCursor};
use oxkv_contracts::keyspace::KEY_ENCODING_VERSION;
use oxkv_contracts::version::SnapshotVersion;

use crate::adapter::{
    Clock, CommitOutcome, Durability, Entropy, ScanRequest, Storage, StorageError, Transport,
    TransportError,
};
use crate::artifact::{
    Component, Event, EventInput, KeyError, Page, PageInput, Snapshot, SnapshotError,
    SnapshotInput, encode_key,
};
use crate::encode::{EncodeError, Encoder};
use crate::verdict::{Receipt, ReceiptSpec, Verdict};

type Records = Vec<(Vec<u8>, Vec<u8>)>;

/// The adapter boundary for one semantic run.
pub struct Ports<'a> {
    /// The storage adapter.
    pub storage: &'a mut dyn Storage,
    /// The clock adapter.
    pub clock: &'a dyn Clock,
    /// The entropy adapter.
    pub entropy: &'a dyn Entropy,
    /// The transport adapter.
    pub transport: &'a dyn Transport,
}

impl<'a> Ports<'a> {
    /// Creates one adapter boundary.
    #[must_use]
    pub const fn new(
        storage: &'a mut dyn Storage,
        clock: &'a dyn Clock,
        entropy: &'a dyn Entropy,
        transport: &'a dyn Transport,
    ) -> Self {
        Self {
            storage,
            clock,
            entropy,
            transport,
        }
    }
}

/// One operation in a portable transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Encode one canonical key.
    Key {
        /// The table name.
        table: String,
        /// The key components.
        key: Vec<Component>,
    },
    /// Write one key.
    Put {
        /// The table name.
        table: String,
        /// The key components.
        key: Vec<Component>,
        /// The value bytes.
        value: Vec<u8>,
        /// The shard commit version.
        commit_version: u64,
    },
    /// Read one key.
    Get {
        /// The table name.
        table: String,
        /// The key components.
        key: Vec<Component>,
    },
    /// Build one canonical snapshot.
    Snapshot {
        /// The table name.
        table: String,
        /// The shard identifier.
        shard_id: Vec<u8>,
        /// The inclusive lower bound.
        prefix_lower: Vec<u8>,
        /// The exclusive upper bound.
        prefix_upper: Vec<u8>,
        /// The manifest digest.
        manifest_digest: [u8; 32],
        /// The shard commit version.
        commit_version: u64,
        /// The authority lease term.
        lease_term: u64,
        /// The maximum records per page.
        page_records: usize,
    },
    /// Build one canonical page.
    Page {
        /// The table name.
        table: String,
        /// The shard identifier.
        shard_id: Vec<u8>,
        /// The inclusive lower bound.
        prefix_lower: Vec<u8>,
        /// The exclusive upper bound.
        prefix_upper: Vec<u8>,
        /// The shard commit version.
        commit_version: u64,
        /// The page sequence.
        page_sequence: u64,
        /// Return keys strictly after this key.
        after: Option<Vec<u8>>,
        /// The maximum records.
        limit: usize,
    },
    /// Build one key cursor token.
    Cursor {
        /// The table name.
        table: String,
        /// The snapshot version.
        snapshot_version: u64,
        /// The last returned key.
        last_key: Vec<u8>,
        /// True for ascending order.
        ascending: bool,
    },
    /// Build one business event.
    Event {
        /// The shard identifier.
        shard_id: Vec<u8>,
        /// The table name.
        table: String,
        /// The canonical key bytes.
        key: Vec<u8>,
        /// The key sequence inside the commit.
        key_sequence: u64,
        /// The shard commit version.
        commit_version: u64,
        /// The rule identity.
        rule_id: String,
        /// The pinned policy version.
        policy_version: [u8; 32],
        /// The emission index inside the commit.
        emission_index: u64,
        /// The effect payload.
        payload: Vec<u8>,
        /// The payload schema.
        payload_schema: String,
        /// The origin request identity.
        origin_request_id: [u8; 32],
    },
    /// Build one receipt and its verdict.
    Receipt {
        /// The receipt facts.
        spec: ReceiptSpec,
    },
    /// Draw deterministic entropy through the adapter.
    Nonce {
        /// The number of bytes.
        length: usize,
    },
    /// Send one opaque frame through the transport.
    Send {
        /// The frame bytes.
        frame: Vec<u8>,
    },
}

impl Operation {
    pub(crate) fn table(&self) -> Option<&str> {
        match self {
            Self::Key { table, .. }
            | Self::Put { table, .. }
            | Self::Get { table, .. }
            | Self::Snapshot { table, .. }
            | Self::Page { table, .. }
            | Self::Cursor { table, .. } => Some(table),
            Self::Event { .. } | Self::Receipt { .. } | Self::Nonce { .. } | Self::Send { .. } => {
                None
            }
        }
    }
}

/// One observable result from the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// A canonical key.
    Key(Vec<u8>),
    /// A canonical snapshot.
    Snapshot(Vec<u8>),
    /// A canonical page.
    Page(Vec<u8>),
    /// An opaque cursor token.
    Cursor(Vec<u8>),
    /// A canonical business event.
    Event(Vec<u8>),
    /// A canonical receipt and its verdict.
    Receipt {
        /// The deterministic verdict.
        verdict: Verdict,
        /// The receipt envelope bytes.
        envelope: Vec<u8>,
    },
    /// The result of one atomic commit.
    Commit(CommitOutcome),
    /// One read value.
    Row(Option<(Vec<u8>, Vec<u8>)>),
    /// One entropy draw.
    Entropy(Vec<u8>),
    /// One transport response.
    Wire(Vec<u8>),
}

impl Observation {
    /// Returns the stable observation tag.
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self {
            Self::Key(_) => 1,
            Self::Snapshot(_) => 2,
            Self::Page(_) => 3,
            Self::Cursor(_) => 4,
            Self::Event(_) => 5,
            Self::Receipt { .. } => 6,
            Self::Commit(_) => 7,
            Self::Row(_) => 8,
            Self::Entropy(_) => 9,
            Self::Wire(_) => 10,
        }
    }

    /// Encodes the observation in canonical form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.u8(self.tag());
        match self {
            Self::Key(bytes)
            | Self::Snapshot(bytes)
            | Self::Page(bytes)
            | Self::Cursor(bytes)
            | Self::Event(bytes)
            | Self::Entropy(bytes)
            | Self::Wire(bytes) => encoder.bytes(bytes),
            Self::Receipt { verdict, envelope } => {
                encoder.bytes(&verdict.encode());
                encoder.bytes(envelope);
            }
            Self::Commit(outcome) => {
                encoder.u64(outcome.applied);
                encoder.u8(outcome.durability.tag());
            }
            Self::Row(row) => match row {
                Some((key, value)) => {
                    encoder.u8(1);
                    encoder.bytes(key);
                    encoder.bytes(value);
                }
                None => encoder.u8(0),
            },
        }
        encoder.finish()
    }
}

/// An error from the semantic core.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    /// A key is invalid.
    #[error("The key is invalid.")]
    Key(#[from] KeyError),
    /// A snapshot or page is invalid.
    #[error("The snapshot or page is invalid.")]
    Snapshot(#[from] SnapshotError),
    /// The storage adapter failed.
    #[error("The storage adapter failed.")]
    Storage(#[from] StorageError),
    /// The transport adapter failed.
    #[error("The transport adapter failed.")]
    Transport(#[from] TransportError),
    /// The canonical encoder failed.
    #[error("The canonical encoder failed.")]
    Encode(#[from] EncodeError),
    /// The executor left one slot empty.
    #[error("The executor left slot {slot} empty.")]
    Incomplete {
        /// The empty slot index.
        slot: usize,
    },
}

impl CoreError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Key(error) => error.code(),
            Self::Snapshot(error) => error.code(),
            Self::Storage(error) => error.code(),
            Self::Transport(error) => error.code(),
            Self::Encode(error) => error.code(),
            Self::Incomplete { .. } => "internal.invariant_violation",
        }
    }

    /// Returns the operator diagnostic for an internal invariant violation.
    ///
    /// The diagnostic is for operators and logs. It is not a wire value. The
    /// public code stays `internal.invariant_violation`.
    #[must_use]
    pub const fn diagnostic(&self) -> Option<InternalDiagnostic> {
        match self {
            Self::Incomplete { .. } => Some(InternalDiagnostic::PortableCodec),
            Self::Storage(error) => error.diagnostic(),
            Self::Key(_) | Self::Snapshot(_) | Self::Transport(_) | Self::Encode(_) => None,
        }
    }
}

/// The one semantic core.
pub struct SemanticCore<'a> {
    ports: Ports<'a>,
}

impl<'a> SemanticCore<'a> {
    /// Creates one semantic core over the adapter boundary.
    #[must_use]
    pub const fn new(ports: Ports<'a>) -> Self {
        Self { ports }
    }

    /// Applies one operation and returns one observation.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] when a key, a snapshot, an adapter, or an
    /// encoding step fails.
    #[allow(clippy::too_many_lines)]
    pub fn apply(&mut self, operation: &Operation) -> Result<Observation, CoreError> {
        match operation {
            Operation::Key { table, key } => {
                let bytes = encode_key(table, key)?;
                Ok(Observation::Key(bytes))
            }
            Operation::Put {
                table,
                key,
                value,
                commit_version: _,
            } => {
                let key = encode_key(table, key)?;
                let mut batch = crate::adapter::Batch::new();
                batch.put(key, value.clone());
                let outcome = self.ports.storage.commit(&batch)?;
                Ok(Observation::Commit(outcome))
            }
            Operation::Get { table, key } => {
                let key = encode_key(table, key)?;
                let value = self.ports.storage.get(&key)?;
                Ok(Observation::Row(value.map(|value| (key, value))))
            }
            Operation::Snapshot {
                table,
                shard_id,
                prefix_lower,
                prefix_upper,
                manifest_digest,
                commit_version,
                lease_term,
                page_records,
            } => {
                let _ = table;
                let records = scan_all(self.ports.storage, prefix_lower, prefix_upper)?;
                let page_records = (*page_records).max(1);
                let pages = records
                    .chunks(page_records)
                    .enumerate()
                    .map(|(index, chunk)| PageInput {
                        page_sequence: u64::try_from(index.saturating_add(1)).unwrap_or(u64::MAX),
                        records: chunk.to_vec(),
                    })
                    .collect();
                let snapshot = Snapshot::from_input(&SnapshotInput {
                    format_major: 1,
                    format_minor: 0,
                    key_encoding_version: KEY_ENCODING_VERSION.into(),
                    manifest_digest: *manifest_digest,
                    shard_id: shard_id.clone(),
                    prefix_lower: prefix_lower.clone(),
                    prefix_upper: prefix_upper.clone(),
                    commit_version: *commit_version,
                    lease_term: *lease_term,
                    pages,
                });
                Ok(Observation::Snapshot(snapshot.encode()))
            }
            Operation::Page {
                table,
                shard_id,
                prefix_lower,
                prefix_upper,
                commit_version,
                page_sequence,
                after,
                limit,
            } => {
                let _ = table;
                let page = self.ports.storage.range(&ScanRequest {
                    lower: prefix_lower,
                    upper: prefix_upper,
                    after: after.as_deref(),
                    limit: *limit,
                })?;
                let mut records = page.records;
                records.sort_by(|left, right| left.0.cmp(&right.0));
                let page = Page::new(
                    *page_sequence,
                    *commit_version,
                    shard_id,
                    prefix_lower,
                    prefix_upper,
                    records,
                );
                Ok(Observation::Page(page.encode()))
            }
            Operation::Cursor {
                table,
                snapshot_version,
                last_key,
                ascending,
            } => {
                let table = oxkv_contracts::keyspace::TableName::parse(table)
                    .map_err(|_| KeyError::InvalidTableName)?;
                let direction = if *ascending {
                    CursorDirection::Ascending
                } else {
                    CursorDirection::Descending
                };
                let cursor = KeyCursor::new(
                    table,
                    SnapshotVersion::new(*snapshot_version),
                    last_key.clone(),
                    direction,
                );
                Ok(Observation::Cursor(cursor.to_token()))
            }
            Operation::Event {
                shard_id,
                table,
                key,
                key_sequence,
                commit_version,
                rule_id,
                policy_version,
                emission_index,
                payload,
                payload_schema,
                origin_request_id,
            } => {
                let event = Event::from_input(&EventInput {
                    shard_id: shard_id.clone(),
                    table: table.clone(),
                    key: key.clone(),
                    key_sequence: *key_sequence,
                    commit_version: *commit_version,
                    committed_at: self.ports.clock.now_millis(),
                    rule_id: rule_id.clone(),
                    policy_version: *policy_version,
                    emission_index: *emission_index,
                    payload: payload.clone(),
                    payload_schema: payload_schema.clone(),
                    origin_request_id: *origin_request_id,
                });
                Ok(Observation::Event(event.encode()))
            }
            Operation::Receipt { spec } => {
                let receipt = Receipt::from_spec(spec);
                Ok(Observation::Receipt {
                    verdict: receipt.verdict,
                    envelope: receipt.encode(),
                })
            }
            Operation::Nonce { length } => {
                let mut bytes = vec![0_u8; *length];
                self.ports.entropy.fill(&mut bytes);
                Ok(Observation::Entropy(bytes))
            }
            Operation::Send { frame } => {
                let response = self.ports.transport.send(frame)?;
                Ok(Observation::Wire(response))
            }
        }
    }
}

fn scan_all(storage: &dyn Storage, lower: &[u8], upper: &[u8]) -> Result<Records, StorageError> {
    let mut records = Vec::new();
    let mut after: Option<Vec<u8>> = None;
    loop {
        let page = storage.range(&ScanRequest {
            lower,
            upper,
            after: after.as_deref(),
            limit: 1024,
        })?;
        if page.records.is_empty() {
            break;
        }
        records.extend(page.records);
        match page.next {
            Some(next) => {
                if after
                    .as_deref()
                    .is_some_and(|previous| next.as_slice() <= previous)
                {
                    return Err(StorageError::Corrupt);
                }
                after = Some(next);
            }
            None => break,
        }
    }
    records.sort_by(|left, right| left.0.cmp(&right.0));
    records.dedup_by(|left, right| left.0 == right.0);
    Ok(records)
}

/// Returns the durable label of a storage commit, for diagnostics only.
#[must_use]
pub const fn durability_label(durability: Durability) -> &'static str {
    match durability {
        Durability::Memory => "memory",
        Durability::Process => "process",
        Durability::Device => "device",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{DeterministicEntropy, InProcessTransport, MemoryStorage, ScriptedClock};

    #[test]
    fn the_core_reads_time_only_through_the_clock_adapter() {
        let mut storage = MemoryStorage::default();
        let clock = ScriptedClock::fixed(1234);
        let entropy = DeterministicEntropy::new(7);
        let transport = InProcessTransport;
        let mut core = SemanticCore::new(Ports::new(&mut storage, &clock, &entropy, &transport));
        let observation = core
            .apply(&Operation::Event {
                shard_id: vec![1],
                table: "accounts".to_owned(),
                key: vec![2],
                key_sequence: 3,
                commit_version: 4,
                rule_id: "rule".to_owned(),
                policy_version: [5; 32],
                emission_index: 6,
                payload: vec![7],
                payload_schema: "schema".to_owned(),
                origin_request_id: [8; 32],
            })
            .expect("the event is valid");
        let Observation::Event(bytes) = observation else {
            panic!("the core returned an event");
        };
        let mut decoder = crate::encode::Decoder::new(&bytes);
        decoder.take(5).expect("the magic exists");
        decoder.text().expect("the version exists");
        decoder.take(32).expect("the event id exists");
        decoder.bytes().expect("the shard id exists");
        decoder.text().expect("the table exists");
        decoder.bytes().expect("the key exists");
        decoder.u64().expect("the key sequence exists");
        decoder.u64().expect("the commit version exists");
        assert_eq!(decoder.u64(), Ok(1234));
    }

    #[test]
    fn a_snapshot_uses_canonical_key_order() {
        let mut storage = MemoryStorage::default();
        storage.insert(vec![3], vec![30]);
        storage.insert(vec![1], vec![10]);
        storage.insert(vec![2], vec![20]);
        let clock = ScriptedClock::fixed(0);
        let entropy = DeterministicEntropy::new(0);
        let transport = InProcessTransport;
        let mut core = SemanticCore::new(Ports::new(&mut storage, &clock, &entropy, &transport));
        let observation = core
            .apply(&Operation::Snapshot {
                table: "accounts".to_owned(),
                shard_id: vec![1],
                prefix_lower: vec![1],
                prefix_upper: vec![4],
                manifest_digest: [0; 32],
                commit_version: 1,
                lease_term: 2,
                page_records: 2,
            })
            .expect("the snapshot is valid");
        let Observation::Snapshot(bytes) = observation else {
            panic!("the core returned a snapshot");
        };
        let snapshot = Snapshot::decode(&bytes).expect("the snapshot decodes");
        assert_eq!(snapshot.pages.len(), 2);
        assert_eq!(
            snapshot.pages[0].records,
            vec![(vec![1], vec![10]), (vec![2], vec![20])]
        );
        assert_eq!(snapshot.pages[1].records, vec![(vec![3], vec![30])]);
    }
}
