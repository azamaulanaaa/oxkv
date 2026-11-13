//! Fuzz entry points for every decoder and the policy evaluator.
//!
//! The harness is deterministic. It uses a fixed corpus and a fixed seed. It
//! checks totality, determinism, step monotonicity, the memory bound, and
//! target parity. It does not start a service and it does not use a network.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use oxkv_contracts::cursor::CursorType;
use oxkv_contracts::keyspace::TableName;
use oxkv_contracts::receipt::ReceiptState;

use crate::canonical::{digest, hex};
use crate::policy::{EvalBudget, FactValue, evaluate};
use crate::rng::FixedSeedRng;

/// The parity digest of the default corpus.
///
/// A native run and a wasm run must produce this value.
pub const PARITY_DIGEST: &str = "8a7d3e4059b4222e64fe7fe75a121e21f9ee3ec003ec9ee6683bf54fdddeec36";

/// The result of one fuzz corpus run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzReport {
    /// The number of probe calls.
    pub cases: u32,
    /// The failure texts.
    pub failures: Vec<String>,
    /// The digest of every observation.
    pub digest: String,
}

impl FuzzReport {
    /// Returns true when every probe passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Decodes one cursor token by type tag.
///
/// # Errors
///
/// Returns the canonical cursor error code for a bad token.
pub fn fuzz_cursor_token(type_tag: u8, bytes: &[u8]) -> Result<(), String> {
    let cursor_type =
        CursorType::from_tag(type_tag).ok_or_else(|| "sync.cursor_invalid".to_owned())?;
    crate::vectors::decode_cursor(cursor_type, bytes).map_err(str::to_owned)
}

/// Decodes one wire frame.
///
/// # Errors
///
/// Returns the canonical frame error code for a bad frame.
pub fn fuzz_frame(bytes: &[u8]) -> Result<u64, String> {
    crate::world::WireFrame::decode(bytes)
        .map(|frame| frame.index)
        .map_err(str::to_owned)
}

/// Decodes one artifact header.
///
/// # Errors
///
/// Returns the canonical format error code for a bad artifact.
pub fn fuzz_artifact(bytes: &[u8]) -> Result<String, String> {
    crate::world::Artifact::decode(bytes)
        .map(|artifact| format!("{}.{}", artifact.format_major, artifact.format_minor))
        .map_err(str::to_owned)
}

/// Decodes one exported snapshot.
///
/// # Errors
///
/// Returns the canonical format error code for a bad snapshot.
pub fn fuzz_snapshot(bytes: &[u8]) -> Result<String, String> {
    crate::world::World::import(bytes)
        .map(|world| format!("{}:{}", world.commit_version, world.generation))
        .map_err(str::to_owned)
}

/// Decodes one snapshot page.
///
/// # Errors
///
/// Returns the canonical format error code for a bad page.
pub fn fuzz_snapshot_page(bytes: &[u8]) -> Result<u64, String> {
    crate::world::SnapshotPage::decode(bytes)
        .map(|page| page.sequence)
        .map_err(str::to_owned)
}

/// Decodes one event cursor.
///
/// # Errors
///
/// Returns the canonical event error code for a bad cursor.
pub fn fuzz_event(bytes: &[u8]) -> Result<(), String> {
    crate::vectors::decode_cursor(CursorType::Event, bytes).map_err(str::to_owned)
}

/// Decodes one search cursor.
///
/// # Errors
///
/// Returns the canonical search error code for a bad cursor.
pub fn fuzz_search(bytes: &[u8]) -> Result<(), String> {
    crate::vectors::decode_cursor(CursorType::Search, bytes).map_err(str::to_owned)
}

/// Parses and evaluates one policy text.
///
/// # Errors
///
/// Returns the canonical policy error code for bad text or a failed
/// evaluation.
pub fn fuzz_policy(bytes: &[u8]) -> Result<String, String> {
    let text = core::str::from_utf8(bytes).map_err(|_| "policy.parse_error".to_owned())?;
    let mut facts = BTreeMap::new();
    facts.insert("tier".to_owned(), FactValue::Text("gold".to_owned()));
    facts.insert("age".to_owned(), FactValue::Int(7));
    let evaluation =
        evaluate(text, &facts, EvalBudget::default()).map_err(|error| error.code().to_owned())?;
    Ok(format!(
        "{}|{}|{}|{}",
        evaluation.verdict.name(),
        evaluation.stats.steps,
        evaluation.stats.depth,
        evaluation.stats.arena
    ))
}

/// Parses one table name.
///
/// # Errors
///
/// Returns the canonical keyspace error code for a bad name.
pub fn fuzz_table(bytes: &[u8]) -> Result<String, String> {
    let text = core::str::from_utf8(bytes).map_err(|_| "key.invalid_utf8".to_owned())?;
    TableName::parse(text)
        .map(|table| table.as_str().to_owned())
        .map_err(|error| error.code().to_owned())
}

/// Looks up one error code.
///
/// # Errors
///
/// Returns a harness error name for an unknown code.
pub fn fuzz_error_code(bytes: &[u8]) -> Result<&'static str, String> {
    let text = core::str::from_utf8(bytes).map_err(|_| "key.invalid_utf8".to_owned())?;
    oxkv_contracts::error::lookup(text)
        .map(|spec| spec.code)
        .ok_or_else(|| crate::vectors::HARNESS_UNKNOWN_VECTOR.to_owned())
}

/// Decodes one receipt wire tag.
///
/// # Errors
///
/// Returns `receipt.state_invalid` for an unknown tag.
pub fn fuzz_receipt(tag: u8) -> Result<&'static str, String> {
    ReceiptState::from_wire_tag(tag)
        .map(ReceiptState::name)
        .ok_or_else(|| "receipt.state_invalid".to_owned())
}

/// Runs the fixed fuzz corpus.
#[must_use]
pub fn run_corpus() -> FuzzReport {
    let mut context = FuzzContext::default();
    run_byte_probes(&mut context);
    run_policy_probes(&mut context);
    FuzzReport {
        cases: context.cases,
        failures: context.failures,
        digest: hex(digest("fuzz", context.transcript.as_bytes()).as_bytes()),
    }
}

#[derive(Default)]
struct FuzzContext {
    cases: u32,
    failures: Vec<String>,
    transcript: String,
}

impl FuzzContext {
    fn probe(&mut self, name: &str, passed: bool) {
        self.cases = self.cases.saturating_add(1);
        let _ = writeln!(self.transcript, "{name}=pass:{passed}");
        if !passed {
            self.failures.push(format!("the probe {name} failed"));
        }
    }

    fn note(&mut self, name: &str, value: &str) {
        let _ = writeln!(self.transcript, "{name}={value}");
    }
}

fn determinism<T: PartialEq + core::fmt::Debug>(
    context: &mut FuzzContext,
    name: &str,
    probe: impl Fn() -> T,
) {
    let first = probe();
    let second = probe();
    context.probe(name, first == second);
    context.note(name, &format!("{first:?}"));
}

fn run_byte_probes(context: &mut FuzzContext) {
    let mut rng = FixedSeedRng::new(crate::scenario::FIXED_SEED);
    for seed_index in 0..32_u64 {
        let length = usize::try_from(rng.next_bounded(65)).unwrap_or(0);
        let mut bytes = Vec::with_capacity(length);
        for _ in 0..length {
            bytes.push(u8::try_from(rng.next_bounded(256)).unwrap_or(0));
        }
        for tag in 1..=8_u8 {
            determinism(context, &format!("cursor-{tag}-{seed_index}"), || {
                fuzz_cursor_token(tag, &bytes)
            });
        }
        determinism(context, &format!("frame-{seed_index}"), || {
            fuzz_frame(&bytes)
        });
        determinism(context, &format!("artifact-{seed_index}"), || {
            fuzz_artifact(&bytes)
        });
        determinism(context, &format!("snapshot-{seed_index}"), || {
            fuzz_snapshot(&bytes)
        });
        determinism(context, &format!("snapshot-page-{seed_index}"), || {
            fuzz_snapshot_page(&bytes)
        });
        determinism(context, &format!("event-{seed_index}"), || {
            fuzz_event(&bytes)
        });
        determinism(context, &format!("search-{seed_index}"), || {
            fuzz_search(&bytes)
        });
        determinism(context, &format!("policy-{seed_index}"), || {
            fuzz_policy(&bytes)
        });
        determinism(context, &format!("table-{seed_index}"), || {
            fuzz_table(&bytes)
        });
        determinism(context, &format!("error-{seed_index}"), || {
            fuzz_error_code(&bytes)
        });
        let receipt_tag = u8::try_from(seed_index % 256).unwrap_or(0);
        determinism(context, &format!("receipt-{seed_index}"), || {
            fuzz_receipt(receipt_tag)
        });
    }
}

fn run_policy_probes(context: &mut FuzzContext) {
    let mut facts = BTreeMap::new();
    facts.insert("tier".to_owned(), FactValue::Text("gold".to_owned()));
    let budget = EvalBudget::default();

    // A valid chain grows in cost with its length. The step counter is
    // monotonic and stays inside the budget.
    let mut previous_steps = 0_u32;
    for length in 1..=16_usize {
        let expression = format!("and({})", vec!["allow"; length].join(","));
        match evaluate(&expression, &facts, budget) {
            Ok(evaluation) => {
                let monotonic = evaluation.stats.steps >= previous_steps;
                let bounded = evaluation.stats.steps <= budget.max_steps
                    && evaluation.stats.arena <= budget.max_arena;
                previous_steps = evaluation.stats.steps;
                context.probe(&format!("policy-chain-{length}"), monotonic && bounded);
                context.note(
                    &format!("policy-chain-{length}"),
                    &format!("{:?}", evaluation.stats),
                );
            }
            Err(error) => {
                context.failures.push(format!(
                    "the policy chain {length} failed: {}",
                    error.code()
                ));
            }
        }
    }

    // Invalid expressions must return a named error, not a panic.
    for text in [
        "",
        "and(",
        "eq(tier)",
        "eq(tier,1.5)",
        "unknown()",
        "not(not(",
        &"not(".repeat(64),
    ] {
        let result = fuzz_policy(text.as_bytes());
        context.probe("policy-invalid", result.is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::{PARITY_DIGEST, run_corpus};

    #[test]
    fn the_corpus_is_total_and_deterministic() {
        let report = run_corpus();
        assert!(report.passed(), "{:?}", report.failures);
        assert!(report.cases > 200);
    }

    #[test]
    fn the_parity_digest_is_pinned() {
        let report = run_corpus();
        assert_eq!(report.digest, PARITY_DIGEST);
    }
}
