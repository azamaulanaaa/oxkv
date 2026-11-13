//! Descriptor generation and generated method metadata.
//!
//! [`compile`] turns one manifest declaration set into one descriptor.
//! [`generate`] turns one descriptor into the generated method surface. The
//! control methods use the same generator as the data methods.

use oxkv_contracts::version::{DescriptorDigest, ManifestDigest};
use oxkv_contracts::{ErrorCode, ErrorSpec};

use crate::descriptor::{
    AuthClass, Descriptor, DescriptorError, DescriptorIdentity, FieldDescriptor, MessageDescriptor,
    MethodDescriptor, Namespace, StreamingShape,
};
use crate::manifest::{ManifestSurface, reference_manifest};

/// Compiles one manifest declaration set into one descriptor.
///
/// The compiler is pure. The function does not read a clock, environment,
/// network, or file.
///
/// # Errors
///
/// Returns [`DescriptorError`] when the declarations are invalid.
pub fn compile(manifest: &ManifestSurface) -> Result<Descriptor, DescriptorError> {
    let messages = manifest
        .messages()
        .iter()
        .map(|message| {
            let fields = message
                .fields()
                .iter()
                .map(|field| {
                    FieldDescriptor::new(
                        field.number(),
                        field.name(),
                        field.cardinality(),
                        field.wire_type(),
                        field.message_type().map(str::to_owned),
                    )
                })
                .collect();
            MessageDescriptor::new(message.name(), fields)
        })
        .collect();
    let mut methods = Vec::with_capacity(manifest.operations().len());
    for operation in manifest.operations() {
        let mut errors = Vec::with_capacity(operation.errors().len());
        for code in operation.errors() {
            let parsed =
                ErrorCode::parse(code).ok_or_else(|| DescriptorError::UnknownErrorCode {
                    code: (*code).to_owned(),
                })?;
            errors.push(parsed);
        }
        methods.push(MethodDescriptor::new(
            operation.path(),
            operation.namespace(),
            operation.request(),
            operation.response(),
            operation.streaming(),
            operation.auth(),
            errors,
        ));
    }
    Descriptor::new(manifest.manifest_digest(), methods, messages)
}

/// One generated method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedMethod {
    method: MethodDescriptor,
}

impl GeneratedMethod {
    fn new(method: MethodDescriptor) -> Self {
        Self { method }
    }

    /// Returns the method path.
    #[must_use]
    pub fn path(&self) -> &str {
        self.method.path()
    }

    /// Returns the method namespace.
    #[must_use]
    pub const fn namespace(&self) -> Namespace {
        self.method.namespace()
    }

    /// Returns the request message name.
    #[must_use]
    pub fn request(&self) -> &str {
        self.method.request()
    }

    /// Returns the response message name.
    #[must_use]
    pub fn response(&self) -> &str {
        self.method.response()
    }

    /// Returns the streaming shape.
    #[must_use]
    pub const fn streaming(&self) -> StreamingShape {
        self.method.streaming()
    }

    /// Returns the auth class.
    #[must_use]
    pub const fn auth(&self) -> AuthClass {
        self.method.auth()
    }

    /// Returns the declared error codes in code order.
    #[must_use]
    pub fn errors(&self) -> &[ErrorCode] {
        self.method.errors()
    }

    /// Returns the declared error registry rows in code order.
    pub fn error_specs(&self) -> impl Iterator<Item = &'static ErrorSpec> + '_ {
        self.method.error_specs()
    }

    /// Returns true when the method is in the control namespace.
    #[must_use]
    pub fn is_control(&self) -> bool {
        self.method.namespace() == Namespace::Control
    }

    /// Returns the underlying descriptor method.
    #[must_use]
    pub const fn descriptor(&self) -> &MethodDescriptor {
        &self.method
    }
}

/// The generated method surface for one descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedSurface {
    descriptor: Descriptor,
    descriptor_digest: DescriptorDigest,
    methods: Vec<GeneratedMethod>,
}

impl GeneratedSurface {
    /// Generates the method surface from a valid descriptor.
    #[must_use]
    pub fn generate(descriptor: Descriptor) -> Self {
        let descriptor_digest = descriptor.digest();
        let methods = descriptor
            .methods()
            .iter()
            .cloned()
            .map(GeneratedMethod::new)
            .collect();
        Self {
            descriptor,
            descriptor_digest,
            methods,
        }
    }

    /// Returns the descriptor.
    #[must_use]
    pub const fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// Returns the descriptor digest.
    #[must_use]
    pub const fn descriptor_digest(&self) -> DescriptorDigest {
        self.descriptor_digest
    }

    /// Returns the manifest digest.
    #[must_use]
    pub const fn manifest_digest(&self) -> ManifestDigest {
        self.descriptor.manifest_digest()
    }

    /// Returns the descriptor identity.
    #[must_use]
    pub fn identity(&self) -> DescriptorIdentity {
        DescriptorIdentity::new(
            self.descriptor.descriptor_version(),
            self.descriptor_digest,
            self.manifest_digest(),
        )
    }

    /// Returns the generated methods in path order.
    #[must_use]
    pub fn methods(&self) -> &[GeneratedMethod] {
        &self.methods
    }

    /// Returns the number of generated methods.
    #[must_use]
    pub fn len(&self) -> usize {
        self.methods.len()
    }

    /// Returns true when the surface has no methods.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.methods.is_empty()
    }

    /// Looks up one generated method by path.
    #[must_use]
    pub fn method(&self, path: &str) -> Option<&GeneratedMethod> {
        self.methods.iter().find(|method| method.path() == path)
    }

    /// Iterates over the control methods.
    pub fn control_methods(&self) -> impl Iterator<Item = &GeneratedMethod> {
        self.methods.iter().filter(|method| method.is_control())
    }
}

/// Compiles one manifest and generates its method surface.
///
/// # Errors
///
/// Returns [`DescriptorError`] when the manifest declarations are invalid.
pub fn surface_from_manifest(
    manifest: &ManifestSurface,
) -> Result<GeneratedSurface, DescriptorError> {
    compile(manifest).map(GeneratedSurface::generate)
}

/// Compiles the reference manifest and generates its method surface.
///
/// # Errors
///
/// Returns [`DescriptorError`] when the fixed reference declarations are
/// invalid.
pub fn reference_surface() -> Result<GeneratedSurface, DescriptorError> {
    surface_from_manifest(&reference_manifest())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{CONTROL_NAMESPACE, CONTROL_OPERATION_PATHS};
    use crate::manifest::reference_manifest;

    #[test]
    fn two_generations_from_one_manifest_are_identical() {
        let manifest = reference_manifest();
        let first = surface_from_manifest(&manifest).expect("the reference manifest compiles");
        let second = surface_from_manifest(&manifest).expect("the reference manifest compiles");
        assert_eq!(
            first.descriptor().canonical_bytes(),
            second.descriptor().canonical_bytes()
        );
        assert_eq!(
            first.descriptor().canonical_text(),
            second.descriptor().canonical_text()
        );
        assert_eq!(first.descriptor_digest(), second.descriptor_digest());
    }

    #[test]
    fn every_control_operation_is_generated_once() {
        let surface = reference_surface().expect("the reference surface compiles");
        assert_eq!(
            surface.control_methods().count(),
            CONTROL_OPERATION_PATHS.len()
        );
        for path in CONTROL_OPERATION_PATHS {
            let count = surface
                .methods()
                .iter()
                .filter(|method| method.path() == path)
                .count();
            assert_eq!(
                count, 1,
                "the control operation {path} is not generated once"
            );
            let method = surface.method(path).expect("the control method exists");
            assert_eq!(method.namespace(), Namespace::Control);
            assert_eq!(method.auth(), AuthClass::Operator);
            assert_eq!(method.descriptor().namespace().name(), CONTROL_NAMESPACE);
        }
    }

    #[test]
    fn the_generated_surface_covers_every_declared_operation() {
        let manifest = reference_manifest();
        let surface = surface_from_manifest(&manifest).expect("the reference manifest compiles");
        assert_eq!(surface.len(), manifest.operations().len());
        for operation in manifest.operations() {
            let method = surface
                .method(operation.path())
                .expect("every operation has one generated method");
            assert_eq!(method.request(), operation.request());
            assert_eq!(method.response(), operation.response());
            assert_eq!(method.streaming(), operation.streaming());
            assert_eq!(method.auth(), operation.auth());
        }
    }

    #[test]
    fn every_declared_error_code_is_in_the_registry() {
        let surface = reference_surface().expect("the reference surface compiles");
        for method in surface.methods() {
            for code in method.errors() {
                assert!(
                    ErrorCode::parse(code.as_str()).is_some(),
                    "the generated code {} is not registered",
                    code.as_str()
                );
            }
        }
    }

    #[test]
    fn the_surface_identity_names_the_descriptor_and_the_manifest() {
        let surface = reference_surface().expect("the reference surface compiles");
        let identity = surface.identity();
        assert_eq!(identity.descriptor_digest(), surface.descriptor_digest());
        assert_eq!(identity.manifest_digest(), surface.manifest_digest());
        assert_eq!(
            identity.descriptor_version(),
            surface.descriptor().descriptor_version()
        );
    }
}
