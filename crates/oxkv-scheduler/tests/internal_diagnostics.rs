//! Tests for the scheduler internal diagnostic.

use oxkv_contracts::InternalDiagnostic;
use oxkv_contracts::version::ShardId;
use oxkv_scheduler::TransitionError;

#[test]
fn a_transition_error_names_the_scheduler_transition() {
    let error = TransitionError::UnknownShard {
        shard_id: ShardId::new(vec![1]),
    };
    assert_eq!(error.code(), "internal.invariant_violation");
    assert_eq!(error.diagnostic(), InternalDiagnostic::SchedulerTransition);
    assert_eq!(error.diagnostic().diagnostic_name(), "scheduler_transition");
}
