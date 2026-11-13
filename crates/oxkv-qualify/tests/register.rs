//! Register and tier policy tests.

use oxkv_qualify::register::{REGISTER, annexes, core_contracts, parse_register_file};
use oxkv_qualify::tiers::{BrowserTargets, parse_tiers};

#[test]
fn the_register_file_lists_c1_to_c5_and_a1_to_a12() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("register.json"),
    )
    .expect("the register file is readable");
    let rows = parse_register_file(&text).expect("the register file parses");
    assert_eq!(rows.len(), 17);
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    for id in ["C1", "C2", "C3", "C4", "C5"] {
        assert!(ids.contains(&id), "the core contract {id} is missing");
    }
    for index in 1..=12 {
        let id = format!("A{index}");
        assert!(ids.contains(&id.as_str()), "the annex {id} is missing");
    }
    for row in &rows {
        assert_ne!(row.version, "");
        if row.surface == "annex" {
            assert!(row.parent.is_some(), "the annex {} has no parent", row.id);
        }
    }
}

#[test]
fn the_code_register_matches_the_file() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("register.json"),
    )
    .expect("the register file is readable");
    let rows = parse_register_file(&text).expect("the register file parses");
    assert_eq!(rows.len(), REGISTER.len());
    for (code, file) in REGISTER.iter().zip(&rows) {
        assert_eq!(code.id, file.id);
        assert_eq!(code.version, file.version);
        assert_eq!(code.parent, file.parent.as_deref());
        assert_eq!(code.vector_id, file.vector_id);
    }
    assert_eq!(core_contracts().len(), 5);
    assert_eq!(annexes().len(), 12);
}

#[test]
fn the_tiers_are_policy_data_with_the_required_work() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("policy/tiers.json"),
    )
    .expect("the tier policy is readable");
    let table = parse_tiers(&text).expect("the tier policy parses");
    let pull_request = table.tier("pull_request").expect("the tier exists");
    assert_eq!(pull_request.browser_targets, BrowserTargets::One);
    assert!(pull_request.native_vectors);

    let nightly = table.tier("nightly").expect("the tier exists");
    assert_eq!(nightly.browser_targets, BrowserTargets::Full);
    assert!(nightly.fuzz);
    assert!(nightly.old_artifacts);

    let release = table.tier("release_candidate").expect("the tier exists");
    assert!(release.gates_release);
    assert_eq!(release.browser_targets, BrowserTargets::Full);
}
