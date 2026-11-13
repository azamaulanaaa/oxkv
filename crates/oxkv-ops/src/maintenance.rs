//! Maintenance windows and the write gate.
//!
//! Time is an injected [`Timestamp`]. The crate never reads a clock.
//! A frozen shard rejects writes. A read still succeeds.

use std::collections::BTreeSet;

use oxkv_contracts::version::ShardId;

use crate::error::OpsError;

/// A monotonic time value in milliseconds.
///
/// The caller supplies the value. The crate does not read a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Timestamp(u64);

impl Timestamp {
    /// Creates a time value.
    #[must_use]
    pub const fn new(millis: u64) -> Self {
        Self(millis)
    }

    /// Returns the time value in milliseconds.
    #[must_use]
    pub const fn millis(self) -> u64 {
        self.0
    }

    /// Adds one duration in milliseconds.
    #[must_use]
    pub const fn plus(self, millis: u64) -> Self {
        Self(self.0.saturating_add(millis))
    }
}

impl core::fmt::Display for Timestamp {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, formatter)
    }
}

/// One published maintenance interval for one shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceWindow {
    /// The shard that the window covers.
    pub shard_id: ShardId,
    /// The first time value inside the window.
    pub opens_at: Timestamp,
    /// The first time value outside the window.
    pub closes_at: Timestamp,
}

impl MaintenanceWindow {
    /// Creates a maintenance window.
    #[must_use]
    pub const fn new(shard_id: ShardId, opens_at: Timestamp, closes_at: Timestamp) -> Self {
        Self {
            shard_id,
            opens_at,
            closes_at,
        }
    }

    /// Returns true when the time value is inside the window.
    #[must_use]
    pub const fn contains(&self, now: Timestamp) -> bool {
        self.opens_at.0 <= now.0 && now.0 < self.closes_at.0
    }

    /// Returns true when the window is valid.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.opens_at.0 < self.closes_at.0
    }
}

/// The freeze state of the shard set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FreezeState {
    frozen: BTreeSet<ShardId>,
    global: bool,
}

impl FreezeState {
    /// Creates an open freeze state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Freezes one shard.
    pub fn freeze_shard(&mut self, shard_id: ShardId) {
        self.frozen.insert(shard_id);
    }

    /// Releases one shard.
    pub fn thaw_shard(&mut self, shard_id: &ShardId) {
        self.frozen.remove(shard_id);
    }

    /// Freezes every shard.
    pub fn freeze_all(&mut self) {
        self.global = true;
    }

    /// Releases every shard.
    pub fn thaw_all(&mut self) {
        self.global = false;
        self.frozen.clear();
    }

    /// Returns true when the shard is frozen.
    #[must_use]
    pub fn is_frozen(&self, shard_id: &ShardId) -> bool {
        self.global || self.frozen.contains(shard_id)
    }

    /// Returns true when every shard is frozen.
    #[must_use]
    pub const fn is_globally_frozen(&self) -> bool {
        self.global
    }

    /// Checks one write admission.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::MaintenanceFrozen`] when the shard is frozen.
    pub fn admit_write(&self, shard_id: &ShardId) -> Result<(), OpsError> {
        if self.is_frozen(shard_id) {
            return Err(OpsError::MaintenanceFrozen {
                shard_id: shard_id.clone(),
            });
        }
        Ok(())
    }

    /// Checks one read admission.
    ///
    /// A freeze never blocks a read. Reads continue from a replica.
    pub const fn admit_read() {}
}

/// A write gate over a set of published windows and a freeze state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteGate {
    windows: Vec<MaintenanceWindow>,
    freeze: FreezeState,
}

impl WriteGate {
    /// Creates an empty write gate.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes one maintenance window.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::MigrationIncomplete`] when the interval is empty.
    pub fn publish_window(&mut self, window: MaintenanceWindow) -> Result<(), OpsError> {
        if !window.is_valid() {
            return Err(OpsError::MigrationIncomplete {
                step: "maintenance_window",
            });
        }
        self.windows.push(window);
        Ok(())
    }

    /// Returns the freeze state.
    #[must_use]
    pub const fn freeze(&self) -> &FreezeState {
        &self.freeze
    }

    /// Returns a mutable freeze state.
    pub fn freeze_mut(&mut self) -> &mut FreezeState {
        &mut self.freeze
    }

    /// Returns true when a published window covers the shard at this time.
    #[must_use]
    pub fn in_window(&self, shard_id: &ShardId, now: Timestamp) -> bool {
        self.windows
            .iter()
            .any(|window| &window.shard_id == shard_id && window.contains(now))
    }

    /// Checks one write admission.
    ///
    /// The gate rejects a write when the shard is frozen or when a published
    /// window covers the shard.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::MaintenanceFrozen`] when the write is refused.
    pub fn admit_write(&self, shard_id: &ShardId, now: Timestamp) -> Result<(), OpsError> {
        self.freeze.admit_write(shard_id)?;
        if self.in_window(shard_id, now) {
            return Err(OpsError::MaintenanceFrozen {
                shard_id: shard_id.clone(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard() -> ShardId {
        ShardId::new(vec![1])
    }

    #[test]
    fn a_window_has_half_open_bounds() {
        let window = MaintenanceWindow::new(shard(), Timestamp::new(10), Timestamp::new(20));
        assert!(!window.contains(Timestamp::new(9)));
        assert!(window.contains(Timestamp::new(10)));
        assert!(window.contains(Timestamp::new(19)));
        assert!(!window.contains(Timestamp::new(20)));
        assert!(window.is_valid());
        assert!(
            !MaintenanceWindow::new(shard(), Timestamp::new(20), Timestamp::new(20)).is_valid()
        );
    }

    #[test]
    fn a_frozen_shard_rejects_a_write() {
        let mut freeze = FreezeState::new();
        freeze.freeze_shard(shard());
        let error = freeze
            .admit_write(&shard())
            .expect_err("the write is refused");
        assert_eq!(error.code(), "shard.frozen_for_maintenance");
        FreezeState::admit_read();
    }

    #[test]
    fn a_global_freeze_rejects_every_shard() {
        let mut freeze = FreezeState::new();
        freeze.freeze_all();
        assert!(freeze.is_globally_frozen());
        assert!(freeze.admit_write(&ShardId::new(vec![9])).is_err());
        freeze.thaw_all();
        assert!(freeze.admit_write(&ShardId::new(vec![9])).is_ok());
    }

    #[test]
    fn a_published_window_rejects_a_write() {
        let mut gate = WriteGate::new();
        gate.publish_window(MaintenanceWindow::new(
            shard(),
            Timestamp::new(10),
            Timestamp::new(20),
        ))
        .expect("the window is valid");
        assert!(gate.admit_write(&shard(), Timestamp::new(15)).is_err());
        assert!(gate.admit_write(&shard(), Timestamp::new(20)).is_ok());
        assert!(
            gate.publish_window(MaintenanceWindow::new(
                shard(),
                Timestamp::new(20),
                Timestamp::new(20)
            ))
            .is_err()
        );
    }
}
