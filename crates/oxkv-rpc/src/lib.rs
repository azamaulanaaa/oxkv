//! Generated RPC for oxkv.
//!
//! The manifest compiles to one neutral `descriptor/v1` surface. The surface
//! lists every method, message type, field, field cardinality, wire type,
//! error code, auth class, and streaming shape. Every build produces the same
//! canonical descriptor bytes and the same `BLAKE3` digest.
//!
//! The crate has these layers:
//!
//! - [`descriptor`] defines the neutral surface, its canonical bytes, its
//!   canonical text, and its digest.
//! - [`manifest`] and [`generator`] compile the manifest and generate method
//!   metadata for the data, sync, and control namespaces.
//! - [`connect`] defines the Connect RPC envelope. A stream frame carries
//!   opaque sync bytes.
//! - [`session`] exposes the descriptor identity and the `SessionHello`
//!   compatibility check. The sync wire owns the handshake.
//! - [`protocol`] is the sans-IO core.
//! - [`transport`] has the in-process reference transport.
//!
//! The crate is portable. It uses no network, clock, randomness, file I/O,
//! floating point, or unsafe code. It does not implement the sync state
//! machine.
//!
//! # Example
//!
//! ```
//! use oxkv_rpc::generator::reference_surface;
//!
//! let surface = reference_surface().expect("the reference manifest compiles");
//! assert!(surface.method("data/get").is_some());
//! assert!(surface.method("control/backup").is_some());
//! assert_eq!(surface.control_methods().count(), 8);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod connect;
pub mod control;
pub mod descriptor;
pub mod error;
pub mod generator;
pub mod manifest;
pub mod protocol;
pub mod session;
pub mod transport;

pub use connect::{
    ConnectBody, ConnectCodec, ConnectEnvelope, ConnectStatus, FRAME_MAGIC, FrameError,
    decode_frame, encode_frame,
};
pub use control::{CONTROL_NAMESPACE, CONTROL_OPERATION_PATHS};
pub use descriptor::{
    AuthClass, DESCRIPTOR_VERSION, Descriptor, DescriptorError, DescriptorIdentity,
    FieldCardinality, FieldDescriptor, MessageDescriptor, MethodDescriptor, Namespace,
    StreamingShape, WireType,
};
pub use error::{ProtocolError, TransportError, TransportFailure};
pub use generator::{
    GeneratedMethod, GeneratedSurface, compile, reference_surface, surface_from_manifest,
};
pub use manifest::{
    FieldDecl, ManifestSurface, MessageDecl, OperationDecl, REFERENCE_MANIFEST_DIGEST,
    reference_manifest,
};
pub use protocol::{CallHandler, CallOutcome, ProtocolCore};
pub use session::{NegotiatedSession, Session, SessionConfig, SessionHelloFacts};
pub use transport::{FramedTransport, InProcessTransport, Transport};
