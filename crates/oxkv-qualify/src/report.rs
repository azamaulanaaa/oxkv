//! One report and one narration from one normalized trace.
//!
//! The report holds the pass or fail result for each run. The narration tells
//! the same story in short sentences. Both read the same native runs.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use oxkv_contracts::version::Hash32;

use crate::fault::FaultKind;
use crate::scenario::ScenarioRun;
use crate::target::Target;
use crate::trace::NormalizedTrace;

/// A qualification report.
#[derive(Debug, Clone)]
pub struct Report {
    runs: Vec<ScenarioRun>,
    case_count: usize,
    target_count: usize,
    trace_digest: String,
    native_traces: Vec<NormalizedTrace>,
}

impl Report {
    /// Builds a report from every run.
    #[must_use]
    pub fn from_runs(runs: Vec<ScenarioRun>) -> Self {
        let mut native: Vec<&ScenarioRun> = runs
            .iter()
            .filter(|run| run.target == Target::Native)
            .collect();
        native.sort_by_key(|run| run.case_id);
        let native_traces: Vec<NormalizedTrace> =
            native.iter().map(|run| run.trace.clone()).collect();
        let digest = trace_set_digest(&native_traces);
        let cases: BTreeSet<&str> = runs.iter().map(|run| run.case_id).collect();
        let targets: BTreeSet<Target> = runs.iter().map(|run| run.target).collect();
        Self {
            case_count: cases.len(),
            target_count: targets.len(),
            trace_digest: crate::canonical::hex(digest.as_bytes()),
            native_traces,
            runs,
        }
    }

    /// Returns every run.
    #[must_use]
    pub fn runs(&self) -> &[ScenarioRun] {
        &self.runs
    }

    /// Returns the number of cases.
    #[must_use]
    pub const fn case_count(&self) -> usize {
        self.case_count
    }

    /// Returns the number of targets.
    #[must_use]
    pub const fn target_count(&self) -> usize {
        self.target_count
    }

    /// Returns the number of runs.
    #[must_use]
    pub fn run_count(&self) -> usize {
        self.runs.len()
    }

    /// Returns the number of failed runs.
    #[must_use]
    pub fn failed_count(&self) -> usize {
        self.runs.iter().filter(|run| !run.passed).count()
    }

    /// Returns true when every run passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failed_count() == 0
    }

    /// Returns the unified trace digest.
    #[must_use]
    pub fn trace_digest(&self) -> &str {
        &self.trace_digest
    }

    /// Returns the fault kinds that the suite injected.
    #[must_use]
    pub fn faults(&self) -> BTreeSet<FaultKind> {
        self.runs
            .iter()
            .flat_map(|run| run.faults.iter().copied())
            .collect()
    }

    /// Renders the report as Markdown.
    #[must_use]
    pub fn render(&self) -> String {
        let mut text = String::new();
        text.push_str("# oxkv qualification report\n\n");
        let _ = writeln!(text, "- Cases: {}", self.case_count);
        let _ = writeln!(text, "- Targets: {}", self.target_count);
        let _ = writeln!(text, "- Runs: {}", self.run_count());
        let _ = writeln!(text, "- Failed: {}", self.failed_count());
        let _ = writeln!(text, "- Trace digest: `{}`", self.trace_digest);
        text.push_str("\n## Cases\n\n");
        text.push_str("| Case | Scenario | Target | Result | Trace |\n");
        text.push_str("| --- | --- | --- | --- | --- |\n");
        for run in &self.runs {
            let _ = writeln!(
                text,
                "| {} | {} | {} | {} | `{}` |",
                run.case_id,
                run.scenario.id(),
                run.target.name(),
                if run.passed { "pass" } else { "fail" },
                run.trace_digest_hex()
            );
        }
        if self.failed_count() > 0 {
            text.push_str("\n## Failures\n\n");
            for run in self.runs.iter().filter(|run| !run.passed) {
                let _ = writeln!(
                    text,
                    "- {} on {}: {}",
                    run.case_id,
                    run.target.name(),
                    run.failure.as_deref().unwrap_or("no reason")
                );
            }
        }
        text
    }

    /// Builds the narration from the same native traces as the report.
    #[must_use]
    pub fn narration(&self) -> Narration {
        let mut text = String::new();
        let _ = writeln!(
            text,
            "The qualification suite ran {} cases on {} targets.",
            self.case_count, self.target_count
        );
        let _ = writeln!(
            text,
            "{} runs finished. {} runs failed.",
            self.run_count(),
            self.failed_count()
        );
        for trace in &self.native_traces {
            let case = trace
                .steps()
                .iter()
                .find(|step| step.operation() == "run.start")
                .and_then(|step| step.field("case"))
                .unwrap_or("unknown");
            let _ = writeln!(text, "The case {case} ran on native.");
            for step in trace.steps() {
                if step.operation() == "run.passed" {
                    text.push_str("The oracle passed.\n");
                } else if step.operation() == "run.failed" {
                    let reason = step.field("reason").unwrap_or("no reason");
                    let _ = writeln!(text, "The oracle failed: {reason}.");
                }
            }
        }
        let _ = writeln!(text, "The unified trace digest is {}.", self.trace_digest);
        Narration {
            text,
            trace_digest: self.trace_digest.clone(),
        }
    }
}

/// The narration for one report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Narration {
    /// The narration text.
    pub text: String,
    /// The trace digest that the narration describes.
    pub trace_digest: String,
}

impl Narration {
    /// Returns the narration text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

fn trace_set_digest(traces: &[NormalizedTrace]) -> Hash32 {
    let mut bytes = Vec::new();
    for trace in traces {
        bytes.extend_from_slice(&trace.canonical_bytes());
    }
    Hash32::of(&bytes)
}
