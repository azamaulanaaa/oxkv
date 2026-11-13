//! Release tiers as policy data.
//!
//! The tier table is a checked-in JSON policy. A tier names its browser
//! target count, its fuzz work, its old-artifact work, and whether it gates a
//! release.

use serde_json::Value;

/// The browser target count for one tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserTargets {
    /// One browser target. A pull request uses this tier.
    One,
    /// The full browser matrix. Nightly and release candidate use this tier.
    Full,
}

impl BrowserTargets {
    /// Returns the canonical text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::Full => "full",
        }
    }
}

/// One tier policy row.
///
/// The row is policy data. Each boolean names one independent switch. A state
/// machine would hide the independent switches that the policy owner sets.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierPolicy {
    /// The tier name.
    pub name: &'static str,
    /// The browser target count.
    pub browser_targets: BrowserTargets,
    /// True when the tier runs native vectors.
    pub native_vectors: bool,
    /// True when the tier runs browser vectors.
    pub browser_vectors: bool,
    /// True when the tier runs the full browser matrix.
    pub full_browser_matrix: bool,
    /// True when the tier runs the fuzz corpus.
    pub fuzz: bool,
    /// True when the tier runs old artifacts.
    pub old_artifacts: bool,
    /// True when the tier gates a release.
    pub gates_release: bool,
}

/// The tier policy table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierTable {
    /// The policy version.
    pub policy_version: String,
    /// The rows.
    pub tiers: Vec<TierPolicy>,
}

impl TierTable {
    /// Returns one row by name.
    #[must_use]
    pub fn tier(&self, name: &str) -> Option<&TierPolicy> {
        self.tiers.iter().find(|tier| tier.name == name)
    }
}

/// Parses the tier policy.
///
/// # Errors
///
/// Returns a text error when the policy does not match the schema.
pub fn parse_tiers(text: &str) -> Result<TierTable, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("the tier policy is invalid: {error}"))?;
    let policy_version = value["policy_version"]
        .as_str()
        .ok_or("the tier policy names no version")?
        .to_owned();
    let tiers = value["tiers"]
        .as_array()
        .ok_or("the tier policy has no row list")?
        .iter()
        .map(|row| {
            let name = row["name"].as_str().ok_or("a tier row has no name")?;
            let browser_targets = match row["browser_targets"].as_str() {
                Some("one") => BrowserTargets::One,
                Some("full") => BrowserTargets::Full,
                _ => return Err("a tier row has a bad browser target count"),
            };
            Ok(TierPolicy {
                name: match name {
                    "pull_request" => "pull_request",
                    "nightly" => "nightly",
                    "release_candidate" => "release_candidate",
                    _ => return Err("a tier row has an unknown name"),
                },
                browser_targets,
                native_vectors: row["native_vectors"].as_bool().unwrap_or(false),
                browser_vectors: row["browser_vectors"].as_bool().unwrap_or(false),
                full_browser_matrix: row["full_browser_matrix"].as_bool().unwrap_or(false),
                fuzz: row["fuzz"].as_bool().unwrap_or(false),
                old_artifacts: row["old_artifacts"].as_bool().unwrap_or(false),
                gates_release: row["gates_release"].as_bool().unwrap_or(false),
            })
        })
        .collect::<Result<Vec<_>, &str>>()?;
    Ok(TierTable {
        policy_version,
        tiers,
    })
}

/// Returns the embedded tier policy.
///
/// # Panics
///
/// Panics when the checked-in policy is invalid. A build test pins the policy.
#[must_use]
pub fn tier_table() -> TierTable {
    parse_tiers(include_str!("../policy/tiers.json")).expect("the checked-in tier policy is valid")
}

/// Returns one tier policy.
#[must_use]
pub fn policy(name: &str) -> Option<TierPolicy> {
    tier_table().tier(name).copied()
}

#[cfg(test)]
mod tests {
    use super::{BrowserTargets, parse_tiers, policy};

    #[test]
    fn a_pull_request_runs_one_browser_target() {
        let tier = policy("pull_request").expect("the tier exists");
        assert_eq!(tier.browser_targets, BrowserTargets::One);
        assert!(!tier.full_browser_matrix);
        assert!(!tier.fuzz);
    }

    #[test]
    fn nightly_runs_the_full_browser_matrix_and_fuzz() {
        let tier = policy("nightly").expect("the tier exists");
        assert_eq!(tier.browser_targets, BrowserTargets::Full);
        assert!(tier.full_browser_matrix);
        assert!(tier.fuzz);
        assert!(tier.old_artifacts);
    }

    #[test]
    fn the_release_candidate_gates_the_release_with_the_browser_matrix() {
        let tier = policy("release_candidate").expect("the tier exists");
        assert!(tier.gates_release);
        assert_eq!(tier.browser_targets, BrowserTargets::Full);
        assert!(tier.full_browser_matrix);
    }

    #[test]
    fn the_policy_names_its_version() {
        let table = parse_tiers(include_str!("../policy/tiers.json")).expect("the policy parses");
        assert_eq!(table.policy_version, "tiers/v1");
        assert_eq!(table.tiers.len(), 3);
    }
}
