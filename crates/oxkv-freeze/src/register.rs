//! The contract and annex register.
//!
//! [`REGISTER`] holds one row for every frozen surface. A core contract has no
//! parent. An annex names its parent core contract.
//!
//! Every row names a version, frozen parts, free parts, a specification, a
//! changelog, a vector corpus, and a migration path.

use core::fmt;

use crate::state::FreezeState;

/// The identifier of one core contract or annex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ContractId {
    /// The manifest schema.
    C1,
    /// The snapshot and page format.
    C2,
    /// The sync wire protocol.
    C3,
    /// The policy language.
    C4,
    /// The receipt and error semantics.
    C5,
    /// The key encoding annex.
    A1,
    /// The tokenizer annex.
    A2,
    /// The event envelope annex.
    A3,
    /// The shard map annex.
    A4,
    /// The scope semantics annex.
    A5,
    /// The projection staleness annex.
    A6,
    /// The audit envelope annex.
    A7,
    /// The cost and explain annex.
    A8,
    /// The cursor types annex.
    A9,
    /// The descriptor annex.
    A10,
    /// The read pin and staleness annex.
    A11,
    /// The migration and backup annex.
    A12,
}

impl ContractId {
    /// Every core contract and annex.
    pub const ALL: &'static [Self] = &[
        Self::C1,
        Self::C2,
        Self::C3,
        Self::C4,
        Self::C5,
        Self::A1,
        Self::A2,
        Self::A3,
        Self::A4,
        Self::A5,
        Self::A6,
        Self::A7,
        Self::A8,
        Self::A9,
        Self::A10,
        Self::A11,
        Self::A12,
    ];

    /// The five core contracts.
    pub const CORE: &'static [Self] = &[Self::C1, Self::C2, Self::C3, Self::C4, Self::C5];

    /// The twelve annexes.
    pub const ANNEXES: &'static [Self] = &[
        Self::A1,
        Self::A2,
        Self::A3,
        Self::A4,
        Self::A5,
        Self::A6,
        Self::A7,
        Self::A8,
        Self::A9,
        Self::A10,
        Self::A11,
        Self::A12,
    ];

    /// Returns the register identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::C1 => "C1",
            Self::C2 => "C2",
            Self::C3 => "C3",
            Self::C4 => "C4",
            Self::C5 => "C5",
            Self::A1 => "A1",
            Self::A2 => "A2",
            Self::A3 => "A3",
            Self::A4 => "A4",
            Self::A5 => "A5",
            Self::A6 => "A6",
            Self::A7 => "A7",
            Self::A8 => "A8",
            Self::A9 => "A9",
            Self::A10 => "A10",
            Self::A11 => "A11",
            Self::A12 => "A12",
        }
    }

    /// Returns true when the identifier names a core contract.
    #[must_use]
    pub const fn is_core(self) -> bool {
        matches!(self, Self::C1 | Self::C2 | Self::C3 | Self::C4 | Self::C5)
    }

    /// Returns true when the identifier names an annex.
    #[must_use]
    pub const fn is_annex(self) -> bool {
        !self.is_core()
    }

    /// Parses a register identifier.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|identifier| identifier.as_str() == text)
    }
}

impl fmt::Display for ContractId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A contract version in `name/vN` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContractVersion(&'static str);

impl ContractVersion {
    /// Creates a contract version from static text.
    #[must_use]
    pub const fn new(text: &'static str) -> Self {
        Self(text)
    }

    /// Returns the version text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ContractVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

/// Returns true when the text has the `name/vN` contract version shape.
#[must_use]
pub fn is_valid_version_text(text: &str) -> bool {
    let Some((name, revision)) = text.rsplit_once("/v") else {
        return false;
    };
    !name.is_empty() && revision.parse::<u16>().is_ok()
}

/// One dependency of a frozen surface.
///
/// A frozen surface may depend on canonical bytes, a qualified version name,
/// or a semantic grammar. It must not depend on a Rust type layout, a serde
/// default, or an enum discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SurfaceDependency {
    /// Canonical bytes with a fixed width and an explicit byte order.
    CanonicalBytes,
    /// A qualified version name.
    QualifiedVersion,
    /// A semantic grammar or AST.
    SemanticGrammar,
    /// A Rust type layout.
    RustTypeLayout,
    /// A serde default value.
    SerdeDefault,
    /// An enum discriminant value.
    EnumDiscriminant,
}

impl SurfaceDependency {
    /// Every dependency.
    pub const ALL: &'static [Self] = &[
        Self::CanonicalBytes,
        Self::QualifiedVersion,
        Self::SemanticGrammar,
        Self::RustTypeLayout,
        Self::SerdeDefault,
        Self::EnumDiscriminant,
    ];

    /// Returns the lowercase dependency name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CanonicalBytes => "canonical bytes",
            Self::QualifiedVersion => "qualified version",
            Self::SemanticGrammar => "semantic grammar",
            Self::RustTypeLayout => "Rust type layout",
            Self::SerdeDefault => "serde default",
            Self::EnumDiscriminant => "enum discriminant",
        }
    }

    /// Returns true when a frozen surface may use the dependency.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(
            self,
            Self::CanonicalBytes | Self::QualifiedVersion | Self::SemanticGrammar
        )
    }

    /// Returns true when a frozen surface must not use the dependency.
    #[must_use]
    pub const fn is_forbidden(self) -> bool {
        !self.is_allowed()
    }
}

impl fmt::Display for SurfaceDependency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One required field of a register row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RowField {
    /// The row name.
    Name,
    /// The contract version.
    Version,
    /// The parent core contract of an annex.
    Parent,
    /// The frozen parts.
    FrozenParts,
    /// The free parts.
    FreeParts,
    /// The specification reference.
    Specification,
    /// The changelog reference.
    Changelog,
    /// The vector corpus reference.
    Vectors,
    /// The migration path.
    Migration,
}

impl RowField {
    /// Every required row field.
    pub const ALL: &'static [Self] = &[
        Self::Name,
        Self::Version,
        Self::Parent,
        Self::FrozenParts,
        Self::FreeParts,
        Self::Specification,
        Self::Changelog,
        Self::Vectors,
        Self::Migration,
    ];

    /// Returns the field name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Version => "version",
            Self::Parent => "parent",
            Self::FrozenParts => "frozen_parts",
            Self::FreeParts => "free_parts",
            Self::Specification => "specification",
            Self::Changelog => "changelog",
            Self::Vectors => "vectors",
            Self::Migration => "migration",
        }
    }
}

impl fmt::Display for RowField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One row of the contract and annex register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterRow {
    /// The register identifier.
    pub id: ContractId,
    /// The contract or annex name.
    pub name: &'static str,
    /// The contract version.
    pub version: ContractVersion,
    /// The parent core contract of an annex.
    pub parent: Option<ContractId>,
    /// The parts that the freeze covers.
    pub frozen_parts: &'static [&'static str],
    /// The parts that stay free.
    pub free_parts: &'static [&'static str],
    /// The specification reference.
    pub specification: &'static str,
    /// The changelog reference.
    pub changelog: &'static str,
    /// The vector corpus reference.
    pub vectors: &'static str,
    /// The migration path.
    pub migration: &'static str,
    /// The dependencies of the frozen surface.
    pub dependencies: &'static [SurfaceDependency],
    /// The freeze state.
    pub state: FreezeState,
}

impl RegisterRow {
    /// Returns every required field that is absent.
    ///
    /// A core contract has no parent. The function does not report a missing
    /// parent for a core contract.
    #[must_use]
    pub fn missing_fields(&self) -> Vec<RowField> {
        let mut missing = Vec::new();
        if self.name.is_empty() {
            missing.push(RowField::Name);
        }
        if self.version.as_str().is_empty() {
            missing.push(RowField::Version);
        }
        if self.id.is_annex() && self.parent.is_none() {
            missing.push(RowField::Parent);
        }
        if self.frozen_parts.is_empty() {
            missing.push(RowField::FrozenParts);
        }
        if self.free_parts.is_empty() {
            missing.push(RowField::FreeParts);
        }
        if self.specification.is_empty() {
            missing.push(RowField::Specification);
        }
        if self.changelog.is_empty() {
            missing.push(RowField::Changelog);
        }
        if self.vectors.is_empty() {
            missing.push(RowField::Vectors);
        }
        if self.migration.is_empty() {
            missing.push(RowField::Migration);
        }
        missing
    }

    /// Returns true when every required field is present.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing_fields().is_empty()
    }

    /// Returns the first forbidden dependency.
    #[must_use]
    pub fn forbidden_dependency(&self) -> Option<SurfaceDependency> {
        self.dependencies
            .iter()
            .copied()
            .find(|dependency| dependency.is_forbidden())
    }
}

/// An error from register validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    /// One identifier has more than one row.
    DuplicateId {
        /// The duplicate identifier.
        id: ContractId,
    },
    /// One identifier has no row.
    MissingRow {
        /// The absent identifier.
        id: ContractId,
    },
    /// One required field is absent.
    MissingField {
        /// The row identifier.
        id: ContractId,
        /// The absent field.
        field: RowField,
    },
    /// An annex has no parent.
    MissingParent {
        /// The annex identifier.
        id: ContractId,
    },
    /// A core contract has a parent.
    UnexpectedParent {
        /// The core contract identifier.
        id: ContractId,
    },
    /// An annex names a parent that is not a core contract.
    ParentIsNotCore {
        /// The annex identifier.
        id: ContractId,
        /// The parent identifier.
        parent: ContractId,
    },
    /// A contract version has the wrong shape.
    InvalidVersion {
        /// The row identifier.
        id: ContractId,
    },
    /// A frozen surface uses a forbidden dependency.
    ForbiddenDependency {
        /// The row identifier.
        id: ContractId,
        /// The forbidden dependency.
        dependency: SurfaceDependency,
    },
}

impl fmt::Display for RegisterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId { id } => write!(formatter, "The register has two rows for {id}."),
            Self::MissingRow { id } => write!(formatter, "The register has no row for {id}."),
            Self::MissingField { id, field } => {
                write!(formatter, "The register row {id} has no {field} field.")
            }
            Self::MissingParent { id } => {
                write!(formatter, "The annex {id} has no parent contract.")
            }
            Self::UnexpectedParent { id } => {
                write!(formatter, "The core contract {id} has a parent.")
            }
            Self::ParentIsNotCore { id, parent } => write!(
                formatter,
                "The annex {id} names {parent} as parent. A parent must be a core contract."
            ),
            Self::InvalidVersion { id } => {
                write!(formatter, "The register row {id} has an invalid version.")
            }
            Self::ForbiddenDependency { id, dependency } => write!(
                formatter,
                "The frozen surface {id} depends on {dependency}."
            ),
        }
    }
}

impl std::error::Error for RegisterError {}

/// The full contract and annex register.
pub const REGISTER: &[RegisterRow] = &[
    RegisterRow {
        id: ContractId::C1,
        name: "Manifest schema",
        version: ContractVersion::new("oxkv/manifest/v1"),
        parent: None,
        frozen_parts: &[
            "table declarations",
            "key schema",
            "fields",
            "indexes",
            "rule metadata",
            "facts",
            "operations",
            "events",
            "receipts",
            "audit",
            "projection",
            "codecs",
            "control",
        ],
        free_parts: &["Rust builders", "generated code"],
        specification: "specs.md section 6.1",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-c1",
        migration: "Forward manifest migration on open. No downgrade.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::QualifiedVersion,
            SurfaceDependency::SemanticGrammar,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::C2,
        name: "Snapshot and page format",
        version: ContractVersion::new("snapshot-page/v1"),
        parent: None,
        frozen_parts: &[
            "header",
            "provenance",
            "page identity",
            "page sequence",
            "checksums",
            "framing",
            "encoding",
            "key order",
        ],
        free_parts: &["codec named in the header", "block size", "cache"],
        specification: "specs.md section 6.1",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-c2",
        migration: "Forward only. A v1 snapshot opens on 1.0 and the previous minor.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::QualifiedVersion,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::C3,
        name: "Sync wire protocol",
        version: ContractVersion::new("sync-wire/v1"),
        parent: None,
        frozen_parts: &[
            "framing",
            "SessionHello",
            "subscribe",
            "diff",
            "cursor",
            "resume",
            "flow control",
        ],
        free_parts: &["transport", "batching"],
        specification: "specs.md section 6.1",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-c3",
        migration: "One extra read version for the major. No write downgrade.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::QualifiedVersion,
            SurfaceDependency::SemanticGrammar,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::C4,
        name: "Policy language",
        version: ContractVersion::new("policy-ast/v1"),
        parent: None,
        frozen_parts: &[
            "AST",
            "evaluation order",
            "determinism",
            "step budget",
            "explanation",
        ],
        free_parts: &["source syntax sugar", "diagnostics"],
        specification: "specs.md section 6.1",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-c4",
        migration: "A grammar change that keeps the AST is additive. It needs no migration.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::SemanticGrammar,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::C5,
        name: "Receipt and error semantics",
        version: ContractVersion::new("receipt-error/v1"),
        parent: None,
        frozen_parts: &[
            "states",
            "idempotency",
            "retry class",
            "error registry",
            "receipt envelope",
        ],
        free_parts: &["scheduler", "outbox storage", "logs"],
        specification: "specs.md section 6.1",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-c5",
        migration: "A new code is additive. A removed code stays reserved.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::QualifiedVersion,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A1,
        name: "Key encoding",
        version: ContractVersion::new("key-encoding/v1"),
        parent: Some(ContractId::C2),
        frozen_parts: &[
            "version byte",
            "table grammar",
            "component tags",
            "system marker",
            "order",
            "bounds",
        ],
        free_parts: &["table set", "schemas"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a1",
        migration: "A new key encoding is a new major format with a forward migration.",
        dependencies: &[SurfaceDependency::CanonicalBytes],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A2,
        name: "Tokenizer",
        version: ContractVersion::new("tokenizer/v1"),
        parent: Some(ContractId::C1),
        frozen_parts: &[
            "identifier",
            "version",
            "Unicode data",
            "normalization",
            "case folding",
            "segmentation",
        ],
        free_parts: &["ranking parameters"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a2",
        migration: "A tokenizer change is a new annex version and a reindex.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::QualifiedVersion,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A3,
        name: "Event envelope",
        version: ContractVersion::new("event/v1"),
        parent: Some(ContractId::C5),
        frozen_parts: &["envelope fields", "event identity", "ordering", "retention"],
        free_parts: &["consumer library"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a3",
        migration: "Old envelopes stay readable. A new field is additive.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::QualifiedVersion,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A4,
        name: "Shard map",
        version: ContractVersion::new("oxkv/map/v1"),
        parent: Some(ContractId::C1),
        frozen_parts: &["routing", "table assignment", "bounds", "version", "digest"],
        free_parts: &["placement control plane"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a4",
        migration: "The map migrates under the writer lease before the first read.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::QualifiedVersion,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A5,
        name: "Scope semantics",
        version: ContractVersion::new("scope/v1"),
        parent: Some(ContractId::C3),
        frozen_parts: &["identity", "version vector", "diffs", "removal reasons"],
        free_parts: &["scope composition"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a5",
        migration: "A scope change is a new scope version. No stored scope changes in place.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::SemanticGrammar,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A6,
        name: "Projection staleness",
        version: ContractVersion::new("projection/v1"),
        parent: Some(ContractId::C4),
        frozen_parts: &["version-distance bound", "epoch", "watermarks"],
        free_parts: &["numeric limits"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a6",
        migration: "A new epoch invalidates old projections. The next open rebuilds them.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::SemanticGrammar,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A7,
        name: "Audit envelope",
        version: ContractVersion::new("audit/v1"),
        parent: Some(ContractId::C5),
        frozen_parts: &["record fields", "hash chain", "checkpoints"],
        free_parts: &["export tooling"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a7",
        migration: "The hash chain appends. An old record stays valid.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::QualifiedVersion,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A8,
        name: "Cost and explain",
        version: ContractVersion::new("cost/v1"),
        parent: Some(ContractId::C1),
        frozen_parts: &["explain schema", "cost error names"],
        free_parts: &["weights", "estimates"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a8",
        migration: "A cost model change keeps the explain schema and adds a version.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::SemanticGrammar,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A9,
        name: "Cursor types",
        version: ContractVersion::new("cursor/v1"),
        parent: Some(ContractId::C3),
        frozen_parts: &["token formats", "fields", "expiry", "errors"],
        free_parts: &["server-side state"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a9",
        migration: "A token change is a new cursor version. An old token expires.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::QualifiedVersion,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A10,
        name: "Descriptor",
        version: ContractVersion::new("descriptor/v1"),
        parent: Some(ContractId::C1),
        frozen_parts: &["method", "message", "field", "code mapping"],
        free_parts: &["language bindings"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a10",
        migration: "A descriptor change is additive. An old binding stays valid.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::QualifiedVersion,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A11,
        name: "Read pin and staleness",
        version: ContractVersion::new("read-pin/v1"),
        parent: Some(ContractId::C3),
        frozen_parts: &["pin modes", "session vector", "staleness errors"],
        free_parts: &["numeric bounds"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a11",
        migration: "A pin mode change is a new annex version. Old sessions keep their mode.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::SemanticGrammar,
        ],
        state: FreezeState::Frozen,
    },
    RegisterRow {
        id: ContractId::A12,
        name: "Migration and backup",
        version: ContractVersion::new("migration/v1"),
        parent: Some(ContractId::C2),
        frozen_parts: &["backup manifest", "restore steps", "migration steps"],
        free_parts: &["tooling", "scheduling"],
        specification: "specs.md section 6.2",
        changelog: "docs/specs/freeze.md#changelog",
        vectors: "30-qualification-suite.md#vectors-a12",
        migration: "Migration is forward only. An interrupted migration resumes on the next open.",
        dependencies: &[
            SurfaceDependency::CanonicalBytes,
            SurfaceDependency::QualifiedVersion,
        ],
        state: FreezeState::Frozen,
    },
];

/// Returns the register row for one identifier.
#[must_use]
pub fn find(id: ContractId) -> Option<&'static RegisterRow> {
    REGISTER.iter().find(|row| row.id == id)
}

/// Returns the register row for one identifier.
///
/// # Errors
///
/// Returns [`RegisterError::MissingRow`] when the register has no row for the
/// identifier.
pub fn require_row(id: ContractId, rows: &[RegisterRow]) -> Result<&RegisterRow, RegisterError> {
    rows.iter()
        .find(|row| row.id == id)
        .ok_or(RegisterError::MissingRow { id })
}

/// Checks every register rule.
///
/// # Errors
///
/// Returns the first missing row, duplicate row, missing field, invalid parent,
/// invalid version, or forbidden dependency.
pub fn validate_register(rows: &[RegisterRow]) -> Result<(), RegisterError> {
    for id in ContractId::ALL {
        let count = rows.iter().filter(|row| row.id == *id).count();
        if count > 1 {
            return Err(RegisterError::DuplicateId { id: *id });
        }
        if count == 0 {
            return Err(RegisterError::MissingRow { id: *id });
        }
    }
    for row in rows {
        if let Some(field) = row.missing_fields().first() {
            if *field == RowField::Parent {
                return Err(RegisterError::MissingParent { id: row.id });
            }
            return Err(RegisterError::MissingField {
                id: row.id,
                field: *field,
            });
        }
        if row.id.is_core() && row.parent.is_some() {
            return Err(RegisterError::UnexpectedParent { id: row.id });
        }
        if let Some(parent) = row.parent
            && !parent.is_core()
        {
            return Err(RegisterError::ParentIsNotCore { id: row.id, parent });
        }
        if !is_valid_version_text(row.version.as_str()) {
            return Err(RegisterError::InvalidVersion { id: row.id });
        }
        if let Some(dependency) = row.forbidden_dependency() {
            return Err(RegisterError::ForbiddenDependency {
                id: row.id,
                dependency,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_register_has_five_contracts_and_twelve_annexes() {
        assert_eq!(ContractId::CORE.len(), 5);
        assert_eq!(ContractId::ANNEXES.len(), 12);
        assert_eq!(ContractId::ALL.len(), REGISTER.len());
    }

    #[test]
    fn every_row_is_complete() {
        for row in REGISTER {
            assert!(row.is_complete(), "{id} is incomplete", id = row.id);
        }
    }

    #[test]
    fn every_annex_names_a_core_parent() {
        for row in REGISTER.iter().filter(|row| row.id.is_annex()) {
            let parent = row.parent.expect("an annex has a parent");
            assert!(parent.is_core(), "{id} has a non-core parent", id = row.id);
        }
    }

    #[test]
    fn every_row_accepts_the_register_rules() {
        validate_register(REGISTER).expect("the register obeys every rule");
    }

    #[test]
    fn a_duplicate_row_is_rejected() {
        let mut rows = REGISTER.to_vec();
        rows.push(REGISTER[0]);
        assert_eq!(
            validate_register(&rows),
            Err(RegisterError::DuplicateId { id: ContractId::C1 })
        );
    }

    #[test]
    fn a_missing_row_is_rejected() {
        let rows = &REGISTER[1..];
        assert_eq!(
            validate_register(rows),
            Err(RegisterError::MissingRow { id: ContractId::C1 })
        );
    }

    #[test]
    fn a_forbidden_dependency_is_rejected() {
        let mut rows = REGISTER.to_vec();
        rows[1].dependencies = &[SurfaceDependency::RustTypeLayout];
        assert_eq!(
            validate_register(&rows),
            Err(RegisterError::ForbiddenDependency {
                id: ContractId::C2,
                dependency: SurfaceDependency::RustTypeLayout,
            })
        );
    }

    #[test]
    fn a_core_parent_is_rejected() {
        let mut rows = REGISTER.to_vec();
        rows[0].parent = Some(ContractId::C2);
        assert_eq!(
            validate_register(&rows),
            Err(RegisterError::UnexpectedParent { id: ContractId::C1 })
        );
    }

    #[test]
    fn an_annex_without_a_parent_is_rejected() {
        let mut rows = REGISTER.to_vec();
        rows[5].parent = None;
        assert_eq!(
            validate_register(&rows),
            Err(RegisterError::MissingParent { id: ContractId::A1 })
        );
    }

    #[test]
    fn an_invalid_version_is_rejected() {
        let mut rows = REGISTER.to_vec();
        rows[0].version = ContractVersion::new("v1");
        assert_eq!(
            validate_register(&rows),
            Err(RegisterError::InvalidVersion { id: ContractId::C1 })
        );
    }

    #[test]
    fn every_version_has_the_name_slash_v_shape() {
        for row in REGISTER {
            assert!(
                is_valid_version_text(row.version.as_str()),
                "{id} has an invalid version",
                id = row.id
            );
        }
    }
}
