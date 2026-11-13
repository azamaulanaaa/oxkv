//! A pure per-key write scheduler for oxkv.
//!
//! The scheduler owns one ordering promise: per key, first in, first out. A
//! request has one of three states: `queued`, `in_flight`, or `terminal`. A
//! key has at most one request in flight. A key may have many queued
//! requests.
//!
//! The scheduler is a pure decision function over state and budget. It does
//! not read a clock, draw randomness, perform I/O, evaluate policy, write a
//! terminal receipt state, or commit data. Equal state and equal budget
//! produce an equal decision.
//!
//! The crate compiles for native and `wasm32-unknown-unknown`. It uses no
//! floating point and no `unsafe` code.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::receipt::RequestId;
//! use oxkv_contracts::version::ShardId;
//! use oxkv_scheduler::{KeyId, RequestAdmission, SchedulerBudget, SchedulerState};
//!
//! let mut state = SchedulerState::new();
//! let shard = ShardId::new(vec![1]);
//! let key = KeyId::new(vec![b'k']);
//! let request = RequestId::from_bytes([1; 32]);
//! state
//!     .admit(RequestAdmission::new(shard.clone(), key, request))
//!     .unwrap();
//!
//! let decision = state.decide(SchedulerBudget::default());
//! assert_eq!(decision.selections.len(), 1);
//! state.apply_selection(&decision).unwrap();
//! state.retire(&shard, request).unwrap();
//! assert_eq!(state.pending_count(), 0);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod admit;
pub mod budget;
pub mod select;
pub mod state;

pub use admit::{AdmissionError, Admitted, RequestAdmission};
pub use budget::{
    BudgetUsage, DEFAULT_MAX_EDGES, DEFAULT_MAX_POSITIONS, SchedulerBudget, starvation_bound,
};
pub use select::{Decision, Selection, Unselected, UnselectedReason};
pub use state::{
    Arrival, DEFAULT_MAX_FAN_IN, KeyId, RequestState, SchedulerConfig, SchedulerState,
    TransitionError,
};
