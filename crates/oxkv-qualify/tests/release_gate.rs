//! The freeze and qualification register gates.
//!
//! The freeze document, the freeze crate, the qualification document, the
//! qualification crate, and the vector corpus must name the same 17 rows.
//! Every row must have a released vector file.

use std::fs;
use std::path::PathBuf;

use oxkv_freeze::{ContractId, REGISTER as FREEZE_REGISTER, find};
use oxkv_qualify::register::REGISTER as QUALIFY_REGISTER;
use oxkv_qualify::vectors::{parse_manifest, parse_vector_file};

/// The number of frozen surfaces.
const ROW_COUNT: usize = 17;

/// Returns the workspace root directory.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Returns the qualification crate directory.
fn qualify_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Returns the table rows that name a register row.
///
/// The function reads one markdown table. It keeps the rows that start with a
/// `C` or an `A` identifier and have the expected column count. It splits each
/// row on the pipe character.
fn register_table(text: &str, columns: usize) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<String> = line
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().replace('`', ""))
            .collect();
        if cells.len() != columns {
            continue;
        }
        let Some(id) = cells.first() else {
            continue;
        };
        let valid = id.len() >= 2
            && matches!(id.as_bytes()[0], b'C' | b'A')
            && id[1..].chars().all(|character| character.is_ascii_digit());
        if valid {
            rows.push(cells);
        }
    }
    rows
}

/// Returns the parent text of one table row.
fn parent_text(row: &[String]) -> &str {
    row.get(3).map_or("", String::as_str)
}

/// Returns the version text of one table row.
fn version_text(row: &[String]) -> &str {
    row.get(2).map_or("", String::as_str)
}

#[test]
fn the_freeze_and_qualification_registers_agree() {
    assert_eq!(FREEZE_REGISTER.len(), ROW_COUNT);
    assert_eq!(QUALIFY_REGISTER.len(), ROW_COUNT);
    for freeze_row in FREEZE_REGISTER {
        let qualify_row = QUALIFY_REGISTER
            .iter()
            .find(|row| row.id == freeze_row.id.as_str())
            .unwrap_or_else(|| panic!("the qualification register omits {}", freeze_row.id));
        assert_eq!(qualify_row.name, freeze_row.name);
        assert_eq!(qualify_row.version, freeze_row.version.as_str());
        assert_eq!(
            qualify_row.parent,
            freeze_row.parent.map(ContractId::as_str)
        );
        assert_eq!(qualify_row.surface.name(), surface_name(freeze_row.id));
    }
}

/// Returns the surface text of one freeze row.
fn surface_name(id: ContractId) -> &'static str {
    if id.is_core() { "core" } else { "annex" }
}

#[test]
fn the_freeze_document_lists_every_register_row() {
    let path = workspace_root().join("docs/specs/freeze.md");
    let text = fs::read_to_string(&path).expect("the freeze document reads");
    let rows = register_table(&text, 7);
    assert_eq!(rows.len(), ROW_COUNT, "the freeze document row count");
    for row in &rows {
        let id = ContractId::parse(&row[0]).expect("the document names a registered row");
        let registered = find(id).expect("the register has the row");
        assert_eq!(row[1], registered.name);
        assert_eq!(version_text(row), registered.version.as_str());
        assert_eq!(
            parent_text(row),
            registered.parent.map_or("none", ContractId::as_str)
        );
        assert_eq!(
            row.get(4).map_or("", String::as_str),
            registered.specification
        );
        assert_eq!(row.get(5).map_or("", String::as_str), registered.vectors);
        assert_eq!(row.get(6).map_or("", String::as_str), registered.migration);
    }
}

#[test]
fn the_qualification_document_lists_every_register_row() {
    let path = workspace_root().join("docs/specs/qualification.md");
    let text = fs::read_to_string(&path).expect("the qualification document reads");
    let rows = register_table(&text, 5);
    assert_eq!(
        rows.len(),
        ROW_COUNT,
        "the qualification document row count"
    );
    for row in &rows {
        let registered = QUALIFY_REGISTER
            .iter()
            .find(|entry| entry.id == row[0])
            .unwrap_or_else(|| panic!("the document names the unknown row {}", row[0]));
        assert_eq!(row[1], registered.name);
        assert_eq!(version_text(row), registered.version);
        assert_eq!(parent_text(row), registered.parent.unwrap_or("none"));
        let vector_file = row.get(4).map_or("", String::as_str);
        assert_eq!(vector_file, format!("{}.json", registered.vector_id));
        assert!(
            qualify_root().join("vectors").join(vector_file).is_file(),
            "the vector file {vector_file} is missing"
        );
    }
}

#[test]
fn every_register_row_has_a_released_vector_corpus() {
    let manifest_text = fs::read_to_string(qualify_root().join("vectors/manifest.json"))
        .expect("the vector manifest reads");
    let manifest = parse_manifest(&manifest_text).expect("the vector manifest parses");
    assert_eq!(manifest.entries.len(), ROW_COUNT);
    for row in &QUALIFY_REGISTER {
        let entry = manifest
            .entry(row.vector_id)
            .unwrap_or_else(|| panic!("the row {} has no vector entry", row.id));
        let path = qualify_root().join(&entry.path);
        assert!(path.is_file(), "the vector file {} is missing", entry.path);
        let file = parse_vector_file(&fs::read_to_string(&path).expect("the vector file reads"))
            .expect("the vector file parses");
        assert_eq!(file.contract, row.id);
        assert_eq!(file.version, row.version);
        assert!(!file.cases.is_empty());
    }
}

#[test]
fn the_freeze_corpus_and_the_qualification_vectors_agree() {
    let text = fs::read_to_string(qualify_root().join("../oxkv-freeze/tests/vectors/corpus.json"))
        .expect("the freeze corpus reads");
    let value: serde_json::Value = serde_json::from_str(&text).expect("the freeze corpus is JSON");
    let entries = value["entries"]
        .as_array()
        .expect("the freeze corpus has entries");
    assert_eq!(entries.len(), ROW_COUNT);
    for entry in entries {
        let id = entry["contract"].as_str().expect("the entry names a row");
        let vector = entry["vector"].as_str().expect("the entry names a vector");
        let row = QUALIFY_REGISTER
            .iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("the freeze corpus names the unknown row {id}"));
        assert_eq!(vector, row.vector_id);
        let path = qualify_root()
            .join("vectors")
            .join(format!("{vector}.json"));
        assert!(path.is_file(), "the vector file {vector}.json is missing");
    }
}
