//! The replication seam for oxkv 1.0.
//!
//! This crate owns one driver interface. A write uses that interface only. It
//! never sees a driver-specific leader concept. Two drivers implement the
//! interface:
//!
//! - [`LeaseDriver`] is a primary-backup group with a time-bounded lease.
//! - [`RaftDriver`] is a Raft-like consensus group with a deterministic
//!   election rule.
//!
//! # The one fence
//!
//! The authority lease term is the only fence. It increases and never
//! repeats. It travels with every log record. A writer presents a
//! [`Leadership`] token. A stale token fails with `replication.stale_epoch`. An
//! expired lease fails with `replication.lease_expired`. A held lease fails
//! with `replication.lease_held`.
//!
//! # One terminal receipt
//!
//! A write is either committed and visible or rejected with a terminal
//! receipt. A caller can stop the leader at each phase boundary. A retry with
//! the same idempotency key resolves to one receipt. The retry never appends a
//! second record for that key.
//!
//! # The read barrier
//!
//! A follower never serves a read past its applied commit index. A read above
//! that index fails with `read.behind_commit`. [`ReadFreshness`] carries the
//! version bound for essay 23. A read over its bound fails with `read.stale`.
//!
//! # Portability
//!
//! The crate compiles for native and `wasm32-unknown-unknown`. It uses no
//! floating point, no clock, no randomness, no I/O on a protocol path, and no
//! `unsafe` code. Time and peer messages are injected.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::receipt::{IdempotencyKey, ReceiptState};
//! use oxkv_repl::{LeaseDriver, LeaseDuration, NodeId, Tick, WriteRequest, submit};
//!
//! let mut driver = LeaseDriver::new(
//!     NodeId::new(1),
//!     [NodeId::new(1)],
//!     LeaseDuration::new(10),
//! )
//! .expect("the member set is valid");
//! let request = WriteRequest::new(IdempotencyKey::new(vec![1]), b"k".to_vec(), b"v".to_vec());
//! let receipt = submit(&mut driver, &request, Tick::ZERO).expect("the write commits");
//! assert_eq!(receipt.state(), ReceiptState::Committed);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod driver;
pub mod error;
pub mod fence;
pub mod ids;
pub mod lease;
pub mod lease_driver;
pub mod log;
pub mod raft;
pub mod read;
pub mod sim;
pub mod state;
pub mod warmup;
pub mod write;

pub use driver::{AdvanceReport, AppendReceipt, ReplicationDriver, TerminalReceipt};
pub use error::ReplError;
pub use fence::{Fence, FenceBook, Leadership};
pub use ids::{NodeId, Tick};
pub use lease::{AuthorityLease, LeaseDuration, LeaseKeeper};
pub use lease_driver::LeaseDriver;
pub use log::{EntryPayload, Log, LogIndex, LogRecord, OwnerFence};
pub use raft::{RaftDriver, RaftMessage, RaftNode, Role};
pub use read::{KeyValue, ReadBarrier, ReadBound, ReadFreshness, ReadView};
pub use sim::{CrashableDriver, KillOutcome, KillPoint, run_kill_point};
pub use state::{AppliedEntry, ApplyReport, KvState};
pub use warmup::WarmupBudget;
pub use write::{WriteAttempt, WritePhase, WriteRequest, submit};
