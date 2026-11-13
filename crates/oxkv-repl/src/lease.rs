//! The authority lease.
//!
//! One holder owns the lease for one term. A grant returns term `N + 1`. A
//! renew extends the expiry. A release clears the owner and keeps the term.
//! A stale writer fails with [`ReplError::StaleEpoch`]. An expired lease fails
//! with [`ReplError::LeaseExpired`]. A held lease fails with
//! [`ReplError::LeaseHeld`].
//!
//! Time is injected. The lease has no clock source and no wall-clock sleep.

use crate::error::ReplError;
use crate::fence::{Fence, FenceBook, Leadership};
use crate::ids::{NodeId, Tick};

/// The lease duration in ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LeaseDuration(u64);

impl LeaseDuration {
    /// Creates a lease duration.
    #[must_use]
    pub const fn new(ticks: u64) -> Self {
        Self(ticks)
    }

    /// Returns the duration in ticks.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One granted lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityLease {
    holder: NodeId,
    term: u64,
    expires_at: Tick,
}

impl AuthorityLease {
    /// Returns the holder.
    #[must_use]
    pub const fn holder(self) -> NodeId {
        self.holder
    }

    /// Returns the lease term.
    #[must_use]
    pub const fn term(self) -> u64 {
        self.term
    }

    /// Returns the expiry tick.
    #[must_use]
    pub const fn expires_at(self) -> Tick {
        self.expires_at
    }
}

/// The state of one authority lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseKeeper {
    fences: FenceBook,
    duration: LeaseDuration,
    lease: Option<AuthorityLease>,
}

impl LeaseKeeper {
    /// Creates an empty lease keeper.
    #[must_use]
    pub const fn new(duration: LeaseDuration) -> Self {
        Self {
            fences: FenceBook::new(),
            duration,
            lease: None,
        }
    }

    /// Returns the current fence.
    #[must_use]
    pub const fn current_fence(&self) -> Fence {
        self.fences.current()
    }

    /// Returns the current lease.
    #[must_use]
    pub const fn lease(&self) -> Option<AuthorityLease> {
        self.lease
    }

    /// Returns the lease duration.
    #[must_use]
    pub const fn duration(&self) -> LeaseDuration {
        self.duration
    }

    /// Acquires the lease for one holder.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::LeaseHeld`] when the current lease is valid at
    /// `now`. Returns [`ReplError::InvariantViolation`] when the term or the
    /// expiry overflows.
    pub fn acquire(&mut self, holder: NodeId, now: Tick) -> Result<Leadership, ReplError> {
        if let Some(lease) = self.lease
            && now < lease.expires_at
        {
            return Err(ReplError::LeaseHeld { term: lease.term });
        }
        let token = self.fences.grant(holder)?;
        let expires_at =
            now.checked_add(self.duration.get())
                .ok_or(ReplError::InvariantViolation {
                    reason: "the lease expiry overflowed",
                })?;
        self.lease = Some(AuthorityLease {
            holder,
            term: token.term_value(),
            expires_at,
        });
        Ok(token)
    }

    /// Renews the lease for the current holder.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] for a stale token. Returns
    /// [`ReplError::LeaseExpired`] when the lease ended at `now`.
    pub fn renew(&mut self, token: &Leadership, now: Tick) -> Result<(), ReplError> {
        self.validate(token)?;
        let lease = self.lease.as_mut().ok_or(ReplError::StaleEpoch {
            presented: token.term_value(),
            current: self.fences.current().get(),
        })?;
        if now >= lease.expires_at {
            return Err(ReplError::LeaseExpired { term: lease.term });
        }
        lease.expires_at =
            now.checked_add(self.duration.get())
                .ok_or(ReplError::InvariantViolation {
                    reason: "the lease expiry overflowed",
                })?;
        Ok(())
    }

    /// Releases the lease for the current holder.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] for a stale token.
    pub fn release(&mut self, token: &Leadership) -> Result<(), ReplError> {
        self.validate(token)?;
        self.lease = None;
        self.fences.clear_owner();
        Ok(())
    }

    /// Checks a token against the current lease.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] when the token does not name the
    /// current holder and term.
    pub fn validate(&self, token: &Leadership) -> Result<(), ReplError> {
        self.fences.validate(token)?;
        match self.lease {
            Some(lease) if lease.holder == token.holder() && lease.term == token.term_value() => {
                Ok(())
            }
            Some(_) | None => Err(ReplError::StaleEpoch {
                presented: token.term_value(),
                current: self.fences.current().get(),
            }),
        }
    }

    /// Checks a token and the lease expiry at one tick.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] for a stale token. Returns
    /// [`ReplError::LeaseExpired`] when the lease ended at `now`.
    pub fn validate_at(&self, token: &Leadership, now: Tick) -> Result<(), ReplError> {
        self.validate(token)?;
        match self.lease {
            Some(lease) if now >= lease.expires_at => {
                Err(ReplError::LeaseExpired { term: lease.term })
            }
            Some(_) => Ok(()),
            None => Err(ReplError::StaleEpoch {
                presented: token.term_value(),
                current: self.fences.current().get(),
            }),
        }
    }

    /// Drops the lease and keeps the current term.
    ///
    /// A crash uses this method. A later grant returns a higher term.
    pub fn invalidate(&mut self) {
        self.lease = None;
        self.fences.clear_owner();
    }

    /// Returns true when the lease is not valid at one tick.
    #[must_use]
    pub fn is_expired(&self, now: Tick) -> bool {
        match self.lease {
            Some(lease) => now >= lease.expires_at,
            None => true,
        }
    }
}
