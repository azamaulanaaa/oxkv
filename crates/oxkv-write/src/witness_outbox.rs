//! A durable, file-backed witness outbox.
//!
//! The commit path stores each signed checkpoint in one file. The store does
//! not wait for witness delivery. A separate worker reads the pending
//! checkpoints and delivers them. A restart reads the pending files and
//! resumes delivery.
//!
//! This module is native only. It uses the file system. The core of the crate
//! stays pure.
//!
//! # File format
//!
//! One checkpoint has one file. The file name is the zero-padded audit
//! sequence with the `checkpoint` extension. The file holds a magic value, a
//! length, the checkpoint body, and a `BLAKE3` checksum of the body. A
//! delivered checkpoint removes its file.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use oxkv_contracts::receipt::AuditSequence;
use oxkv_contracts::version::{Hash32, ShardId, Signature};
use oxkv_security::audit::AuditCheckpoint;
use oxkv_security::secret::KeyIdentifier;
use oxkv_security::witness::WitnessSink;

const MAGIC: &[u8; 8] = b"OXKVOUT1";
const EXTENSION: &str = "checkpoint";
const TEMPORARY_EXTENSION: &str = "tmp";
const CHECKSUM_LEN: usize = 32;
const HEADER_LEN: usize = 8 + 4;

/// A durable file-backed outbox for signed witness checkpoints.
///
/// The type implements [`WitnessSink`]. The commit path calls `enqueue` after
/// the commit. The call writes one file. It does not wait for delivery.
#[derive(Debug)]
pub struct FileWitnessOutbox {
    directory: PathBuf,
    pending: VecDeque<AuditCheckpoint>,
    last_error: Option<String>,
}

impl FileWitnessOutbox {
    /// Opens an outbox in one directory.
    ///
    /// The method creates the directory when it is absent. The method reads
    /// every pending checkpoint in audit sequence order. A restart therefore
    /// resumes the pending delivery.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the directory is not readable or writable.
    /// Returns [`io::ErrorKind::InvalidData`] when one checkpoint file is
    /// corrupt.
    pub fn open(directory: impl AsRef<Path>) -> io::Result<Self> {
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory)?;
        let mut paths = Vec::new();
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().and_then(OsStr::to_str) == Some(EXTENSION) {
                paths.push(path);
            }
        }
        paths.sort();
        let mut pending = VecDeque::with_capacity(paths.len());
        for path in paths {
            let bytes = fs::read(&path)?;
            let checkpoint = decode_checkpoint(&bytes)
                .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;
            pending.push_back(checkpoint);
        }
        Ok(Self {
            directory,
            pending,
            last_error: None,
        })
    }

    /// Returns the outbox directory.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Returns the number of checkpoints that wait for delivery.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Returns true when no checkpoint waits for delivery.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Returns the pending checkpoints in audit sequence order.
    #[must_use]
    pub fn pending(&self) -> &VecDeque<AuditCheckpoint> {
        &self.pending
    }

    /// Returns the last storage error.
    ///
    /// The method returns a value when the last `enqueue` call could not write
    /// the checkpoint file. The checkpoint stays in memory for delivery.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Delivers one pending checkpoint to a witness sink.
    ///
    /// The method removes the checkpoint file after the sink accepts the
    /// checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the checkpoint file cannot be removed. The
    /// checkpoint stays pending after the error.
    pub fn deliver_next(
        &mut self,
        witness: &mut dyn WitnessSink,
    ) -> io::Result<Option<AuditCheckpoint>> {
        let Some(checkpoint) = self.pending.pop_front() else {
            return Ok(None);
        };
        let path = self.path_for(&checkpoint);
        if let Err(error) = fs::remove_file(&path) {
            self.pending.push_front(checkpoint);
            return Err(error);
        }
        witness.enqueue(checkpoint.clone());
        Ok(Some(checkpoint))
    }

    /// Delivers every pending checkpoint to a witness sink.
    ///
    /// The method returns the number of delivered checkpoints.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a checkpoint file cannot be removed.
    pub fn drain_to(&mut self, witness: &mut dyn WitnessSink) -> io::Result<usize> {
        let mut delivered = 0;
        while self.deliver_next(witness)?.is_some() {
            delivered += 1;
        }
        Ok(delivered)
    }

    /// Returns the file path for one checkpoint.
    fn path_for(&self, checkpoint: &AuditCheckpoint) -> PathBuf {
        self.directory.join(format!(
            "{:020}.{EXTENSION}",
            checkpoint.audit_sequence.get()
        ))
    }

    /// Writes one checkpoint file.
    fn persist(&self, checkpoint: &AuditCheckpoint) -> io::Result<()> {
        let path = self.path_for(checkpoint);
        let temporary = path.with_extension(TEMPORARY_EXTENSION);
        fs::write(&temporary, encode_checkpoint(checkpoint))?;
        fs::rename(&temporary, &path)
    }
}

impl WitnessSink for FileWitnessOutbox {
    fn enqueue(&mut self, checkpoint: AuditCheckpoint) {
        if let Err(error) = self.persist(&checkpoint) {
            self.last_error = Some(error.to_string());
        }
        self.pending.push_back(checkpoint);
    }
}

/// Encodes one checkpoint for the outbox file.
fn encode_checkpoint(checkpoint: &AuditCheckpoint) -> Vec<u8> {
    let mut payload = Vec::new();
    push_bytes(&mut payload, checkpoint.shard_id.as_bytes());
    push_u64(&mut payload, checkpoint.audit_sequence.get());
    payload.extend_from_slice(checkpoint.record_hash.as_bytes());
    payload.extend_from_slice(checkpoint.previous_checkpoint_hash.as_bytes());
    push_bytes(&mut payload, checkpoint.key_id.as_str().as_bytes());
    payload.extend_from_slice(checkpoint.signature.as_bytes());
    payload.extend_from_slice(checkpoint.checkpoint_hash.as_bytes());
    let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len() + CHECKSUM_LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(Hash32::of(&payload).as_bytes());
    bytes
}

/// Decodes one checkpoint from an outbox file.
fn decode_checkpoint(bytes: &[u8]) -> Result<AuditCheckpoint, &'static str> {
    if bytes.len() < HEADER_LEN + CHECKSUM_LEN {
        return Err("The outbox file is too short.");
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        return Err("The outbox file has an unknown format.");
    }
    let mut length_bytes = [0_u8; 4];
    length_bytes.copy_from_slice(&bytes[MAGIC.len()..HEADER_LEN]);
    let length = usize::try_from(u32::from_be_bytes(length_bytes))
        .map_err(|_| "The outbox file has an invalid length.")?;
    let payload_end = HEADER_LEN
        .checked_add(length)
        .ok_or("The outbox file has an invalid length.")?;
    if bytes.len() != payload_end + CHECKSUM_LEN {
        return Err("The outbox file has an invalid length.");
    }
    let payload = &bytes[HEADER_LEN..payload_end];
    if Hash32::of(payload).as_bytes() != &bytes[payload_end..] {
        return Err("The outbox file checksum does not match.");
    }
    let mut reader = Reader::new(payload);
    let shard_id = ShardId::new(reader.bytes_field()?.to_vec());
    let audit_sequence = AuditSequence::new(reader.read_u64()?);
    let record_hash = reader.read_hash32()?;
    let previous_checkpoint_hash = reader.read_hash32()?;
    let key_id = core::str::from_utf8(reader.bytes_field()?)
        .map_err(|_| "The outbox file has an invalid key identifier.")?;
    let key_id = KeyIdentifier::new(key_id);
    let mut signature = [0_u8; 64];
    signature.copy_from_slice(reader.take(64)?);
    let checkpoint_hash = reader.read_hash32()?;
    if reader.remaining() != 0 {
        return Err("The outbox file has trailing bytes.");
    }
    Ok(AuditCheckpoint {
        shard_id,
        audit_sequence,
        record_hash,
        previous_checkpoint_hash,
        key_id,
        signature: Signature::from_bytes(signature),
        checkpoint_hash,
    })
}

/// A bounds-checked reader for one outbox payload.
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
    fn take(&mut self, length: usize) -> Result<&'a [u8], &'static str> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or("The outbox file has an invalid length.")?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or("The outbox file is too short.")?;
        self.offset = end;
        Ok(value)
    }

    /// Reads one big-endian `u64`.
    fn read_u64(&mut self) -> Result<u64, &'static str> {
        let mut value = [0_u8; 8];
        value.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(value))
    }

    /// Reads one length-prefixed byte field.
    fn bytes_field(&mut self) -> Result<&'a [u8], &'static str> {
        let length = usize::try_from(self.read_u64()?)
            .map_err(|_| "The outbox file has an invalid length.")?;
        self.take(length)
    }

    /// Reads one hash value.
    fn read_hash32(&mut self) -> Result<Hash32, &'static str> {
        let mut value = [0_u8; 32];
        value.copy_from_slice(self.take(32)?);
        Ok(Hash32::from_bytes(value))
    }
}

/// Appends one length-prefixed byte field.
fn push_bytes(output: &mut Vec<u8>, value: &[u8]) {
    push_u64(output, u64::try_from(value.len()).unwrap_or(u64::MAX));
    output.extend_from_slice(value);
}

/// Appends one big-endian `u64`.
fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}
