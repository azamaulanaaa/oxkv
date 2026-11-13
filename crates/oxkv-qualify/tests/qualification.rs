//! End-to-end qualification tests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use oxkv_qualify::differential::compare_scenario_targets;
use oxkv_qualify::fault::FaultKind;
use oxkv_qualify::scenario::{ScenarioKind, default_cases, run_case};
use oxkv_qualify::suite::Suite;
use oxkv_qualify::target::Target;

#[test]
fn the_full_matrix_passes() {
    let outcome = Suite::new().run();
    assert!(
        outcome.passed(),
        "the suite failed:\n{}\ndifferences: {:?}\nfuzz: {:?}",
        outcome.report.render(),
        outcome.differences,
        outcome.fuzz.failures
    );
    assert_eq!(outcome.report.case_count(), default_cases().len());
    assert_eq!(outcome.report.target_count(), 3);
    assert_eq!(outcome.report.run_count(), default_cases().len() * 3);
    assert_eq!(outcome.report.failed_count(), 0);
    assert_ne!(outcome.vector_observations.len(), 0);
}

#[test]
fn every_scenario_and_fault_runs() {
    let outcome = Suite::new().run();
    let scenarios: BTreeSet<ScenarioKind> = outcome.runs.iter().map(|run| run.scenario).collect();
    for scenario in ScenarioKind::ALL {
        assert!(
            scenarios.contains(&scenario),
            "the scenario {scenario} did not run"
        );
    }
    let faults = outcome.report.faults();
    for fault in FaultKind::ALL {
        assert!(faults.contains(&fault), "the fault {fault} did not run");
    }
}

#[test]
fn the_same_case_has_one_trace_on_every_target() {
    let outcome = Suite::new().run();
    let mut by_case: BTreeMap<&str, Vec<_>> = BTreeMap::new();
    for run in &outcome.runs {
        by_case.entry(run.case_id).or_default().push(run.clone());
    }
    for (case, runs) in by_case {
        assert_eq!(
            runs.len(),
            Target::ALL.len(),
            "the case {case} has a bad target count"
        );
        if let Err(differences) = compare_scenario_targets(&runs) {
            panic!("the case {case} differs between targets: {differences:?}");
        }
        let first = runs[0].trace.digest();
        for run in &runs[1..] {
            assert_eq!(
                run.trace.digest(),
                first,
                "the case {case} has a target-specific trace"
            );
        }
    }
}

#[test]
fn the_report_and_narration_share_one_trace() {
    let outcome = Suite::new().run();
    assert_eq!(
        outcome.narration.trace_digest,
        outcome.report.trace_digest()
    );
    assert!(
        outcome
            .narration
            .text()
            .contains(outcome.report.trace_digest())
    );
    assert!(
        outcome
            .report
            .render()
            .contains(outcome.report.trace_digest())
    );
}

#[test]
fn a_different_trace_fails_the_comparator() {
    let case = default_cases()
        .into_iter()
        .find(|case| case.scenario == ScenarioKind::NativeBrowserNative)
        .expect("the case exists");
    let native = run_case(&case, Target::Native);
    let mut browser = native.clone();
    browser.target = Target::Browser;
    assert!(compare_scenario_targets(&[native.clone(), browser]).is_ok());

    let mut bad = native.clone();
    bad.trace.record("extra", &[]);
    assert!(compare_scenario_targets(&[native, bad]).is_err());
}

#[test]
fn the_native_source_has_no_target_conditional() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_files(&root, &mut files);
    assert_ne!(files.len(), 0);
    for file in files {
        let text = std::fs::read_to_string(&file).expect("the source file is readable");
        for forbidden in [
            "cfg(target_arch",
            "cfg!(target_arch",
            "target_os",
            "std::env::var",
        ] {
            assert!(
                !text.contains(forbidden),
                "the file {} contains {forbidden}",
                file.display()
            );
        }
    }
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}
