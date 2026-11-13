//! A deterministic authority and replica model for the scenarios.
//!
//! The model is intentionally small. It holds rows, receipts, an audit chain,
//! a snapshot builder, a wire frame codec, and an artifact codec. Every
//! operation is synchronous and side-effect free outside the model.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::version::Hash32;

use crate::canonical::{CanonicalWriter, digest};
use crate::clock::LogicalClock;
use crate::fault::{CommitStep, FaultPlan};

/// One committed row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) value: Vec<u8>,
    pub(crate) key_version: u64,
}

/// The outcome of one write attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WriteOutcome {
    /// The write is durable.
    Committed {
        /// The shard commit version.
        commit_version: u64,
        /// The new key version.
        key_version: u64,
        /// The receipt identity.
        receipt_id: Hash32,
        /// The steps that ran.
        steps: Vec<CommitStep>,
    },
    /// The base version did not match.
    Conflict {
        /// The current key version. Zero means the key is absent.
        current_key_version: u64,
        /// The steps that ran.
        steps: Vec<CommitStep>,
    },
    /// The authority refused the write.
    Rejected {
        /// The canonical error code.
        code: &'static str,
        /// The steps that ran.
        steps: Vec<CommitStep>,
    },
    /// The commit crashed at one step.
    Crashed {
        /// The step that crashed.
        step: CommitStep,
        /// The steps that ran before the crash.
        steps: Vec<CommitStep>,
    },
}

impl WriteOutcome {
    /// Returns the canonical outcome name.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Committed { .. } => "committed",
            Self::Conflict { .. } => "conflict",
            Self::Rejected { .. } => "rejected",
            Self::Crashed { .. } => "crashed",
        }
    }

    /// Returns the named error code when the outcome is not `Committed`.
    pub(crate) fn error_code(&self) -> Option<&'static str> {
        match self {
            Self::Committed { .. } | Self::Crashed { .. } => None,
            Self::Conflict { .. } => Some("write.version_conflict"),
            Self::Rejected { code, .. } => Some(code),
        }
    }
}

/// One committed state.
#[derive(Debug, Clone, Default)]
pub(crate) struct World {
    pub(crate) rows: BTreeMap<Vec<u8>, Row>,
    pub(crate) index: BTreeSet<Vec<u8>>,
    pub(crate) receipts: BTreeMap<[u8; 32], u8>,
    pub(crate) audit: Vec<Hash32>,
    pub(crate) policy_versions: BTreeMap<u32, String>,
    pub(crate) active_policy: u32,
    pub(crate) commit_version: u64,
    pub(crate) generation: u64,
    pub(crate) history: Vec<WireFrame>,
    pub(crate) clock: LogicalClock,
}

impl World {
    /// Creates an empty world at logical time zero.
    #[must_use]
    pub(crate) fn new() -> Self {
        let mut world = Self {
            clock: LogicalClock::new(0, 1),
            ..Self::default()
        };
        world.policy_versions.insert(1, "v1".to_owned());
        world.active_policy = 1;
        world
    }

    /// Returns the row count.
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// Returns one row.
    pub(crate) fn row(&self, key: &[u8]) -> Option<&Row> {
        self.rows.get(key)
    }

    /// Writes one row with an optional base key version.
    ///
    /// A crash leaves the published state unchanged. The journal, the index,
    /// the receipts, and the audit chain change together at the publish step.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn commit_write(
        &mut self,
        write_index: u64,
        key: &[u8],
        value: &[u8],
        base: Option<u64>,
        plan: &FaultPlan,
    ) -> WriteOutcome {
        let mut steps = Vec::new();
        let _now = self.clock.tick();

        steps.push(CommitStep::Validate);
        if plan.crashes_at(CommitStep::Validate) {
            return WriteOutcome::Crashed {
                step: CommitStep::Validate,
                steps,
            };
        }

        // The conditional-write header is lost in transit. The authority sees
        // an unconditional write and refuses it because the write carried a
        // base version on the client side.
        let lost = plan.faults().iter().any(|fault| {
            matches!(
                fault,
                crate::fault::Fault::ConditionalWriteLoss { write_index: index }
                    if *index == write_index
            )
        });
        if lost && base.is_some() {
            steps.push(CommitStep::Apply);
            return WriteOutcome::Rejected {
                code: "write.missing_base_version",
                steps,
            };
        }

        let prior_version = self.rows.get(key).map(|row| row.key_version);
        if let Some(expected) = base {
            let current = prior_version.unwrap_or(0);
            if current != expected {
                steps.push(CommitStep::Apply);
                return WriteOutcome::Conflict {
                    current_key_version: current,
                    steps,
                };
            }
        }

        steps.push(CommitStep::Apply);
        if plan.crashes_at(CommitStep::Apply) {
            return WriteOutcome::Crashed {
                step: CommitStep::Apply,
                steps,
            };
        }

        let new_key_version = prior_version.unwrap_or(0).saturating_add(1);
        let new_commit_version = self.commit_version.saturating_add(1);
        let receipt_id = digest("receipt", &{
            let mut writer = CanonicalWriter::new();
            writer.bytes(key);
            writer.u64(new_key_version);
            writer.u64(new_commit_version);
            writer.finish()
        });
        let audit_body = {
            let mut writer = CanonicalWriter::new();
            writer.bytes(key);
            writer.bytes(value);
            writer.u64(new_commit_version);
            writer.finish()
        };
        let previous_audit = self
            .audit
            .last()
            .copied()
            .unwrap_or_else(|| Hash32::of(b"genesis"));
        let audit_hash = {
            let mut writer = CanonicalWriter::new();
            writer.bytes(previous_audit.as_bytes());
            writer.bytes(&audit_body);
            Hash32::of(&writer.finish())
        };

        steps.push(CommitStep::Receipt);
        if plan.crashes_at(CommitStep::Receipt) {
            return WriteOutcome::Crashed {
                step: CommitStep::Receipt,
                steps,
            };
        }
        steps.push(CommitStep::Audit);
        if plan.crashes_at(CommitStep::Audit) {
            return WriteOutcome::Crashed {
                step: CommitStep::Audit,
                steps,
            };
        }
        steps.push(CommitStep::Index);
        if plan.crashes_at(CommitStep::Index) {
            return WriteOutcome::Crashed {
                step: CommitStep::Index,
                steps,
            };
        }

        steps.push(CommitStep::Publish);
        if plan.crashes_at(CommitStep::Publish) {
            return WriteOutcome::Crashed {
                step: CommitStep::Publish,
                steps,
            };
        }

        self.rows.insert(
            key.to_vec(),
            Row {
                value: value.to_vec(),
                key_version: new_key_version,
            },
        );
        self.index.insert(key.to_vec());
        self.receipts.insert(*receipt_id.as_bytes(), 2);
        self.audit.push(audit_hash);
        self.commit_version = new_commit_version;
        self.generation = self.generation.saturating_add(1);
        self.history.push(WireFrame {
            index: u64::try_from(self.history.len()).unwrap_or(u64::MAX),
            key: key.to_vec(),
            value: value.to_vec(),
            key_version: new_key_version,
        });

        WriteOutcome::Committed {
            commit_version: new_commit_version,
            key_version: new_key_version,
            receipt_id,
            steps,
        }
    }

    /// Encodes the full state in canonical bytes.
    pub(crate) fn export(&self) -> Vec<u8> {
        let mut writer = CanonicalWriter::new();
        writer.text("oxkv/export/v1");
        writer.u64(self.commit_version);
        writer.u64(self.generation);
        writer.u64(u64::try_from(self.rows.len()).unwrap_or(u64::MAX));
        for (key, row) in &self.rows {
            writer.bytes(key);
            writer.bytes(&row.value);
            writer.u64(row.key_version);
        }
        writer.u64(u64::try_from(self.receipts.len()).unwrap_or(u64::MAX));
        for (receipt, state) in &self.receipts {
            writer.bytes(receipt);
            writer.u8(*state);
        }
        writer.u64(u64::try_from(self.audit.len()).unwrap_or(u64::MAX));
        for hash in &self.audit {
            writer.bytes(hash.as_bytes());
        }
        writer.u64(u64::from(self.active_policy));
        writer.u64(u64::try_from(self.policy_versions.len()).unwrap_or(u64::MAX));
        for (version, content) in &self.policy_versions {
            writer.u64(u64::from(*version));
            writer.text(content);
        }
        writer.finish()
    }

    /// Decodes a full state from canonical bytes.
    pub(crate) fn import(bytes: &[u8]) -> Result<Self, &'static str> {
        let mut reader = Reader::new(bytes);
        let magic = reader.text()?;
        if magic != "oxkv/export/v1" {
            return Err("format.unsupported_version");
        }
        let commit_version = reader.u64()?;
        let generation = reader.u64()?;
        let row_count = reader.count()?;
        let mut rows = BTreeMap::new();
        let mut index = BTreeSet::new();
        for _ in 0..row_count {
            let key = reader.bytes()?.to_vec();
            let value = reader.bytes()?.to_vec();
            let key_version = reader.u64()?;
            index.insert(key.clone());
            rows.insert(key, Row { value, key_version });
        }
        let receipt_count = reader.count()?;
        let mut receipts = BTreeMap::new();
        for _ in 0..receipt_count {
            let receipt = reader.bytes()?;
            let receipt: [u8; 32] = receipt
                .try_into()
                .map_err(|_| "format.truncated_snapshot")?;
            let state = reader.u8()?;
            receipts.insert(receipt, state);
        }
        let audit_count = reader.count()?;
        let mut audit = Vec::new();
        for _ in 0..audit_count {
            let hash = reader.bytes()?;
            let hash: [u8; 32] = hash.try_into().map_err(|_| "format.truncated_snapshot")?;
            audit.push(Hash32::from_bytes(hash));
        }
        let active_policy =
            u32::try_from(reader.u64()?).map_err(|_| "format.truncated_snapshot")?;
        let policy_count = reader.count()?;
        let mut policy_versions = BTreeMap::new();
        for _ in 0..policy_count {
            let version = u32::try_from(reader.u64()?).map_err(|_| "format.truncated_snapshot")?;
            let content = reader.text()?;
            policy_versions.insert(version, content);
        }
        reader.finish()?;
        Ok(Self {
            rows,
            index,
            receipts,
            audit,
            policy_versions,
            active_policy,
            commit_version,
            generation,
            history: Vec::new(),
            clock: LogicalClock::new(0, 1),
        })
    }

    /// Returns the frames after one applied count.
    pub(crate) fn frames_after(&self, applied: u64) -> Vec<WireFrame> {
        self.history
            .iter()
            .filter(|frame| frame.index >= applied)
            .cloned()
            .collect()
    }

    /// Applies one frame. A lower key version is a duplicate and changes no
    /// state.
    pub(crate) fn apply_frame(&mut self, frame: &WireFrame) -> bool {
        if let Some(row) = self.rows.get(&frame.key)
            && row.key_version >= frame.key_version
        {
            return false;
        }
        self.rows.insert(
            frame.key.clone(),
            Row {
                value: frame.value.clone(),
                key_version: frame.key_version,
            },
        );
        self.index.insert(frame.key.clone());
        true
    }

    /// Rebuilds the index from the rows.
    pub(crate) fn rebuild_index(&mut self) {
        self.index = self.rows.keys().cloned().collect();
    }
}

/// CRC32C over a byte slice.
#[must_use]
pub(crate) fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            if crc & 1 == 1 {
                crc = (crc >> 1) ^ 0x82f6_3b78;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// One snapshot page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotPage {
    pub(crate) sequence: u64,
    pub(crate) payload: Vec<u8>,
    pub(crate) checksum: u32,
}

/// A snapshot artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) format_major: u16,
    pub(crate) format_minor: u16,
    pub(crate) commit_version: u64,
    pub(crate) rows: Vec<(Vec<u8>, Vec<u8>, u64)>,
    pub(crate) pages: Vec<SnapshotPage>,
}

impl Snapshot {
    /// Builds a snapshot from one world.
    pub(crate) fn build(world: &World, chunk_size: usize) -> Self {
        let rows: Vec<(Vec<u8>, Vec<u8>, u64)> = world
            .rows
            .iter()
            .map(|(key, row)| (key.clone(), row.value.clone(), row.key_version))
            .collect();
        let mut body = CanonicalWriter::new();
        body.u64(u64::try_from(rows.len()).unwrap_or(u64::MAX));
        for (key, value, key_version) in &rows {
            body.bytes(key);
            body.bytes(value);
            body.u64(*key_version);
        }
        let body = body.finish();
        let chunk = chunk_size.max(1);
        let pages = body
            .chunks(chunk)
            .enumerate()
            .map(|(index, payload)| SnapshotPage {
                sequence: u64::try_from(index).unwrap_or(u64::MAX),
                payload: payload.to_vec(),
                checksum: crc32c(payload),
            })
            .collect();
        Self {
            format_major: 1,
            format_minor: 0,
            commit_version: world.commit_version,
            rows,
            pages,
        }
    }

    /// Returns the full page body.
    pub(crate) fn body(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for page in &self.pages {
            body.extend_from_slice(&page.payload);
        }
        body
    }

    /// Verifies the page sequence and every checksum.
    pub(crate) fn verify(&self) -> Result<(), &'static str> {
        if self.format_major != 1 {
            return Err("format.unsupported_version");
        }
        let mut expected = 0;
        for page in &self.pages {
            if page.sequence != expected {
                return Err("format.page_out_of_bounds");
            }
            if crc32c(&page.payload) != page.checksum {
                return Err("format.corrupt_page");
            }
            expected = expected.saturating_add(1);
        }
        Ok(())
    }

    /// Returns the snapshot with one corrupt page.
    pub(crate) fn corrupt(mut self, page_sequence: u64) -> Self {
        if let Some(page) = self
            .pages
            .iter_mut()
            .find(|page| page.sequence == page_sequence)
        {
            page.checksum ^= 0xffff_ffff;
        }
        self
    }

    /// Restores the rows into one world.
    pub(crate) fn restore_into(&self, world: &mut World) -> Result<(), &'static str> {
        self.verify()?;
        let body = self.body();
        let mut reader = Reader::new(&body);
        let count = reader.count()?;
        let mut rows = BTreeMap::new();
        for _ in 0..count {
            let key = reader.bytes()?.to_vec();
            let value = reader.bytes()?.to_vec();
            let key_version = reader.u64()?;
            rows.insert(key, Row { value, key_version });
        }
        reader.finish()?;
        world.rows = rows;
        world.rebuild_index();
        world.commit_version = self.commit_version;
        world.generation = world.generation.saturating_add(1);
        Ok(())
    }
}

/// One sync frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WireFrame {
    pub(crate) index: u64,
    pub(crate) key: Vec<u8>,
    pub(crate) value: Vec<u8>,
    pub(crate) key_version: u64,
}

impl WireFrame {
    /// Encodes the frame in canonical bytes.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut writer = CanonicalWriter::new();
        writer.text("oxkv/frame/v1");
        writer.u64(self.index);
        writer.bytes(&self.key);
        writer.bytes(&self.value);
        writer.u64(self.key_version);
        let body = writer.finish();
        let checksum = crc32c(&body);
        let mut bytes = body;
        bytes.extend_from_slice(&checksum.to_be_bytes());
        bytes
    }

    /// Decodes one frame.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 4 {
            return Err("sync.cursor_invalid");
        }
        let body_length = bytes.len() - 4;
        let checksum = u32::from_be_bytes([
            bytes[body_length],
            bytes[body_length + 1],
            bytes[body_length + 2],
            bytes[body_length + 3],
        ]);
        let body = &bytes[..body_length];
        if crc32c(body) != checksum {
            return Err("sync.cursor_invalid");
        }
        let mut reader = Reader::new(body);
        let magic = reader.text()?;
        if magic != "oxkv/frame/v1" {
            return Err("sync.unsupported_version");
        }
        let index = reader.u64()?;
        let key = reader.bytes()?.to_vec();
        let value = reader.bytes()?.to_vec();
        let key_version = reader.u64()?;
        reader.finish()?;
        Ok(Self {
            index,
            key,
            value,
            key_version,
        })
    }
}

/// A resume cursor over the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WireCursor {
    pub(crate) generation: u64,
    pub(crate) applied: u64,
    pub(crate) floor: u64,
}

impl WireCursor {
    /// Creates a cursor.
    pub(crate) const fn new(generation: u64, applied: u64, floor: u64) -> Self {
        Self {
            generation,
            applied,
            floor,
        }
    }

    /// Encodes the cursor.
    pub(crate) fn encode(self) -> Vec<u8> {
        let mut writer = CanonicalWriter::new();
        writer.text("oxkv/wire-cursor/v1");
        writer.u64(self.generation);
        writer.u64(self.applied);
        writer.u64(self.floor);
        writer.finish()
    }

    /// Decodes the cursor.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let mut reader = Reader::new(bytes);
        if reader.text()? != "oxkv/wire-cursor/v1" {
            return Err("sync.cursor_invalid");
        }
        let generation = reader.u64()?;
        let applied = reader.u64()?;
        let floor = reader.u64()?;
        reader.finish()?;
        Ok(Self {
            generation,
            applied,
            floor,
        })
    }

    /// Returns the canonical text form.
    pub(crate) fn describe(self) -> String {
        format!("g{}:a{}:f{}", self.generation, self.applied, self.floor)
    }
}

/// A self-describing artifact for the format-upgrade scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Artifact {
    pub(crate) format_major: u16,
    pub(crate) format_minor: u16,
    pub(crate) manifest_digest: Option<[u8; 32]>,
    pub(crate) rows: Vec<(Vec<u8>, Vec<u8>, u64)>,
}

impl Artifact {
    /// Encodes the artifact.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut writer = CanonicalWriter::new();
        writer.text("oxkv/artifact/v1");
        writer.u32(u32::from(self.format_major));
        writer.u32(u32::from(self.format_minor));
        writer.u8(u8::from(self.manifest_digest.is_some()));
        if let Some(digest) = self.manifest_digest {
            writer.bytes(&digest);
        }
        writer.u64(u64::try_from(self.rows.len()).unwrap_or(u64::MAX));
        for (key, value, key_version) in &self.rows {
            writer.bytes(key);
            writer.bytes(value);
            writer.u64(*key_version);
        }
        writer.finish()
    }

    /// Decodes the artifact.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let mut reader = Reader::new(bytes);
        if reader.text()? != "oxkv/artifact/v1" {
            return Err("format.unsupported_version");
        }
        let major = u16::try_from(reader.u32()?).map_err(|_| "format.unsupported_version")?;
        let minor = u16::try_from(reader.u32()?).map_err(|_| "format.unsupported_version")?;
        if major != 1 {
            return Err("format.unsupported_version");
        }
        let present = reader.u8()?;
        let manifest_digest = if present == 1 {
            Some(
                reader
                    .bytes()?
                    .try_into()
                    .map_err(|_| "format.missing_provenance")?,
            )
        } else {
            None
        };
        let count = reader.count()?;
        let mut rows = Vec::new();
        for _ in 0..count {
            rows.push((
                reader.bytes()?.to_vec(),
                reader.bytes()?.to_vec(),
                reader.u64()?,
            ));
        }
        reader.finish()?;
        Ok(Self {
            format_major: major,
            format_minor: minor,
            manifest_digest,
            rows,
        })
    }

    /// Verifies the required provenance.
    pub(crate) fn require_provenance(self) -> Result<Self, &'static str> {
        if self.manifest_digest.is_some() {
            Ok(self)
        } else {
            Err("format.missing_provenance")
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], &'static str> {
        let end = self
            .position
            .checked_add(length)
            .ok_or("format.truncated_snapshot")?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or("format.truncated_snapshot")?;
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, &'static str> {
        let bytes = self.take(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn bytes(&mut self) -> Result<&'a [u8], &'static str> {
        let length = self.u64()?;
        let length = usize::try_from(length).map_err(|_| "format.truncated_snapshot")?;
        self.take(length)
    }

    fn text(&mut self) -> Result<String, &'static str> {
        let bytes = self.bytes()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| "key.invalid_utf8")
    }

    fn count(&mut self) -> Result<usize, &'static str> {
        let count = self.u64()?;
        usize::try_from(count).map_err(|_| "format.truncated_snapshot")
    }

    fn finish(&self) -> Result<(), &'static str> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err("format.truncated_snapshot")
        }
    }
}
