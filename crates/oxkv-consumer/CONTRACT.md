# Reference effect consumer contract

This document describes the state machine in `oxkv-consumer`. The crate is a
library. The deployment supplies the event transport, the provider client, the
clock, and the limits.

## Scope

The consumer performs one effect for one committed business event. It keeps
three kinds of durable state:

1. A cursor for each key.
2. A completion record for each key and event identity.
3. A dead-letter record for each terminal event.

The consumer does not write the authority. The handler receives no authority
handle. A data change that the effect needs becomes a new request with its own
key and base version.

## Per-key partition

Work partitions by key. Each key has one cursor and at most one in-flight
event. An unacknowledged event blocks its key only. Other keys continue.

A per-shard cursor is not the contract. A slow or poisoned key must not delay
another key on the same shard.

## Delivery steps

1. `begin` validates the event and reserves the key.
2. The deployment runs the effect with the prepared attempt.
3. `ack` writes the completion record and advances the key cursor.
4. `nack` schedules a retry or writes a dead-letter record.

`deliver` runs the three steps with one in-process handler.

## Idempotency

Delivery is at least once. The consumer checks the completion record before an
effect. A duplicate delivery returns `duplicate` and runs no effect.

The provider idempotency key is `oxkv/<rule_id>/<event_id>`. The event id
derives from the shard, the key, the commit version, the rule identity, and the
emission index. The same event produces the same key on every delivery.

The crate does not offer exactly-once delivery. The provider key narrows the
duplicate window. The completion record is the audit record.

## Retry

A retryable handler failure returns `consumer.handler_failed`. The consumer
schedules a retry with a deterministic exponential backoff.

- The attempt cap is `RetryPolicy::max_attempts`.
- The time cap is `RetryPolicy::deadline_ms`.
- The delay grows from `base_delay_ms` to `cap_delay_ms`.
- The jitter comes from the event identity, so a replay produces the same
  delay.
- A provider delay hint raises the delay up to the cap.

A retry does not advance the key cursor.

## Terminal failure

A terminal failure writes one dead-letter record and advances the key cursor in
the same step. The terminal causes are:

| Cause | Error code |
| --- | --- |
| Unknown envelope version | `event.unknown_version` |
| Unsupported payload schema | `event.poison` |
| Malformed payload | `event.poison` |
| Missing required payload field | `event.poison` |
| Exhausted attempt budget | `consumer.dead_lettered` |
| Exhausted time budget | `consumer.dead_lettered` |
| Permanent handler failure | `consumer.dead_lettered` |
| Handler panic | `consumer.dead_lettered` |

The record holds the event, the attempt count, the timestamps, the cause, and
the record version. The record holds no secret and no provider key. The
`Debug` form redacts the payload text.

## Backlog and retention

The backlog budget is the maximum number of in-flight events. A delivery above
the budget returns `consumer.too_slow`.

The completion retention floor is injected. A cursor below the floor returns
`consumer.cursor_expired`. The caller must backfill from a snapshot.

## Lag and visibility

The consumer exposes:

- the cursor for each key,
- the lag for each key against a high watermark,
- the last attempt count for each key,
- the in-flight depth,
- the dead-letter depth.

## Redrive

An operator can list the dead-letter records and take one record. `redrive`
runs one fresh attempt with the original key. It returns a `RedriveAudit`
record with the event id, the key, the operator, the time, and the attempt
number.

A failed redrive writes a new dead-letter record. A redrive can return
`blocked` while another event is in flight on the same key.

## Integration

Essay 24 supplies the wire envelope. `EventEnvelope` is the consumer-side view
of that envelope. The field set follows section 9.9 of the 1.0 specification.

Essay 28 reads `dead_letters`, `take_dead_letter`, and `redrive` for the
operator path. The audit record is the redrive record.
