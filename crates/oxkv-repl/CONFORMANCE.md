# Replication seam conformance

This crate implements essay 21 and the replication rows of specification
section 9.8. One driver instance serves one shard. A shard uses one ownership
mechanism. The two drivers are alternatives, not a pair of live authorities.

| Requirement | Implementation | Test |
| --- | --- | --- |
| One driver interface | `driver::ReplicationDriver` | Both drivers pass the same tests. |
| One fence | `fence::Fence`, `fence::Leadership` | `a_fence_increases_and_never_repeats` |
| Stale writer | `replication.stale_epoch` | `a_stale_lease_writer_fails_with_a_named_error` |
| Expired lease | `replication.lease_expired` | `an_expired_lease_fails_with_a_named_error` |
| Held lease | `replication.lease_held` | `a_held_lease_fails_with_a_named_error` |
| Durable append | `log::Log` is the durable boundary | Leader-kill tests |
| One terminal receipt | `write::TerminalReceipt` | `a_receipt_is_committed_or_rejected_and_never_both` |
| One committed state | Commit and apply are separate steps | `the_lease_kill_points_converge_to_one_committed_state`, `the_raft_kill_points_converge_to_one_committed_state` |
| No split brain | The higher term wins | `two_raft_leaders_do_not_commit_two_states`, `two_lease_holders_do_not_commit_two_states` |
| Read barrier | `read::ReadBarrier` | `a_read_past_the_applied_index_fails_with_a_named_error` |
| Bounded staleness | `read::ReadFreshness` | `a_read_over_its_bound_fails_with_a_named_error` |
| Deterministic election | Lowest live member campaigns first | `a_raft_election_is_deterministic` |
| Driver parity | One operation log yields one logical result | `both_drivers_return_the_same_logical_result` |
| Repeated kills | One commit per idempotency key | `repeated_kills_duplicate_no_commit` |

## Read modes

The default read mode is `AtLeast(V)`. A leader read confirms the authority
lease or the leader term before it serves. A follower read uses an explicit
log-index pin. A read above the applied index fails with `read.behind_commit`.
A read over its declared bound fails with `read.stale`.

## Not in this crate

A membership change is a log entry in the driver model. This crate does not
expose a membership-change operation. The member set is fixed at construction.
A later essay adds the one-member-at-a-time change on top of the `Config` entry
that this crate already defines.
