//! The durable hydration journal.
//!
//! The journal records the phase, the snapshot identity, every verified page,
//! the applied tail watermark, and the swap commit. A restart loads the journal
//! and resumes at the last durable boundary. A generation becomes visible only
//! after the swap commit.

// The file adapter and its codec exist only on native targets. The core keeps
// the seam. Allow the unused codec items on wasm32.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use std::collections::BTreeSet;

#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

use oxkv_contracts::version::{ContentDigest, PageSequence, ScopeIdentity, ScopeVersionVector};

use crate::error::ReplicaError;

/// One durable hydration phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HydrationPhase {
    /// The replica holds no generation.
    #[default]
    Cold,
    /// The replica transfers and verifies snapshot pages.
    Transfer,
    /// The replica verified every snapshot page.
    Verified,
    /// The replica applies the diff tail.
    Tail,
    /// The replica recorded the swap commit and will publish the new generation.
    Swap,
    /// The replica published the new generation.
    Published,
    /// The replica abandoned the new generation.
    Aborted,
}

impl HydrationPhase {
    /// Returns the canonical phase text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Transfer => "transfer",
            Self::Verified => "verified",
            Self::Tail => "tail",
            Self::Swap => "swap",
            Self::Published => "published",
            Self::Aborted => "aborted",
        }
    }

    /// Returns the durable code of the phase.
    const fn code(self) -> u8 {
        match self {
            Self::Cold => 0,
            Self::Transfer => 1,
            Self::Verified => 2,
            Self::Tail => 3,
            Self::Swap => 4,
            Self::Published => 5,
            Self::Aborted => 6,
        }
    }

    /// Returns the phase for one durable code.
    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Cold),
            1 => Some(Self::Transfer),
            2 => Some(Self::Verified),
            3 => Some(Self::Tail),
            4 => Some(Self::Swap),
            5 => Some(Self::Published),
            6 => Some(Self::Aborted),
            _ => None,
        }
    }

    /// Returns true when the generation is visible to a reader.
    #[must_use]
    pub const fn is_published(self) -> bool {
        matches!(self, Self::Published)
    }

    /// Returns true when the phase stops hydration.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Published | Self::Aborted)
    }
}

/// The durable record of one replica hydration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationJournal {
    /// The current durable phase.
    pub phase: HydrationPhase,
    /// The scope identity of the snapshot.
    pub scope_identity: ScopeIdentity,
    /// The version vector that hydration must reach.
    pub target_version: ScopeVersionVector,
    /// The expected snapshot page count.
    pub expected_pages: u64,
    /// The expected snapshot content digest.
    pub content_digest: ContentDigest,
    /// The generation that hydration builds.
    pub generation: u64,
    /// The generation that the pointer published before hydration.
    pub published_before: Option<u64>,
    /// Every verified page sequence.
    pub verified: BTreeSet<PageSequence>,
    /// The applied tail watermark.
    pub applied_watermark: u64,
    /// The generation named by the durable swap commit.
    pub swap_commit: Option<u64>,
}

impl HydrationJournal {
    /// Creates a journal at the transfer phase.
    #[must_use]
    pub fn new(
        scope_identity: ScopeIdentity,
        target_version: ScopeVersionVector,
        expected_pages: u64,
        content_digest: ContentDigest,
        generation: u64,
        published_before: Option<u64>,
    ) -> Self {
        Self {
            phase: HydrationPhase::Transfer,
            scope_identity,
            target_version,
            expected_pages,
            content_digest,
            generation,
            published_before,
            verified: BTreeSet::new(),
            applied_watermark: 0,
            swap_commit: None,
        }
    }

    /// Returns true when the journal names the same snapshot identity.
    #[must_use]
    pub fn has_identity(
        &self,
        scope_identity: &ScopeIdentity,
        target_version: &ScopeVersionVector,
        expected_pages: u64,
        content_digest: ContentDigest,
    ) -> bool {
        self.scope_identity == *scope_identity
            && self.target_version == *target_version
            && self.expected_pages == expected_pages
            && self.content_digest == content_digest
    }

    /// Returns true when the page sequence is verified.
    #[must_use]
    pub fn is_verified(&self, sequence: PageSequence) -> bool {
        self.verified.contains(&sequence)
    }

    /// Records one verified page. The method is idempotent.
    pub fn record_verified(&mut self, sequence: PageSequence) {
        self.verified.insert(sequence);
    }

    /// Returns the number of verified pages in the contiguous prefix.
    #[must_use]
    pub fn verified_prefix(&self) -> u64 {
        let mut sequence = 0_u64;
        while self.verified.contains(&PageSequence::new(sequence)) {
            sequence = sequence.saturating_add(1);
        }
        sequence
    }

    /// Returns the first page sequence that the journal did not verify.
    #[must_use]
    pub fn first_unverified(&self) -> PageSequence {
        PageSequence::new(self.verified_prefix())
    }

    /// Returns true when every page in the range is verified.
    #[must_use]
    pub fn covers(&self, page_count: u64) -> bool {
        (0..page_count).all(|sequence| self.verified.contains(&PageSequence::new(sequence)))
    }

    /// Advances the applied watermark. The method rejects a gap.
    ///
    /// Returns `false` when the sequence is not the next contiguous value.
    #[must_use]
    pub fn advance_watermark(&mut self, sequence: u64) -> bool {
        if sequence == self.applied_watermark.saturating_add(1) {
            self.applied_watermark = sequence;
            true
        } else {
            false
        }
    }
}

/// The durable journal seam.
///
/// The core takes this seam and holds no I/O. A caller supplies a store. The
/// `FileJournalStore` adapter is native-only. A wasm32 caller supplies its
/// own store over the host storage API.
pub trait HydrationJournalStore {
    /// Loads the durable journal.
    ///
    /// # Errors
    ///
    /// Returns a named error when the stored journal is corrupt or unreadable.
    fn load_journal(&self) -> Result<Option<HydrationJournal>, ReplicaError>;

    /// Saves one journal state.
    ///
    /// # Errors
    ///
    /// Returns a named error when the store cannot write the journal.
    fn save_journal(&mut self, journal: &HydrationJournal) -> Result<(), ReplicaError>;
}

/// An in-memory journal store.
#[derive(Debug, Clone, Default)]
pub struct MemoryJournalStore {
    journal: Option<HydrationJournal>,
}

impl MemoryJournalStore {
    /// Creates an empty store.
    #[must_use]
    pub const fn new() -> Self {
        Self { journal: None }
    }

    /// Returns the stored journal.
    #[must_use]
    pub const fn journal(&self) -> Option<&HydrationJournal> {
        self.journal.as_ref()
    }
}

impl HydrationJournalStore for MemoryJournalStore {
    fn load_journal(&self) -> Result<Option<HydrationJournal>, ReplicaError> {
        Ok(self.journal.clone())
    }

    fn save_journal(&mut self, journal: &HydrationJournal) -> Result<(), ReplicaError> {
        self.journal = Some(journal.clone());
        Ok(())
    }
}

/// The journal magic bytes.
const JOURNAL_MAGIC: [u8; 4] = *b"OXJN";

/// The journal format major version that this adapter reads and writes.
const JOURNAL_FORMAT_MAJOR: u16 = 1;

/// The journal format minor version that this adapter reads and writes.
const JOURNAL_FORMAT_MINOR: u16 = 0;

/// The fixed header byte count before the payload.
const JOURNAL_HEADER_BYTES: usize = 4 + 2 + 2 + 8;

/// A durable journal store over one file path.
///
/// The adapter writes one canonical binary frame. It replaces the file with a
/// rename after it syncs the new bytes. A crash leaves the old frame or the new
/// frame. A corrupt frame fails closed with a named error.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileJournalStore {
    path: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileJournalStore {
    /// Creates a store for one caller-supplied file path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the journal file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl HydrationJournalStore for FileJournalStore {
    fn load_journal(&self) -> Result<Option<HydrationJournal>, ReplicaError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => decode_journal(&bytes).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(ReplicaError::JournalIo { operation: "load" }),
        }
    }

    fn save_journal(&mut self, journal: &HydrationJournal) -> Result<(), ReplicaError> {
        use std::io::Write as _;

        let bytes = encode_journal(journal);
        let temporary = temporary_path(&self.path);
        let mut file = std::fs::File::create(&temporary).map_err(|_| ReplicaError::JournalIo {
            operation: "create",
        })?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ReplicaError::JournalIo { operation: "write" })?;
        drop(file);
        std::fs::rename(&temporary, &self.path).map_err(|_| {
            let _ = std::fs::remove_file(&temporary);
            ReplicaError::JournalIo {
                operation: "replace",
            }
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from("journal"),
        std::ffi::OsString::from,
    );
    name.push(".tmp");
    path.with_file_name(name)
}

/// Encodes one journal frame.
fn encode_journal(journal: &HydrationJournal) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(journal.phase.code());
    payload.extend_from_slice(journal.scope_identity.as_bytes());
    let vector = journal.target_version.as_vector();
    write_u32(
        &mut payload,
        u32::try_from(vector.len()).unwrap_or(u32::MAX),
    );
    for (shard_id, commit_version) in vector.iter() {
        write_bytes(&mut payload, shard_id.as_bytes());
        write_u64(&mut payload, commit_version.get());
    }
    write_u64(&mut payload, journal.expected_pages);
    payload.extend_from_slice(journal.content_digest.as_bytes());
    write_u64(&mut payload, journal.generation);
    write_optional_u64(&mut payload, journal.published_before);
    write_u32(
        &mut payload,
        u32::try_from(journal.verified.len()).unwrap_or(u32::MAX),
    );
    for sequence in &journal.verified {
        write_u64(&mut payload, sequence.get());
    }
    write_u64(&mut payload, journal.applied_watermark);
    write_optional_u64(&mut payload, journal.swap_commit);

    let mut bytes = Vec::with_capacity(JOURNAL_HEADER_BYTES + payload.len() + 32);
    bytes.extend_from_slice(&JOURNAL_MAGIC);
    write_u16(&mut bytes, JOURNAL_FORMAT_MAJOR);
    write_u16(&mut bytes, JOURNAL_FORMAT_MINOR);
    write_u64(&mut bytes, u64::try_from(payload.len()).unwrap_or(u64::MAX));
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(ContentDigest::of_bytes(&payload).as_bytes());
    bytes
}

/// Decodes one journal frame.
///
/// The function rejects a frame with a bad magic value, an unknown format
/// version, a truncated payload, a bad digest, or an unknown phase.
fn decode_journal(bytes: &[u8]) -> Result<HydrationJournal, ReplicaError> {
    let mut reader = JournalReader::new(bytes);
    if reader.exact(4)? != JOURNAL_MAGIC.as_slice() {
        return Err(corrupt("the journal magic does not match"));
    }
    let major = reader.u16()?;
    let _minor = reader.u16()?;
    if major != JOURNAL_FORMAT_MAJOR {
        return Err(corrupt("the journal format version is not supported"));
    }
    let payload_length = reader.u64()?;
    let payload_length = usize::try_from(payload_length)
        .map_err(|_| corrupt("the journal payload length is too large"))?;
    let payload = reader.exact(payload_length)?;
    let digest = ContentDigest::from_bytes(reader.hash32()?);
    if digest != ContentDigest::of_bytes(payload) {
        return Err(corrupt("the journal digest does not match"));
    }
    reader.finish()?;

    let mut payload = JournalReader::new(payload);
    let phase = HydrationPhase::from_code(payload.u8()?)
        .ok_or_else(|| corrupt("the journal phase is unknown"))?;
    let scope_identity = ScopeIdentity::from_bytes(payload.hash32()?);
    let vector_count = payload.u32()?;
    let mut vector = oxkv_contracts::version::VersionVector::new();
    for _ in 0..vector_count {
        let shard_id = oxkv_contracts::version::ShardId::new(payload.bytes()?.to_vec());
        let commit_version = oxkv_contracts::version::CommitVersion::new(payload.u64()?);
        vector.set(shard_id, commit_version);
    }
    let expected_pages = payload.u64()?;
    let content_digest = ContentDigest::from_bytes(payload.hash32()?);
    let generation = payload.u64()?;
    let published_before = payload.optional_u64()?;
    let verified_count = payload.u32()?;
    let mut verified = BTreeSet::new();
    for _ in 0..verified_count {
        verified.insert(PageSequence::new(payload.u64()?));
    }
    let applied_watermark = payload.u64()?;
    let swap_commit = payload.optional_u64()?;
    payload.finish()?;

    Ok(HydrationJournal {
        phase,
        scope_identity,
        target_version: ScopeVersionVector::from_vector(vector),
        expected_pages,
        content_digest,
        generation,
        published_before,
        verified,
        applied_watermark,
        swap_commit,
    })
}

fn corrupt(reason: &'static str) -> ReplicaError {
    ReplicaError::JournalCorrupt { reason }
}

fn write_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    write_u32(bytes, u32::try_from(value.len()).unwrap_or(u32::MAX));
    bytes.extend_from_slice(value);
}

fn write_optional_u64(bytes: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            bytes.push(1);
            write_u64(bytes, value);
        }
        None => bytes.push(0),
    }
}

/// A strict reader over one journal frame.
struct JournalReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> JournalReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn exact(&mut self, length: usize) -> Result<&'a [u8], ReplicaError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| corrupt("the journal length overflows"))?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| corrupt("the journal payload is truncated"))?;
        self.offset = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, ReplicaError> {
        Ok(self.exact(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ReplicaError> {
        let bytes = self.exact(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, ReplicaError> {
        let bytes = self.exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, ReplicaError> {
        let bytes = self.exact(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn bytes(&mut self) -> Result<&'a [u8], ReplicaError> {
        let length = self.u32()?;
        let length =
            usize::try_from(length).map_err(|_| corrupt("the journal byte length is too large"))?;
        self.exact(length)
    }

    fn hash32(&mut self) -> Result<[u8; 32], ReplicaError> {
        let bytes = self.exact(32)?;
        let mut hash = [0_u8; 32];
        hash.copy_from_slice(bytes);
        Ok(hash)
    }

    fn optional_u64(&mut self) -> Result<Option<u64>, ReplicaError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.u64()?)),
            _ => Err(corrupt("the journal optional flag is unknown")),
        }
    }

    fn finish(&self) -> Result<(), ReplicaError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(corrupt("the journal has trailing bytes"))
        }
    }
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::version::{CommitVersion, ShardId, VersionVector};

    use super::*;

    fn sample_journal() -> HydrationJournal {
        let mut journal = HydrationJournal::new(
            ScopeIdentity::from_bytes([5; 32]),
            ScopeVersionVector::from_vector(VersionVector::from_entries([(
                ShardId::new(vec![1]),
                CommitVersion::new(7),
            )])),
            3,
            ContentDigest::of_bytes(b"snapshot"),
            2,
            Some(1),
        );
        journal.record_verified(PageSequence::new(0));
        journal.record_verified(PageSequence::new(2));
        assert!(journal.advance_watermark(1));
        journal.phase = HydrationPhase::Swap;
        journal.swap_commit = Some(2);
        journal
    }

    #[test]
    fn a_journal_frame_round_trips() {
        let journal = sample_journal();
        let bytes = encode_journal(&journal);
        assert_eq!(decode_journal(&bytes).expect("the frame decodes"), journal);
    }
}
