//! The workspace version gate.
//!
//! The 0.12 release publishes one `crate_version` value. This test reads the
//! root manifest, every crate manifest, and the lock file. It fails when a
//! package or a path dependency uses a different version.

use std::fs;
use std::path::{Path, PathBuf};

/// The published 0.12 crate version.
const RELEASE_VERSION: &str = "0.12.0";

/// Returns the workspace root directory.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Returns the value of `version` in the `[package]` table.
fn package_version(text: &str) -> Option<String> {
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package && let Some(value) = line.strip_prefix("version = ") {
            return Some(value.trim().trim_matches('"').to_owned());
        }
    }
    None
}

/// Returns the version requirement on every path dependency line.
fn path_dependency_versions(text: &str) -> Vec<String> {
    let mut versions = Vec::new();
    for line in text.lines() {
        if !line.contains("path = ") {
            continue;
        }
        let Some((_, rest)) = line.split_once("version = ") else {
            continue;
        };
        let value = rest
            .split([',', '}'])
            .next()
            .unwrap_or_default()
            .trim()
            .trim_matches('"');
        versions.push(value.to_owned());
    }
    versions
}

/// Returns every crate manifest path.
fn crate_manifests(root: &Path) -> Vec<PathBuf> {
    let mut manifests = Vec::new();
    for entry in fs::read_dir(root.join("crates")).expect("the crate directory is readable") {
        let directory = entry.expect("a crate directory is readable").path();
        let manifest = directory.join("Cargo.toml");
        if manifest.is_file() {
            manifests.push(manifest);
        }
    }
    manifests
}

#[test]
fn every_workspace_package_uses_version_one() {
    let root = workspace_root();
    let root_manifest =
        fs::read_to_string(root.join("Cargo.toml")).expect("the root manifest reads");
    assert_eq!(
        package_version(&root_manifest).as_deref(),
        Some(RELEASE_VERSION),
        "the root package does not use the release version"
    );

    let manifests = crate_manifests(&root);
    assert_eq!(manifests.len(), 39, "the crate count changed");
    for manifest in &manifests {
        let text = fs::read_to_string(manifest).expect("a crate manifest reads");
        assert_eq!(
            package_version(&text).as_deref(),
            Some(RELEASE_VERSION),
            "{} does not use the release version",
            manifest.display()
        );
    }
}

#[test]
fn every_path_dependency_uses_version_one() {
    let root = workspace_root();
    let mut manifests = vec![root.join("Cargo.toml")];
    manifests.extend(crate_manifests(&root));
    for manifest in &manifests {
        let text = fs::read_to_string(manifest).expect("a manifest reads");
        for version in path_dependency_versions(&text) {
            assert_eq!(
                version,
                RELEASE_VERSION,
                "{} has a path dependency on {version}",
                manifest.display()
            );
        }
    }
}

#[test]
fn the_lock_file_uses_version_one_for_every_oxkv_package() {
    let root = workspace_root();
    let lock = fs::read_to_string(root.join("Cargo.lock")).expect("the lock file reads");
    let mut in_package = false;
    let mut name = String::new();
    let mut version = String::new();
    let mut checked = 0;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            check_lock_package(&name, &version, &mut checked);
            in_package = true;
            name.clear();
            version.clear();
            continue;
        }
        if in_package {
            if let Some(value) = line.strip_prefix("name = ") {
                name = value.trim().trim_matches('"').to_owned();
            } else if let Some(value) = line.strip_prefix("version = ") {
                version = value.trim().trim_matches('"').to_owned();
            } else if line.starts_with('[') {
                check_lock_package(&name, &version, &mut checked);
                in_package = false;
                name.clear();
                version.clear();
            }
        }
    }
    check_lock_package(&name, &version, &mut checked);
    assert_eq!(checked, 40, "the lock file does not list every package");
}

/// Checks one lock package when it is a workspace package.
fn check_lock_package(name: &str, version: &str, checked: &mut usize) {
    if !name.starts_with("oxkv") {
        return;
    }
    assert_eq!(
        version, RELEASE_VERSION,
        "the lock file pins {name} at {version}"
    );
    *checked += 1;
}
