//! Freeze state transitions.

use oxkv_freeze::{
    FreezeState, FreezeTransitionError, Phase, Release, state_at_release, transition,
};

#[test]
fn draft_moves_to_candidate_then_frozen() {
    assert_eq!(
        transition(FreezeState::Draft, FreezeState::Candidate),
        Ok(FreezeState::Candidate)
    );
    assert_eq!(
        transition(FreezeState::Candidate, FreezeState::Frozen),
        Ok(FreezeState::Frozen)
    );
}

#[test]
fn draft_moves_directly_to_frozen_at_1_0() {
    assert_eq!(
        transition(FreezeState::Draft, FreezeState::Frozen),
        Ok(FreezeState::Frozen)
    );
}

#[test]
fn a_frozen_state_is_final_inside_1_x() {
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
fn a_candidate_cannot_return_to_draft() {
    assert_eq!(
        transition(FreezeState::Candidate, FreezeState::Draft),
        Err(FreezeTransitionError::CandidateCannotRevert {
            to: FreezeState::Draft
        })
    );
}

#[test]
fn each_state_repeats_without_change() {
    for state in FreezeState::ALL {
        assert_eq!(transition(*state, *state), Ok(*state));
    }
}

#[test]
fn phase_b_and_phase_c_items_are_candidates() {
    for phase in Phase::ALL {
        let expected = if matches!(phase, Phase::B | Phase::C) {
            FreezeState::Candidate
        } else {
            FreezeState::Draft
        };
        assert_eq!(phase.initial_state(), expected, "phase {phase}");
    }
}

#[test]
fn version_1_0_freezes_every_item() {
    for phase in Phase::ALL {
        assert_eq!(state_at_release(*phase, 1), FreezeState::Frozen);
        assert_eq!(state_at_release(*phase, 0), phase.initial_state());
    }
    assert!(Release::one_zero().freezes_all());
}

#[test]
fn state_flags_match_the_contract() {
    assert!(FreezeState::Draft.may_break());
    assert!(FreezeState::Candidate.may_break());
    assert!(!FreezeState::Frozen.may_break());
    assert!(FreezeState::Candidate.needs_version_on_break());
    assert!(FreezeState::Candidate.needs_migration_on_break());
    assert!(!FreezeState::Draft.needs_version_on_break());
    assert!(!FreezeState::Draft.needs_migration_on_break());
}
