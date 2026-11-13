//! The contract and annex register.
//!
//! The register lists the five core contracts and the twelve annexes. Every
//! row names one version, one parent for an annex, and one vector corpus.

/// The kind of one register row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    /// A core contract.
    Core,
    /// An annex that belongs to a core contract.
    Annex,
}

impl Surface {
    /// Returns the canonical surface name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Annex => "annex",
        }
    }
}

/// One register row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContractRow {
    /// The short id, such as `C1` or `A12`.
    pub id: &'static str,
    /// The contract or annex name.
    pub name: &'static str,
    /// The frozen contract version.
    pub version: &'static str,
    /// The parent contract id for an annex.
    pub parent: Option<&'static str>,
    /// The row kind.
    pub surface: Surface,
    /// The vector corpus id.
    pub vector_id: &'static str,
}

impl ContractRow {
    /// Returns true when the row is an annex.
    #[must_use]
    pub const fn is_annex(self) -> bool {
        matches!(self.surface, Surface::Annex)
    }
}

/// Every core contract and annex.
pub static REGISTER: [ContractRow; 17] = [
    ContractRow {
        id: "C1",
        name: "Manifest schema",
        version: "oxkv/manifest/v1",
        parent: None,
        surface: Surface::Core,
        vector_id: "C1-manifest",
    },
    ContractRow {
        id: "C2",
        name: "Snapshot and page format",
        version: "snapshot-page/v1",
        parent: None,
        surface: Surface::Core,
        vector_id: "C2-snapshot",
    },
    ContractRow {
        id: "C3",
        name: "Sync wire protocol",
        version: "sync-wire/v1",
        parent: None,
        surface: Surface::Core,
        vector_id: "C3-sync-wire",
    },
    ContractRow {
        id: "C4",
        name: "Policy language",
        version: "policy-ast/v1",
        parent: None,
        surface: Surface::Core,
        vector_id: "C4-policy",
    },
    ContractRow {
        id: "C5",
        name: "Receipt and error semantics",
        version: "receipt-error/v1",
        parent: None,
        surface: Surface::Core,
        vector_id: "C5-receipt-error",
    },
    ContractRow {
        id: "A1",
        name: "Key encoding",
        version: "key-encoding/v1",
        parent: Some("C2"),
        surface: Surface::Annex,
        vector_id: "A1-key-encoding",
    },
    ContractRow {
        id: "A2",
        name: "Tokenizer",
        version: "tokenizer/v1",
        parent: Some("C1"),
        surface: Surface::Annex,
        vector_id: "A2-tokenizer",
    },
    ContractRow {
        id: "A3",
        name: "Event envelope",
        version: "event/v1",
        parent: Some("C5"),
        surface: Surface::Annex,
        vector_id: "A3-event",
    },
    ContractRow {
        id: "A4",
        name: "Shard map",
        version: "oxkv/map/v1",
        parent: Some("C1"),
        surface: Surface::Annex,
        vector_id: "A4-shard-map",
    },
    ContractRow {
        id: "A5",
        name: "Scope semantics",
        version: "scope/v1",
        parent: Some("C3"),
        surface: Surface::Annex,
        vector_id: "A5-scope",
    },
    ContractRow {
        id: "A6",
        name: "Projection staleness",
        version: "projection/v1",
        parent: Some("C4"),
        surface: Surface::Annex,
        vector_id: "A6-projection",
    },
    ContractRow {
        id: "A7",
        name: "Audit envelope",
        version: "audit/v1",
        parent: Some("C5"),
        surface: Surface::Annex,
        vector_id: "A7-audit",
    },
    ContractRow {
        id: "A8",
        name: "Cost and explain",
        version: "cost/v1",
        parent: Some("C1"),
        surface: Surface::Annex,
        vector_id: "A8-cost",
    },
    ContractRow {
        id: "A9",
        name: "Cursor types",
        version: "cursor/v1",
        parent: Some("C3"),
        surface: Surface::Annex,
        vector_id: "A9-cursor",
    },
    ContractRow {
        id: "A10",
        name: "Descriptor",
        version: "descriptor/v1",
        parent: Some("C1"),
        surface: Surface::Annex,
        vector_id: "A10-descriptor",
    },
    ContractRow {
        id: "A11",
        name: "Read pin and staleness",
        version: "read-pin/v1",
        parent: Some("C3"),
        surface: Surface::Annex,
        vector_id: "A11-read-pin",
    },
    ContractRow {
        id: "A12",
        name: "Migration and backup",
        version: "migration/v1",
        parent: Some("C2"),
        surface: Surface::Annex,
        vector_id: "A12-migration",
    },
];

/// Returns one register row by id.
#[must_use]
pub fn row(id: &str) -> Option<&'static ContractRow> {
    REGISTER.iter().find(|row| row.id == id)
}

/// Returns every core contract.
#[must_use]
pub fn core_contracts() -> Vec<&'static ContractRow> {
    REGISTER
        .iter()
        .filter(|row| matches!(row.surface, Surface::Core))
        .collect()
}

/// Returns every annex.
#[must_use]
pub fn annexes() -> Vec<&'static ContractRow> {
    REGISTER
        .iter()
        .filter(|row| matches!(row.surface, Surface::Annex))
        .collect()
}

/// One row of the checked-in register file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterFileRow {
    /// The short id.
    pub id: String,
    /// The name.
    pub name: String,
    /// The contract version.
    pub version: String,
    /// The parent id for an annex.
    pub parent: Option<String>,
    /// The surface text.
    pub surface: String,
    /// The vector corpus id.
    pub vector_id: String,
}

/// Parses the checked-in register file.
///
/// # Errors
///
/// Returns a text error when the file does not match the register schema.
pub fn parse_register_file(text: &str) -> Result<Vec<RegisterFileRow>, String> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| format!("the register file is invalid: {error}"))?;
    let rows = value["rows"]
        .as_array()
        .ok_or("the register file has no row list")?;
    rows.iter()
        .map(|row| {
            Ok(RegisterFileRow {
                id: row["id"]
                    .as_str()
                    .ok_or("a register row has no id")?
                    .to_owned(),
                name: row["name"]
                    .as_str()
                    .ok_or("a register row has no name")?
                    .to_owned(),
                version: row["version"]
                    .as_str()
                    .ok_or("a register row has no version")?
                    .to_owned(),
                parent: row["parent"].as_str().map(str::to_owned),
                surface: row["surface"]
                    .as_str()
                    .ok_or("a register row has no surface")?
                    .to_owned(),
                vector_id: row["vector_id"]
                    .as_str()
                    .ok_or("a register row has no vector id")?
                    .to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{REGISTER, annexes, core_contracts, parse_register_file, row};

    #[test]
    fn the_register_lists_five_contracts_and_twelve_annexes() {
        assert_eq!(REGISTER.len(), 17);
        assert_eq!(core_contracts().len(), 5);
        assert_eq!(annexes().len(), 12);
    }

    #[test]
    fn the_checked_in_file_matches_the_code() {
        let file = parse_register_file(include_str!("../register.json")).expect("the file parses");
        assert_eq!(file.len(), REGISTER.len());
        for (row, file_row) in REGISTER.iter().zip(&file) {
            assert_eq!(row.id, file_row.id);
            assert_eq!(row.name, file_row.name);
            assert_eq!(row.version, file_row.version);
            assert_eq!(row.parent, file_row.parent.as_deref());
            assert_eq!(row.surface.name(), file_row.surface);
            assert_eq!(row.vector_id, file_row.vector_id);
        }
    }

    #[test]
    fn every_annex_names_a_parent() {
        for annex in annexes() {
            let parent = annex.parent.expect("the annex names a parent");
            assert!(row(parent).is_some(), "the parent {parent} is missing");
        }
    }

    #[test]
    fn every_id_is_unique() {
        for (index, left) in REGISTER.iter().enumerate() {
            for right in &REGISTER[index + 1..] {
                assert_ne!(left.id, right.id);
                assert_ne!(left.vector_id, right.vector_id);
            }
        }
    }
}
