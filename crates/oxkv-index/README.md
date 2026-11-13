# oxkv-index

The per-shard inverted index and the frozen tokenizer contract for oxkv search.

The crate is portable. It compiles for native and `wasm32-unknown-unknown`. It
uses integer arithmetic only. It does not read a clock, draw randomness, or
perform I/O on a semantic path. It uses no `unsafe` code.

## Tokenizer contract

The tokenizer annex is `tokenizer/v1`. The Unicode word tokenizer is the frozen
search tokenizer. Its identity fixes every contract field:

| Field | Value |
| --- | --- |
| Identifier | `unicode-word` |
| Contract version | `tokenizer/v1` |
| Unicode data version | `16.0.0` |
| Normalization | `nfc` |
| Case folding | `full` |
| Segmentation | `unicode-word-run` |
| Stop words | none |

The ASCII word tokenizer is the second in-tree implementation. Its identifier
is `ascii-word`. It is the oracle for ASCII corpora.

A token starts at a letter, a number, or connector punctuation. It continues
over letters, marks, numbers, and connector punctuation. The tokenizer applies
NFC first. It applies full case folding second. It assigns one position to each
token. The position starts at zero.

A new behaviour needs a new identifier or a new contract version. A reader
rejects an unknown identity with `search.tokenizer_unknown`.

The Unicode tables come from the Unicode Character Database 16.0.0. The
generator is `tools/generate_unicode_tables.mjs`. It runs at development time
only.

## Logical keyspace

A posting is a system key in the `0x05` namespace. The storage layer prepends
the table prefix and the `0xFF` system marker:

```text
P || 0xFF || 0x05 || field || term || document
```

`PostingKey` encodes the logical part only. The crate does not write to the
store.

## Manifest declaration

`IndexManifest` declares these items:

- The indexed fields.
- The tokenizer identity of each field.
- The positions flag of each field.
- The filter flag names and search flag names of each field.
- The scorer and its integer parameters.
- The index overhead budget.
- The token budget.

The scorer is document-local. It uses the term frequency and the field length.
It does not use a corpus statistic. The parameters `k1_milli`, `b_milli`, and
`reference_length` are integers. The score is in thousandths.

## Write transaction

`ShardIndex::begin_write` stages one atomic index write. `put` and `delete`
validate the key bounds, the fields, the flags, and the token budget. `commit`
builds one delta segment and checks the overhead budget. It applies the segment
only when every check passes. A failed `commit` leaves the index unchanged.

The caller must commit the data page and the index page in one store batch. The
crate owns the index half of that commit.

## Replica build

`build_scope_index` builds an index from scope members. It filters every member
through the shard bounds and the scope predicate. It sorts the members by key.
The input order does not change the bytes. It never reads or copies an authority
segment.

The authority index and the replica index use the same tokenizer and the same
scorer. The ranking is identical for the same document-local features.

## Search

`ShardIndex::search` reads one pin. It filters candidates through the shard
bounds and the scope before it ranks. It sorts hits by score descending and
canonical key ascending. It applies the limit after the sort. The result reports
the total hit count before the limit.

A request declares the field list, the tokenizer identity, the match mode, the
required filter flags, the required search flags, and the positions flag. An
empty field list selects every searchable field.

`DerivedIndex` is the derive-on-read oracle. It tokenizes documents at query
time. It must return the same result as the inverted index for the same
documents and scorer.

## Named errors

| Condition | Code |
| --- | --- |
| Index digest or segment checksum mismatch | `search.index_mismatch` |
| Tokenizer identity disagreement | `search.tokenizer_mismatch` |
| Unknown tokenizer identity | `search.tokenizer_unknown` |
| Overhead above the manifest budget | `index.overhead_exceeded` |
| Token count or token length above the budget | `search.token_budget_exceeded` |
| Pin outside the retained range | `search.pin_expired` |

A search never returns a shorter result in place of an error.

## Immutable segments

A segment holds the per-field postings, the document map, the tombstone set, and
the tokenizer identity of every indexed field. The segment checksum covers the
canonical body. The index digest covers the header and every segment.

A commit adds a delta segment. A delete adds a tombstone in the same commit. A
search at a pin reads the segments at or below the pin and applies the
tombstones at or below the pin.

`compact` merges the visible documents into one segment at the current version.
It drops the older segments and the older pins. A search before and after
compaction returns the same result at the current version.

## Golden vectors

`tests/vectors/tokenizer.json` pins the tokenizer contract. It covers empty
text, multibyte text, case folding, combining marks, and segmentation.

`tests/vectors/index.json` pins the segment bytes, the index bytes, the index
digest, and the ranked hits. The same test is built for native and `wasm32`.
The pinned bytes are the target-independent answer.

## Verification

```text
cargo test -p oxkv-index
cargo clippy -p oxkv-index --all-targets -- -D warnings
cargo check -p oxkv-index --target wasm32-unknown-unknown
cargo fmt --check -p oxkv-index
```

## Use by later essays

Essay 19 (search pin) uses `SearchRequest`, `SearchResult`, and the pin error.
Essay 20 (cost) uses `term_document_frequency`, `segments`, and
`overhead_bytes`. The crate does not include the query coordinator.
