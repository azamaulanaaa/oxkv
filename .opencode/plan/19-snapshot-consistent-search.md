# 19. Snapshot-consistent search

Search is a read. Every search and point read in one interaction must use one read pin, so a user never sees a hit that a get contradicts. This essay defines: one pinned scope version vector shared by search and get, stable pagination and ranking for the life of the pin, scope filtering at the pinned `policy_version`, a deterministic cross-prefix merge, and a named error when the pin is unreadable. It binds C2 (snapshot and page format), C3 (sync wire protocol), C4 (policy language), A9 (cursor types), and A11 (read pin and staleness) at 1.0.

## What the greater goal demands

The query is the unit of ownership. A client declares a policy-filtered scope, and matching data materializes locally. Search belongs to that materialization, not a separate service with a separate truth. A replica holds a scope, an index, and an outbox, and no write authority. It must answer search and get from the scope version vector it claims to hold.

Authority serializes writes. A search must observe a committed version, not a write halfway through. It must not read prefixes at unrelated versions and call the mix one answer. A cross-prefix search is an explicit scatter-gather, because the shard is the unit of placement, transaction, fan-out, and scope. The gather must be deterministic and bounded.

A disconnected replica must serve its scope at its pin and expose lag.

## Required capabilities

The read pin must be opaque to clients and interpretable by every targeted shard. A search must carry it, and a get must accept it. It must name the data, `policy_version`, `tokenizer_version`, `map_version`, and `statistics_version`.

Every posting must carry visibility metadata. It is visible at `commit_version` V only when its commit is at most V and no delete or superseding update at or below V removes it. Row and postings must commit in one shard transaction.

A `SearchCursor` must bind to the pin and a fingerprint of the query, scope, and sort. Later pages must resume at the same pin. A mismatch must fail with `search.cursor_invalid`.

Ranking must be a pure function of pinned data and document-local features. Ties must break on canonical key bytes, never shard arrival. Scores and tokenization must agree across native, wasm32, and every replica.

Scope must compile into the index query and run before top-k selection. A post-filter underfills pages and leaks the existence and count of hidden records.

A cross-prefix search must merge ordered shard streams deterministically. Each shard must return enough entries for an exact global offset and limit. The search comparator must be `(score descending, canonical key ascending)`. Fan-out must have a fixed bound, and exceeding it must fail with `search.fanout_exceeded`, not truncate. Exact mode is the default. Coverage mode is opt-in and must mark the result partial. A coverage search must not advance an exact pagination cursor.

An unreadable pin must fail with `search.pin_expired` reporting the requested and oldest readable versions. It must never fall back to newer data. A search pin is a scope version vector plus a query fingerprint. It uses the A11 pin modes, `AtLeast(V)` and `Exactly(V)`.

## Invariants that must hold

1. **One pin, one answer.** At a pin, every hit point-reads to a row that satisfies the query and scope.
2. **No phantom hits.** A hit committed after the pin, or deleted at or below it, never appears.
3. **No missing hits.** Every matching in-scope row appears on exactly one page.
4. **Stable pagination.** Page boundaries never shift while the pin is readable. No page repeats or skips a row under concurrent writes.
5. **Stable ranking.** One query, scope, sort, and pin produce one order on authority, every replica, and both targets.
6. **Scope containment.** No hit or count reveals a row outside the pinned scope. A policy change does not alter an active pin.
7. **Atomic visibility.** A row and its postings become visible together, or neither does.
8. **Deterministic merge.** Merge order depends only on the sort tuple and canonical key bytes, not network timing.
9. **Failure is named.** Expired pin, invalid `SearchCursor`, unavailable shard, and exceeded fan-out each return a distinct error from C5. None becomes a silent re-read.
10. **Offline parity.** A disconnected replica returns the same answer for its local pin as a connected reader.
11. **Read-only search.** Search never mutates the index, visibility watermark, or outbox.

## Decisions and technical choices

A **versioned index** puts versions on postings and filters visibility at query time. It permits concurrent writes during a long pin and needs no lock. It costs index growth, tombstones, and a watermark that respects every readable pin.

A **copy-on-write index** publishes an immutable segment set per version. A pin names a segment set, so queries see no partial write and pagination is simple. It costs write amplification and retention pressure from long pins.

A **read-locked snapshot** holds a lock for the whole interaction. It gives strong equality with little metadata, but a paused client holds resources on every shard. It blocks compaction and cannot survive disconnection.

The design must combine the first two. The index publishes immutable segments, each carrying `commit_version` metadata. A pin names a watermark plus the segment set readable there. Queries filter by `commit_version` and never mutate a segment. Segments retire only when no readable pin references them. Pins have a bounded lifetime, and expiry produces `search.pin_expired`. This avoids locks and makes pagination stable.

The read pin must be authority-issued and monotonic. A shard serves a pin when its `commit_version` is inside its retained range. A cross-shard pin is valid only when every targeted shard can serve it. Exact mode must return a named error and no data if one shard cannot. Coverage mode is opt-in and must return a coverage descriptor.

Pagination must use keyset cursors over the full sort tuple, not offsets. The `SearchCursor` carries the pin, query fingerprint, and last tuple. The server must reject deep offsets and validate every `SearchCursor` as a claim on a pinned view.

Ranking must use document-local features only. Global corpus statistics must not affect a 1.0 ranking. The manifest must state the scorer and its integer parameters, and the compiler must enforce it. Scores must be fixed-point, or the evaluation order fixed.

The merge must be a bounded heap merge over `(score descending, canonical key ascending)`. Each shard must return its top `offset + limit` entries. The coordinator must stop at the requested window, not buffer all results.

## Dependencies

A1 (key encoding) fixes tie-breaks. C2 (snapshot and page format) encodes the read pin, segment set, and checksums. C1 (manifest schema) declares searchable fields, tokenizers, indexes, and sortable fields. C4 (policy language) supplies deterministic, bounded scope evaluation at a pinned `policy_version`. A5 (scope semantics) aligns search with replication. C3 (sync wire protocol) carries pins and `SearchCursor` values across resume. Hydration proves the served segment set. Policy projection supplies pinned rule facts. The inverted index commits inside the write transaction. A4 (shard map) fixes the fan-out set. A11 (read pin and staleness) defines lag reporting. The cost model depends on this contract.

## Acceptance criteria

A writer commits updates and deletes between page one and page two. The reader still gets every original hit once with original values, and every get at the pin agrees.

A deleted row never appears before the delete and never appears at or after it. An updated row never mixes old and new fields.

Three or more prefixes produce a merged result with deliberate ties. The order is byte-identical on authority, native, wasm32, and in-process.

A restricted principal searches a scope with hidden rows. The result and total count exclude them, and page filling is not reduced.

An expired pin returns `search.pin_expired` with both versions. A changed query, scope, sort, or pin returns `search.cursor_invalid`.

A replica offline for a day answers its scope at its local pin, matches authority there, and exposes lag.

A crash between row and index writes leaves neither visible. A corrupted segment fails checksum validation. A fan-out above the bound fails, not truncates.

## What would falsify this design

The design is false if a get denies a hit, or a complete search omits a get, at the same pin. It is false if two nodes at one pin rank a query differently. It is false if a page repeats or skips a row under writes. It is false if an expired pin serves newer data, or a post-filter leaks a hidden count. It is false if a pin needs a lock across client think time. It is false if cross-shard scoring needs unpinnable statistics. It is false if the merge is unbounded or order-dependent. It is false if a tokenizer or policy change alters an active pin.

## Open questions

1. Does 1.0 promise one scope version vector, or a per-shard pin plus a watermark? One pin explains better. Per-shard pins serve better when shards advance independently.
2. How long may a pin be held? A fixed lifetime is predictable but can expire mid-review. Renewable pins cost retention.
3. Should authority and replicas share one retention rule? A replica may need days. Authority may reclaim in minutes.
4. Global corpus statistics or document-local scores in 1.0? Document-local scores are the 1.0 rule. Global statistics are deferred.
5. Is a coverage result acceptable when a shard is unavailable? Yes, only on explicit request, and it must carry a coverage descriptor. Exact mode is the default.
6. Does an active pin survive policy revocation? Consistency favors the pin. Security may favor revocation. Clients must see the choice.
