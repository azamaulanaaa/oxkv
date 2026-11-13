//! One portable semantic core with explicit platform adapters.
//!
//! This crate implements the native and wasm32 parity contract. The
//! [`core`] module holds every semantic decision and every byte shape. The
//! [`adapter`] module supplies storage, time, entropy, scheduling, and
//! transport. An adapter never decides a verdict and never shapes a byte.
//!
//! The [`parity`] module runs one transcript through two executors. The first
//! executor is single-threaded. The second executor interleaves independent
//! work. Both executors must produce identical observable bytes.
//!
//! The crate has no target conditional on a semantic path. It does not read a
//! clock, draw randomness, read the environment, read a locale, perform I/O,
//! or use floating point on a semantic path. The `static_check` integration
//! test enforces these rules.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::receipt::ReceiptState;
//! use oxkv_portable::verdict::{VerdictSpec, decide};
//!
//! let verdict = decide(&VerdictSpec {
//!     base_version: 3,
//!     current_key_version: 3,
//!     policy_allows: true,
//!     stale_facts: false,
//!     strict: false,
//!     queue_deadline_passed: false,
//!     commit_version: 4,
//! });
//! assert_eq!(verdict.state, ReceiptState::Committed);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod adapter;
pub mod artifact;
pub mod core;
pub mod encode;
pub mod parity;
pub mod verdict;

pub use adapter::{
    Batch, BatchEntry, Clock, CommitOutcome, DeterministicEntropy, Durability, Entropy,
    FixedScheduler, InProcessTransport, MemoryStorage, ScanPage, ScanRequest, Scheduler,
    ScriptedClock, Storage, StorageError, Transport, TransportError,
};
pub use artifact::{
    Component, DecodedKey, Event, EventInput, KeyError, Page, PageInput, Snapshot, SnapshotError,
    SnapshotInput, decode_key, encode_event, encode_key,
};
pub use core::{CoreError, Observation, Operation, Ports, SemanticCore, durability_label};
pub use encode::{Decoder, EncodeError, Encoder, decode_hex, hex_lower};
pub use parity::{
    Executor, InterleavedExecutor, Observations, ParityError, SequentialExecutor, Transcript,
    compare, run_transcript,
};
pub use verdict::{Receipt, ReceiptSpec, Verdict, VerdictError, VerdictSpec, decide};
