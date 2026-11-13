//! Compaction control as a pure policy.
//!
//! Compaction has a byte-per-second budget and a files-per-hour budget. A pin
//! blocks compaction. A pause has an expiry. A quota threshold ends a
//! forgotten pause. A lost pin returns `ops.backup_pin_lost`.

use crate::backup::PinLedger;
use crate::error::OpsError;
use crate::maintenance::Timestamp;

/// The compaction budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionBudget {
    /// The byte budget for one second.
    pub bytes_per_second: u64,
    /// The file budget for one hour.
    pub files_per_hour: u64,
}

impl CompactionBudget {
    /// Creates a compaction budget.
    #[must_use]
    pub const fn new(bytes_per_second: u64, files_per_hour: u64) -> Self {
        Self {
            bytes_per_second,
            files_per_hour,
        }
    }
}

impl Default for CompactionBudget {
    fn default() -> Self {
        Self::new(1_048_576, 60)
    }
}

/// The mutable state of the compaction engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionState {
    /// The number of input files.
    pub input_files: u64,
    /// The backlog in bytes.
    pub backlog_bytes: u64,
    /// The age of the oldest input in milliseconds.
    pub oldest_input_age_millis: u64,
    /// The bytes rewritten since the start.
    pub bytes_rewritten: u64,
    /// The last pass time.
    pub last_pass_at: Timestamp,
}

impl CompactionState {
    /// Creates a compaction state.
    #[must_use]
    pub const fn new(
        input_files: u64,
        backlog_bytes: u64,
        oldest_input_age_millis: u64,
        now: Timestamp,
    ) -> Self {
        Self {
            input_files,
            backlog_bytes,
            oldest_input_age_millis,
            bytes_rewritten: 0,
            last_pass_at: now,
        }
    }

    /// Returns one report record.
    #[must_use]
    pub const fn report(&self) -> CompactionReport {
        CompactionReport {
            backlog_bytes: self.backlog_bytes,
            oldest_input_age_millis: self.oldest_input_age_millis,
            bytes_rewritten: self.bytes_rewritten,
        }
    }
}

/// One compaction report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionReport {
    /// The backlog in bytes.
    pub backlog_bytes: u64,
    /// The age of the oldest input in milliseconds.
    pub oldest_input_age_millis: u64,
    /// The bytes rewritten since the start.
    pub bytes_rewritten: u64,
}

/// The result of one compaction pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionPass {
    /// The policy is paused.
    Paused,
    /// A backup pin blocks deletion.
    BlockedByPin,
    /// There is no work.
    Idle,
    /// The pass rewrote files and bytes.
    Ran {
        /// The number of input files that the pass rewrote.
        files: u64,
        /// The bytes that the pass rewrote.
        bytes: u64,
    },
}

/// The compaction control policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionPolicy {
    /// The budget.
    pub budget: CompactionBudget,
    /// The pause expiry.
    pub pause_until: Option<Timestamp>,
    /// The backlog threshold that ends a forgotten pause. A value of zero
    /// disables the threshold.
    pub quota_threshold_bytes: u64,
}

impl CompactionPolicy {
    /// Creates a compaction policy.
    #[must_use]
    pub const fn new(budget: CompactionBudget, quota_threshold_bytes: u64) -> Self {
        Self {
            budget,
            pause_until: None,
            quota_threshold_bytes,
        }
    }

    /// Pauses compaction until the expiry.
    pub fn pause(&mut self, now: Timestamp, duration_millis: u64) -> Timestamp {
        let until = now.plus(duration_millis);
        self.pause_until = Some(until);
        until
    }

    /// Resumes compaction.
    pub fn resume(&mut self) {
        self.pause_until = None;
    }

    /// Returns true when compaction is paused at this time and backlog.
    #[must_use]
    pub fn is_paused(&self, now: Timestamp, backlog_bytes: u64) -> bool {
        let Some(until) = self.pause_until else {
            return false;
        };
        if self.quota_threshold_bytes != 0 && backlog_bytes >= self.quota_threshold_bytes {
            return false;
        }
        now < until
    }

    /// Runs one compaction pass.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::BackupPinLost`] when a pin is lost.
    pub fn run_pass(
        &self,
        now: Timestamp,
        pins: &PinLedger,
        state: &mut CompactionState,
    ) -> Result<CompactionPass, OpsError> {
        if let Some(pin) = pins.lost_pin() {
            return Err(OpsError::BackupPinLost {
                pin: pin.to_owned(),
            });
        }
        if self.is_paused(now, state.backlog_bytes) {
            return Ok(CompactionPass::Paused);
        }
        if pins.any_held() {
            return Ok(CompactionPass::BlockedByPin);
        }
        if state.backlog_bytes == 0 || state.input_files == 0 {
            return Ok(CompactionPass::Idle);
        }
        let elapsed = now.millis().saturating_sub(state.last_pass_at.millis());
        let byte_budget = elapsed
            .saturating_mul(self.budget.bytes_per_second)
            .saturating_div(1_000)
            .max(1);
        let file_budget = elapsed
            .saturating_mul(self.budget.files_per_hour)
            .saturating_div(3_600_000)
            .max(1);
        let bytes = state.backlog_bytes.min(byte_budget);
        let files = state.input_files.min(file_budget);
        state.backlog_bytes = state.backlog_bytes.saturating_sub(bytes);
        state.input_files = state.input_files.saturating_sub(files);
        state.bytes_rewritten = state.bytes_rewritten.saturating_add(bytes);
        state.last_pass_at = now;
        Ok(CompactionPass::Ran { files, bytes })
    }
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self::new(CompactionBudget::default(), 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::version::ShardId;

    use crate::backup::{BackupPin, PinLedger};

    fn held_pin() -> PinLedger {
        let mut ledger = PinLedger::new();
        ledger.hold(&BackupPin::new("pin-1", vec![ShardId::new(vec![1])]));
        ledger
    }

    #[test]
    fn a_pause_holds_the_file_count() {
        let mut policy = CompactionPolicy::default();
        let mut state = CompactionState::new(10, 10_000, 100, Timestamp::new(0));
        policy.pause(Timestamp::new(0), 600_000);
        for time in [0, 60_000, 600_000 - 1] {
            let pass = policy
                .run_pass(Timestamp::new(time), &PinLedger::new(), &mut state)
                .expect("the pass runs");
            assert_eq!(pass, CompactionPass::Paused);
            assert_eq!(state.input_files, 10);
        }
    }

    #[test]
    fn an_expired_pause_reduces_the_file_count() {
        let mut policy = CompactionPolicy::default();
        let mut state = CompactionState::new(10, 10_000, 100, Timestamp::new(0));
        policy.pause(Timestamp::new(0), 1_000);
        let pass = policy
            .run_pass(Timestamp::new(2_000), &PinLedger::new(), &mut state)
            .expect("the pass runs");
        assert!(matches!(pass, CompactionPass::Ran { .. }));
        assert!(state.input_files < 10);
    }

    #[test]
    fn a_resume_reduces_the_file_count() {
        let mut policy = CompactionPolicy::default();
        let mut state = CompactionState::new(10, 10_000, 100, Timestamp::new(0));
        policy.pause(Timestamp::new(0), 600_000);
        policy.resume();
        let pass = policy
            .run_pass(Timestamp::new(1_000), &PinLedger::new(), &mut state)
            .expect("the pass runs");
        assert!(matches!(pass, CompactionPass::Ran { .. }));
    }

    #[test]
    fn a_quota_threshold_ends_a_forgotten_pause() {
        let mut policy = CompactionPolicy::new(CompactionBudget::default(), 5_000);
        policy.pause(Timestamp::new(0), 600_000);
        assert!(policy.is_paused(Timestamp::new(1), 4_999));
        assert!(!policy.is_paused(Timestamp::new(1), 5_000));
    }

    #[test]
    fn a_pin_blocks_compaction() {
        let policy = CompactionPolicy::default();
        let mut state = CompactionState::new(10, 10_000, 100, Timestamp::new(0));
        let pass = policy
            .run_pass(Timestamp::new(1_000), &held_pin(), &mut state)
            .expect("the pass runs");
        assert_eq!(pass, CompactionPass::BlockedByPin);
        assert_eq!(state.input_files, 10);
    }

    #[test]
    fn a_lost_pin_returns_the_named_error() {
        let policy = CompactionPolicy::default();
        let mut state = CompactionState::new(10, 10_000, 100, Timestamp::new(0));
        let mut pins = held_pin();
        pins.lose("pin-1");
        let error = policy
            .run_pass(Timestamp::new(1_000), &pins, &mut state)
            .expect_err("the pin is lost");
        assert_eq!(error.code(), "ops.backup_pin_lost");
    }

    #[test]
    fn an_idle_pass_changes_nothing() {
        let policy = CompactionPolicy::default();
        let mut state = CompactionState::new(0, 0, 0, Timestamp::new(0));
        let pass = policy
            .run_pass(Timestamp::new(1_000), &PinLedger::new(), &mut state)
            .expect("the pass runs");
        assert_eq!(pass, CompactionPass::Idle);
        assert_eq!(state.report().bytes_rewritten, 0);
    }
}
