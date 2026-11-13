//! The replicated write path.
//!
//! A write advances through named phases. The caller can stop the leader at
//! each phase boundary. A retry with the same idempotency key gets the same
//! terminal receipt. A write is either committed and visible or rejected with
//! a terminal receipt. It is never both.

use oxkv_contracts::receipt::IdempotencyKey;
use oxkv_contracts::version::LeaseTerm;

use crate::driver::{ReplicationDriver, TerminalReceipt};
use crate::error::ReplError;
use crate::fence::Leadership;
use crate::ids::Tick;
use crate::log::{EntryPayload, LogIndex};

const MAX_STEPS: usize = 8;

/// One client write request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    /// The client idempotency key.
    pub idempotency: IdempotencyKey,
    /// The user key.
    pub key: Vec<u8>,
    /// The user value.
    pub value: Vec<u8>,
}

impl WriteRequest {
    /// Creates a write request.
    #[must_use]
    pub fn new(idempotency: IdempotencyKey, key: Vec<u8>, value: Vec<u8>) -> Self {
        Self {
            idempotency,
            key,
            value,
        }
    }
}

/// The phase of one write attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WritePhase {
    /// The attempt holds leadership and waits to append.
    Start,
    /// The record is durable but not committed.
    Appended,
    /// The record is committed but not applied.
    Committed,
    /// The record is applied but the caller has no receipt.
    Applied,
    /// The attempt has one terminal receipt.
    Terminal,
}

/// One write attempt.
///
/// The attempt holds the leadership token. It keeps no receipt until the
/// terminal phase. A caller may drop the attempt at any phase. A new attempt
/// with the same idempotency key retries from the durable log.
#[derive(Debug, Clone)]
pub struct WriteAttempt {
    request: WriteRequest,
    phase: WritePhase,
    leadership: Option<Leadership>,
    index: Option<LogIndex>,
    term: LeaseTerm,
    receipt: Option<TerminalReceipt>,
}

impl WriteAttempt {
    /// Creates a write attempt.
    #[must_use]
    pub fn new(request: WriteRequest) -> Self {
        Self {
            request,
            phase: WritePhase::Start,
            leadership: None,
            index: None,
            term: LeaseTerm::new(0),
            receipt: None,
        }
    }

    /// Returns the current phase.
    #[must_use]
    pub const fn phase(&self) -> WritePhase {
        self.phase
    }

    /// Returns the terminal receipt.
    #[must_use]
    pub const fn receipt(&self) -> Option<TerminalReceipt> {
        self.receipt
    }

    /// Returns the leadership token.
    #[must_use]
    pub const fn leadership(&self) -> Option<Leadership> {
        self.leadership
    }

    /// Returns the record index after an append.
    #[must_use]
    pub const fn index(&self) -> Option<LogIndex> {
        self.index
    }

    /// Acquires leadership and checks for an existing receipt.
    ///
    /// # Errors
    ///
    /// Returns a retryable driver error. A named rejection closes the attempt
    /// with a terminal receipt.
    pub fn start<D: ReplicationDriver>(
        &mut self,
        driver: &mut D,
        now: Tick,
    ) -> Result<WritePhase, ReplError> {
        if self.phase != WritePhase::Start {
            return Ok(self.phase);
        }
        let token = match driver.acquire(now) {
            Ok(token) => token,
            Err(error) => return self.close_or_fail(error),
        };
        self.leadership = Some(token);
        self.term = token.term();
        if let Some(receipt) = driver.receipt(&self.request.idempotency) {
            driver.apply(driver.commit_index())?;
            self.receipt = Some(receipt);
            self.phase = WritePhase::Terminal;
            return Ok(self.phase);
        }
        Ok(self.phase)
    }

    /// Advances one phase.
    ///
    /// # Errors
    ///
    /// Returns a retryable driver error. A named rejection closes the attempt
    /// with a terminal receipt.
    pub fn step<D: ReplicationDriver>(
        &mut self,
        driver: &mut D,
        now: Tick,
    ) -> Result<WritePhase, ReplError> {
        match self.phase {
            WritePhase::Start => {
                let token = self.leadership.ok_or(ReplError::InvariantViolation {
                    reason: "the write attempt has no leadership",
                })?;
                let payload = EntryPayload::Data {
                    idempotency: self.request.idempotency.clone(),
                    key: self.request.key.clone(),
                    value: self.request.value.clone(),
                };
                match driver.append(&token, payload, now) {
                    Ok(append) => {
                        self.index = Some(append.index());
                        self.term = append.term();
                        self.phase = if append.is_committed() {
                            WritePhase::Committed
                        } else {
                            WritePhase::Appended
                        };
                        Ok(self.phase)
                    }
                    Err(error) => self.close_or_fail(error),
                }
            }
            WritePhase::Appended => {
                let token = self.leadership.ok_or(ReplError::InvariantViolation {
                    reason: "the write attempt has no leadership",
                })?;
                if let Err(error) = driver.advance(&token, now) {
                    return self.close_or_fail(error);
                }
                let index = self.index.ok_or(ReplError::InvariantViolation {
                    reason: "the appended write has no index",
                })?;
                if driver.commit_index() >= index {
                    self.phase = WritePhase::Committed;
                }
                Ok(self.phase)
            }
            WritePhase::Committed => {
                if let Err(error) = driver.apply(driver.commit_index()) {
                    return self.close_or_fail(error);
                }
                self.phase = WritePhase::Applied;
                Ok(self.phase)
            }
            WritePhase::Applied => {
                let receipt = driver.receipt(&self.request.idempotency).ok_or(
                    ReplError::InvariantViolation {
                        reason: "the applied write has no committed receipt",
                    },
                )?;
                self.receipt = Some(receipt);
                self.phase = WritePhase::Terminal;
                Ok(self.phase)
            }
            WritePhase::Terminal => Ok(self.phase),
        }
    }

    /// Runs the attempt to a terminal receipt.
    ///
    /// # Errors
    ///
    /// Returns a retryable driver error. Returns
    /// [`ReplError::TierUnavailable`] when the commit does not advance inside
    /// the step budget.
    pub fn run<D: ReplicationDriver>(
        &mut self,
        driver: &mut D,
        now: Tick,
    ) -> Result<TerminalReceipt, ReplError> {
        self.start(driver, now)?;
        for _ in 0..MAX_STEPS {
            if self.phase == WritePhase::Terminal {
                break;
            }
            self.step(driver, now)?;
        }
        if self.phase == WritePhase::Terminal {
            return self.receipt.ok_or(ReplError::InvariantViolation {
                reason: "the terminal attempt has no receipt",
            });
        }
        Err(ReplError::TierUnavailable { tier: "commit" })
    }

    fn close_or_fail(&mut self, error: ReplError) -> Result<WritePhase, ReplError> {
        match TerminalReceipt::from_rejection(self.term, error) {
            Ok(receipt) => {
                self.receipt = Some(receipt);
                self.phase = WritePhase::Terminal;
                Ok(self.phase)
            }
            Err(retryable) => Err(retryable),
        }
    }
}

/// Runs one write to a terminal receipt.
///
/// # Errors
///
/// Returns a retryable driver error. A named rejection is a successful return
/// with a rejected receipt.
pub fn submit<D: ReplicationDriver>(
    driver: &mut D,
    request: &WriteRequest,
    now: Tick,
) -> Result<TerminalReceipt, ReplError> {
    WriteAttempt::new(request.clone()).run(driver, now)
}
