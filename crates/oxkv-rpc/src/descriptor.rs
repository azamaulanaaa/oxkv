//! The neutral `descriptor/v1` RPC descriptor.
//!
//! The descriptor is a pure function of the manifest. It lists every method,
//! message type, field, field cardinality, wire type, error code, auth class,
//! and streaming shape.
//!
//! No Rust type layout, serde default, or enum discriminant is contractual.
//! [`Descriptor::canonical_bytes`] and [`Descriptor::canonical_text`] are the
//! published forms. [`Descriptor::digest`] is the `BLAKE3` digest of the
//! canonical bytes.

use std::fmt::Write as _;

use oxkv_contracts::version::{DescriptorDigest, DescriptorVersion, Hash32, ManifestDigest};
use oxkv_contracts::{ErrorCode, ErrorSpec, InternalDiagnostic};

/// The descriptor contract version.
pub const DESCRIPTOR_VERSION: DescriptorVersion = DescriptorVersion::new(1);

/// The namespace of one generated method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Namespace {
    /// A data-plane method.
    Data,
    /// A sync-plane method.
    Sync,
    /// A control-plane method.
    Control,
}

impl Namespace {
    /// Returns the namespace name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Sync => "sync",
            Self::Control => "control",
        }
    }

    /// Returns the stable wire tag.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        match self {
            Self::Data => 1,
            Self::Sync => 2,
            Self::Control => 3,
        }
    }

    /// Reads a namespace from its stable wire tag.
    #[must_use]
    pub const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Data),
            2 => Some(Self::Sync),
            3 => Some(Self::Control),
            _ => None,
        }
    }
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The authentication class of one method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AuthClass {
    /// The method needs no credential.
    Anonymous,
    /// The method needs an authenticated principal.
    Authenticated,
    /// The method needs an operator principal.
    Operator,
}

impl AuthClass {
    /// Returns the auth class name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::Authenticated => "authenticated",
            Self::Operator => "operator",
        }
    }

    /// Returns the stable wire tag.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        match self {
            Self::Anonymous => 1,
            Self::Authenticated => 2,
            Self::Operator => 3,
        }
    }

    /// Reads an auth class from its stable wire tag.
    #[must_use]
    pub const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Anonymous),
            2 => Some(Self::Authenticated),
            3 => Some(Self::Operator),
            _ => None,
        }
    }
}

impl std::fmt::Display for AuthClass {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The streaming shape of one method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StreamingShape {
    /// One request and one response.
    Unary,
    /// One request and a server stream.
    ServerStream,
}

impl StreamingShape {
    /// Returns the streaming shape name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unary => "unary",
            Self::ServerStream => "server_stream",
        }
    }

    /// Returns the stable wire tag.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        match self {
            Self::Unary => 1,
            Self::ServerStream => 2,
        }
    }

    /// Reads a streaming shape from its stable wire tag.
    #[must_use]
    pub const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Unary),
            2 => Some(Self::ServerStream),
            _ => None,
        }
    }
}

impl std::fmt::Display for StreamingShape {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The cardinality of one message field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FieldCardinality {
    /// The field must be present.
    Required,
    /// The field may be absent.
    Optional,
    /// The field may appear many times.
    Repeated,
}

impl FieldCardinality {
    /// Returns the cardinality name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
            Self::Repeated => "repeated",
        }
    }

    /// Returns the stable wire tag.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        match self {
            Self::Required => 1,
            Self::Optional => 2,
            Self::Repeated => 3,
        }
    }

    /// Reads a cardinality from its stable wire tag.
    #[must_use]
    pub const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Required),
            2 => Some(Self::Optional),
            3 => Some(Self::Repeated),
            _ => None,
        }
    }
}

impl std::fmt::Display for FieldCardinality {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The wire type of one message field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WireType {
    /// A boolean value.
    Bool,
    /// An unsigned 32-bit integer.
    Uint32,
    /// An unsigned 64-bit integer.
    Uint64,
    /// A signed 64-bit integer.
    Int64,
    /// An opaque byte string.
    Bytes,
    /// A UTF-8 string.
    String,
    /// A declared message type.
    Message,
}

impl WireType {
    /// Returns the wire type name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Uint32 => "uint32",
            Self::Uint64 => "uint64",
            Self::Int64 => "int64",
            Self::Bytes => "bytes",
            Self::String => "string",
            Self::Message => "message",
        }
    }

    /// Returns the stable wire tag.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        match self {
            Self::Bool => 1,
            Self::Uint32 => 2,
            Self::Uint64 => 3,
            Self::Int64 => 4,
            Self::Bytes => 5,
            Self::String => 6,
            Self::Message => 7,
        }
    }

    /// Reads a wire type from its stable wire tag.
    #[must_use]
    pub const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Bool),
            2 => Some(Self::Uint32),
            3 => Some(Self::Uint64),
            4 => Some(Self::Int64),
            5 => Some(Self::Bytes),
            6 => Some(Self::String),
            7 => Some(Self::Message),
            _ => None,
        }
    }
}

impl std::fmt::Display for WireType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// An error from descriptor validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DescriptorError {
    /// The descriptor has no methods.
    #[error("The descriptor has no methods.")]
    NoMethods,
    /// The descriptor has no messages.
    #[error("The descriptor has no messages.")]
    NoMessages,
    /// A method path is empty.
    #[error("The method path is empty.")]
    EmptyMethodPath,
    /// A message name is empty.
    #[error("The message name is empty.")]
    EmptyMessageName,
    /// A field name is empty.
    #[error("The field name is empty in the message {message}.")]
    EmptyFieldName {
        /// The message name.
        message: String,
    },
    /// A method path does not match its namespace.
    #[error("The method path {path} does not use the {namespace} namespace.")]
    NamespacePrefix {
        /// The method path.
        path: String,
        /// The expected namespace.
        namespace: Namespace,
    },
    /// A method path appears more than one time.
    #[error("The method path appears more than one time: {path}.")]
    DuplicateMethod {
        /// The duplicate method path.
        path: String,
    },
    /// A message name appears more than one time.
    #[error("The message name appears more than one time: {name}.")]
    DuplicateMessage {
        /// The duplicate message name.
        name: String,
    },
    /// A field name appears more than one time inside one message.
    #[error("The field {field} appears more than one time in the message {message}.")]
    DuplicateFieldName {
        /// The message name.
        message: String,
        /// The field name.
        field: String,
    },
    /// A field number appears more than one time inside one message.
    #[error("The field number {number} appears more than one time in the message {message}.")]
    DuplicateFieldNumber {
        /// The message name.
        message: String,
        /// The field number.
        number: u32,
    },
    /// A field number is zero.
    #[error("The field {field} in the message {message} uses the reserved number zero.")]
    ZeroFieldNumber {
        /// The message name.
        message: String,
        /// The field name.
        field: String,
    },
    /// A field declares the message wire type without a type name.
    #[error("The field {field} in the message {message} has no message type name.")]
    MissingMessageType {
        /// The message name.
        message: String,
        /// The field name.
        field: String,
    },
    /// A scalar field declares a message type name.
    #[error("The field {field} in the message {message} has a message type name for a scalar.")]
    UnexpectedMessageType {
        /// The message name.
        message: String,
        /// The field name.
        field: String,
    },
    /// A method or field names an unknown message.
    #[error("The method or field {owner} names the unknown message {message}.")]
    UnknownMessage {
        /// The method path or field owner.
        owner: String,
        /// The unknown message name.
        message: String,
    },
    /// An error code is not in the canonical registry.
    #[error("The error code is not registered: {code}.")]
    UnknownErrorCode {
        /// The unknown error code.
        code: String,
    },
}

impl DescriptorError {
    /// Returns the canonical error code for this validation failure.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoMethods
            | Self::NoMessages
            | Self::EmptyMethodPath
            | Self::EmptyMessageName
            | Self::EmptyFieldName { .. }
            | Self::DuplicateMethod { .. }
            | Self::DuplicateMessage { .. }
            | Self::DuplicateFieldName { .. }
            | Self::DuplicateFieldNumber { .. } => "manifest.duplicate_name",
            Self::NamespacePrefix { .. }
            | Self::UnknownMessage { .. }
            | Self::UnknownErrorCode { .. } => "manifest.operation_unknown_field",
            Self::ZeroFieldNumber { .. }
            | Self::MissingMessageType { .. }
            | Self::UnexpectedMessageType { .. } => "manifest.unknown_field",
        }
    }

    /// Returns the operator diagnostic for the failed descriptor check.
    ///
    /// The diagnostic is for operators and logs. It is not a wire value. The
    /// public code stays one of the `manifest.*` codes.
    #[must_use]
    pub const fn diagnostic(&self) -> InternalDiagnostic {
        match self {
            Self::NoMethods
            | Self::NoMessages
            | Self::EmptyMethodPath
            | Self::EmptyMessageName
            | Self::EmptyFieldName { .. }
            | Self::NamespacePrefix { .. }
            | Self::DuplicateMethod { .. }
            | Self::DuplicateMessage { .. }
            | Self::DuplicateFieldName { .. }
            | Self::DuplicateFieldNumber { .. }
            | Self::ZeroFieldNumber { .. }
            | Self::MissingMessageType { .. }
            | Self::UnexpectedMessageType { .. }
            | Self::UnknownMessage { .. }
            | Self::UnknownErrorCode { .. } => InternalDiagnostic::RpcDescriptor,
        }
    }
}

/// One field of one message type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldDescriptor {
    number: u32,
    name: String,
    cardinality: FieldCardinality,
    wire_type: WireType,
    message_type: Option<String>,
}

impl FieldDescriptor {
    pub(crate) fn new(
        number: u32,
        name: impl Into<String>,
        cardinality: FieldCardinality,
        wire_type: WireType,
        message_type: Option<String>,
    ) -> Self {
        Self {
            number,
            name: name.into(),
            cardinality,
            wire_type,
            message_type,
        }
    }

    /// Returns the field number.
    #[must_use]
    pub const fn number(&self) -> u32 {
        self.number
    }

    /// Returns the field name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the field cardinality.
    #[must_use]
    pub const fn cardinality(&self) -> FieldCardinality {
        self.cardinality
    }

    /// Returns the field wire type.
    #[must_use]
    pub const fn wire_type(&self) -> WireType {
        self.wire_type
    }

    /// Returns the message type name of a message field.
    #[must_use]
    pub fn message_type(&self) -> Option<&str> {
        self.message_type.as_deref()
    }
}

/// One message type of the descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageDescriptor {
    name: String,
    fields: Vec<FieldDescriptor>,
}

impl MessageDescriptor {
    pub(crate) fn new(name: impl Into<String>, mut fields: Vec<FieldDescriptor>) -> Self {
        fields.sort_by_key(FieldDescriptor::number);
        Self {
            name: name.into(),
            fields,
        }
    }

    /// Returns the message name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the fields in field-number order.
    #[must_use]
    pub fn fields(&self) -> &[FieldDescriptor] {
        &self.fields
    }

    /// Returns one field by name.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&FieldDescriptor> {
        self.fields.iter().find(|field| field.name == name)
    }
}

/// One method of the descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodDescriptor {
    path: String,
    namespace: Namespace,
    request: String,
    response: String,
    streaming: StreamingShape,
    auth: AuthClass,
    errors: Vec<ErrorCode>,
}

impl MethodDescriptor {
    pub(crate) fn new(
        path: impl Into<String>,
        namespace: Namespace,
        request: impl Into<String>,
        response: impl Into<String>,
        streaming: StreamingShape,
        auth: AuthClass,
        mut errors: Vec<ErrorCode>,
    ) -> Self {
        errors.sort_by_key(|code| code.as_str());
        Self {
            path: path.into(),
            namespace,
            request: request.into(),
            response: response.into(),
            streaming,
            auth,
            errors,
        }
    }

    /// Returns the method path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the method namespace.
    #[must_use]
    pub const fn namespace(&self) -> Namespace {
        self.namespace
    }

    /// Returns the request message name.
    #[must_use]
    pub fn request(&self) -> &str {
        &self.request
    }

    /// Returns the response message name.
    #[must_use]
    pub fn response(&self) -> &str {
        &self.response
    }

    /// Returns the streaming shape.
    #[must_use]
    pub const fn streaming(&self) -> StreamingShape {
        self.streaming
    }

    /// Returns the auth class.
    #[must_use]
    pub const fn auth(&self) -> AuthClass {
        self.auth
    }

    /// Returns the declared error codes in code order.
    #[must_use]
    pub fn errors(&self) -> &[ErrorCode] {
        &self.errors
    }

    /// Returns the declared error registry rows in code order.
    pub fn error_specs(&self) -> impl Iterator<Item = &'static ErrorSpec> + '_ {
        self.errors.iter().map(|code| code.spec())
    }
}

/// The published identity of one descriptor.
///
/// The sync wire carries this identity in `SessionHello`. The RPC layer
/// compares it with its own identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DescriptorIdentity {
    descriptor_version: DescriptorVersion,
    descriptor_digest: DescriptorDigest,
    manifest_digest: ManifestDigest,
}

impl DescriptorIdentity {
    /// Creates a descriptor identity.
    #[must_use]
    pub const fn new(
        descriptor_version: DescriptorVersion,
        descriptor_digest: DescriptorDigest,
        manifest_digest: ManifestDigest,
    ) -> Self {
        Self {
            descriptor_version,
            descriptor_digest,
            manifest_digest,
        }
    }

    /// Returns the descriptor contract version.
    #[must_use]
    pub const fn descriptor_version(self) -> DescriptorVersion {
        self.descriptor_version
    }

    /// Returns the descriptor digest.
    #[must_use]
    pub const fn descriptor_digest(self) -> DescriptorDigest {
        self.descriptor_digest
    }

    /// Returns the manifest digest.
    #[must_use]
    pub const fn manifest_digest(self) -> ManifestDigest {
        self.manifest_digest
    }
}

/// The neutral `descriptor/v1` surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    manifest_digest: ManifestDigest,
    methods: Vec<MethodDescriptor>,
    messages: Vec<MessageDescriptor>,
}

impl Descriptor {
    /// Creates a descriptor and validates it.
    ///
    /// The constructor sorts methods by path, messages by name, fields by
    /// number, and error codes by code.
    ///
    /// # Errors
    ///
    /// Returns [`DescriptorError`] when the descriptor has a duplicate name, a
    /// missing type, an invalid path, or an unknown error code.
    pub fn new(
        manifest_digest: ManifestDigest,
        mut methods: Vec<MethodDescriptor>,
        mut messages: Vec<MessageDescriptor>,
    ) -> Result<Self, DescriptorError> {
        methods.sort_by(|left, right| left.path.cmp(&right.path));
        messages.sort_by(|left, right| left.name.cmp(&right.name));
        let descriptor = Self {
            manifest_digest,
            methods,
            messages,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    /// Returns the descriptor contract version.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn descriptor_version(&self) -> DescriptorVersion {
        DESCRIPTOR_VERSION
    }

    /// Returns the manifest digest that produced this descriptor.
    #[must_use]
    pub const fn manifest_digest(&self) -> ManifestDigest {
        self.manifest_digest
    }

    /// Returns the methods in path order.
    #[must_use]
    pub fn methods(&self) -> &[MethodDescriptor] {
        &self.methods
    }

    /// Returns the messages in name order.
    #[must_use]
    pub fn messages(&self) -> &[MessageDescriptor] {
        &self.messages
    }

    /// Looks up one method by path.
    #[must_use]
    pub fn method(&self, path: &str) -> Option<&MethodDescriptor> {
        self.methods.iter().find(|method| method.path == path)
    }

    /// Looks up one message by name.
    #[must_use]
    pub fn message(&self, name: &str) -> Option<&MessageDescriptor> {
        self.messages
            .iter()
            .find(|message| message.name.as_str() == name)
    }

    /// Returns the `BLAKE3` digest of the canonical descriptor bytes.
    #[must_use]
    pub fn digest(&self) -> DescriptorDigest {
        DescriptorDigest::from_bytes(*Hash32::of(&self.canonical_bytes()).as_bytes())
    }

    /// Returns the canonical descriptor bytes.
    ///
    /// The form uses fixed-width big-endian integers and length-prefixed
    /// strings. It has no floating point value and no Rust type layout.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"OXKVRPC\0");
        bytes.extend_from_slice(&self.descriptor_version().revision().to_be_bytes());
        bytes.extend_from_slice(self.manifest_digest.as_bytes());
        push_count(&mut bytes, self.methods.len());
        for method in &self.methods {
            push_string(&mut bytes, &method.path);
            bytes.push(method.namespace.wire_tag());
            push_string(&mut bytes, &method.request);
            push_string(&mut bytes, &method.response);
            bytes.push(method.streaming.wire_tag());
            bytes.push(method.auth.wire_tag());
            push_count(&mut bytes, method.errors.len());
            for code in &method.errors {
                push_string(&mut bytes, code.as_str());
            }
        }
        push_count(&mut bytes, self.messages.len());
        for message in &self.messages {
            push_string(&mut bytes, &message.name);
            push_count(&mut bytes, message.fields.len());
            for field in &message.fields {
                bytes.extend_from_slice(&field.number.to_be_bytes());
                push_string(&mut bytes, &field.name);
                bytes.push(field.cardinality.wire_tag());
                bytes.push(field.wire_type.wire_tag());
                if let Some(message_type) = &field.message_type {
                    push_string(&mut bytes, message_type);
                }
            }
        }
        bytes
    }

    /// Returns the canonical descriptor text.
    ///
    /// The text form is deterministic. It has one item per line. A quoted
    /// string escapes a backslash, a quote, and a non-printable byte.
    #[must_use]
    pub fn canonical_text(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(text, "oxkv-descriptor/v1");
        let _ = writeln!(text, "descriptor_version {}", self.descriptor_version());
        let _ = writeln!(text, "manifest_digest {}", self.manifest_digest);
        let _ = writeln!(text, "methods {}", self.methods.len());
        for method in &self.methods {
            push_quoted(&mut text, &method.path);
            text.push(' ');
            text.push_str(method.namespace.name());
            text.push(' ');
            push_quoted(&mut text, &method.request);
            text.push(' ');
            push_quoted(&mut text, &method.response);
            text.push(' ');
            text.push_str(method.streaming.name());
            text.push(' ');
            text.push_str(method.auth.name());
            text.push(' ');
            if method.errors.is_empty() {
                text.push('-');
            } else {
                for (index, code) in method.errors.iter().enumerate() {
                    if index > 0 {
                        text.push(',');
                    }
                    text.push_str(code.as_str());
                }
            }
            text.push('\n');
        }
        let _ = writeln!(text, "messages {}", self.messages.len());
        for message in &self.messages {
            push_quoted(&mut text, &message.name);
            text.push(' ');
            text.push_str(&message.fields.len().to_string());
            text.push('\n');
            for field in &message.fields {
                text.push_str(&field.number.to_string());
                text.push(' ');
                push_quoted(&mut text, &field.name);
                text.push(' ');
                text.push_str(field.cardinality.name());
                text.push(' ');
                text.push_str(field.wire_type.name());
                text.push(' ');
                match &field.message_type {
                    Some(message_type) => push_quoted(&mut text, message_type),
                    None => text.push('-'),
                }
                text.push('\n');
            }
        }
        let _ = writeln!(text, "end");
        text
    }

    fn validate(&self) -> Result<(), DescriptorError> {
        if self.methods.is_empty() {
            return Err(DescriptorError::NoMethods);
        }
        if self.messages.is_empty() {
            return Err(DescriptorError::NoMessages);
        }
        validate_methods(&self.methods)?;
        validate_messages(&self.messages)?;
        validate_references(&self.methods, &self.messages)
    }
}

fn validate_methods(methods: &[MethodDescriptor]) -> Result<(), DescriptorError> {
    for (index, method) in methods.iter().enumerate() {
        if method.path.is_empty() {
            return Err(DescriptorError::EmptyMethodPath);
        }
        let prefix = method.namespace.name();
        if !method.path.starts_with(prefix) || !method.path[prefix.len()..].starts_with('/') {
            return Err(DescriptorError::NamespacePrefix {
                path: method.path.clone(),
                namespace: method.namespace,
            });
        }
        if methods[index + 1..]
            .iter()
            .any(|other| other.path == method.path)
        {
            return Err(DescriptorError::DuplicateMethod {
                path: method.path.clone(),
            });
        }
    }
    Ok(())
}

fn validate_messages(messages: &[MessageDescriptor]) -> Result<(), DescriptorError> {
    for (index, message) in messages.iter().enumerate() {
        if message.name.is_empty() {
            return Err(DescriptorError::EmptyMessageName);
        }
        if messages[index + 1..]
            .iter()
            .any(|other| other.name == message.name)
        {
            return Err(DescriptorError::DuplicateMessage {
                name: message.name.clone(),
            });
        }
        validate_fields(message)?;
    }
    Ok(())
}

fn validate_fields(message: &MessageDescriptor) -> Result<(), DescriptorError> {
    for (index, field) in message.fields.iter().enumerate() {
        if field.name.is_empty() {
            return Err(DescriptorError::EmptyFieldName {
                message: message.name.clone(),
            });
        }
        if field.number == 0 {
            return Err(DescriptorError::ZeroFieldNumber {
                message: message.name.clone(),
                field: field.name.clone(),
            });
        }
        if message.fields[index + 1..]
            .iter()
            .any(|other| other.name == field.name)
        {
            return Err(DescriptorError::DuplicateFieldName {
                message: message.name.clone(),
                field: field.name.clone(),
            });
        }
        if message.fields[index + 1..]
            .iter()
            .any(|other| other.number == field.number)
        {
            return Err(DescriptorError::DuplicateFieldNumber {
                message: message.name.clone(),
                number: field.number,
            });
        }
        match (field.wire_type, &field.message_type) {
            (WireType::Message, None) => {
                return Err(DescriptorError::MissingMessageType {
                    message: message.name.clone(),
                    field: field.name.clone(),
                });
            }
            (WireType::Message, Some(type_name)) if type_name.is_empty() => {
                return Err(DescriptorError::MissingMessageType {
                    message: message.name.clone(),
                    field: field.name.clone(),
                });
            }
            (WireType::Message, Some(_)) | (_, None) => {}
            (_, Some(_)) => {
                return Err(DescriptorError::UnexpectedMessageType {
                    message: message.name.clone(),
                    field: field.name.clone(),
                });
            }
        }
    }
    Ok(())
}

fn validate_references(
    methods: &[MethodDescriptor],
    messages: &[MessageDescriptor],
) -> Result<(), DescriptorError> {
    for method in methods {
        for name in [&method.request, &method.response] {
            if !messages.iter().any(|message| message.name == *name) {
                return Err(DescriptorError::UnknownMessage {
                    owner: method.path.clone(),
                    message: name.clone(),
                });
            }
        }
    }
    for message in messages {
        for field in &message.fields {
            if let Some(type_name) = &field.message_type
                && !messages.iter().any(|other| other.name == *type_name)
            {
                return Err(DescriptorError::UnknownMessage {
                    owner: format!("{}.{}", message.name, field.name),
                    message: type_name.clone(),
                });
            }
        }
    }
    Ok(())
}

fn push_count(bytes: &mut Vec<u8>, count: usize) {
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    bytes.extend_from_slice(&count.to_be_bytes());
}

fn push_string(bytes: &mut Vec<u8>, value: &str) {
    let length = u32::try_from(value.len()).unwrap_or(u32::MAX);
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn push_quoted(text: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    text.push('"');
    for &byte in value.as_bytes() {
        match byte {
            b'"' => text.push_str("\\\""),
            b'\\' => text.push_str("\\\\"),
            b'\n' => text.push_str("\\n"),
            b'\r' => text.push_str("\\r"),
            b'\t' => text.push_str("\\t"),
            0x20..=0x7e => text.push(char::from(byte)),
            _ => {
                text.push_str("\\x");
                text.push(char::from(HEX[usize::from(byte >> 4)]));
                text.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
        }
    }
    text.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(number: u32, name: &str, wire_type: WireType) -> FieldDescriptor {
        FieldDescriptor::new(number, name, FieldCardinality::Required, wire_type, None)
    }

    fn sample_descriptor() -> Descriptor {
        let messages = vec![
            MessageDescriptor::new(
                "GetRequest",
                vec![
                    field(2, "key", WireType::Bytes),
                    field(1, "table", WireType::String),
                ],
            ),
            MessageDescriptor::new("GetResponse", vec![field(1, "value", WireType::Bytes)]),
        ];
        let methods = vec![
            MethodDescriptor::new(
                "data/get",
                Namespace::Data,
                "GetRequest",
                "GetResponse",
                StreamingShape::Unary,
                AuthClass::Authenticated,
                vec![ErrorCode::parse("key.not_found").expect("the code is registered")],
            ),
            MethodDescriptor::new(
                "data/put",
                Namespace::Data,
                "GetRequest",
                "GetResponse",
                StreamingShape::Unary,
                AuthClass::Authenticated,
                Vec::new(),
            ),
        ];
        Descriptor::new(ManifestDigest::from_bytes([7; 32]), methods, messages)
            .expect("the sample descriptor is valid")
    }

    #[test]
    fn the_canonical_bytes_are_stable() {
        let first = sample_descriptor();
        let second = sample_descriptor();
        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
        assert_eq!(first.canonical_text(), second.canonical_text());
        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.digest(), first.digest());
    }

    #[test]
    fn the_canonical_bytes_start_with_the_magic_and_the_version() {
        let bytes = sample_descriptor().canonical_bytes();
        assert_eq!(&bytes[..8], b"OXKVRPC\0");
        assert_eq!(&bytes[8..10], &1_u16.to_be_bytes());
    }

    #[test]
    fn the_canonical_text_quotes_and_escapes() {
        let field = FieldDescriptor::new(
            1,
            "a\"b\\c\n",
            FieldCardinality::Optional,
            WireType::String,
            None,
        );
        let message = MessageDescriptor::new("M\"x", vec![field]);
        let descriptor = Descriptor::new(
            ManifestDigest::from_bytes([0; 32]),
            vec![MethodDescriptor::new(
                "data/get",
                Namespace::Data,
                "M\"x",
                "M\"x",
                StreamingShape::Unary,
                AuthClass::Anonymous,
                Vec::new(),
            )],
            vec![message],
        )
        .expect("the descriptor is valid");
        let text = descriptor.canonical_text();
        assert!(text.contains("\"data/get\""));
        assert!(text.contains("\"a\\\"b\\\\c\\n\""));
        assert!(text.contains("\"M\\\"x\""));
    }

    #[test]
    fn a_method_path_must_match_its_namespace() {
        let messages = vec![MessageDescriptor::new(
            "M",
            vec![field(1, "v", WireType::Bytes)],
        )];
        let result = Descriptor::new(
            ManifestDigest::from_bytes([0; 32]),
            vec![MethodDescriptor::new(
                "control/backup",
                Namespace::Data,
                "M",
                "M",
                StreamingShape::Unary,
                AuthClass::Operator,
                Vec::new(),
            )],
            messages,
        );
        assert_eq!(
            result,
            Err(DescriptorError::NamespacePrefix {
                path: "control/backup".to_owned(),
                namespace: Namespace::Data,
            })
        );
    }

    #[test]
    fn a_duplicate_field_number_is_rejected() {
        let messages = vec![MessageDescriptor::new(
            "M",
            vec![
                field(1, "a", WireType::Bytes),
                field(1, "b", WireType::Bytes),
            ],
        )];
        let result = Descriptor::new(
            ManifestDigest::from_bytes([0; 32]),
            vec![MethodDescriptor::new(
                "data/get",
                Namespace::Data,
                "M",
                "M",
                StreamingShape::Unary,
                AuthClass::Anonymous,
                Vec::new(),
            )],
            messages,
        );
        assert_eq!(
            result,
            Err(DescriptorError::DuplicateFieldNumber {
                message: "M".to_owned(),
                number: 1,
            })
        );
    }

    #[test]
    fn a_message_field_must_name_a_declared_message() {
        let messages = vec![MessageDescriptor::new(
            "M",
            vec![FieldDescriptor::new(
                1,
                "child",
                FieldCardinality::Required,
                WireType::Message,
                Some("Child".to_owned()),
            )],
        )];
        let result = Descriptor::new(
            ManifestDigest::from_bytes([0; 32]),
            vec![MethodDescriptor::new(
                "data/get",
                Namespace::Data,
                "M",
                "M",
                StreamingShape::Unary,
                AuthClass::Anonymous,
                Vec::new(),
            )],
            messages,
        );
        assert_eq!(
            result,
            Err(DescriptorError::UnknownMessage {
                owner: "M.child".to_owned(),
                message: "Child".to_owned(),
            })
        );
    }

    #[test]
    fn every_wire_tag_round_trips() {
        for tag in 1..=3 {
            assert!(Namespace::from_wire_tag(tag).is_some());
            assert!(AuthClass::from_wire_tag(tag).is_some());
            assert!(FieldCardinality::from_wire_tag(tag).is_some());
        }
        for tag in 1..=2 {
            assert!(StreamingShape::from_wire_tag(tag).is_some());
        }
        for tag in 1..=7 {
            assert!(WireType::from_wire_tag(tag).is_some());
        }
        assert!(Namespace::from_wire_tag(0).is_none());
        assert!(WireType::from_wire_tag(8).is_none());
    }
}
