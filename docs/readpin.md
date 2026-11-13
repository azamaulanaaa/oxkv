# Read pins and bounded staleness

This document describes the `oxkv-readpin` crate. The crate implements the
read contract of essay 23 and annex `read-pin/v1`. The consolidated
specification wins on conflict.

## Four lifetimes

| Type | Lifetime | Expiry error |
| --- | --- | --- |
| `AuthorityReadPin` | Authority compaction and a time bound | `read.pin_expired`, `read.pin_unreadable` |
| `ReplicaLocalPin` | The applied version vector of one replica | None. It stays readable offline. |
| `ResumeCursor` | Authority diff retention | `sync.cursor_expired` plus a snapshot offer |
| `ReceiptRetention` | Receipt retention | `receipt.expired` |

The four types are distinct. Authority retention does not expire a local pin.
Receipt retention does not expire a read pin.

## Read modes

`SessionMode::BoundedStaleness` is the default. The replica serves its applied
version vector. The response reports the served vector and the staleness
measure. The measure has two axes: version lag in commits and replication
silence in injected milliseconds. The read fails with `read.stale` when either
axis exceeds its budget. The failure carries no data.

`SessionMode::ReadYourWrites` is explicit. The client raises the session
minimum to the commit version of a terminal receipt. The minimum is a version
vector. A scalar cannot construct a `SessionToken`.

A replica that cannot apply the minimum returns `ReadOutcome::Blocked`. The
caller can apply diffs and retry. The caller can also call `into_result` to get
`read.behind_commit`. The minimum applies to the shards in the read pin. A
shard outside the pin does not block the read.

## Admission order

`evaluate` checks these conditions in order:

1. The authority pin is readable.
2. The replica is hydrated.
3. The target page is resident, when the request requires residency.
4. Read-your-writes has applied the session minimum.
5. The replica can serve the pin.
6. The measured staleness is inside the declared bound.

A failure returns a named error and no data.

## Pin modes

`PinMode::AtLeast` is the default. It serves the pin version or a newer
version. `PinMode::Exactly` serves the pin version or a named error. An exact
read never reports a version above the pin.

## Result modes

`ResultMode::Exact` is the default. Every targeted shard must serve the pin.
`ResultMode::Coverage` is opt-in. It returns a `CoverageDescriptor` with the
answered shards, the missing shards, and the served vector. A coverage result
does not claim exactness.

## Use by other essays

- Essay 15 can use `ReplicaView` for the applied vector, the high-water mark,
  and the hydration state.
- Essay 19 can use `ReadResponse::search_version_vector` to build a
  `SearchCursor` at the same pin.

## Golden vectors

The file `crates/oxkv-readpin/tests/vectors/readpin.json` holds the pinned
cases. The runner is `crates/oxkv-readpin/tests/vectors.rs`. The cases cover
the 24-hour offline read, the 7-day resume, the expired authority pin, the
expired sync cursor, the session vector minimum, read-your-writes, the stale
read, the exact pin, and the scalar minimum rejection.
