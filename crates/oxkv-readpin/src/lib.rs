//! Pins, session minimums, bounded-staleness reads, and read-your-writes.
//!
//! The crate separates four lifetimes that the read contract must not mix:
//!
//! - [`pin::AuthorityReadPin`] is bounded. It expires under authority
//!   retention with `read.pin_expired` or `read.pin_unreadable`.
//! - [`pin::ReplicaLocalPin`] is the applied version vector of a replica. It
//!   stays readable offline. It does not expire with authority retention.
//! - [`resume::ResumeCursor`] is bounded by authority diff retention. An
//!   expired cursor returns `sync.cursor_expired` and offers a snapshot.
//! - [`retention::ReceiptRetention`] is separate from every read pin.
//!
//! A session minimum is a version vector. A scalar cannot construct one:
//!
//! ```compile_fail
//! use oxkv_contracts::version::CommitVersion;
//! use oxkv_readpin::SessionToken;
//!
//! let token = SessionToken::new(CommitVersion::new(7));
//! ```
//!
//! # Default read mode
//!
//! [`read::SessionMode::BoundedStaleness`] is the default. The replica serves
//! its applied version vector. The response reports the served vector and the
//! staleness measure. A read that cannot meet its bound returns `read.stale`
//! and no data.
//!
//! # Read-your-writes
//!
//! [`read::SessionMode::ReadYourWrites`] raises the session minimum to the
//! commit version of a terminal receipt. A replica that cannot apply the
//! minimum returns [`read::ReadOutcome::Blocked`], or
//! [`read::ReadError::BehindCommit`] through [`read::ReadOutcome::into_result`].
//!
//! # Replica and search use
//!
//! [`read::ReplicaView`] holds the applied vector, the persisted high-water
//! mark, and the hydration state. Essay 15 can use it as the read half of a
//! replica. [`read::ReadResponse::served`] is the pin that essay 19 can put in
//! a `SearchCursor`.
//!
//! The crate uses no clock, randomness, I/O, floating point, or unsafe code.
//! The caller injects time through [`time::Timestamp`].
//!
//! The repository guide is `docs/readpin.md`.

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod bound;
pub mod pin;
pub mod read;
pub mod resume;
pub mod retention;
pub mod session;
pub mod time;

pub use bound::{BoundRequest, StalenessBound, StalenessMeasure};
pub use pin::{AuthorityReadPin, PinError, ReplicaLocalPin};
pub use read::{
    AuthorityView, BlockedRead, CoverageDescriptor, HydrationState, PinMode, ReadError,
    ReadOutcome, ReadRequest, ReadResponse, ReadTarget, ReplicaView, Residency, ResultMode,
    SessionMode, evaluate,
};
pub use resume::{CursorExpired, CursorExpiryReason, ResumeCursor, ResumeOutcome};
pub use retention::{ReceiptExpired, ReceiptRetention};
pub use session::{SessionToken, SessionTokenError};
pub use time::{DurationMillis, Timestamp};
