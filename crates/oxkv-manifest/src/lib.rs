//! Manifest v1 for oxkv.
//!
//! The crate defines the frozen manifest schema, the canonical JSON subset, the
//! validation pass, and the deterministic descriptor compiler.
//!
//! - The canonical JSON subset has integers in the i64 range, strings,
//!   booleans, arrays, and objects. It has no floats. The parser rejects a
//!   duplicate key.
//! - The manifest declares user tables by name and key schema. It declares
//!   fields, indexes, rules, facts, operations, events, receipts, audit,
//!   projection, codecs, and control operations.
//! - The manifest does not embed a mutable rule body and does not include the
//!   active policy version. A rule body change does not change the digest.
//! - The manifest validator does not read the shard map and does not include
//!   placement.
//! - The compiler emits canonical descriptor bytes. Two clean builds produce
//!   the same bytes on native and wasm32.
//!
//! The crate is portable. It does not use a clock, randomness, I/O, floating
//! point, or unsafe code.
//!
//! # Example
//!
//! ```
//! use oxkv_manifest::{compile, Manifest};
//!
//! let text = r#"{
//!   "audit":{"export":"disabled","retention":"audit_retention","visibility":"none"},
//!   "codecs":{"digest_algorithm":"blake3","format_versions":[],"tokenizer_versions":[]},
//!   "control":[],
//!   "events":[],
//!   "facts":[],
//!   "indexes":[],
//!   "operations":[],
//!   "projection":{"epoch_rule":"fact-set","invalidation":[],"watermark_rule":"contiguous"},
//!   "receipts":{"reason_codes":[],"retention":"receipt_retention","retry_classes":["Never"]},
//!   "rules":{"declarations":[]},
//!   "schema":{"revision":1,"version":"oxkv/manifest/v1"},
//!   "tables":[]
//! }"#;
//!
//! let manifest = Manifest::parse(text).expect("the manifest is valid");
//! let sealed = manifest.with_digest();
//! sealed.verify_digest().expect("the digest matches");
//! let descriptor = compile(&sealed).expect("the manifest compiles");
//! assert_eq!(descriptor.manifest_digest(), sealed.digest());
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

mod descriptor;
mod error;
mod json;
mod manifest;
pub mod schema;
mod validate;

pub use descriptor::{DESCRIPTOR_VERSION, Descriptor, compile};
pub use error::{
    CODE_ARTIFACT_DRIFT, CODE_DUPLICATE_NAME, CODE_FLOAT_NOT_ALLOWED, CODE_INDEX_SCOPE_UNREACHABLE,
    CODE_INDEX_UNKNOWN_FIELD, CODE_INVALID_VALUE, CODE_JSON_SYNTAX, CODE_KEY_SCHEMA_MISSING,
    CODE_MISSING_FIELD, CODE_OPERATION_CROSS_SHARD, CODE_OPERATION_UNKNOWN_FIELD,
    CODE_POLICY_CATALOG_MISSING, CODE_RESERVED_TABLE_NAME, CODE_RULE_UNBOUNDED,
    CODE_RULE_UNKNOWN_PATH, CODE_TYPE_MISMATCH, CODE_UNKNOWN_FIELD, CODE_UNSUPPORTED_SCHEMA,
    ManifestError,
};
pub use schema::{
    Audit, AuditVisibility, AuthClass, Codecs, ComponentType, ControlKind, ControlOperation,
    DIGEST_ALGORITHM, Encoding, Event, ExportClass, Fact, Field, FieldType, Index, KeyComponent,
    Manifest, Operation, Projection, RULE_COST_BUDGET, Receipts, RetentionClass, Rule, RuleEffect,
    RuleScope, RuleSet, SCHEMA_VERSION, Schema, StreamingShape, Table, TableLimits,
    retry_class_name, retry_class_parse,
};
