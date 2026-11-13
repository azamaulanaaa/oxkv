//! Request signatures.
//!
//! A signature binds the request identity, the body, the idempotency key, the
//! base version, the shard, the nonce, and the expiry. The verifier uses an
//! injected key. The crate does not read a clock or draw randomness.

use core::fmt;

use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
use oxkv_contracts::version::{Hash32, KeyVersion, ShardId};

use crate::auth::LogicalTime;
use crate::error::SecurityError;

/// A 32-byte request nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Nonce([u8; 32]);

impl Nonce {
    /// Creates a nonce from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the raw nonce bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Nonce {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex(&self.0))
    }
}

/// A request signature value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestSignature([u8; 32]);

impl RequestSignature {
    /// Creates a signature from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the raw signature bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for RequestSignature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RequestSignature")
            .field(&hex(&self.0))
            .finish()
    }
}

/// The signed fields of one request.
///
/// The `Debug` output prints the body digest, not the body.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestFields {
    /// The request identity.
    pub request_id: RequestId,
    /// The opaque request body.
    pub body: Vec<u8>,
    /// The client idempotency key.
    pub idempotency_key: IdempotencyKey,
    /// The base key version for the write.
    pub base_version: KeyVersion,
    /// The target shard.
    pub shard_id: ShardId,
    /// The single-use request nonce.
    pub nonce: Nonce,
    /// The last logical time at which the request is valid.
    pub expires_at: LogicalTime,
}

impl RequestFields {
    /// Returns the digest of the idempotent body.
    ///
    /// The digest covers the body and the base version. It does not cover the
    /// nonce or the expiry. A retry keeps the same digest.
    #[must_use]
    pub fn body_digest(&self) -> Hash32 {
        let mut bytes = Vec::new();
        push_bytes(&mut bytes, &self.body);
        push_u64(&mut bytes, self.base_version.get());
        domain_hash(b"oxkv/request-body/v1", &bytes)
    }

    /// Returns the digest of every signed field.
    #[must_use]
    pub fn input_digest(&self) -> Hash32 {
        domain_hash(b"oxkv/request-input/v1", &canonical_bytes(self))
    }
}

impl fmt::Debug for RequestFields {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestFields")
            .field("request_id", &self.request_id)
            .field("body_digest", &self.body_digest())
            .field("idempotency_key", &self.idempotency_key)
            .field("base_version", &self.base_version)
            .field("shard_id", &self.shard_id)
            .field("nonce", &self.nonce)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// A request with a signature.
#[derive(Clone, PartialEq, Eq)]
pub struct SignedRequest {
    fields: RequestFields,
    signature: RequestSignature,
}

impl SignedRequest {
    /// Returns the signed fields.
    #[must_use]
    pub const fn fields(&self) -> &RequestFields {
        &self.fields
    }

    /// Returns the request identity.
    #[must_use]
    pub const fn request_id(&self) -> RequestId {
        self.fields.request_id
    }

    /// Returns the opaque body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.fields.body
    }

    /// Returns the idempotency key.
    #[must_use]
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.fields.idempotency_key
    }

    /// Returns the base key version.
    #[must_use]
    pub const fn base_version(&self) -> KeyVersion {
        self.fields.base_version
    }

    /// Returns the target shard.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.fields.shard_id
    }

    /// Returns the request nonce.
    #[must_use]
    pub const fn nonce(&self) -> Nonce {
        self.fields.nonce
    }

    /// Returns the expiry time.
    #[must_use]
    pub const fn expires_at(&self) -> LogicalTime {
        self.fields.expires_at
    }

    /// Returns the request signature.
    #[must_use]
    pub const fn signature(&self) -> RequestSignature {
        self.signature
    }

    /// Returns the digest of the idempotent body.
    #[must_use]
    pub fn body_digest(&self) -> Hash32 {
        self.fields.body_digest()
    }

    /// Returns the digest of every signed field.
    #[must_use]
    pub fn input_digest(&self) -> Hash32 {
        self.fields.input_digest()
    }
}

impl fmt::Debug for SignedRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignedRequest")
            .field("fields", &self.fields)
            .field("signature", &self.signature)
            .finish()
    }
}

/// An in-tree request key.
///
/// The key prints as `[REDACTED]`. The key binds a request with a keyed
/// `BLAKE3` value. The value is not a wire contract.
pub struct RequestKey([u8; 32]);

impl RequestKey {
    /// Creates a request key.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Signs one set of request fields.
    #[must_use]
    pub fn sign(&self, fields: RequestFields) -> SignedRequest {
        let signature = blake3::keyed_hash(&self.0, &canonical_bytes(&fields));
        SignedRequest {
            fields,
            signature: RequestSignature(*signature.as_bytes()),
        }
    }
}

impl fmt::Debug for RequestKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RequestKey([REDACTED])")
    }
}

/// A verifier for one signed request.
pub trait SignatureVerifier {
    /// Verifies the signature, the expiry, and the signed fields.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::Unauthenticated`] for an expired request or a
    /// signature mismatch.
    fn verify_request(
        &self,
        request: &SignedRequest,
        now: LogicalTime,
    ) -> Result<(), SecurityError>;
}

impl SignatureVerifier for RequestKey {
    fn verify_request(
        &self,
        request: &SignedRequest,
        now: LogicalTime,
    ) -> Result<(), SecurityError> {
        if now > request.expires_at() {
            return Err(SecurityError::Unauthenticated);
        }
        let expected = blake3::keyed_hash(&self.0, &canonical_bytes(request.fields()));
        if !constant_time_eq(expected.as_bytes(), request.signature().as_bytes()) {
            return Err(SecurityError::Unauthenticated);
        }
        Ok(())
    }
}

fn canonical_bytes(fields: &RequestFields) -> Vec<u8> {
    let mut bytes = Vec::new();
    push_bytes(&mut bytes, fields.request_id.as_bytes());
    push_bytes(&mut bytes, &fields.body);
    push_bytes(&mut bytes, fields.idempotency_key.as_bytes());
    push_u64(&mut bytes, fields.base_version.get());
    push_bytes(&mut bytes, fields.shard_id.as_bytes());
    push_bytes(&mut bytes, fields.nonce.as_bytes());
    push_u64(&mut bytes, fields.expires_at.get());
    bytes
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn push_bytes(output: &mut Vec<u8>, value: &[u8]) {
    push_u64(output, u64::try_from(value.len()).unwrap_or(u64::MAX));
    output.extend_from_slice(value);
}

fn domain_hash(tag: &[u8], bytes: &[u8]) -> Hash32 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(tag);
    hasher.update(&[0]);
    hasher.update(bytes);
    Hash32::from_bytes(*hasher.finalize().as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    let mut index = 0;
    while index < left.len() {
        difference |= left[index] ^ right[index];
        index += 1;
    }
    difference == 0
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}
