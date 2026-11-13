//! The Connect RPC envelope.
//!
//! Connect is the RPC envelope for generated RPC. One call carries one request.
//! A server-streaming call carries zero or more stream frames and one end
//! frame. A stream frame payload is opaque bytes. A sync frame travels in that
//! payload without a second schema.
//!
//! This module is sans-IO. It encodes and decodes frames in memory. It does not
//! open a socket.

use oxkv_contracts::ErrorCode;

use crate::error::TransportFailure;

/// The Connect frame magic.
pub const FRAME_MAGIC: &[u8; 8] = b"OXKVCN01";

/// The RPC codec selected in `SessionHello`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectCodec {
    /// JSON with stable field order.
    Json,
    /// The binary codec.
    Binary,
    /// A codec that this build does not provide.
    Other(&'static str),
}

impl ConnectCodec {
    /// Returns the codec name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Binary => "binary",
            Self::Other(name) => name,
        }
    }

    /// Reads a supported codec from its name.
    #[must_use]
    pub const fn from_name(name: &str) -> Option<Self> {
        if str_eq(name, "json") {
            Some(Self::Json)
        } else if str_eq(name, "binary") {
            Some(Self::Binary)
        } else {
            None
        }
    }
}

impl std::fmt::Display for ConnectCodec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

const fn str_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// The outcome of one End frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectStatus {
    code: Option<ErrorCode>,
}

impl ConnectStatus {
    /// Creates a successful end status.
    #[must_use]
    pub const fn success() -> Self {
        Self { code: None }
    }

    /// Creates an error end status.
    #[must_use]
    pub const fn error(code: ErrorCode) -> Self {
        Self { code: Some(code) }
    }

    /// Returns true for a successful end status.
    #[must_use]
    pub const fn is_success(self) -> bool {
        self.code.is_none()
    }

    /// Returns the error code of an error end status.
    #[must_use]
    pub const fn code(self) -> Option<ErrorCode> {
        self.code
    }
}

/// The body of one Connect envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectBody {
    /// A call from the client to the server.
    Call {
        /// The method path.
        method: String,
        /// The opaque request bytes.
        payload: Vec<u8>,
    },
    /// One server-stream message.
    Message {
        /// The opaque message bytes.
        payload: Vec<u8>,
    },
    /// The end of a server stream.
    End {
        /// The end status.
        status: ConnectStatus,
    },
}

impl ConnectBody {
    /// Returns the body wire tag.
    #[must_use]
    pub const fn wire_tag(&self) -> u8 {
        match self {
            Self::Call { .. } => 1,
            Self::Message { .. } => 2,
            Self::End { .. } => 3,
        }
    }
}

/// One Connect envelope.
///
/// The sequence orders the frames of one direction. The stream identifier
/// groups the response frames of one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectEnvelope {
    sequence: u64,
    stream_id: u64,
    body: ConnectBody,
}

impl ConnectEnvelope {
    /// Creates a call envelope.
    #[must_use]
    pub fn call(
        sequence: u64,
        stream_id: u64,
        method: impl Into<String>,
        payload: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            sequence,
            stream_id,
            body: ConnectBody::Call {
                method: method.into(),
                payload: payload.into(),
            },
        }
    }

    /// Creates a stream message envelope.
    #[must_use]
    pub fn message(sequence: u64, stream_id: u64, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            sequence,
            stream_id,
            body: ConnectBody::Message {
                payload: payload.into(),
            },
        }
    }

    /// Creates a successful end envelope.
    #[must_use]
    pub const fn end_success(sequence: u64, stream_id: u64) -> Self {
        Self {
            sequence,
            stream_id,
            body: ConnectBody::End {
                status: ConnectStatus::success(),
            },
        }
    }

    /// Creates an error end envelope.
    #[must_use]
    pub const fn end_error(sequence: u64, stream_id: u64, code: ErrorCode) -> Self {
        Self {
            sequence,
            stream_id,
            body: ConnectBody::End {
                status: ConnectStatus::error(code),
            },
        }
    }

    /// Returns the sequence number.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the stream identifier.
    #[must_use]
    pub const fn stream_id(&self) -> u64 {
        self.stream_id
    }

    /// Returns the body.
    #[must_use]
    pub const fn body(&self) -> &ConnectBody {
        &self.body
    }

    /// Returns the method path of a call.
    #[must_use]
    pub fn method(&self) -> Option<&str> {
        match &self.body {
            ConnectBody::Call { method, .. } => Some(method),
            ConnectBody::Message { .. } | ConnectBody::End { .. } => None,
        }
    }

    /// Returns the opaque payload of a call or a message.
    #[must_use]
    pub fn payload(&self) -> Option<&[u8]> {
        match &self.body {
            ConnectBody::Call { payload, .. } | ConnectBody::Message { payload } => Some(payload),
            ConnectBody::End { .. } => None,
        }
    }

    /// Returns the end status.
    #[must_use]
    pub const fn status(&self) -> Option<ConnectStatus> {
        match &self.body {
            ConnectBody::End { status } => Some(*status),
            ConnectBody::Call { .. } | ConnectBody::Message { .. } => None,
        }
    }

    /// Returns true for a call.
    #[must_use]
    pub const fn is_call(&self) -> bool {
        matches!(self.body, ConnectBody::Call { .. })
    }

    /// Returns true for a stream message.
    #[must_use]
    pub const fn is_message(&self) -> bool {
        matches!(self.body, ConnectBody::Message { .. })
    }

    /// Returns true for an end frame.
    #[must_use]
    pub const fn is_end(&self) -> bool {
        matches!(self.body, ConnectBody::End { .. })
    }
}

/// An error from Connect frame encoding or decoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The frame ends early.
    #[error("The frame is truncated.")]
    Truncated,
    /// The frame magic is wrong.
    #[error("The frame magic is wrong.")]
    BadMagic,
    /// The body tag is unknown.
    #[error("The frame body tag is unknown.")]
    UnknownBodyTag,
    /// The end status tag is unknown.
    #[error("The frame end status tag is unknown.")]
    UnknownStatusTag,
    /// The frame has trailing bytes.
    #[error("The frame has trailing bytes.")]
    TrailingBytes,
    /// A string is not valid UTF-8.
    #[error("The frame string is not valid UTF-8.")]
    InvalidUtf8,
    /// The error code is not in the local registry.
    #[error("The frame names an unknown error code.")]
    UnknownErrorCode,
}

impl FrameError {
    /// Returns the transport failure for this frame error.
    #[must_use]
    pub const fn transport_failure() -> TransportFailure {
        TransportFailure::CorruptFrame
    }
}

/// Encodes one Connect envelope to its canonical frame bytes.
#[must_use]
pub fn encode_frame(envelope: &ConnectEnvelope) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(FRAME_MAGIC);
    bytes.extend_from_slice(&envelope.sequence.to_be_bytes());
    bytes.extend_from_slice(&envelope.stream_id.to_be_bytes());
    bytes.push(envelope.body.wire_tag());
    match &envelope.body {
        ConnectBody::Call { method, payload } => {
            push_string(&mut bytes, method);
            push_bytes(&mut bytes, payload);
        }
        ConnectBody::Message { payload } => push_bytes(&mut bytes, payload),
        ConnectBody::End { status } => match status.code {
            None => bytes.push(0),
            Some(code) => {
                bytes.push(1);
                push_string(&mut bytes, code.as_str());
            }
        },
    }
    bytes
}

/// Decodes one Connect envelope from its canonical frame bytes.
///
/// # Errors
///
/// Returns [`FrameError`] when the magic, a tag, a length, or a string is
/// invalid.
pub fn decode_frame(bytes: &[u8]) -> Result<ConnectEnvelope, FrameError> {
    let mut reader = Reader::new(bytes);
    if reader.read_exact(FRAME_MAGIC.len())? != FRAME_MAGIC {
        return Err(FrameError::BadMagic);
    }
    let sequence = reader.read_u64()?;
    let stream_id = reader.read_u64()?;
    let body = match reader.read_u8()? {
        1 => ConnectBody::Call {
            method: reader.read_string()?,
            payload: reader.read_bytes()?.to_vec(),
        },
        2 => ConnectBody::Message {
            payload: reader.read_bytes()?.to_vec(),
        },
        3 => {
            let status = match reader.read_u8()? {
                0 => ConnectStatus::success(),
                1 => {
                    let code = ErrorCode::parse(&reader.read_string()?)
                        .ok_or(FrameError::UnknownErrorCode)?;
                    ConnectStatus::error(code)
                }
                _ => return Err(FrameError::UnknownStatusTag),
            };
            ConnectBody::End { status }
        }
        _ => return Err(FrameError::UnknownBodyTag),
    };
    if !reader.is_finished() {
        return Err(FrameError::TrailingBytes);
    }
    Ok(ConnectEnvelope {
        sequence,
        stream_id,
        body,
    })
}

fn push_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    let length = u32::try_from(value.len()).unwrap_or(u32::MAX);
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value);
}

fn push_string(bytes: &mut Vec<u8>, value: &str) {
    push_bytes(bytes, value.as_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_exact(&mut self, length: usize) -> Result<&'a [u8], FrameError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(FrameError::Truncated)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(FrameError::Truncated)?;
        self.offset = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, FrameError> {
        let slice = self.read_exact(1)?;
        Ok(slice[0])
    }

    fn read_u64(&mut self) -> Result<u64, FrameError> {
        let slice = self.read_exact(8)?;
        let mut value = [0_u8; 8];
        value.copy_from_slice(slice);
        Ok(u64::from_be_bytes(value))
    }

    fn read_length(&mut self) -> Result<usize, FrameError> {
        let slice = self.read_exact(4)?;
        let mut value = [0_u8; 4];
        value.copy_from_slice(slice);
        usize::try_from(u32::from_be_bytes(value)).map_err(|_| FrameError::Truncated)
    }

    fn read_bytes(&mut self) -> Result<&'a [u8], FrameError> {
        let length = self.read_length()?;
        self.read_exact(length)
    }

    fn read_string(&mut self) -> Result<String, FrameError> {
        let bytes = self.read_bytes()?;
        core::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| FrameError::InvalidUtf8)
    }

    const fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(name: &'static str) -> ErrorCode {
        ErrorCode::parse(name).expect("the code is registered")
    }

    #[test]
    fn every_body_round_trips_through_the_frame() {
        let frames = [
            ConnectEnvelope::call(1, 2, "data/get", vec![0, 1, 0xff]),
            ConnectEnvelope::message(2, 2, vec![7, 8, 9]),
            ConnectEnvelope::end_success(3, 2),
            ConnectEnvelope::end_error(4, 2, code("rpc.unknown_method")),
        ];
        for frame in frames {
            assert_eq!(decode_frame(&encode_frame(&frame)), Ok(frame));
        }
    }

    #[test]
    fn a_bad_magic_fails() {
        let mut bytes = encode_frame(&ConnectEnvelope::end_success(0, 0));
        bytes[0] = b'X';
        assert_eq!(decode_frame(&bytes), Err(FrameError::BadMagic));
    }

    #[test]
    fn a_truncated_frame_fails() {
        let bytes = encode_frame(&ConnectEnvelope::call(0, 0, "data/get", Vec::new()));
        for end in 0..bytes.len() {
            assert!(decode_frame(&bytes[..end]).is_err());
        }
    }

    #[test]
    fn trailing_bytes_fail() {
        let mut bytes = encode_frame(&ConnectEnvelope::end_success(0, 0));
        bytes.push(0);
        assert_eq!(decode_frame(&bytes), Err(FrameError::TrailingBytes));
    }

    #[test]
    fn every_codec_name_is_stable() {
        assert_eq!(ConnectCodec::Json.name(), "json");
        assert_eq!(ConnectCodec::Binary.name(), "binary");
        assert_eq!(ConnectCodec::Other("cbor").name(), "cbor");
        assert_eq!(ConnectCodec::from_name("json"), Some(ConnectCodec::Json));
        assert_eq!(
            ConnectCodec::from_name("binary"),
            Some(ConnectCodec::Binary)
        );
        assert_eq!(ConnectCodec::from_name("cbor"), None);
    }

    #[test]
    fn every_status_is_stable() {
        assert!(ConnectStatus::success().is_success());
        assert_eq!(ConnectStatus::success().code(), None);
        let error = ConnectStatus::error(code("sync.negotiation_required"));
        assert!(!error.is_success());
        assert_eq!(
            error.code().map(ErrorCode::as_str),
            Some("sync.negotiation_required")
        );
    }
}
