//! The one fence that crosses the replication seam.
//!
//! A [`Fence`] wraps the authority lease term. The term is the only value that
//! fences a write. It increases and never repeats. It travels with every log
//! record.
//!
//! [`FenceBook`] holds the highest term that the driver observed and the
//! current owner. A write with a lower term fails with
//! [`ReplError::StaleEpoch`]. A write with a higher term wins.

use core::fmt;

use oxkv_contracts::version::LeaseTerm;

use crate::error::ReplError;
use crate::ids::NodeId;

/// A monotonically increasing fencing token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fence {
    term: LeaseTerm,
}

impl Fence {
    /// Creates a fence from a lease term.
    #[must_use]
    pub const fn new(term: LeaseTerm) -> Self {
        Self { term }
    }

    /// Creates the first fence for a new shard.
    ///
    /// This is the zero term. No writer may hold it. The first grant returns
    /// term one.
    #[must_use]
    pub const fn zero() -> Self {
        Self::new(LeaseTerm::new(0))
    }

    /// Returns the lease term.
    #[must_use]
    pub const fn term(self) -> LeaseTerm {
        self.term
    }

    /// Returns the integer term value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.term.get()
    }

    /// Returns the next fence.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the term would overflow.
    /// A term must never repeat, so a wrapping add is not an option.
    pub fn next(self) -> Result<Self, ReplError> {
        let next = self
            .term
            .get()
            .checked_add(1)
            .ok_or(ReplError::InvariantViolation {
                reason: "the lease term overflowed",
            })?;
        Ok(Self::new(LeaseTerm::new(next)))
    }
}

impl fmt::Display for Fence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.term, formatter)
    }
}

/// Proof that one node holds leadership for one fence.
///
/// A driver returns this value from `acquire` and from `transfer`. Every
/// write and every leader read presents it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Leadership {
    holder: NodeId,
    fence: Fence,
}

impl Leadership {
    /// Creates a leadership token.
    #[must_use]
    pub const fn new(holder: NodeId, fence: Fence) -> Self {
        Self { holder, fence }
    }

    /// Returns the holder node.
    #[must_use]
    pub const fn holder(self) -> NodeId {
        self.holder
    }

    /// Returns the fence.
    #[must_use]
    pub const fn fence(self) -> Fence {
        self.fence
    }

    /// Returns the lease term.
    #[must_use]
    pub const fn term(self) -> LeaseTerm {
        self.fence.term()
    }

    /// Returns the integer term value.
    #[must_use]
    pub const fn term_value(self) -> u64 {
        self.fence.get()
    }
}

impl fmt::Display for Leadership {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "holder={} term={}", self.holder, self.fence)
    }
}

/// The highest observed term and the current owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FenceBook {
    current: Fence,
    owner: Option<NodeId>,
}

impl Default for FenceBook {
    fn default() -> Self {
        Self::new()
    }
}

impl FenceBook {
    /// Creates an empty fence book at term zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: Fence::zero(),
            owner: None,
        }
    }

    /// Returns the current fence.
    #[must_use]
    pub const fn current(self) -> Fence {
        self.current
    }

    /// Returns the current owner.
    #[must_use]
    pub const fn owner(self) -> Option<NodeId> {
        self.owner
    }

    /// Grants the next term to one owner.
    ///
    /// The term increases by one. The previous owner loses all authority.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the term would overflow.
    pub fn grant(&mut self, owner: NodeId) -> Result<Leadership, ReplError> {
        let fence = self.current.next()?;
        self.current = fence;
        self.owner = Some(owner);
        Ok(Leadership::new(owner, fence))
    }

    /// Observes a term from a peer message.
    ///
    /// A higher term wins. A lower term fails with [`ReplError::StaleEpoch`].
    /// The same term with a different owner also fails.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] when the term is lower than the
    /// current term, or when the term is equal and the owner differs.
    pub fn observe(&mut self, holder: NodeId, term: LeaseTerm) -> Result<(), ReplError> {
        if term < self.current.term() {
            return Err(ReplError::StaleEpoch {
                presented: term.get(),
                current: self.current.get(),
            });
        }
        if term == self.current.term() && self.owner != Some(holder) {
            return Err(ReplError::StaleEpoch {
                presented: term.get(),
                current: self.current.get(),
            });
        }
        self.current = Fence::new(term);
        self.owner = Some(holder);
        Ok(())
    }

    /// Checks a leadership token against the book.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] when the token does not name the
    /// current term and owner.
    pub fn validate(self, token: &Leadership) -> Result<(), ReplError> {
        if token.term() == self.current.term() && self.owner == Some(token.holder()) {
            return Ok(());
        }
        Err(ReplError::StaleEpoch {
            presented: token.term_value(),
            current: self.current.get(),
        })
    }

    /// Drops the owner and keeps the current term.
    ///
    /// A later grant returns a strictly higher term.
    pub fn clear_owner(&mut self) {
        self.owner = None;
    }
}
