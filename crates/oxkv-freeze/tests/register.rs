//! Register coverage and golden register vectors.

use oxkv_freeze::{
    ContractId, FreezeState, REGISTER, Release, RowField, SurfaceDependency, find,
    is_valid_version_text, require_row, validate_register,
};
use serde_json::Value;

fn golden_register() -> Value {
    serde_json::from_str(include_str!("vectors/register.json")).expect("the vector file is JSON")
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("the value is an array")
        .iter()
        .map(|item| item.as_str().expect("the item is a string").to_owned())
        .collect()
}

#[test]
fn every_frozen_surface_has_a_register_row() {
    for id in ContractId::ALL {
        assert!(
            find(*id).is_some(),
            "the frozen surface {id} has no register row"
        );
    }
    assert_eq!(REGISTER.len(), ContractId::ALL.len());
}

#[test]
fn every_row_has_all_required_fields() {
    for row in REGISTER {
        assert!(
            row.is_complete(),
            "{id} is missing {missing:?}",
            id = row.id,
            missing = row.missing_fields()
        );
        assert_eq!(row.missing_fields(), Vec::<RowField>::new());
    }
}

#[test]
fn core_contracts_and_annexes_are_present() {
    assert_eq!(ContractId::CORE.len(), 5);
    assert_eq!(ContractId::ANNEXES.len(), 12);
    for id in ContractId::CORE {
        assert!(id.is_core());
        assert!(!id.is_annex());
        assert!(require_row(*id, REGISTER).is_ok());
    }
    for id in ContractId::ANNEXES {
        assert!(id.is_annex());
        assert!(require_row(*id, REGISTER).is_ok());
    }
}

#[test]
fn every_row_obeys_the_register_rules() {
    validate_register(REGISTER).expect("the register obeys every rule");
    for row in REGISTER {
        assert!(is_valid_version_text(row.version.as_str()));
        assert_ne!(row.frozen_parts, &[] as &[&str]);
        assert_ne!(row.free_parts, &[] as &[&str]);
        assert_ne!(row.specification, "");
        assert_ne!(row.changelog, "");
        assert_ne!(row.vectors, "");
        assert_ne!(row.migration, "");
        assert_eq!(row.state, FreezeState::Frozen);
    }
}

#[test]
fn every_annex_names_a_core_parent() {
    for row in REGISTER.iter().filter(|row| row.id.is_annex()) {
        let parent = row.parent.expect("an annex names a parent");
        assert!(
            parent.is_core(),
            "{id} names a non-core parent",
            id = row.id
        );
    }
    for row in REGISTER.iter().filter(|row| row.id.is_core()) {
        assert_eq!(row.parent, None, "{id} must not have a parent", id = row.id);
    }
}

#[test]
fn no_frozen_surface_depends_on_a_forbidden_layout() {
    for row in REGISTER {
        for dependency in row.dependencies {
            assert!(
                dependency.is_allowed(),
                "{id} depends on {dependency}",
                id = row.id
            );
        }
        assert_eq!(row.forbidden_dependency(), None);
    }
    for dependency in [
        SurfaceDependency::RustTypeLayout,
        SurfaceDependency::SerdeDefault,
        SurfaceDependency::EnumDiscriminant,
    ] {
        assert!(dependency.is_forbidden());
    }
}

#[test]
fn one_zero_freezes_every_register_row() {
    let release = Release::one_zero();
    assert_eq!(release.major, 1);
    assert_eq!(release.minor, 0);
    assert!(release.freezes_all());
    for row in REGISTER {
        assert_eq!(row.state, FreezeState::Frozen);
    }
}

#[test]
fn a_freeze_without_a_row_fails() {
    assert!(require_row(ContractId::A12, &[]).is_err());
    assert_eq!(
        require_row(ContractId::C1, &REGISTER[1..]),
        Err(oxkv_freeze::RegisterError::MissingRow { id: ContractId::C1 })
    );
}

#[test]
fn golden_register_rows_are_pinned() {
    let file = golden_register();
    assert_eq!(file["contract"], "freeze-register/v1");
    let rows = file["rows"].as_array().expect("the vector has rows");
    assert_eq!(rows.len(), REGISTER.len());
    for expected in rows {
        let id = ContractId::parse(expected["id"].as_str().expect("the row has an id"))
            .expect("the row id is registered");
        let row = find(id).expect("the row exists");
        assert_eq!(
            row.name,
            expected["name"].as_str().expect("the row has a name")
        );
        assert_eq!(
            row.version.as_str(),
            expected["version"].as_str().expect("the row has a version")
        );
        assert_eq!(
            row.parent.map(ContractId::as_str),
            expected["parent"].as_str()
        );
        assert_eq!(
            string_list(&expected["frozen_parts"]),
            row.frozen_parts
                .iter()
                .map(|part| (*part).to_owned())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            string_list(&expected["free_parts"]),
            row.free_parts
                .iter()
                .map(|part| (*part).to_owned())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            row.specification,
            expected["specification"]
                .as_str()
                .expect("the row has a specification")
        );
        assert_eq!(
            row.changelog,
            expected["changelog"]
                .as_str()
                .expect("the row has a changelog")
        );
        assert_eq!(
            row.vectors,
            expected["vectors"].as_str().expect("the row has vectors")
        );
        assert_eq!(
            row.migration,
            expected["migration"]
                .as_str()
                .expect("the row has a migration")
        );
        assert_eq!(
            string_list(&expected["dependencies"]),
            row.dependencies
                .iter()
                .map(|dependency| dependency.name().to_owned())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            row.state.name(),
            expected["state"].as_str().expect("the row has a state")
        );
    }
}
