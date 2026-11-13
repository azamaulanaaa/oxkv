//! The read barrier and bounded reads.
//!
//! A follower never serves a read past its applied commit index. A caller
//! names the index for the read. [`ReadBarrier`] rejects an index above the
//! applied index with `read.behind_commit`.
//!
//! The bounded read types in this module are the input for essay 23. A read
//! declares its bound before it runs. A read over the bound returns
//! `read.stale` and no data.

use oxkv_contracts::version::{CommitVersion, LeaseTerm};

use crate::error::ReplError;
use crate::log::LogIndex;

/// One user key and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyValue {
    /// The user key.
    pub key: Vec<u8>,
    /// The user value.
    pub value: Vec<u8>,
}

/// The state at one applied log index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadView {
    at: LogIndex,
    commit_version: CommitVersion,
    term: LeaseTerm,
    records: Vec<KeyValue>,
}

impl ReadView {
    /// Creates a read view.
    #[must_use]
    pub const fn new(
        at: LogIndex,
        commit_version: CommitVersion,
        term: LeaseTerm,
        records: Vec<KeyValue>,
    ) -> Self {
        Self {
            at,
            commit_version,
            term,
            records,
        }
    }

    /// Returns the applied index of the view.
    #[must_use]
    pub const fn at(&self) -> LogIndex {
        self.at
    }

    /// Returns the commit version of the view.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the term of the last applied record.
    #[must_use]
    pub const fn term(&self) -> LeaseTerm {
        self.term
    }

    /// Returns the records in key order.
    #[must_use]
    pub fn records(&self) -> &[KeyValue] {
        &self.records
    }

    /// Returns one value by key.
    #[must_use]
    pub fn value(&self, key: &[u8]) -> Option<&[u8]> {
        self.records
            .iter()
            .find(|record| record.key == key)
            .map(|record| record.value.as_slice())
    }
}

/// The applied position that guards a follower read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadBarrier {
    applied: LogIndex,
}

impl ReadBarrier {
    /// Creates a barrier at one applied index.
    #[must_use]
    pub const fn new(applied: LogIndex) -> Self {
        Self { applied }
    }

    /// Returns the applied index.
    #[must_use]
    pub const fn applied(self) -> LogIndex {
        self.applied
    }

    /// Moves the barrier forward.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the new index is lower
    /// than the old index.
    pub fn advance(&mut self, applied: LogIndex) -> Result<(), ReplError> {
        if applied < self.applied {
            return Err(ReplError::InvariantViolation {
                reason: "the applied index moved backwards",
            });
        }
        self.applied = applied;
        Ok(())
    }

    /// Checks one read against the barrier.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::ReadBehindCommit`] when the read asks for an index
    /// above the applied index. The caller must not serve stale data.
    pub fn admit(self, requested: LogIndex) -> Result<(), ReplError> {
        if requested > self.applied {
            return Err(ReplError::ReadBehindCommit {
                requested: requested.get(),
                applied: self.applied.get(),
            });
        }
        Ok(())
    }
}

/// The version bound of one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadBound {
    /// The read must serve at least this commit version.
    AtLeast(CommitVersion),
    /// The read must serve exactly this commit version.
    Exactly(CommitVersion),
    /// The read has no version bound.
    Unbounded,
}

/// The freshness contract of one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadFreshness {
    bound: ReadBound,
    max_staleness: Option<u64>,
}

impl ReadFreshness {
    /// Creates a freshness contract.
    #[must_use]
    pub const fn new(bound: ReadBound, max_staleness: Option<u64>) -> Self {
        Self {
            bound,
            max_staleness,
        }
    }

    /// Returns the version bound.
    #[must_use]
    pub const fn bound(self) -> ReadBound {
        self.bound
    }

    /// Returns the maximum allowed staleness in versions.
    #[must_use]
    pub const fn max_staleness(self) -> Option<u64> {
        self.max_staleness
    }

    /// Checks the served version against the bound.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::ReadBehindCommit`] when the served version is
    /// below an `AtLeast` or `Exactly` bound. Returns [`ReplError::ReadStale`]
    /// when the served version is over the bound or over the staleness limit.
    pub fn check(self, served: CommitVersion, current: CommitVersion) -> Result<(), ReplError> {
        match self.bound {
            ReadBound::AtLeast(required) => {
                if served < required {
                    return Err(ReplError::ReadBehindCommit {
                        requested: required.get(),
                        applied: served.get(),
                    });
                }
            }
            ReadBound::Exactly(required) => {
                if served < required {
                    return Err(ReplError::ReadBehindCommit {
                        requested: required.get(),
                        applied: served.get(),
                    });
                }
                if served > required {
                    return Err(ReplError::ReadStale {
                        served: served.get(),
                        bound: required.get(),
                    });
                }
            }
            ReadBound::Unbounded => {}
        }
        if let Some(limit) = self.max_staleness {
            let lag = current.get().saturating_sub(served.get());
            if lag > limit {
                return Err(ReplError::ReadStale {
                    served: served.get(),
                    bound: current.get().saturating_sub(limit),
                });
            }
        }
        Ok(())
    }
}
