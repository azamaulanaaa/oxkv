//! Freeze states and the transition check.
//!
//! A surface starts as [`FreezeState::Draft`]. Phase B and Phase C items become
//! [`FreezeState::Candidate`]. Version 1.0 makes every register row
//! [`FreezeState::Frozen`].
//!
//! A frozen surface cannot change inside 1.x. A candidate may change only with
//! a new version and a migration note. A draft surface may change.

use core::fmt;

/// The freeze state of one contract or annex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FreezeState {
    /// The surface may break.
    Draft,
    /// The surface has a version. A break needs a new version and a migration
    /// note.
    Candidate,
    /// The surface must not break inside 1.x.
    Frozen,
}

impl FreezeState {
    /// Every freeze state in release order.
    pub const ALL: &'static [Self] = &[Self::Draft, Self::Candidate, Self::Frozen];

    /// Returns the lowercase state name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Candidate => "candidate",
            Self::Frozen => "frozen",
        }
    }

    /// Returns true when the state is frozen.
    #[must_use]
    pub const fn is_frozen(self) -> bool {
        matches!(self, Self::Frozen)
    }

    /// Returns true when the surface may break without a new version.
    #[must_use]
    pub const fn may_break(self) -> bool {
        !self.is_frozen()
    }

    /// Returns true when a break needs a new version.
    #[must_use]
    pub const fn needs_version_on_break(self) -> bool {
        matches!(self, Self::Candidate | Self::Frozen)
    }

    /// Returns true when a break needs a migration note.
    #[must_use]
    pub const fn needs_migration_on_break(self) -> bool {
        matches!(self, Self::Candidate | Self::Frozen)
    }
}

impl fmt::Display for FreezeState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// An error from a freeze state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeTransitionError {
    /// A frozen surface cannot change state inside 1.x.
    FrozenIsFinal {
        /// The current state.
        from: FreezeState,
        /// The requested state.
        to: FreezeState,
    },
    /// A candidate cannot return to draft.
    CandidateCannotRevert {
        /// The requested state.
        to: FreezeState,
    },
}

impl fmt::Display for FreezeTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrozenIsFinal { from, to } => write!(
                formatter,
                "The freeze state {from} cannot change to {to}. A frozen surface is final inside 1.x."
            ),
            Self::CandidateCannotRevert { to } => write!(
                formatter,
                "The freeze state candidate cannot change to {to}. A candidate needs a new version."
            ),
        }
    }
}

impl std::error::Error for FreezeTransitionError {}

/// Moves one surface from one freeze state to another.
///
/// # Errors
///
/// Returns [`FreezeTransitionError::FrozenIsFinal`] when the current state is
/// frozen. Returns [`FreezeTransitionError::CandidateCannotRevert`] when a
/// candidate requests the draft state.
///
/// # Examples
///
/// ```
/// use oxkv_freeze::{transition, FreezeState};
///
/// assert_eq!(
///     transition(FreezeState::Draft, FreezeState::Candidate),
///     Ok(FreezeState::Candidate)
/// );
/// ```
pub fn transition(
    from: FreezeState,
    to: FreezeState,
) -> Result<FreezeState, FreezeTransitionError> {
    if from == to {
        return Ok(to);
    }
    match from {
        FreezeState::Draft => Ok(to),
        FreezeState::Candidate => {
            if to == FreezeState::Frozen {
                Ok(to)
            } else {
                Err(FreezeTransitionError::CandidateCannotRevert { to })
            }
        }
        FreezeState::Frozen => Err(FreezeTransitionError::FrozenIsFinal { from, to }),
    }
}

/// The release phase of one work item.
///
/// The phases follow the early plan. The wave table in the consolidated
/// specification replaces the linear phase list as the release gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Phase {
    /// Storage, keys, and the shard map.
    A,
    /// Manifest, policy, and versioned writes.
    B,
    /// Scope, wire, replica, hydration, projection, and search.
    C,
    /// Offline reads and search.
    D,
    /// Replication and bounded staleness.
    E,
    /// Events, generated RPC, rule lifecycle, and operations.
    F,
    /// Backup, restore, migration, security, and qualification.
    G,
    /// Falsifiers and the measurement gate.
    H,
    /// The 1.0 freeze.
    I,
}

impl Phase {
    /// Every phase.
    pub const ALL: &'static [Self] = &[
        Self::A,
        Self::B,
        Self::C,
        Self::D,
        Self::E,
        Self::F,
        Self::G,
        Self::H,
        Self::I,
    ];

    /// Returns the phase letter.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::E => "E",
            Self::F => "F",
            Self::G => "G",
            Self::H => "H",
            Self::I => "I",
        }
    }

    /// Returns the initial freeze state of an item in this phase.
    ///
    /// Phase B and Phase C items become candidates. Every other phase item
    /// starts as draft.
    #[must_use]
    pub const fn initial_state(self) -> FreezeState {
        match self {
            Self::B | Self::C => FreezeState::Candidate,
            Self::A | Self::D | Self::E | Self::F | Self::G | Self::H | Self::I => {
                FreezeState::Draft
            }
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One release of the contract register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Release {
    /// The major version.
    pub major: u16,
    /// The minor version.
    pub minor: u16,
    /// The freeze state that this release applies to every register row.
    pub state: FreezeState,
}

impl Release {
    /// The 1.0 release. It freezes every core contract and every annex.
    pub const ONE_ZERO: Self = Self {
        major: 1,
        minor: 0,
        state: FreezeState::Frozen,
    };

    /// Returns the 1.0 release.
    #[must_use]
    pub const fn one_zero() -> Self {
        Self::ONE_ZERO
    }

    /// Returns true when this release freezes every register row.
    #[must_use]
    pub const fn freezes_all(self) -> bool {
        self.state.is_frozen()
    }
}

impl fmt::Display for Release {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{} ({})", self.major, self.minor, self.state)
    }
}

/// Returns the freeze state of one item at a release.
///
/// A major release of 1 or more freezes the item. A release before 1.0 keeps
/// the initial phase state.
#[must_use]
pub const fn state_at_release(phase: Phase, major: u16) -> FreezeState {
    if major >= 1 {
        FreezeState::Frozen
    } else {
        phase.initial_state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_moves_to_candidate_and_frozen() {
        assert_eq!(
            transition(FreezeState::Draft, FreezeState::Candidate),
            Ok(FreezeState::Candidate)
        );
        assert_eq!(
            transition(FreezeState::Draft, FreezeState::Frozen),
            Ok(FreezeState::Frozen)
        );
    }

    #[test]
    fn candidate_moves_to_frozen_only() {
        assert_eq!(
            transition(FreezeState::Candidate, FreezeState::Frozen),
            Ok(FreezeState::Frozen)
        );
        assert_eq!(
            transition(FreezeState::Candidate, FreezeState::Draft),
            Err(FreezeTransitionError::CandidateCannotRevert {
                to: FreezeState::Draft
            })
        );
    }

    #[test]
    fn frozen_is_final() {
        for to in [FreezeState::Draft, FreezeState::Candidate] {
            assert_eq!(
                transition(FreezeState::Frozen, to),
                Err(FreezeTransitionError::FrozenIsFinal {
                    from: FreezeState::Frozen,
                    to
                })
            );
        }
    }

    #[test]
    fn same_state_is_idempotent() {
        for state in FreezeState::ALL {
            assert_eq!(transition(*state, *state), Ok(*state));
        }
    }

    #[test]
    fn phase_b_and_c_are_candidates() {
        assert_eq!(Phase::B.initial_state(), FreezeState::Candidate);
        assert_eq!(Phase::C.initial_state(), FreezeState::Candidate);
        assert_eq!(Phase::A.initial_state(), FreezeState::Draft);
    }

    #[test]
    fn one_zero_freezes_all() {
        assert!(Release::one_zero().freezes_all());
        assert_eq!(state_at_release(Phase::B, 1), FreezeState::Frozen);
        assert_eq!(state_at_release(Phase::B, 0), FreezeState::Candidate);
    }
}
