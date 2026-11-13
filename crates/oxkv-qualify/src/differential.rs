//! Differential comparison across targets and versions.
//!
//! One expected output is the anchor. The comparator checks that every target
//! produced the same bytes or the same named error. It also compares a vector
//! across contract versions.

use crate::scenario::ScenarioRun;
use crate::target::Target;
use crate::vectors::{Expected, VectorObservation};

/// One difference between two observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// The vector or case id.
    pub id: String,
    /// The expected side.
    pub expected: String,
    /// The actual side.
    pub actual: String,
}

impl core::fmt::Display for Difference {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "{}: expected {} but got {}",
            self.id, self.expected, self.actual
        )
    }
}

/// Compares one observation with its expected value.
///
/// # Errors
///
/// Returns one [`Difference`] when the bytes or the named error differ.
pub fn compare_expected(
    observation: &VectorObservation,
    expected: &Expected,
) -> Result<(), Difference> {
    if observation.matches(expected) {
        Ok(())
    } else {
        Err(Difference {
            id: observation.vector_id.clone(),
            expected: describe_expected(expected),
            actual: describe_observation(observation),
        })
    }
}

/// Compares observations from every target.
///
/// # Errors
///
/// Returns every difference. The first observation is the reference.
pub fn compare_targets(
    observations: &[(Target, VectorObservation)],
) -> Result<(), Vec<Difference>> {
    let Some((_, reference)) = observations.first() else {
        return Ok(());
    };
    let mut differences = Vec::new();
    for (target, observation) in &observations[1..] {
        if observation.bytes != reference.bytes || observation.error != reference.error {
            differences.push(Difference {
                id: format!("{}/{}", reference.vector_id, target.name()),
                expected: describe_observation(reference),
                actual: describe_observation(observation),
            });
        }
    }
    if differences.is_empty() {
        Ok(())
    } else {
        Err(differences)
    }
}

/// Compares two named versions of one vector.
///
/// A stable minor version must keep the same bytes. A major change must return
/// one named error.
///
/// # Errors
///
/// Returns one [`Difference`] when the versions diverge without a named error.
pub fn compare_versions(
    id: &str,
    old: &VectorObservation,
    new: &VectorObservation,
) -> Result<(), Difference> {
    if old.bytes == new.bytes && old.error == new.error {
        return Ok(());
    }
    if new.error.is_some() {
        return Ok(());
    }
    Err(Difference {
        id: id.to_owned(),
        expected: "the same bytes or one named error".to_owned(),
        actual: describe_observation(new),
    })
}

/// Compares the same case across targets in the scenario matrix.
///
/// # Errors
///
/// Returns every difference in the pass flag or the trace digest.
pub fn compare_scenario_targets(runs: &[ScenarioRun]) -> Result<(), Vec<Difference>> {
    let Some(reference) = runs.first() else {
        return Ok(());
    };
    let mut differences = Vec::new();
    for run in &runs[1..] {
        if run.passed != reference.passed {
            differences.push(Difference {
                id: format!("{}/pass", reference.case_id),
                expected: reference.passed.to_string(),
                actual: run.passed.to_string(),
            });
        }
        if run.trace.digest() != reference.trace.digest() {
            differences.push(Difference {
                id: format!("{}/trace", reference.case_id),
                expected: reference.trace.digest_hex(),
                actual: run.trace.digest_hex(),
            });
        }
    }
    if differences.is_empty() {
        Ok(())
    } else {
        Err(differences)
    }
}

fn describe_expected(expected: &Expected) -> String {
    match expected {
        Expected::Bytes(bytes) => format!("bytes:{}", crate::canonical::hex(bytes)),
        Expected::Error(error) => format!("error:{error}"),
    }
}

fn describe_observation(observation: &VectorObservation) -> String {
    if let Some(error) = &observation.error {
        format!("error:{error}")
    } else if let Some(bytes) = &observation.bytes {
        format!("bytes:{}", crate::canonical::hex(bytes))
    } else {
        "empty".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use crate::target::Target;
    use crate::vectors::{Expected, VectorObservation};

    use super::{compare_expected, compare_scenario_targets, compare_targets, compare_versions};

    fn observation(bytes: &[u8]) -> VectorObservation {
        VectorObservation {
            contract: "C1".to_owned(),
            vector_id: "v".to_owned(),
            bytes: Some(bytes.to_vec()),
            error: None,
        }
    }

    #[test]
    fn equal_targets_pass() {
        let left = (Target::Native, observation(b"a"));
        let right = (Target::Browser, observation(b"a"));
        assert!(compare_targets(&[left, right]).is_ok());
    }

    #[test]
    fn different_targets_fail() {
        let left = (Target::Native, observation(b"a"));
        let right = (Target::Browser, observation(b"b"));
        let differences = compare_targets(&[left, right]).expect_err("the bytes differ");
        assert_eq!(differences.len(), 1);
    }

    #[test]
    fn one_named_error_matches() {
        let value = observation(b"a");
        assert!(compare_expected(&value, &Expected::Bytes(b"a".to_vec())).is_ok());
        assert!(compare_expected(&value, &Expected::Error("x".to_owned())).is_err());
    }

    #[test]
    fn a_major_version_change_needs_a_named_error() {
        let old = observation(b"a");
        let mut new = observation(b"b");
        assert!(compare_versions("v", &old, &new).is_err());
        new.error = Some("format.unsupported_version".to_owned());
        new.bytes = None;
        assert!(compare_versions("v", &old, &new).is_ok());
    }

    #[test]
    fn an_empty_run_list_passes() {
        assert!(compare_scenario_targets(&[]).is_ok());
    }
}
