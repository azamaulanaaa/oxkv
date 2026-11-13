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
use oxkv_contracts::version::{CommitVersion, ContentDigest, PrefixBounds};
use oxkv_index::manifest::IndexManifest;
use oxkv_index::posting::{DocumentField, IndexDocument, PostingKey};
use oxkv_index::segment::Segment;
use oxkv_index::tokenizer::{TokenBudget, TokenizerIdentity};
use oxkv_index::{FieldSpec, ScorerSpec, ShardIndex};

use crate::canonical::{digest, hex};
use crate::policy::{EvalBudget, FactValue, evaluate};
use crate::rng::FixedSeedRng;

/// The parity digest of the default corpus.
///
/// A native run and a wasm run must produce this value.
pub const PARITY_DIGEST: &str = "46cc802bc3683abd5a7f165b089e84330da5fcb5d4713c72f5c3e686a6379898";

/// The result of one fuzz corpus run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzReport {
    /// The number of probe calls.
    pub cases: u32,
    /// The failure texts.
    pub failures: Vec<String>,
    /// The name of every probe that ran.
    pub probes: Vec<String>,
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

/// Decodes one index artifact.
///
/// The decoder is the real `oxkv-index` decoder. It checks the index digest,
/// the index header, every segment checksum, and every segment body.
///
/// # Errors
///
/// Returns the canonical index error code for a bad artifact.
pub fn fuzz_index(bytes: &[u8]) -> Result<String, String> {
    ShardIndex::decode(bytes, index_manifest(), index_bounds())
        .map(|index| {
            format!(
                "{}:{}",
                index.commit_version().get(),
                index.segments().len()
            )
        })
        .map_err(|error| error.code().to_owned())
}

/// Decodes one index segment.
///
/// The decoder is the real `oxkv-index` decoder. It checks the segment
/// checksum, the segment header, the posting list, the document map, and the
/// tombstone set.
///
/// # Errors
///
/// Returns the canonical index error code for a bad segment.
pub fn fuzz_index_segment(bytes: &[u8]) -> Result<String, String> {
    Segment::decode(bytes, &index_manifest())
        .map(|segment| {
            format!(
                "{}:{}:{}",
                segment.commit_version().get(),
                segment.postings().len(),
                segment.documents().len()
            )
        })
        .map_err(|error| error.code().to_owned())
}

/// Decodes one logical posting key.
///
/// # Errors
///
/// Returns the canonical index error code for a bad key.
pub fn fuzz_index_posting_key(bytes: &[u8]) -> Result<String, String> {
    PostingKey::decode(bytes)
        .map(|key| format!("{}:{}:{}", key.field(), key.term(), key.document().len()))
        .map_err(|error| error.code().to_owned())
}

/// Returns the fixed manifest of the index fuzz probes.
fn index_manifest() -> IndexManifest {
    let field = FieldSpec::new("title", TokenizerIdentity::unicode_word()).with_positions(true);
    IndexManifest::new(
        vec![field],
        TokenizerIdentity::unicode_word(),
        ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
        100_000,
        TokenBudget::DEFAULT,
    )
    .expect("the index manifest is valid")
}

/// Returns the fixed shard bounds of the index fuzz probes.
fn index_bounds() -> PrefixBounds {
    PrefixBounds::new(b"a".to_vec(), b"z".to_vec())
}

/// Builds one valid index with one document and one posting list.
fn index_fixture() -> Vec<u8> {
    let mut index = ShardIndex::new(index_manifest(), index_bounds()).expect("the index is valid");
    let document = IndexDocument::new(b"a", vec![DocumentField::new("title", "cafe world", 0, 0)]);
    let mut transaction = index
        .begin_write(CommitVersion::new(1))
        .expect("the version advances");
    transaction.put(&document).expect("the document is valid");
    transaction.commit().expect("the commit succeeds");
    index.encode()
}

/// Returns one valid segment with one document and one posting list.
fn segment_fixture() -> Vec<u8> {
    let index = index_fixture();
    let segment_start = index
        .windows(4)
        .position(|window| window == b"OXS1")
        .expect("the index holds one segment");
    let length_offset = segment_start
        .checked_sub(8)
        .expect("the segment has a length");
    let mut length_bytes = [0_u8; 8];
    length_bytes.copy_from_slice(&index[length_offset..segment_start]);
    let length = usize::try_from(u64::from_be_bytes(length_bytes)).expect("the length fits");
    index[segment_start..segment_start + length].to_vec()
}

/// Appends a fresh digest to one body.
fn reseal(body: &[u8]) -> Vec<u8> {
    let digest = ContentDigest::of_bytes(body);
    let mut bytes = body.to_vec();
    bytes.extend_from_slice(digest.as_bytes());
    bytes
}

/// Runs the fixed fuzz corpus.
#[must_use]
pub fn run_corpus() -> FuzzReport {
    let mut context = FuzzContext::default();
    run_byte_probes(&mut context);
    run_index_probes(&mut context);
    run_policy_probes(&mut context);
    FuzzReport {
        cases: context.cases,
        failures: context.failures,
        probes: context.probes,
        digest: hex(digest("fuzz", context.transcript.as_bytes()).as_bytes()),
    }
}

#[derive(Default)]
struct FuzzContext {
    cases: u32,
    failures: Vec<String>,
    probes: Vec<String>,
    transcript: String,
}

impl FuzzContext {
    fn probe(&mut self, name: &str, passed: bool) {
        self.cases = self.cases.saturating_add(1);
        self.probes.push(name.to_owned());
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
        determinism(context, &format!("index-{seed_index}"), || {
            fuzz_index(&bytes)
        });
        determinism(context, &format!("index-segment-{seed_index}"), || {
            fuzz_index_segment(&bytes)
        });
        determinism(context, &format!("index-posting-{seed_index}"), || {
            fuzz_index_posting_key(&bytes)
        });
    }
}

fn run_index_probes(context: &mut FuzzContext) {
    let index = index_fixture();
    let segment = segment_fixture();

    // The fixtures must decode. This check proves that the probes use the
    // real decoder on a real artifact.
    context.probe(
        "index-fixture",
        fuzz_index(&index).is_ok() && fuzz_index_segment(&segment).is_ok(),
    );

    // The index digest path.
    let mut bad_digest = index.clone();
    if let Some(last) = bad_digest.last_mut() {
        *last ^= 0x01;
    }
    determinism(context, "index-digest", || fuzz_index(&bad_digest));

    // The index header path. Each body is truncated and resealed, so the
    // digest passes and the header reader runs.
    for (position, cut) in [0_usize, 1, 4, 6, 40].into_iter().enumerate() {
        let cut = cut.min(index.len().saturating_sub(32));
        let bytes = reseal(&index[..cut]);
        determinism(context, &format!("index-header-{position}"), || {
            fuzz_index(&bytes)
        });
    }

    // The segment header path.
    let body_length = segment.len().saturating_sub(32);
    if body_length >= 6 {
        let mut bad_version = segment.clone();
        bad_version[4] = 0;
        bad_version[5] = 99;
        let digest = ContentDigest::of_bytes(&bad_version[..body_length]);
        bad_version[body_length..].copy_from_slice(digest.as_bytes());
        determinism(context, "index-segment-version", || {
            fuzz_index_segment(&bad_version)
        });
    }

    // The posting list path. The first `cafe` term is inside the posting
    // list. Each cut lands in one posting entry, and each body is resealed.
    if let Some(term_start) = segment.windows(4).position(|window| window == b"cafe") {
        for (position, extra) in [0_usize, 4, 12, 21, 33].into_iter().enumerate() {
            let cut = term_start + 4 + extra;
            if cut < body_length {
                let bytes = reseal(&segment[..cut]);
                determinism(context, &format!("index-postings-{position}"), || {
                    fuzz_index_segment(&bytes)
                });
            }
        }
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
    use super::{
        PARITY_DIGEST, fuzz_index, fuzz_index_posting_key, fuzz_index_segment, run_corpus,
    };

    #[test]
    fn the_corpus_is_total_and_deterministic() {
        let report = run_corpus();
        assert!(report.passed(), "{:?}", report.failures);
        assert!(report.cases > 200);
    }

    #[test]
    fn the_default_corpus_includes_the_index_decoder() {
        let report = run_corpus();
        assert!(
            report
                .probes
                .iter()
                .any(|probe| probe.starts_with("index-")),
            "the default corpus has no index probe"
        );
        assert!(
            report
                .probes
                .iter()
                .any(|probe| probe.starts_with("index-segment-")),
            "the default corpus has no index segment probe"
        );
        assert!(
            report
                .probes
                .iter()
                .any(|probe| probe.starts_with("index-posting-")),
            "the default corpus has no posting key probe"
        );
    }

    #[test]
    fn a_corrupt_index_returns_a_named_error() {
        let index = fuzz_index(&[0xff_u8; 8]).expect_err("the bytes are too short");
        assert_eq!(index, "search.index_mismatch");
        assert!(
            oxkv_contracts::error::lookup(&index).is_some(),
            "the code {index} is not registered"
        );

        let segment = fuzz_index_segment(b"OXS1").expect_err("the bytes are too short");
        assert_eq!(segment, "search.index_mismatch");
        assert!(
            oxkv_contracts::error::lookup(&segment).is_some(),
            "the code {segment} is not registered"
        );

        let posting = fuzz_index_posting_key(&[0xff_u8; 8]).expect_err("the key is malformed");
        assert_eq!(posting, "search.index_mismatch");
        assert!(
            oxkv_contracts::error::lookup(&posting).is_some(),
            "the code {posting} is not registered"
        );
    }

    #[test]
    fn a_truncated_posting_list_returns_a_named_error() {
        let segment = super::segment_fixture();
        let term_start = segment
            .windows(4)
            .position(|window| window == b"cafe")
            .expect("the posting list holds the term");
        let bytes = super::reseal(&segment[..term_start + 4]);
        let error = fuzz_index_segment(&bytes).expect_err("the posting entry is truncated");
        assert_eq!(error, "search.index_mismatch");
        assert!(
            oxkv_contracts::error::lookup(&error).is_some(),
            "the code {error} is not registered"
        );
    }

    #[test]
    fn the_parity_digest_is_pinned() {
        let report = run_corpus();
        assert_eq!(report.digest, PARITY_DIGEST);
    }
}
