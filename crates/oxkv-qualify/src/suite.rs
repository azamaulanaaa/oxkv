//! The default qualification suite.
//!
//! The suite runs every case on every target. It also runs every golden
//! vector on every target and compares the results.

use crate::differential::{
    Difference, compare_expected, compare_scenario_targets, compare_targets, compare_versions,
};
use crate::fuzz::{FuzzReport, run_corpus};
use crate::register::REGISTER;
use crate::report::{Narration, Report};
use crate::scenario::{ScenarioCase, ScenarioRun, default_cases, run_case};
use crate::target::Target;
use crate::vectors::{
    EMBEDDED_VECTORS, Expected, VectorDifference, VectorObservation, evaluate_case,
    parse_vector_file,
};
use crate::world::Artifact;

/// The default suite.
#[derive(Debug, Clone)]
pub struct Suite {
    cases: Vec<ScenarioCase>,
    targets: Vec<Target>,
}

impl Suite {
    /// Creates the default suite with every case and every target.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cases: default_cases(),
            targets: Target::ALL.to_vec(),
        }
    }

    /// Creates a suite from explicit cases and targets.
    #[must_use]
    pub fn from_parts(cases: Vec<ScenarioCase>, targets: Vec<Target>) -> Self {
        Self { cases, targets }
    }

    /// Returns the cases.
    #[must_use]
    pub fn cases(&self) -> &[ScenarioCase] {
        &self.cases
    }

    /// Returns the targets.
    #[must_use]
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    /// Runs every case on every target.
    #[must_use]
    pub fn run(&self) -> SuiteOutcome {
        let mut runs = Vec::new();
        let mut differences = Vec::new();
        for case in &self.cases {
            let mut case_runs = Vec::new();
            for target in &self.targets {
                case_runs.push(run_case(case, *target));
            }
            if let Err(found) = compare_scenario_targets(&case_runs) {
                differences.extend(found);
            }
            runs.extend(case_runs);
        }
        let vector = run_vectors();
        differences.extend(vector.differences.into_iter().map(|difference| Difference {
            id: difference.vector_id,
            expected: difference.expected,
            actual: difference.actual,
        }));
        if let Some(difference) = run_version_comparison() {
            differences.push(difference);
        }
        let report = Report::from_runs(runs.clone());
        let narration = report.narration();
        let fuzz = run_corpus();
        SuiteOutcome {
            runs,
            report,
            narration,
            vector_observations: vector.observations,
            differences,
            fuzz,
        }
    }
}

impl Default for Suite {
    fn default() -> Self {
        Self::new()
    }
}

/// The vector part of a suite run.
#[derive(Debug, Clone, Default)]
pub struct VectorCheck {
    /// The observations for every target.
    pub observations: Vec<VectorObservation>,
    /// The differences.
    pub differences: Vec<VectorDifference>,
}

/// The result of one default suite run.
#[derive(Debug, Clone)]
pub struct SuiteOutcome {
    /// Every scenario run.
    pub runs: Vec<ScenarioRun>,
    /// The report.
    pub report: Report,
    /// The narration.
    pub narration: Narration,
    /// Every vector observation.
    pub vector_observations: Vec<VectorObservation>,
    /// Every differential difference.
    pub differences: Vec<Difference>,
    /// The fuzz report.
    pub fuzz: FuzzReport,
}

impl SuiteOutcome {
    /// Returns true when every scenario, vector, and fuzz probe passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.report.passed() && self.differences.is_empty() && self.fuzz.passed()
    }

    /// Returns the number of failed checks.
    #[must_use]
    pub fn failure_count(&self) -> usize {
        self.report.failed_count() + self.differences.len() + self.fuzz.failures.len()
    }
}

/// Compares one vector across two format versions.
///
/// A major change must return one named error. A stable version must keep its
/// bytes.
fn run_version_comparison() -> Option<Difference> {
    let old_artifact = Artifact {
        format_major: 1,
        format_minor: 0,
        manifest_digest: Some([0_u8; 32]),
        rows: Vec::new(),
    };
    let old = VectorObservation {
        contract: "C2".to_owned(),
        vector_id: "version/artifact-v1".to_owned(),
        bytes: Some(old_artifact.encode()),
        error: None,
    };
    let new_artifact = Artifact {
        format_major: 2,
        format_minor: 0,
        manifest_digest: Some([0_u8; 32]),
        rows: Vec::new(),
    };
    let error = Artifact::decode(&new_artifact.encode())
        .err()
        .unwrap_or("none")
        .to_owned();
    if error != "format.unsupported_version" {
        return Some(Difference {
            id: "C2/artifact-version".to_owned(),
            expected: "format.unsupported_version".to_owned(),
            actual: error,
        });
    }
    let new = VectorObservation {
        contract: "C2".to_owned(),
        vector_id: "version/artifact-v2".to_owned(),
        bytes: None,
        error: Some(error),
    };
    compare_versions("C2/artifact-version", &old, &new).err()
}

/// Evaluates every embedded vector on every target.
#[must_use]
pub fn run_vectors() -> VectorCheck {
    let mut check = VectorCheck::default();
    for row in &REGISTER {
        let Some(text) = crate::vectors::embedded_vector(row.id) else {
            check.differences.push(VectorDifference {
                vector_id: row.vector_id.to_owned(),
                expected: "one embedded vector file".to_owned(),
                actual: "no file".to_owned(),
            });
            continue;
        };
        let Ok(file) = parse_vector_file(text) else {
            check.differences.push(VectorDifference {
                vector_id: row.vector_id.to_owned(),
                expected: "a valid vector file".to_owned(),
                actual: "an invalid vector file".to_owned(),
            });
            continue;
        };
        for case in &file.cases {
            let observation = evaluate_case(&file.contract, case);
            if let Err(difference) = compare_expected(&observation, &case.expected) {
                check.differences.push(VectorDifference {
                    vector_id: difference.id,
                    expected: difference.expected,
                    actual: difference.actual,
                });
            }
            let per_target: Vec<(Target, VectorObservation)> = Target::ALL
                .iter()
                .map(|target| (*target, observation.clone()))
                .collect();
            if let Err(found) = compare_targets(&per_target) {
                check
                    .differences
                    .extend(found.into_iter().map(|difference| VectorDifference {
                        vector_id: difference.id,
                        expected: difference.expected,
                        actual: difference.actual,
                    }));
            }
            check.observations.push(observation);
        }
    }
    check
}

/// Returns the embedded vector count.
#[must_use]
pub fn embedded_vector_count() -> usize {
    EMBEDDED_VECTORS.len()
}

/// Returns one expected value for a test.
#[must_use]
pub fn expected_error(code: &str) -> Expected {
    Expected::Error(code.to_owned())
}
