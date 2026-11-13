# 18. Inverted index

Search is first class, so the index is part of the commit. A write commits only when its postings commit. If a search can miss a document that a get returns at the same version, the claim is false. This essay specifies a per-shard inverted index, built inside the write transaction, governed by A2 (tokenizer), and guarded by a named mismatch error.

## What the greater goal demands

A query is the unit of ownership. A client declares a policy-filtered scope. Matching data materializes locally and stays searchable offline. Offline search is honest only when the replica owns an index over exactly its scope. A replica that calls an authority to search is not query-materialized.

Authority owns truth and serializes writes. A replica is read-only with scope, index, and outbox. Its index is not a best-effort cache. It advances atomically with the diff tail at the same version.

A data shard contains one user table and its system sub-namespaces, and it is the unit of placement, ownership, transaction, fan-out, scope, and replication. The index takes that boundary: one shard, one index, one table range. A cross-shard search is a scatter-gather with a deterministic merge. A cross-shard mutation is rejected with `shard.cross_mutation_rejected`.

One deterministic, bounded policy language governs scope and validation. The index obeys it twice. An indexed field must be declared in the manifest and evaluable by the policy. A search must filter candidates through the same scope predicate that governs reads. A second evaluator is forbidden.

Index maintenance is not a side effect. It is a state transition inside the transaction. An index updated by a downstream consumer would lag, and a lagging index breaks the commit boundary.

## Required capabilities

1. **Same-commit maintenance.** The data page, index page, and commit record must form one atomic unit. Recovery produces both or neither. No committed version lacks postings.
2. **Versioned tokenizer.** The manifest must name a tokenizer by identifier and version. Segments must record that pair. A reader must reject a mismatched pair.
3. **Document and field granularity.** A document must be a key. The manifest must declare indexed fields, positions, and whether a field is searchable, filterable, or both. Postings must be per field.
4. **Delete and update.** A delete must tombstone every posting of the key in the same commit. An update is a delete plus an insert in one transaction. No posting may return a key after its delete commit.
5. **Compaction interaction.** Compaction must merge segments, remap document identifiers, and preserve the token stream. A merge must not change the result set for any retained version. Index bytes count against the primary quota.
6. **Overhead budget.** The manifest must declare an index overhead budget per table. A write or compaction that would exceed it must fail with `index.overhead_exceeded`. Silent field dropping is forbidden.
7. **Named mismatch error.** Every segment must carry a checksum, and every commit record an index digest. A reader that detects disagreement returns `search.index_mismatch`. It must not return a coverage result or fall back to a hidden scan.
8. **Replica index construction.** A replica must build its own index from scope members under the same `tokenizer_version` and the manifest scorer. It must not mirror the authority per-shard index bytes. Hydration must hand over the scope pages and the index at one scope version vector.
9. **Two in-tree implementations per seam.** The tokenizer seam must have at least two, such as ASCII word and Unicode word. The storage seam must have at least two, such as immutable postings and a derive-on-read scan. The second is the oracle for the first.

## Invariants that must hold

- **Atomic visibility.** At every version V, a search pinned at V observes exactly the documents committed at or before V.
- **Determinism.** The same bytes and tokenizer pair produce the same tokens on native and wasm32. No floating-point behaviour, locale, or clock may affect tokenization.
- **Tokenizer immutability.** A tokenizer version never changes behaviour. New behaviour requires a new version and a reindex.
- **Total deletion.** After a delete commit, no posting references the deleted key at that version or later.
- **Scope containment.** A shard index holds postings only for keys inside its prefix range.
- **Bounded analysis.** Token count, token length, and analyzer steps must have declared caps. A document that exceeds a cap must fail with a named error.
- **Replay equality.** Rebuilding from the log with the same tokenizer pair must produce byte-identical segments.
- **No silent degradation.** A condition that prevents a complete answer at the pinned version must surface as a named error, not a smaller result.

## Decisions and technical choices

**Embedded, not external.** An embedded index lives in the shard store and commits with the data. An external index service is a second system with its own log and lag. Derive-on-read builds postings per query and stores nothing.

Embedded is the requirement. Only it satisfies the commit boundary, offline replica search, and replay equality without a second consistency protocol. An external service adds dual writes, lag, and a second truth. Derive-on-read has unbounded cost and cannot serve a large shard within a step budget. It remains the oracle, and a fallback for tables the manifest marks as tiny.

**Segments and postings.** The index must use immutable segments of per-field postings, a document map, and a tombstone set. A commit adds a segment or a delta segment. Compaction merges segments and drops tombstoned entries. Readers search the segment set at one pinned version.

**Tokenizer as A2.** The tokenizer defines the bytes of the index. If the authority tokenizes "Café" into one term and a replica splits it into two, offline search disagrees with online search for identical data. If a tokenizer changes behaviour without a `tokenizer_version` change, a stored segment silently means something else. A2 must cover the identifier, version, Unicode data version, normalization form, case-folding rule, segmentation rule, and stop-word list. Golden vectors must pin all of it. The tokenizer is A2, a frozen annex of C1.

**Deterministic, document-local scoring.** Ranking must use integer arithmetic with parameters declared in the manifest. It must use document-local features only. Global corpus statistics must not affect a 1.0 ranking. Floating-point scoring risks native and wasm disagreement and fragile golden vectors. A table may declare a fixed scorer and parameters, and a change to either is a manifest change with a receipt.

**Scope predicate.** Candidate postings must pass the same policy evaluator used for reads and writes. The index must not become a side channel that reveals hidden fields.

## Dependencies

A1 (key encoding) fixes prefix-to-table mapping and byte order. C2 (snapshot and page format) defines an index page type and checksum. C1 (manifest schema) declares fields, tokenizers, indexed fields, positions, budget, and scorer. C4 (policy language) supplies the single scope evaluator. C5 (receipt and error semantics) makes index failure a terminal, named outcome. C3 (sync wire protocol) carries `tokenizer_version`. A11 (read pin and staleness) fixes the pin-based read view, so item 18 cannot close before it. The cost model bounds fan-out and merge work. A4 (shard map) defines the scatter-gather set.

## Acceptance criteria

- Fault injection between every write step yields both data and postings, or neither, on recovery.
- Golden tokenizer vectors produce identical tokens and segment bytes on native and wasm32.
- A randomized corpus shows agreement between the index and the derive-on-read oracle at every version.
- A search at each version equals the set of documents committed at or before that version.
- Results are identical before and after compaction, and index bytes stay within budget.
- A flipped index byte produces `search.index_mismatch`, never a short result. A rebuild produces byte-identical segments.
- An offline replica searches its scope, exposes lag and version, and resumes without full resync.
- A cross-shard query merges deterministically. A cross-shard mutation returns the named rejection.
- An unknown tokenizer version returns a named error.

## What would falsify this design

- A committed write that search at the same version cannot find.
- Any input where native and wasm32 tokenize differently.
- A tokenizer defect that forces a behaviour change under an unchanged version.
- Overhead above budget on realistic corpora, or unbounded compaction cost.
- A replica that cannot reproduce the index from diffs alone, so offline search diverges.
- A nonzero mismatch-error rate in normal operation.
- Integer scoring too coarse for useful ranking, forcing a float specification.

## Open questions

1. Does 1.0 store positions, or only term frequencies?
2. Is the tokenizer per table, per field, or per manifest?
3. Does 1.0 include stemming, or does each language ship as a tokenizer version?
4. Is the overhead budget a hard rejection or a soft warning?
5. Does the replica receive index segments, or rebuild from documents?
6. How does compaction choose segment count, and who schedules it?
7. Does the index use the data page format or a distinct page type?
8. Should ranking parameters be per table, or is one scorer enough?
9. What are the fan-out, term, and time budgets for a cross-shard search?
10. Does a token cap fail the write or drop the field?
