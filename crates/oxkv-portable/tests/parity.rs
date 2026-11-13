//! Parity and golden vector tests for the portable core.

use oxkv_portable::{
    Clock, Component, DeterministicEntropy, Durability, Entropy, InProcessTransport,
    InterleavedExecutor, MemoryStorage, Observations, Operation, ParityError, Ports, ReceiptSpec,
    ScriptedClock, SequentialExecutor, Snapshot, Storage, Transcript, Transport, TransportError,
    VerdictSpec, compare, encode_key, run_transcript,
};

const PREFIX_LOWER: [u8; 10] = [0x01, b'a', b'c', b'c', b'o', b'u', b'n', b't', b's', 0x00];
const PREFIX_UPPER: [u8; 10] = [0x01, b'a', b'c', b'c', b'o', b'u', b'n', b't', b's', 0x01];

fn key(value: &str) -> Vec<Component> {
    vec![Component::Text(value.to_owned())]
}

fn golden_transcript() -> Transcript {
    let alpha = encode_key("accounts", &key("alpha")).expect("the key is valid");
    let mut transcript = Transcript::new(Vec::new());
    transcript.push(Operation::Key {
        table: "accounts".to_owned(),
        key: key("alpha"),
    });
    transcript.push(Operation::Put {
        table: "accounts".to_owned(),
        key: key("alpha"),
        value: b"one".to_vec(),
        commit_version: 1,
    });
    transcript.push(Operation::Put {
        table: "accounts".to_owned(),
        key: key("beta"),
        value: b"two".to_vec(),
        commit_version: 2,
    });
    transcript.push(Operation::Get {
        table: "accounts".to_owned(),
        key: key("alpha"),
    });
    transcript.push(Operation::Get {
        table: "accounts".to_owned(),
        key: key("missing"),
    });
    transcript.push(Operation::Snapshot {
        table: "accounts".to_owned(),
        shard_id: vec![0x01],
        prefix_lower: PREFIX_LOWER.to_vec(),
        prefix_upper: PREFIX_UPPER.to_vec(),
        manifest_digest: [0x11; 32],
        commit_version: 7,
        lease_term: 3,
        page_records: 1,
    });
    transcript.push(Operation::Page {
        table: "accounts".to_owned(),
        shard_id: vec![0x01],
        prefix_lower: PREFIX_LOWER.to_vec(),
        prefix_upper: PREFIX_UPPER.to_vec(),
        commit_version: 7,
        page_sequence: 2,
        after: Some(alpha),
        limit: 10,
    });
    transcript.push(Operation::Cursor {
        table: "accounts".to_owned(),
        snapshot_version: 7,
        last_key: b"alpha".to_vec(),
        ascending: true,
    });
    transcript.push(Operation::Event {
        shard_id: vec![0x01],
        table: "accounts".to_owned(),
        key: b"alpha".to_vec(),
        key_sequence: 1,
        commit_version: 7,
        rule_id: "on_write".to_owned(),
        policy_version: [0x22; 32],
        emission_index: 0,
        payload: b"effect".to_vec(),
        payload_schema: "effect/v1".to_owned(),
        origin_request_id: [0x33; 32],
    });
    transcript.push(Operation::Receipt {
        spec: committed_receipt(),
    });
    transcript.push(Operation::Nonce { length: 8 });
    transcript.push(Operation::Send {
        frame: b"hello".to_vec(),
    });
    transcript
}

fn committed_receipt() -> ReceiptSpec {
    ReceiptSpec {
        verdict: VerdictSpec {
            base_version: 3,
            current_key_version: 3,
            policy_allows: true,
            stale_facts: false,
            strict: false,
            queue_deadline_passed: false,
            commit_version: 7,
        },
        receipt_id: [0x44; 32],
        request_id: [0x55; 32],
        idempotency_key: b"key-1".to_vec(),
        body_digest: [0x66; 32],
        policy_version: [0x22; 32],
        manifest_digest: [0x11; 32],
        shard_id: vec![0x01],
        audit_sequence: 9,
    }
}

fn run_with(executor: &dyn oxkv_portable::Executor, transcript: &Transcript) -> Observations {
    let mut storage = MemoryStorage::new(Durability::Memory);
    let clock = ScriptedClock::new(vec![100, 200]);
    let entropy = DeterministicEntropy::new(7);
    let transport = InProcessTransport;
    run_transcript(
        executor,
        transcript,
        Ports::new(&mut storage, &clock, &entropy, &transport),
    )
    .expect("the transcript runs")
}

#[test]
fn the_same_transcript_produces_identical_bytes_on_both_executors() {
    let transcript = golden_transcript();
    let sequential = run_with(&SequentialExecutor, &transcript);
    let interleaved = run_with(&InterleavedExecutor::new(4), &transcript);
    assert_eq!(sequential.len(), transcript.len());
    assert_eq!(interleaved.len(), transcript.len());
    assert_eq!(compare(&sequential, &interleaved), Ok(()));
    assert_eq!(sequential.to_bytes(), interleaved.to_bytes());
}

#[test]
fn the_sequence_matches_the_golden_vector() {
    let transcript = golden_transcript();
    let observations = run_with(&SequentialExecutor, &transcript);
    let golden = load_golden();
    assert_eq!(golden.contract, "portable-parity/v1");
    assert_eq!(golden.observations.len(), observations.len());
    for (index, expected) in golden.observations.iter().enumerate() {
        let actual = observations
            .get(index)
            .unwrap_or_else(|| panic!("the run has no slot {index}"));
        assert_eq!(
            expected.hex,
            oxkv_portable::hex_lower(&actual.encode()),
            "the observation bytes changed at slot {index}"
        );
    }
    assert_eq!(
        golden.sequence_hex,
        oxkv_portable::hex_lower(&observations.to_bytes())
    );
}

#[test]
fn a_snapshot_from_one_run_hydrates_in_another_run() {
    let transcript = golden_transcript();
    let first = run_with(&SequentialExecutor, &transcript);
    let second = run_with(&InterleavedExecutor::new(4), &transcript);
    let first_snapshot = snapshot_bytes(&first);
    let second_snapshot = snapshot_bytes(&second);
    assert_eq!(first_snapshot, second_snapshot);
    let decoded = Snapshot::decode(&first_snapshot).expect("the snapshot decodes");
    assert_eq!(decoded.encode(), first_snapshot);
    assert_eq!(decoded.pages.len(), 2);
}

#[test]
fn a_scripted_clock_and_entropy_reproduce_the_run() {
    let transcript = golden_transcript();
    let first = run_with(&SequentialExecutor, &transcript);
    let second = run_with(&SequentialExecutor, &transcript);
    assert_eq!(first.to_bytes(), second.to_bytes());

    let mut storage = MemoryStorage::default();
    let clock = ScriptedClock::new(vec![999]);
    let entropy = DeterministicEntropy::new(8);
    let transport = InProcessTransport;
    let third = run_transcript(
        &SequentialExecutor,
        &transcript,
        Ports::new(&mut storage, &clock, &entropy, &transport),
    )
    .expect("the transcript runs");
    assert_ne!(first.to_bytes(), third.to_bytes());
}

#[test]
fn a_thread_count_change_does_not_change_the_result() {
    let transcript = golden_transcript();
    let one = run_with(&InterleavedExecutor::new(1), &transcript);
    let eight = run_with(&InterleavedExecutor::new(8), &transcript);
    assert_eq!(compare(&one, &eight), Ok(()));
}

#[test]
fn an_adapter_cannot_change_a_verdict() {
    let receipt = ReceiptSpec {
        verdict: VerdictSpec {
            base_version: 2,
            current_key_version: 3,
            policy_allows: true,
            stale_facts: false,
            strict: false,
            queue_deadline_passed: false,
            commit_version: 7,
        },
        receipt_id: [1; 32],
        request_id: [2; 32],
        idempotency_key: b"same".to_vec(),
        body_digest: [3; 32],
        policy_version: [4; 32],
        manifest_digest: [5; 32],
        shard_id: vec![6],
        audit_sequence: 7,
    };
    let transcript = Transcript::new(vec![Operation::Receipt { spec: receipt }]);

    let mut plain_storage = MemoryStorage::default();
    let plain_clock = ScriptedClock::fixed(0);
    let plain_entropy = DeterministicEntropy::new(0);
    let plain_transport = InProcessTransport;
    let plain = run_transcript(
        &SequentialExecutor,
        &transcript,
        Ports::new(
            &mut plain_storage,
            &plain_clock,
            &plain_entropy,
            &plain_transport,
        ),
    )
    .expect("the receipt runs");

    let mut hostile_storage = HostileStorage;
    let hostile_clock = HostileClock;
    let hostile_entropy = HostileEntropy;
    let hostile_transport = HostileTransport;
    let hostile = run_transcript(
        &InterleavedExecutor::new(8),
        &transcript,
        Ports::new(
            &mut hostile_storage,
            &hostile_clock,
            &hostile_entropy,
            &hostile_transport,
        ),
    )
    .expect("the receipt runs");
    assert_eq!(plain.to_bytes(), hostile.to_bytes());
}

#[test]
fn a_failed_comparison_names_the_slot() {
    let left = Observations::default();
    let mut storage = MemoryStorage::default();
    let clock = ScriptedClock::fixed(0);
    let entropy = DeterministicEntropy::new(0);
    let transport = InProcessTransport;
    let right = run_transcript(
        &SequentialExecutor,
        &Transcript::new(vec![Operation::Nonce { length: 1 }]),
        Ports::new(&mut storage, &clock, &entropy, &transport),
    )
    .expect("the nonce runs");
    assert_eq!(
        compare(&left, &right),
        Err(ParityError::Length { left: 0, right: 1 })
    );
}

fn snapshot_bytes(observations: &Observations) -> Vec<u8> {
    observations
        .values()
        .find_map(|observation| match observation {
            oxkv_portable::Observation::Snapshot(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .expect("the run has a snapshot")
}

struct Golden {
    contract: String,
    observations: Vec<GoldenObservation>,
    sequence_hex: String,
}

struct GoldenObservation {
    hex: String,
}

fn load_golden() -> Golden {
    let value: serde_json::Value = serde_json::from_str(include_str!("vectors/sequence.json"))
        .expect("the golden file is JSON");
    let contract = value["contract"]
        .as_str()
        .expect("the golden file names a contract")
        .to_owned();
    let observations = value["observations"]
        .as_array()
        .expect("the golden file lists observations")
        .iter()
        .map(|entry| GoldenObservation {
            hex: entry["hex"]
                .as_str()
                .expect("the observation has hex")
                .to_owned(),
        })
        .collect();
    let sequence_hex = value["sequence_hex"]
        .as_str()
        .expect("the golden file has a sequence hash")
        .to_owned();
    Golden {
        contract,
        observations,
        sequence_hex,
    }
}

struct HostileStorage;

impl Storage for HostileStorage {
    fn get(&self, _key: &[u8]) -> Result<Option<Vec<u8>>, oxkv_portable::StorageError> {
        Err(oxkv_portable::StorageError::Unavailable)
    }

    fn range(
        &self,
        _request: &oxkv_portable::ScanRequest<'_>,
    ) -> Result<oxkv_portable::ScanPage, oxkv_portable::StorageError> {
        Err(oxkv_portable::StorageError::Unavailable)
    }

    fn commit(
        &mut self,
        _batch: &oxkv_portable::Batch,
    ) -> Result<oxkv_portable::CommitOutcome, oxkv_portable::StorageError> {
        Err(oxkv_portable::StorageError::BatchNotAtomic)
    }

    fn durability(&self) -> Durability {
        Durability::Memory
    }
}

struct HostileClock;

impl Clock for HostileClock {
    fn now_millis(&self) -> u64 {
        u64::MAX
    }
}

struct HostileEntropy;

impl Entropy for HostileEntropy {
    fn fill(&self, output: &mut [u8]) {
        output.fill(0xFF);
    }
}

struct HostileTransport;

impl Transport for HostileTransport {
    fn send(&self, _frame: &[u8]) -> Result<Vec<u8>, TransportError> {
        Err(TransportError::Disconnected)
    }
}
