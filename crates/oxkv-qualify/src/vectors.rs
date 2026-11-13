//! The append-only golden vector corpus.
//!
//! Every register row owns one vector file. A vector has a deterministic input
//! and one expected output. The output is either canonical bytes or one named
//! error.
//!
//! The test runner reads the checked-in files. It never regenerates them.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use oxkv_contracts::cursor::{
    CostCursor, CostFrontier, CursorType, EventCursor, EventPosition, EventSequence,
    HydrationCursor, KeyCursor, ProjectionCursor, QueryFingerprint, ReceiptCursor, ReceiptSequence,
    ScopeGeneration, SearchCursor, SortTuple, SortValue, SyncCursor,
};
use oxkv_contracts::error::{ErrorSpec, lookup};
use oxkv_contracts::keyspace::{SYSTEM_NAMESPACES, TableName};
use oxkv_contracts::receipt::{ReceiptId, ReceiptState};
use oxkv_contracts::version::{
    ArtifactKind, CommitVersion, ContentDigest, DigestDomain, FactSourceId, FactVersion, Hash32,
    PageSequence, ProjectionEpoch, ProjectionWatermarks, ScopeVersionVector, ShardId,
    SnapshotIdentity, SnapshotVersion, VersionAxis, VersionVector,
};
use oxkv_contracts::{CursorDirection, CursorToken};
use serde_json::Value;

use crate::canonical::{canonical_json, digest, hex, unhex};
use crate::policy::{EvalBudget, FactValue, evaluate};
use crate::register::REGISTER;
use crate::world::crc32c;

/// A harness error name. It is not a contract error code.
pub const HARNESS_UNKNOWN_VECTOR: &str = "qualify.vector_unknown";

/// One expected vector result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expected {
    /// Canonical bytes, as a hexadecimal string.
    Bytes(Vec<u8>),
    /// One named error.
    Error(String),
}

/// One vector case.
#[derive(Debug, Clone)]
pub struct VectorCase {
    /// The unique vector id.
    pub id: String,
    /// The evaluator kind.
    pub kind: String,
    /// The deterministic input.
    pub input: Value,
    /// The expected result.
    pub expected: Expected,
}

/// One vector file.
#[derive(Debug, Clone)]
pub struct VectorFile {
    /// The contract id, such as `C1`.
    pub contract: String,
    /// The frozen contract version.
    pub version: String,
    /// The cases.
    pub cases: Vec<VectorCase>,
}

/// One actual vector result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorObservation {
    /// The contract id.
    pub contract: String,
    /// The vector id.
    pub vector_id: String,
    /// Canonical bytes, when the case produced bytes.
    pub bytes: Option<Vec<u8>>,
    /// One named error, when the case failed.
    pub error: Option<String>,
}

impl VectorObservation {
    fn bytes(contract: &str, vector_id: &str, bytes: Vec<u8>) -> Self {
        Self {
            contract: contract.to_owned(),
            vector_id: vector_id.to_owned(),
            bytes: Some(bytes),
            error: None,
        }
    }

    fn error(contract: &str, vector_id: &str, error: impl Into<String>) -> Self {
        Self {
            contract: contract.to_owned(),
            vector_id: vector_id.to_owned(),
            bytes: None,
            error: Some(error.into()),
        }
    }

    /// Returns true when the observation matches the expectation.
    #[must_use]
    pub fn matches(&self, expected: &Expected) -> bool {
        match expected {
            Expected::Bytes(bytes) => self.bytes.as_deref() == Some(bytes.as_slice()),
            Expected::Error(error) => {
                self.bytes.is_none() && self.error.as_deref() == Some(error.as_str())
            }
        }
    }
}

/// One difference between an observation and an expectation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorDifference {
    /// The vector id.
    pub vector_id: String,
    /// The expected text.
    pub expected: String,
    /// The actual text.
    pub actual: String,
}

impl core::fmt::Display for VectorDifference {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "{} expected {} but got {}",
            self.vector_id, self.expected, self.actual
        )
    }
}

/// Parses one vector file.
///
/// # Errors
///
/// Returns a text error when the file does not match the vector schema.
pub fn parse_vector_file(text: &str) -> Result<VectorFile, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("the vector file is not valid JSON: {error}"))?;
    let contract = value["contract"]
        .as_str()
        .ok_or("the vector file names no contract")?
        .to_owned();
    let version = value["version"]
        .as_str()
        .ok_or("the vector file names no version")?
        .to_owned();
    let cases = value["cases"]
        .as_array()
        .ok_or("the vector file has no case list")?
        .iter()
        .map(parse_case)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(VectorFile {
        contract,
        version,
        cases,
    })
}

fn parse_case(value: &Value) -> Result<VectorCase, String> {
    let id = value["id"]
        .as_str()
        .ok_or("a vector case has no id")?
        .to_owned();
    let kind = value["kind"]
        .as_str()
        .ok_or_else(|| format!("the vector {id} has no kind"))?
        .to_owned();
    let input = value["input"].clone();
    let expected_value = value
        .get("expected")
        .ok_or_else(|| format!("the vector {id} has no expected value"))?;
    let expected = if let Some(text) = expected_value["bytes_hex"].as_str() {
        Expected::Bytes(unhex(text).ok_or_else(|| format!("the vector {id} has bad hex"))?)
    } else if let Some(error) = expected_value["error"].as_str() {
        Expected::Error(error.to_owned())
    } else {
        return Err(format!("the vector {id} has no bytes or error"));
    };
    Ok(VectorCase {
        id,
        kind,
        input,
        expected,
    })
}

/// Evaluates one vector case.
///
/// A contract error is part of the observation, not a harness failure. An
/// unknown case kind returns a named harness error.
#[must_use]
pub fn evaluate_case(contract: &str, case: &VectorCase) -> VectorObservation {
    match case.kind.as_str() {
        "error" => error_vector(contract, case),
        "error_shape" => error_shape_vector(contract, case),
        "receipt" => receipt_vector(contract, case),
        "receipt_transition" => receipt_transition_vector(contract, case),
        "cursor" => cursor_vector(contract, case),
        "cursor_decode" => cursor_decode_vector(contract, case),
        "version" => version_vector(contract, case),
        "namespace" => namespace_vector(contract, case),
        "table" => table_vector(contract, case),
        "provenance" => provenance_vector(contract, case),
        "digest" => digest_vector(contract, case),
        "entails" => entails_vector(contract, case),
        "policy" => policy_vector(contract, case),
        "page_checksum" => page_checksum_vector(contract, case),
        "frame" => frame_vector(contract, case),
        "frame_decode" => frame_decode_vector(contract, case),
        "artifact" => artifact_vector(contract, case),
        "audit_chain" => audit_chain_vector(contract, case),
        "descriptor" => descriptor_vector(contract, case),
        "migration" => migration_vector(contract, case),
        "projection" => projection_vector(contract, case),
        "tokenizer" => tokenizer_vector(contract, case),
        "scope_identity" => scope_identity_vector(contract, case),
        other => VectorObservation::error(
            contract,
            &case.id,
            format!("{HARNESS_UNKNOWN_VECTOR}:{other}"),
        ),
    }
}

fn text_field<'a>(value: &'a Value, field: &str, vector: &str) -> Result<&'a str, String> {
    value[field]
        .as_str()
        .ok_or_else(|| format!("the vector {vector} has no {field} text"))
}

fn u64_field(value: &Value, field: &str, vector: &str) -> Result<u64, String> {
    value[field]
        .as_u64()
        .ok_or_else(|| format!("the vector {vector} has no {field} integer"))
}

fn bool_field(value: &Value, field: &str, vector: &str) -> Result<bool, String> {
    value[field]
        .as_bool()
        .ok_or_else(|| format!("the vector {vector} has no {field} boolean"))
}

fn error_text(spec: &ErrorSpec) -> Vec<u8> {
    format!(
        "{}|{}|{}|{}|{}",
        spec.code,
        spec.kind.name(),
        spec.receipt.map_or("none", ReceiptState::name),
        spec.retry.name(),
        spec.terminal
    )
    .into_bytes()
}

fn error_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(code) = text_field(&case.input, "code", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    match lookup(code) {
        Some(spec) => VectorObservation::bytes(contract, &case.id, error_text(spec)),
        None => VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR),
    }
}

fn error_shape_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(code) = text_field(&case.input, "code", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let valid = oxkv_contracts::error::is_valid_code_shape(code);
    VectorObservation::bytes(
        contract,
        &case.id,
        if valid { "true" } else { "false" }.as_bytes().to_vec(),
    )
}

fn receipt_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(name) = text_field(&case.input, "name", &case.id) else {
        return VectorObservation::error(contract, &case.id, "receipt.state_invalid");
    };
    match ReceiptState::from_name(name) {
        Some(state) => VectorObservation::bytes(
            contract,
            &case.id,
            format!(
                "{}|{}|{}",
                state.name(),
                state.wire_tag(),
                state.is_terminal()
            )
            .into_bytes(),
        ),
        None => VectorObservation::error(contract, &case.id, "receipt.state_invalid"),
    }
}

fn receipt_transition_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(from) = text_field(&case.input, "from", &case.id) else {
        return VectorObservation::error(contract, &case.id, "receipt.state_invalid");
    };
    let Ok(to) = text_field(&case.input, "to", &case.id) else {
        return VectorObservation::error(contract, &case.id, "receipt.state_invalid");
    };
    let Some(from_state) = ReceiptState::from_name(from) else {
        return VectorObservation::error(contract, &case.id, "receipt.state_invalid");
    };
    let Some(to_state) = ReceiptState::from_name(to) else {
        return VectorObservation::error(contract, &case.id, "receipt.state_invalid");
    };
    if from_state.is_terminal() || !to_state.is_terminal() {
        VectorObservation::error(contract, &case.id, "receipt.state_invalid")
    } else {
        VectorObservation::bytes(
            contract,
            &case.id,
            format!("{}->{}", from_state.name(), to_state.name()).into_bytes(),
        )
    }
}

fn cursor_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(name) = text_field(&case.input, "type", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Some(cursor_type) = CursorType::ALL
        .iter()
        .copied()
        .find(|cursor_type| cursor_type.name() == name)
    else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    VectorObservation::bytes(contract, &case.id, sample_cursor_token(cursor_type))
}

fn cursor_decode_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(name) = text_field(&case.input, "type", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Ok(token_hex) = text_field(&case.input, "token_hex", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Some(cursor_type) = CursorType::ALL
        .iter()
        .copied()
        .find(|cursor_type| cursor_type.name() == name)
    else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Some(token) = unhex(token_hex) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    match decode_cursor(cursor_type, &token) {
        Ok(()) => VectorObservation::bytes(contract, &case.id, b"ok".to_vec()),
        Err(code) => VectorObservation::error(contract, &case.id, code),
    }
}

fn version_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(name) = text_field(&case.input, "axis", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    match VersionAxis::ALL
        .iter()
        .copied()
        .find(|axis| axis.name() == name)
    {
        Some(axis) => VectorObservation::bytes(
            contract,
            &case.id,
            format!("{}|{}", axis.name(), axis.group().name()).into_bytes(),
        ),
        None => VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR),
    }
}

fn namespace_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(label) = text_field(&case.input, "label", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    match SYSTEM_NAMESPACES
        .iter()
        .find(|namespace| namespace.label == label)
    {
        Some(namespace) => VectorObservation::bytes(
            contract,
            &case.id,
            format!(
                "{}|{}|{}|{}",
                namespace.label, namespace.id, namespace.commit_domain, namespace.scope_visible
            )
            .into_bytes(),
        ),
        None => VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR),
    }
}

fn table_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(name) = text_field(&case.input, "name", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    match TableName::parse(name) {
        Ok(table) => VectorObservation::bytes(
            contract,
            &case.id,
            format!("ok|{}|{}", table.as_str(), table.is_catalog()).into_bytes(),
        ),
        Err(error) => VectorObservation::error(contract, &case.id, error.code()),
    }
}

fn provenance_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(name) = text_field(&case.input, "kind", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Some(kind) = ArtifactKind::ALL
        .iter()
        .copied()
        .find(|kind| kind.name() == name)
    else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let fields: Vec<&str> = kind
        .required_fields()
        .iter()
        .map(|field| field.name())
        .collect();
    VectorObservation::bytes(
        contract,
        &case.id,
        format!("{}|{}", kind.name(), fields.join(",")).into_bytes(),
    )
}

fn digest_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(domain_name) = text_field(&case.input, "domain", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Ok(bytes_hex) = text_field(&case.input, "bytes_hex", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Some(bytes) = unhex(bytes_hex) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Some(domain) = digest_domain(domain_name) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    VectorObservation::bytes(
        contract,
        &case.id,
        Hash32::of_domain(domain, &bytes).as_bytes().to_vec(),
    )
}
fn digest_domain(name: &str) -> Option<DigestDomain> {
    match name {
        "content" => Some(DigestDomain::Content),
        "snapshot" => Some(DigestDomain::Snapshot),
        "page" => Some(DigestDomain::Page),
        "manifest" => Some(DigestDomain::Manifest),
        "policy" => Some(DigestDomain::Policy),
        "map" => Some(DigestDomain::Map),
        "descriptor" => Some(DigestDomain::Descriptor),
        "scope" => Some(DigestDomain::Scope),
        "audit-record" => Some(DigestDomain::AuditRecord),
        "event-id" => Some(DigestDomain::EventId),
        "receipt-id" => Some(DigestDomain::ReceiptId),
        "cursor-token" => Some(DigestDomain::CursorToken),
        _ => None,
    }
}

fn vector_from_json(value: &Value, vector: &str) -> Result<VersionVector, String> {
    let entries = value
        .as_array()
        .ok_or_else(|| format!("the vector {vector} has no version list"))?;
    let mut result = VersionVector::new();
    for entry in entries {
        let pair = entry
            .as_array()
            .ok_or_else(|| format!("the vector {vector} has a bad version entry"))?;
        if pair.len() != 2 {
            return Err(format!("the vector {vector} has a bad version entry"));
        }
        let shard = pair[0]
            .as_str()
            .and_then(unhex)
            .ok_or_else(|| format!("the vector {vector} has a bad shard id"))?;
        let version = pair[1]
            .as_u64()
            .ok_or_else(|| format!("the vector {vector} has a bad commit version"))?;
        result.set(ShardId::new(shard), CommitVersion::new(version));
    }
    Ok(result)
}

fn entails_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(left) = vector_from_json(&case.input["left"], &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Ok(right) = vector_from_json(&case.input["right"], &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    VectorObservation::bytes(
        contract,
        &case.id,
        if left.entails(&right) {
            "true"
        } else {
            "false"
        }
        .as_bytes()
        .to_vec(),
    )
}

fn policy_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(expression) = text_field(&case.input, "expression", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Ok(facts) = policy_facts(&case.input["facts"], &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let budget = EvalBudget {
        max_steps: case.input["budget"]["max_steps"]
            .as_u64()
            .unwrap_or(1024)
            .try_into()
            .unwrap_or(1024),
        max_depth: case.input["budget"]["max_depth"]
            .as_u64()
            .unwrap_or(32)
            .try_into()
            .unwrap_or(32),
        max_arena: case.input["budget"]["max_arena"]
            .as_u64()
            .unwrap_or(65536)
            .try_into()
            .unwrap_or(65536),
    };
    match evaluate(expression, &facts, budget) {
        Ok(evaluation) => VectorObservation::bytes(
            contract,
            &case.id,
            format!(
                "{}|{}|{}|{}",
                evaluation.verdict.name(),
                evaluation.stats.steps,
                evaluation.stats.depth,
                evaluation.stats.arena
            )
            .into_bytes(),
        ),
        Err(error) => VectorObservation::error(contract, &case.id, error.code()),
    }
}

fn policy_facts(value: &Value, vector: &str) -> Result<BTreeMap<String, FactValue>, String> {
    let map = value
        .as_object()
        .ok_or_else(|| format!("the vector {vector} has no fact map"))?;
    let mut facts = BTreeMap::new();
    for (name, tagged) in map {
        let fact = if let Some(integer) = tagged["int"].as_i64() {
            FactValue::Int(integer)
        } else if let Some(flag) = tagged["bool"].as_bool() {
            FactValue::Bool(flag)
        } else if let Some(text) = tagged["text"].as_str() {
            FactValue::Text(text.to_owned())
        } else if let Some(bytes) = tagged["bytes_hex"].as_str().and_then(unhex) {
            FactValue::Bytes(bytes)
        } else {
            return Err(format!("the vector {vector} has a bad fact value"));
        };
        facts.insert(name.clone(), fact);
    }
    Ok(facts)
}

fn page_checksum_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(payload_hex) = text_field(&case.input, "payload_hex", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Some(payload) = unhex(payload_hex) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let checksum = crc32c(&payload);
    if let Some(expected) = case.input["expected_crc32c"].as_u64()
        && u64::from(checksum) != expected
    {
        return VectorObservation::error(contract, &case.id, "format.corrupt_page");
    }
    VectorObservation::bytes(contract, &case.id, format!("{checksum:08x}").into_bytes())
}

fn frame_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(key_hex) = text_field(&case.input, "key_hex", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Ok(value_hex) = text_field(&case.input, "value_hex", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Some(key) = unhex(key_hex) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Some(value) = unhex(value_hex) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Ok(index) = u64_field(&case.input, "index", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Ok(key_version) = u64_field(&case.input, "key_version", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let frame = crate::world::WireFrame {
        index,
        key,
        value,
        key_version,
    };
    VectorObservation::bytes(contract, &case.id, frame.encode())
}

fn frame_decode_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(bytes_hex) = text_field(&case.input, "bytes_hex", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Some(bytes) = unhex(bytes_hex) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    match crate::world::WireFrame::decode(&bytes) {
        Ok(frame) => VectorObservation::bytes(
            contract,
            &case.id,
            format!(
                "ok|{}|{}|{}",
                frame.index,
                hex(&frame.key),
                frame.key_version
            )
            .into_bytes(),
        ),
        Err(code) => VectorObservation::error(contract, &case.id, code),
    }
}

fn artifact_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(major) = u64_field(&case.input, "format_major", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Ok(minor) = u64_field(&case.input, "format_minor", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let provenance = bool_field(&case.input, "provenance", &case.id).unwrap_or(true);
    let artifact = crate::world::Artifact {
        format_major: u16::try_from(major).unwrap_or(u16::MAX),
        format_minor: u16::try_from(minor).unwrap_or(u16::MAX),
        manifest_digest: if provenance {
            Some(*Hash32::of(b"manifest").as_bytes())
        } else {
            None
        },
        rows: Vec::new(),
    };
    let decoded = match crate::world::Artifact::decode(&artifact.encode()) {
        Ok(decoded) => decoded,
        Err(code) => return VectorObservation::error(contract, &case.id, code),
    };
    match decoded.require_provenance() {
        Ok(decoded) => VectorObservation::bytes(
            contract,
            &case.id,
            format!("ok|{}.{}", decoded.format_major, decoded.format_minor).into_bytes(),
        ),
        Err(code) => VectorObservation::error(contract, &case.id, code),
    }
}

fn audit_chain_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Some(bodies) = case.input["bodies"].as_array() else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let mut previous = Hash32::of(b"genesis");
    for body in bodies {
        let Some(bytes) = body.as_str().and_then(unhex) else {
            return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
        };
        let mut material = Vec::new();
        material.extend_from_slice(previous.as_bytes());
        material.extend_from_slice(&bytes);
        previous = Hash32::of(&material);
    }
    VectorObservation::bytes(contract, &case.id, previous.as_bytes().to_vec())
}

fn descriptor_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Some(methods) = case.input["methods"].as_array() else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let mut text = Vec::new();
    for method in methods {
        let Some(method) = method.as_str() else {
            return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
        };
        text.push(Value::String(method.to_owned()));
    }
    let value = Value::Array(text);
    match canonical_json(&value) {
        Ok(json) => VectorObservation::bytes(contract, &case.id, json.into_bytes()),
        Err(error) => VectorObservation::error(contract, &case.id, error.code()),
    }
}

fn migration_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Ok(from) = u64_field(&case.input, "from_major", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let Ok(to) = u64_field(&case.input, "to_major", &case.id) else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    if to < from {
        return VectorObservation::error(contract, &case.id, "ops.downgrade_not_allowed");
    }
    if to > from.saturating_add(1) {
        return VectorObservation::error(contract, &case.id, "format.unsupported_version");
    }
    VectorObservation::bytes(contract, &case.id, b"migrate".to_vec())
}

fn projection_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Some(entries) = case.input["watermarks"].as_array() else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let mut sorted: Vec<(&str, u64)> = Vec::new();
    for entry in entries {
        let Some(pair) = entry.as_array() else {
            return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
        };
        if pair.len() != 2 {
            return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
        }
        let Some(source) = pair[0].as_str() else {
            return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
        };
        let Some(version) = pair[1].as_u64() else {
            return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
        };
        sorted.push((source, version));
    }
    sorted.sort_unstable();
    let epoch = case.input["epoch"].as_u64().unwrap_or(0);
    let mut text = format!("epoch={epoch}");
    for (source, version) in sorted {
        let _ = write!(text, "|{source}={version}");
    }
    VectorObservation::bytes(contract, &case.id, text.into_bytes())
}

fn tokenizer_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Some(entries) = case.input["fields"].as_array() else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let mut sorted: Vec<(&str, &str)> = Vec::new();
    for entry in entries {
        let Some(pair) = entry.as_array() else {
            return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
        };
        if pair.len() != 2 {
            return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
        }
        let (Some(field), Some(version)) = (pair[0].as_str(), pair[1].as_str()) else {
            return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
        };
        sorted.push((field, version));
    }
    sorted.sort_unstable();
    let text = sorted
        .iter()
        .map(|(field, version)| format!("{field}={version}"))
        .collect::<Vec<_>>()
        .join("|");
    VectorObservation::bytes(contract, &case.id, text.into_bytes())
}

fn scope_identity_vector(contract: &str, case: &VectorCase) -> VectorObservation {
    let Some(tables) = case.input["tables"].as_array() else {
        return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
    };
    let mut names: Vec<&str> = Vec::new();
    for table in tables {
        let Some(name) = table.as_str() else {
            return VectorObservation::error(contract, &case.id, HARNESS_UNKNOWN_VECTOR);
        };
        names.push(name);
    }
    names.sort_unstable();
    let mut material = crate::canonical::CanonicalWriter::new();
    material.text("scope/v1");
    for name in names {
        material.text(name);
    }
    VectorObservation::bytes(
        contract,
        &case.id,
        digest("scope", &material.finish()).as_bytes().to_vec(),
    )
}

fn sample_cursor_token(cursor_type: CursorType) -> Vec<u8> {
    match cursor_type {
        CursorType::Key => KeyCursor::new(
            TableName::parse_user("accounts").expect("the table name is valid"),
            SnapshotVersion::new(18),
            vec![b'k', 1],
            CursorDirection::Ascending,
        )
        .to_token(),
        CursorType::Sync => SyncCursor::new(
            ScopeGeneration::new(3),
            ScopeVersionVector::from_vector(VersionVector::from_entries([
                (ShardId::new(vec![1]), CommitVersion::new(18)),
                (ShardId::new(vec![2]), CommitVersion::new(9)),
            ])),
            ScopeVersionVector::from_vector(VersionVector::from_entries([(
                ShardId::new(vec![1]),
                CommitVersion::new(4),
            )])),
            Hash32::from_bytes([7; 32]),
        )
        .to_token(),
        CursorType::Hydration => HydrationCursor::new(
            SnapshotIdentity::new(
                ShardId::new(vec![1]),
                CommitVersion::new(18),
                ContentDigest::from_bytes([8; 32]),
            ),
            PageSequence::new(2),
            ContentDigest::from_bytes([9; 32]),
        )
        .to_token(),
        CursorType::Receipt => {
            ReceiptCursor::new(ReceiptId::from_bytes([10; 32]), ReceiptSequence::new(5)).to_token()
        }
        CursorType::Event => {
            let mut entries = BTreeMap::new();
            entries.insert(vec![b'a'], EventSequence::new(4));
            entries.insert(vec![b'b'], EventSequence::new(7));
            EventCursor::new(EventPosition::PerKey(entries)).to_token()
        }
        CursorType::Search => SearchCursor::new(
            ScopeVersionVector::from_vector(VersionVector::from_entries([(
                ShardId::new(vec![1]),
                CommitVersion::new(18),
            )])),
            QueryFingerprint::from_bytes([11; 32]),
            SortTuple::new(vec![
                SortValue::Integer(42),
                SortValue::Text("alpha".to_owned()),
                SortValue::Bytes(vec![1, 2, 3]),
            ]),
        )
        .to_token(),
        CursorType::Projection => {
            let mut watermarks = ProjectionWatermarks::new();
            watermarks.set(FactSourceId::new("accounts"), FactVersion::new(12));
            ProjectionCursor::new(watermarks, ProjectionEpoch::new(41)).to_token()
        }
        CursorType::Cost => {
            let mut frontiers = BTreeMap::new();
            frontiers.insert(ShardId::new(vec![1]), CostFrontier::new(10));
            frontiers.insert(ShardId::new(vec![2]), CostFrontier::new(20));
            CostCursor::new(
                frontiers,
                ScopeVersionVector::from_vector(VersionVector::from_entries([
                    (ShardId::new(vec![1]), CommitVersion::new(18)),
                    (ShardId::new(vec![2]), CommitVersion::new(9)),
                ])),
                5,
            )
            .to_token()
        }
    }
}

pub(crate) fn decode_cursor(cursor_type: CursorType, token: &[u8]) -> Result<(), &'static str> {
    let result = match cursor_type {
        CursorType::Key => KeyCursor::from_token(token).map(|_| ()),
        CursorType::Sync => SyncCursor::from_token(token).map(|_| ()),
        CursorType::Hydration => HydrationCursor::from_token(token).map(|_| ()),
        CursorType::Receipt => ReceiptCursor::from_token(token).map(|_| ()),
        CursorType::Event => EventCursor::from_token(token).map(|_| ()),
        CursorType::Search => SearchCursor::from_token(token).map(|_| ()),
        CursorType::Projection => ProjectionCursor::from_token(token).map(|_| ()),
        CursorType::Cost => CostCursor::from_token(token).map(|_| ()),
    };
    result.map_err(oxkv_contracts::CursorError::code)
}

/// One manifest entry.
#[derive(Debug, Clone)]
pub struct VectorEntry {
    /// The vector corpus id.
    pub id: String,
    /// The contract id.
    pub contract: String,
    /// The path from the crate root.
    pub path: String,
    /// The `BLAKE3` digest of the file bytes.
    pub digest: String,
    /// True when the vector is released.
    pub released: bool,
}

/// The vector manifest.
#[derive(Debug, Clone)]
pub struct VectorManifest {
    /// The manifest schema.
    pub vector_version: String,
    /// The entries.
    pub entries: Vec<VectorEntry>,
}

impl VectorManifest {
    /// Returns one entry by vector id.
    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&VectorEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

/// Parses the vector manifest.
///
/// # Errors
///
/// Returns a text error when the manifest does not match the schema.
pub fn parse_manifest(text: &str) -> Result<VectorManifest, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("the manifest is not valid JSON: {error}"))?;
    let vector_version = value["vector_version"]
        .as_str()
        .ok_or("the manifest names no vector version")?
        .to_owned();
    let entries = value["vectors"]
        .as_array()
        .ok_or("the manifest has no vector list")?
        .iter()
        .map(|entry| {
            Ok(VectorEntry {
                id: entry["id"]
                    .as_str()
                    .ok_or("a manifest entry has no id")?
                    .to_owned(),
                contract: entry["contract"]
                    .as_str()
                    .ok_or("a manifest entry has no contract")?
                    .to_owned(),
                path: entry["path"]
                    .as_str()
                    .ok_or("a manifest entry has no path")?
                    .to_owned(),
                digest: entry["digest"]
                    .as_str()
                    .ok_or("a manifest entry has no digest")?
                    .to_owned(),
                released: entry["released"].as_bool().unwrap_or(false),
            })
        })
        .collect::<Result<Vec<_>, &str>>()?;
    Ok(VectorManifest {
        vector_version,
        entries,
    })
}

/// Returns the register row vector sources, embedded at build time.
pub static EMBEDDED_VECTORS: &[(&str, &str)] = &[
    ("C1-manifest", include_str!("../vectors/C1-manifest.json")),
    ("C2-snapshot", include_str!("../vectors/C2-snapshot.json")),
    ("C3-sync-wire", include_str!("../vectors/C3-sync-wire.json")),
    ("C4-policy", include_str!("../vectors/C4-policy.json")),
    (
        "C5-receipt-error",
        include_str!("../vectors/C5-receipt-error.json"),
    ),
    (
        "A1-key-encoding",
        include_str!("../vectors/A1-key-encoding.json"),
    ),
    ("A2-tokenizer", include_str!("../vectors/A2-tokenizer.json")),
    ("A3-event", include_str!("../vectors/A3-event.json")),
    ("A4-shard-map", include_str!("../vectors/A4-shard-map.json")),
    ("A5-scope", include_str!("../vectors/A5-scope.json")),
    (
        "A6-projection",
        include_str!("../vectors/A6-projection.json"),
    ),
    ("A7-audit", include_str!("../vectors/A7-audit.json")),
    ("A8-cost", include_str!("../vectors/A8-cost.json")),
    ("A9-cursor", include_str!("../vectors/A9-cursor.json")),
    (
        "A10-descriptor",
        include_str!("../vectors/A10-descriptor.json"),
    ),
    ("A11-read-pin", include_str!("../vectors/A11-read-pin.json")),
    (
        "A12-migration",
        include_str!("../vectors/A12-migration.json"),
    ),
];

/// Returns one embedded vector file by contract id.
#[must_use]
pub fn embedded_vector(contract_id: &str) -> Option<&'static str> {
    let row = REGISTER.iter().find(|row| row.id == contract_id)?;
    EMBEDDED_VECTORS
        .iter()
        .find(|(id, _)| *id == row.vector_id)
        .map(|(_, text)| *text)
}

#[cfg(test)]
mod tests {
    use super::{EMBEDDED_VECTORS, Expected, evaluate_case, parse_manifest, parse_vector_file};
    use crate::register::REGISTER;

    #[test]
    fn every_register_row_has_an_embedded_vector_file() {
        assert_eq!(EMBEDDED_VECTORS.len(), REGISTER.len());
        for row in &REGISTER {
            let text = super::embedded_vector(row.id).expect("the row has a vector file");
            let file = parse_vector_file(text).expect("the vector file parses");
            assert_eq!(file.contract, row.id);
            assert_eq!(file.version, row.version);
            assert!(!file.cases.is_empty());
        }
    }

    #[test]
    fn every_embedded_case_matches_its_expected_value() {
        for row in &REGISTER {
            let text = super::embedded_vector(row.id).expect("the row has a vector file");
            let file = parse_vector_file(text).expect("the vector file parses");
            for case in &file.cases {
                let observation = evaluate_case(&file.contract, case);
                assert!(
                    observation.matches(&case.expected),
                    "the vector {} does not match: expected {:?}, got {:?}",
                    case.id,
                    case.expected,
                    observation
                );
            }
        }
    }

    #[test]
    fn the_manifest_lists_every_vector() {
        let manifest =
            parse_manifest(include_str!("../vectors/manifest.json")).expect("the manifest parses");
        assert_eq!(manifest.entries.len(), REGISTER.len());
        for row in &REGISTER {
            let entry = manifest.entry(row.vector_id).expect("the row is listed");
            assert_eq!(entry.contract, row.id);
            assert!(entry.released);
        }
    }

    #[test]
    fn an_unknown_kind_is_a_named_harness_error() {
        let case = super::VectorCase {
            id: "x".to_owned(),
            kind: "not_a_kind".to_owned(),
            input: serde_json::Value::Null,
            expected: Expected::Error("qualify.vector_unknown:not_a_kind".to_owned()),
        };
        let observation = evaluate_case("C1", &case);
        assert!(observation.matches(&case.expected));
    }

    #[test]
    fn an_unknown_receipt_state_fails_decode() {
        let case = super::VectorCase {
            id: "receipt/unknown".to_owned(),
            kind: "receipt".to_owned(),
            input: serde_json::json!({ "name": "Canceled" }),
            expected: Expected::Error("receipt.state_invalid".to_owned()),
        };
        let observation = evaluate_case("C5", &case);
        assert!(observation.matches(&case.expected));
    }
}
