//! The scenario matrix.
//!
//! A scenario is one definition. The matrix runs it on [`Target::Native`],
//! [`Target::Browser`], and [`Target::NativeAgain`]. No scenario reads a target
//! to decide a verdict or to shape a byte.

use std::collections::BTreeMap;

use oxkv_contracts::receipt::ReceiptState;
use oxkv_contracts::version::Hash32;

use crate::canonical::hex;
use crate::fault::{CommitStep, Fault, FaultKind, FaultPlan};
use crate::policy::{EvalBudget, FactValue, Verdict, evaluate};
use crate::rng::FixedSeedRng;
use crate::target::Target;
use crate::trace::NormalizedTrace;
use crate::world::{Artifact, Snapshot, WireCursor, World, WriteOutcome};

/// The fixed seed for the default suite.
pub const FIXED_SEED: u64 = 0x4f58_4b56_5155_414c;

/// One scenario definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScenarioKind {
    /// S1. Native to browser to native.
    NativeBrowserNative,
    /// S2. Disconnect and resume.
    DisconnectResume,
    /// S3. Conditional-write conflict.
    Conflict,
    /// S4. Rule change and rollback.
    RuleChange,
    /// S5. Snapshot restore with a corrupt page.
    SnapshotRestore,
    /// S6. Format upgrade.
    FormatUpgrade,
    /// F1. Crash at one commit step.
    CommitCrash,
}

impl ScenarioKind {
    /// Every scenario definition.
    pub const ALL: [Self; 7] = [
        Self::NativeBrowserNative,
        Self::DisconnectResume,
        Self::Conflict,
        Self::RuleChange,
        Self::SnapshotRestore,
        Self::FormatUpgrade,
        Self::CommitCrash,
    ];

    /// Returns the canonical scenario id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::NativeBrowserNative => "S1",
            Self::DisconnectResume => "S2",
            Self::Conflict => "S3",
            Self::RuleChange => "S4",
            Self::SnapshotRestore => "S5",
            Self::FormatUpgrade => "S6",
            Self::CommitCrash => "F1",
        }
    }

    /// Returns the canonical scenario name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NativeBrowserNative => "native-browser-native",
            Self::DisconnectResume => "disconnect-resume",
            Self::Conflict => "conflict",
            Self::RuleChange => "rule-change",
            Self::SnapshotRestore => "snapshot-restore",
            Self::FormatUpgrade => "format-upgrade",
            Self::CommitCrash => "commit-crash",
        }
    }
}

impl core::fmt::Display for ScenarioKind {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.id())
    }
}

/// One case in the matrix.
#[derive(Debug, Clone)]
pub struct ScenarioCase {
    /// The unique case id.
    pub id: &'static str,
    /// The scenario definition.
    pub scenario: ScenarioKind,
    /// The injected faults.
    pub faults: FaultPlan,
    /// A short description for the report.
    pub description: &'static str,
}

impl ScenarioCase {
    fn new(
        id: &'static str,
        scenario: ScenarioKind,
        faults: FaultPlan,
        description: &'static str,
    ) -> Self {
        Self {
            id,
            scenario,
            faults,
            description,
        }
    }

    /// Returns the fault kinds in this case.
    #[must_use]
    pub fn fault_kinds(&self) -> Vec<FaultKind> {
        self.faults
            .faults()
            .iter()
            .map(|fault| fault.kind())
            .collect()
    }
}

/// Returns the default case list.
///
/// The list covers every scenario and every fault kind. A later contract adds
/// a case. It does not edit a released vector.
#[must_use]
pub fn default_cases() -> Vec<ScenarioCase> {
    let mut cases = Vec::new();
    cases.push(ScenarioCase::new(
        "S1-round-trip",
        ScenarioKind::NativeBrowserNative,
        FaultPlan::new(),
        "Export, load, export, load, and compare bytes.",
    ));
    let mut cut_and_duplicate = FaultPlan::new();
    cut_and_duplicate.push(Fault::LinkCut { after_frame: 1 });
    cut_and_duplicate.push(Fault::DuplicateDelivery {
        frame_index: 1,
        copies: 1,
    });
    cases.push(ScenarioCase::new(
        "S2-link-cut",
        ScenarioKind::DisconnectResume,
        cut_and_duplicate,
        "Cut the link after frame 1 and resume from a cursor.",
    ));
    cases.push(ScenarioCase::new(
        "S2-cursor-expiry",
        ScenarioKind::DisconnectResume,
        FaultPlan::with(Fault::CursorExpiry { after_frames: 2 }),
        "Expire a cursor and fall back to a snapshot.",
    ));
    cases.push(ScenarioCase::new(
        "S2-truncated-frame",
        ScenarioKind::DisconnectResume,
        FaultPlan::with(Fault::TruncatedFrame { frame_index: 1 }),
        "Truncate frame 1 and request it again.",
    ));
    cases.push(ScenarioCase::new(
        "S3-conflict",
        ScenarioKind::Conflict,
        FaultPlan::new(),
        "Two writers use base version 1. Expect Conflict.",
    ));
    cases.push(ScenarioCase::new(
        "S3-conditional-loss",
        ScenarioKind::Conflict,
        FaultPlan::with(Fault::ConditionalWriteLoss { write_index: 2 }),
        "Lose the conditional header and refuse the write.",
    ));
    cases.push(ScenarioCase::new(
        "S4-rule-change",
        ScenarioKind::RuleChange,
        FaultPlan::new(),
        "Pin v1, stage v2, and roll back to v1.",
    ));
    cases.push(ScenarioCase::new(
        "S5-restore",
        ScenarioKind::SnapshotRestore,
        FaultPlan::new(),
        "Restore a clean snapshot.",
    ));
    cases.push(ScenarioCase::new(
        "S5-corrupt-page",
        ScenarioKind::SnapshotRestore,
        FaultPlan::with(Fault::CorruptPage { page_sequence: 0 }),
        "Corrupt page 0. Expect format.corrupt_page.",
    ));
    cases.push(ScenarioCase::new(
        "S6-format-upgrade",
        ScenarioKind::FormatUpgrade,
        FaultPlan::new(),
        "Open a v1 artifact and reject a v2 artifact.",
    ));
    for step in CommitStep::ALL {
        let id = match step {
            CommitStep::Validate => "F1-crash-validate",
            CommitStep::Apply => "F1-crash-apply",
            CommitStep::Receipt => "F1-crash-receipt",
            CommitStep::Audit => "F1-crash-audit",
            CommitStep::Index => "F1-crash-index",
            CommitStep::Publish => "F1-crash-publish",
        };
        cases.push(ScenarioCase::new(
            id,
            ScenarioKind::CommitCrash,
            FaultPlan::with(Fault::CrashAtCommit { step }),
            "Crash at one commit step and publish one intact generation.",
        ));
    }
    cases
}

/// One run of one case on one target.
#[derive(Debug, Clone)]
pub struct ScenarioRun {
    /// The case id.
    pub case_id: &'static str,
    /// The scenario definition.
    pub scenario: ScenarioKind,
    /// The target label.
    pub target: Target,
    /// The injected fault kinds.
    pub faults: Vec<FaultKind>,
    /// The normalized trace.
    pub trace: NormalizedTrace,
    /// True when the scenario oracle passed.
    pub passed: bool,
    /// The failure text, when the oracle failed.
    pub failure: Option<String>,
}

impl ScenarioRun {
    /// Returns the trace digest as lowercase hexadecimal text.
    #[must_use]
    pub fn trace_digest_hex(&self) -> String {
        self.trace.digest_hex()
    }
}

/// Runs one case on one target.
#[must_use]
pub fn run_case(case: &ScenarioCase, target: Target) -> ScenarioRun {
    let mut trace = NormalizedTrace::new();
    trace.record(
        "run.start",
        &[
            ("case", case.id.to_owned()),
            ("scenario", case.scenario.id().to_owned()),
            ("seed", FIXED_SEED.to_string()),
            ("fault_count", case.faults.faults().len().to_string()),
        ],
    );
    for fault in case.faults.faults() {
        trace.record("fault.injected", &[("fault", fault.describe())]);
    }

    let result = match case.scenario {
        ScenarioKind::NativeBrowserNative => native_browser_native(&mut trace),
        ScenarioKind::DisconnectResume => disconnect_resume(&mut trace, &case.faults),
        ScenarioKind::Conflict => conflict(&mut trace, &case.faults),
        ScenarioKind::RuleChange => rule_change(&mut trace),
        ScenarioKind::SnapshotRestore => snapshot_restore(&mut trace, &case.faults),
        ScenarioKind::FormatUpgrade => format_upgrade(&mut trace),
        ScenarioKind::CommitCrash => commit_crash(&mut trace, &case.faults),
    };

    let passed = result.is_ok();
    if let Err(reason) = &result {
        trace.record("run.failed", &[("reason", reason.clone())]);
    } else {
        let digest = trace.digest_hex();
        trace.record("run.passed", &[("digest", digest)]);
    }

    ScenarioRun {
        case_id: case.id,
        scenario: case.scenario,
        target,
        faults: case.fault_kinds(),
        trace,
        passed,
        failure: result.err(),
    }
}

type ScenarioResult = Result<(), String>;

fn write(
    world: &mut World,
    trace: &mut NormalizedTrace,
    index: u64,
    key: &[u8],
    value: &[u8],
    base: Option<u64>,
) {
    let outcome = world.commit_write(index, key, value, base, &FaultPlan::new());
    record_write(trace, &outcome, key);
}

fn record_write(trace: &mut NormalizedTrace, outcome: &WriteOutcome, key: &[u8]) {
    let mut fields = vec![("key", hex(key)), ("outcome", outcome.name().to_owned())];
    if let Some(code) = outcome.error_code() {
        fields.push(("error", code.to_owned()));
    }
    match outcome {
        WriteOutcome::Committed {
            commit_version,
            key_version,
            ..
        } => {
            fields.push(("commit_version", commit_version.to_string()));
            fields.push(("key_version", key_version.to_string()));
        }
        WriteOutcome::Conflict {
            current_key_version,
            ..
        } => {
            fields.push(("current_key_version", current_key_version.to_string()));
        }
        WriteOutcome::Crashed { step, .. } => {
            fields.push(("step", step.name().to_owned()));
        }
        WriteOutcome::Rejected { .. } => {}
    }
    trace.record("write", &fields);
}

fn native_browser_native(trace: &mut NormalizedTrace) -> ScenarioResult {
    let mut authority = World::new();
    let mut rng = FixedSeedRng::new(FIXED_SEED);
    for index in 0..4_u64 {
        let key = format!("accounts/{index}").into_bytes();
        let value = format!("value-{}-{index}", rng.next_bounded(1000)).into_bytes();
        write(&mut authority, trace, index, &key, &value, None);
    }

    let first = authority.export();
    trace.record("export", &[("digest", hex(Hash32::of(&first).as_bytes()))]);

    let mut browser = World::import(&first).map_err(str::to_owned)?;
    let second = browser.export();
    trace.record(
        "import",
        &[
            ("target", "browser".to_owned()),
            ("digest", hex(Hash32::of(&second).as_bytes())),
        ],
    );

    let native_again = World::import(&second).map_err(str::to_owned)?;
    let third = native_again.export();
    trace.record(
        "import",
        &[
            ("target", "native-again".to_owned()),
            ("digest", hex(Hash32::of(&third).as_bytes())),
        ],
    );

    if first != second || second != third {
        return Err("the export bytes differ between targets".to_owned());
    }
    if authority.rows != browser.rows || authority.rows != native_again.rows {
        return Err("a loaded row differs from the authority row".to_owned());
    }
    browser.rebuild_index();
    if browser.index != authority.index {
        return Err("the loaded index differs from the authority index".to_owned());
    }
    trace.record(
        "compare",
        &[
            ("equal", "true".to_owned()),
            ("bytes", first.len().to_string()),
        ],
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn disconnect_resume(trace: &mut NormalizedTrace, plan: &FaultPlan) -> ScenarioResult {
    let mut authority = World::new();
    for index in 0..4_u64 {
        let key = format!("stream/{index}").into_bytes();
        let value = format!("frame-{index}").into_bytes();
        write(&mut authority, trace, index, &key, &value, None);
    }
    let frames = authority.frames_after(0);
    let cut = plan.link_cut_after().map_or(frames.len(), |after| {
        usize::try_from(after.saturating_add(1)).unwrap_or(usize::MAX)
    });
    let cut = cut.min(frames.len());

    let mut replica = World::new();
    let mut applied_indices = Vec::new();
    let mut duplicates = 0_u32;
    let mut fallback = false;
    for frame in frames.iter().take(cut) {
        let encoded = frame.encode();
        if plan.truncated_frame() == Some(frame.index) {
            let short = &encoded[..encoded.len().saturating_sub(1)];
            let error = crate::world::WireFrame::decode(short)
                .err()
                .unwrap_or("no error");
            if error != "sync.cursor_invalid" {
                return Err(format!("the truncated frame returned {error}"));
            }
            trace.record(
                "frame.truncated",
                &[
                    ("frame", frame.index.to_string()),
                    ("error", error.to_owned()),
                ],
            );
        }
        let decoded = crate::world::WireFrame::decode(&encoded)
            .map_err(|code| format!("the frame decoder returned {code}"))?;
        replica.apply_frame(&decoded);
        applied_indices.push(decoded.index);
        let copies = plan.duplicate_copies(frame.index);
        for _ in 0..copies {
            replica.apply_frame(&decoded);
            duplicates = duplicates.saturating_add(1);
            trace.record("frame.duplicate", &[("frame", frame.index.to_string())]);
        }
    }
    trace.record(
        "link.applied",
        &[
            ("frames", applied_indices.len().to_string()),
            ("duplicates", duplicates.to_string()),
        ],
    );

    let mut applied = u64::try_from(cut).unwrap_or(u64::MAX);
    if cut < frames.len() {
        let cursor = WireCursor::new(1, applied, 0);
        let encoded = cursor.encode();
        let decoded = WireCursor::decode(&encoded)
            .map_err(|code| format!("the cursor decoder returned {code}"))?;
        trace.record(
            "resume",
            &[("cursor", decoded.describe()), ("error", "none".to_owned())],
        );
        if plan.cursor_expires_after(applied) {
            trace.record(
                "cursor.expired",
                &[("error", "sync.cursor_expired".to_owned())],
            );
            let snapshot = Snapshot::build(&authority, 32);
            snapshot.restore_into(&mut replica).map_err(str::to_owned)?;
            applied = u64::try_from(frames.len()).unwrap_or(u64::MAX);
            fallback = true;
            trace.record("snapshot.fallback", &[("rows", replica.len().to_string())]);
        } else {
            for frame in frames.iter().skip(cut) {
                replica.apply_frame(frame);
                applied_indices.push(frame.index);
                applied = applied.saturating_add(1);
            }
            trace.record("resume.applied", &[("frames", applied.to_string())]);
        }
    } else if plan.cursor_expires_after(applied) {
        trace.record(
            "cursor.expired",
            &[("error", "sync.cursor_expired".to_owned())],
        );
        let snapshot = Snapshot::build(&authority, 32);
        snapshot.restore_into(&mut replica).map_err(str::to_owned)?;
        applied = u64::try_from(frames.len()).unwrap_or(u64::MAX);
        fallback = true;
        trace.record("snapshot.fallback", &[("rows", replica.len().to_string())]);
    }

    if replica.rows != authority.rows {
        return Err("the replica rows differ from the authority rows".to_owned());
    }
    if applied < u64::try_from(frames.len()).unwrap_or(u64::MAX) {
        return Err("the replica did not reach the end of the frame stream".to_owned());
    }
    let expected: Vec<u64> = frames.iter().map(|frame| frame.index).collect();
    let mut contiguous = applied_indices;
    contiguous.sort_unstable();
    contiguous.dedup();
    if !fallback && contiguous != expected {
        return Err("the applied frame stream has a gap".to_owned());
    }
    let duplicate_budget: u32 = (0..u64::try_from(frames.len()).unwrap_or(u64::MAX))
        .map(|index| u32::from(plan.duplicate_copies(index)))
        .sum();
    if duplicates > duplicate_budget {
        return Err("the duplicate count exceeds the at-least-once bound".to_owned());
    }
    trace.record(
        "resume.complete",
        &[
            ("frames", applied.to_string()),
            ("duplicates", duplicates.to_string()),
            ("fallback", fallback.to_string()),
        ],
    );
    Ok(())
}

fn conflict(trace: &mut NormalizedTrace, plan: &FaultPlan) -> ScenarioResult {
    let mut world = World::new();
    let key = b"account/1";
    write(&mut world, trace, 0, key, b"v1", None);
    let base = world.row(key).map(|row| row.key_version);
    write(&mut world, trace, 1, key, b"v2", base);

    let outcome = world.commit_write(2, key, b"v3", base, plan);
    record_write(trace, &outcome, key);
    let row = world.row(key).ok_or("the key is absent")?;
    if row.value != b"v2" {
        return Err("the losing writer overwrote the row".to_owned());
    }
    match &outcome {
        WriteOutcome::Conflict {
            current_key_version,
            ..
        } => {
            if *current_key_version != 2 {
                return Err("the conflict does not name key version 2".to_owned());
            }
        }
        WriteOutcome::Rejected { code, .. } if *code == "write.missing_base_version" => {
            trace.record(
                "conditional.loss",
                &[
                    ("error", (*code).to_owned()),
                    ("overwrite", "false".to_owned()),
                ],
            );
        }
        other => {
            return Err(format!(
                "the losing writer returned {} instead of Conflict",
                other.name()
            ));
        }
    }
    trace.record(
        "conflict",
        &[
            ("current_key_version", "2".to_owned()),
            ("value", "v2".to_owned()),
        ],
    );
    Ok(())
}

fn rule_change(trace: &mut NormalizedTrace) -> ScenarioResult {
    let mut world = World::new();
    world.policy_versions.insert(2, "v2".to_owned());
    let mut facts = BTreeMap::new();
    facts.insert("tier".to_owned(), FactValue::Text("gold".to_owned()));
    facts.insert("region".to_owned(), FactValue::Text("us".to_owned()));
    let pinned_sample = facts.clone();

    let before_audit = world.audit.len();
    let v1 = evaluate("eq(tier,\"gold\")", &pinned_sample, EvalBudget::default())
        .map_err(|error| error.code().to_owned())?;
    trace.record(
        "policy.pinned",
        &[
            ("policy_version", "1".to_owned()),
            ("verdict", v1.verdict.name().to_owned()),
        ],
    );
    if v1.verdict != Verdict::Allow {
        return Err("the pinned v1 policy does not allow the sample".to_owned());
    }

    let dry_run_v2 = evaluate(
        "and(eq(tier,\"gold\"),eq(region,\"eu\"))",
        &pinned_sample,
        EvalBudget::default(),
    )
    .map_err(|error| error.code().to_owned())?;
    let dry_run_again = evaluate(
        "and(eq(tier,\"gold\"),eq(region,\"eu\"))",
        &pinned_sample,
        EvalBudget::default(),
    )
    .map_err(|error| error.code().to_owned())?;
    if dry_run_v2 != dry_run_again {
        return Err("the dry run is not deterministic".to_owned());
    }
    world.audit.push(Hash32::of(b"rule-change:v2"));
    world.active_policy = 2;
    trace.record(
        "rule.stage",
        &[
            ("policy_version", "2".to_owned()),
            ("receipt", ReceiptState::Committed.name().to_owned()),
            ("verdict", dry_run_v2.verdict.name().to_owned()),
        ],
    );
    let after_stage_audit = world.audit.len();
    if after_stage_audit.saturating_sub(before_audit) != 1 {
        return Err("the rule change did not write one audit record".to_owned());
    }

    world.audit.push(Hash32::of(b"rule-rollback:v1"));
    world.active_policy = 1;
    let after_rollback_audit = world.audit.len();
    if after_rollback_audit.saturating_sub(after_stage_audit) != 1 {
        return Err("the rollback did not write one audit record".to_owned());
    }
    let v1_after = evaluate("eq(tier,\"gold\")", &pinned_sample, EvalBudget::default())
        .map_err(|error| error.code().to_owned())?;
    if v1_after.verdict != v1.verdict {
        return Err("the pinned verdict changed after the rollback".to_owned());
    }
    trace.record(
        "rule.rollback",
        &[
            ("policy_version", "1".to_owned()),
            ("audit_records", world.audit.len().to_string()),
            ("verdict", v1_after.verdict.name().to_owned()),
        ],
    );
    Ok(())
}

fn snapshot_restore(trace: &mut NormalizedTrace, plan: &FaultPlan) -> ScenarioResult {
    let mut authority = World::new();
    for index in 0..4_u64 {
        let key = format!("page/{index}").into_bytes();
        let value = format!("row-{index}").into_bytes();
        write(&mut authority, trace, index, &key, &value, None);
    }
    let mut snapshot = Snapshot::build(&authority, 32);
    if let Some(page) = plan.corrupt_page() {
        snapshot = snapshot.corrupt(page);
        trace.record("snapshot.corrupt", &[("page", page.to_string())]);
    }

    let mut restored = World::new();
    match snapshot.restore_into(&mut restored) {
        Ok(()) => {
            trace.record(
                "snapshot.restore",
                &[
                    ("outcome", "ok".to_owned()),
                    ("rows", restored.len().to_string()),
                ],
            );
            if restored.rows != authority.rows {
                return Err("the restored rows differ from the authority rows".to_owned());
            }
            if plan.corrupt_page().is_some() {
                return Err("the corrupt snapshot restored without an error".to_owned());
            }
            return Ok(());
        }
        Err(code) => {
            trace.record(
                "snapshot.restore",
                &[
                    ("outcome", "error".to_owned()),
                    ("error", code.to_owned()),
                    ("rows", restored.len().to_string()),
                ],
            );
            if plan.corrupt_page().is_none() {
                return Err(format!("the clean snapshot returned {code}"));
            }
            if code != "format.corrupt_page" {
                return Err(format!("the corrupt snapshot returned {code}"));
            }
            if restored.len() != 0 {
                return Err("the failed restore exposed partial state".to_owned());
            }
        }
    }

    let clean = Snapshot::build(&authority, 32);
    clean.restore_into(&mut restored).map_err(str::to_owned)?;
    if restored.rows != authority.rows {
        return Err("the clean restore differs from the authority rows".to_owned());
    }
    trace.record(
        "snapshot.restore",
        &[
            ("outcome", "ok".to_owned()),
            ("rows", restored.len().to_string()),
        ],
    );
    Ok(())
}

fn format_upgrade(trace: &mut NormalizedTrace) -> ScenarioResult {
    let old = Artifact {
        format_major: 1,
        format_minor: 0,
        manifest_digest: Some(*Hash32::of(b"manifest").as_bytes()),
        rows: vec![
            (b"format/key".to_vec(), b"recorded".to_vec(), 3),
            (b"format/other".to_vec(), b"value".to_vec(), 1),
        ],
    };
    let decoded = Artifact::decode(&old.encode()).map_err(str::to_owned)?;
    if decoded.rows != old.rows {
        return Err("the old artifact did not keep its recorded values".to_owned());
    }
    if decoded.format_minor != 0 {
        return Err("the old artifact did not keep its format version".to_owned());
    }
    trace.record(
        "artifact.open",
        &[
            ("format", "1.0".to_owned()),
            ("rows", decoded.rows.len().to_string()),
        ],
    );

    let newer = Artifact {
        format_major: 2,
        format_minor: 0,
        manifest_digest: Some(*Hash32::of(b"manifest").as_bytes()),
        rows: Vec::new(),
    };
    let error = Artifact::decode(&newer.encode()).err().unwrap_or("none");
    if error != "format.unsupported_version" {
        return Err(format!("the newer artifact returned {error}"));
    }
    trace.record(
        "artifact.reject",
        &[("format", "2.0".to_owned()), ("error", error.to_owned())],
    );

    let missing = Artifact {
        format_major: 1,
        format_minor: 0,
        manifest_digest: None,
        rows: Vec::new(),
    };
    let decoded = Artifact::decode(&missing.encode()).map_err(str::to_owned)?;
    let error = decoded.require_provenance().err().unwrap_or("none");
    if error != "format.missing_provenance" {
        return Err(format!("the artifact without provenance returned {error}"));
    }
    trace.record(
        "artifact.reject",
        &[
            ("provenance", "missing".to_owned()),
            ("error", error.to_owned()),
        ],
    );
    Ok(())
}

fn commit_crash(trace: &mut NormalizedTrace, plan: &FaultPlan) -> ScenarioResult {
    let mut world = World::new();
    write(&mut world, trace, 0, b"crash/key", b"stable", None);
    let generation = world.generation;
    let rows = world.rows.clone();
    let audit = world.audit.clone();
    let receipts = world.receipts.clone();

    let outcome = world.commit_write(1, b"crash/key", b"unstable", Some(1), plan);
    record_write(trace, &outcome, b"crash/key");
    let step = match &outcome {
        WriteOutcome::Crashed { step, .. } => *step,
        other => {
            return Err(format!(
                "the crash case returned {} instead of Crashed",
                other.name()
            ));
        }
    };
    trace.record(
        "crash",
        &[
            ("step", step.name().to_owned()),
            ("generation", generation.to_string()),
        ],
    );

    world.rebuild_index();
    if world.generation != generation {
        return Err("the crash published a second generation".to_owned());
    }
    if world.rows != rows || world.audit != audit || world.receipts != receipts {
        return Err("the crash exposed partial state".to_owned());
    }
    trace.record(
        "recovery",
        &[
            ("generation", world.generation.to_string()),
            ("rows", world.rows.len().to_string()),
            ("audit", world.audit.len().to_string()),
        ],
    );
    Ok(())
}
