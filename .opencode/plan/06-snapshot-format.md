# 6. Snapshot and page format v1

A ShardSnapshot is the authority's state written to bytes that another machine must trust without asking a question. That machine is a browser replica. It hydrates a ScopeSnapshot built from the authority's ShardSnapshots, reads it offline for a day, and later resumes from a copy of unknown age. The byte layout is therefore a public contract from the first release, not an internal encoding that can drift. Before C2 freezes, it must answer five questions. What is this file? Is it intact? Can I read it? What do I do with a field I do not know? How does it become the next version? One prohibition outranks convenience: a reader never guesses.

## What the greater goal demands

A query is the unit of ownership. Hydration must stream ScopeSnapshot pages into the replica. If the replica rebuilt its files from a record stream, it would need a second copy of the encoding rules, and the copies would drift. Requirement: the replica stores ScopeSnapshot pages without re-encoding. The page the authority wrote is the page the replica reads.

Offline reads force a long-lived reader. A replica may hold a ShardSnapshot from a shard that runs an older format than the client binary. A user may reopen a browser tab after an upgrade. Requirement: a reader accepts a documented window of older formats and names the error outside it.

A data shard contains one user table and its system sub-namespaces. A ShardSnapshot is one shard's range at one `commit_version`. A ScopeSnapshot is one scope at one scope version vector. The header must carry that identity, so a reader rejects a page from another shard. The format is C2, one of the core contracts that freeze at 1.0, so it is versioned from the first commit. A browser that holds a v0 ShardSnapshot does not care that the crate is 0.x.

The bytes must contain no clock value, no random identifier, and no map iteration order. Two writers that encode the same ordered records with the same parameters must produce the same bytes. Otherwise a digest identifies an opinion, not content.

Human-readable comes before fast, so text tools must identify the format, `snapshot_page_version`, `shard_id`, and `commit_version` or `version_vector` without a decoder. Parsing is an attack surface, so every length field needs a maximum.

## Required capabilities

1. **Self-describing header.** The header must carry the format identifier, `snapshot_page_version` major and minor, required-feature bitset, `shard_id`, prefix bounds, `commit_version` for a ShardSnapshot, `version_vector` for a ScopeSnapshot, page size, codec identifiers, provenance block, and directory location.
2. **Framed pages.** Each page must carry `page_id`, `page_sequence`, its key range, record count, codec identifier, uncompressed length, and CRC32C. Pages must be addressable by byte offset, so a transfer resumes at a page boundary.
3. **Page directory.** One directory must list every page with its offset, length, first key, last key, `page_id`, `page_sequence`, and integrity value. It must carry its own integrity value, so one verified read establishes the layout.
4. **Per-object integrity.** Every page and the directory must carry an integrity value that the reader checks before use. The content digest covers the directory and every page digest. The content seal covers the content digest and the `lease_term`. The `lease_term` must not enter the content digest.
5. **Unknown fields.** The specification must mark each field required, optional, or skippable. A reader must ignore an unknown skippable field and reject a file that omits a required one.
6. **Unknown versions.** A reader must accept only the major versions it implements. It may accept a higher minor version only when every required-feature bit is known. Otherwise it must reject with a named error.
7. **Migration.** The one D12 migration framework must rewrite a ShardSnapshot from version N to N+1 and report every change to the digest and layout.
8. **Named errors.** The format must define `format.unsupported_version`, `format.corrupt_page`, `format.page_out_of_bounds`, `format.missing_required_feature`, `format.truncated_snapshot`, `format.missing_provenance`, and `format.page_too_large`.

## Invariants that must hold

- **Byte identity.** The same records, parameters, and codec versions must produce the same bytes and digest on native and wasm32.
- **One version per ShardSnapshot.** Every page must belong to one shard and one `commit_version`. A mixed set is invalid.
- **Order inside bounds.** Keys in a page must be strictly ascending, and every key must fall inside the declared prefix range.
- **No partial acceptance.** A page either verifies or hydration fails. A reader must not merge an unverified page.
- **Bounded allocation.** A reader must reject an over-limit length field before it allocates, and peak memory must depend on the page budget.
- **No network, no external schema.** A reader must parse a ShardSnapshot or ScopeSnapshot from the file alone.
- **Deterministic rejection.** The same invalid input must produce the same error name and offset on every platform.
- **Canonical encoding.** Exactly one byte sequence must represent a logical ShardSnapshot or ScopeSnapshot. A reader must reject overlong length prefixes and unsorted keys.

## Decisions and technical choices

**CRC and a cryptographic hash, at different levels.** CRC32C per page detects random corruption from a disk or proxy at almost no cost. A cryptographic digest detects tampering and gives content identity, but it needs a full pass. Each page carries CRC32C, because hydration verifies thousands of small objects. The footer carries one cryptographic digest over the directory and the page digests. That digest is the `content_digest` the wire protocol negotiates.

**Layered integrity.** Whole-artifact integrity alone discards a multi-gigabyte download for one flipped bit. Per-page integrity alone lets an attacker reorder or drop pages. The two layers cover both: a corrupt page is refetched alone, and a reordered page breaks the footer digest.

**Two named artifacts.** A ShardSnapshot covers one shard at one `commit_version` and includes the user table and every system sub-namespace. A ScopeSnapshot covers one scope at one scope version vector and includes only scope user rows and needed projection facts. The authority builds a ScopeSnapshot deterministically from one or more ShardSnapshots. A replica hydrates a ScopeSnapshot. The two artifacts do not share one header shape, but they share the page format.

**Seal is separate from lease.** The content digest covers the artifact bytes. The content seal covers the content digest and the `lease_term`. The `lease_term` must not enter the content digest. A sealed ShardSnapshot remains valid across a lease change, and a new authority may serve the same sealed bytes. The `lease_term` fences new writes and unsealed in-flight pages only.

**Rejection, not interpretation.** Best-effort interpretation is how formats become folklore. A reader that guesses at a truncated length field or an unknown tag returns wrong data with no error. The design rejects on any structural uncertainty. The one allowance is an enumerated list of skippable fields within a known major version. That list belongs to the specification, not to a heuristic.

**Text header, binary body.** The header is a line-oriented text preamble with fixed field names and decimal or hexadecimal values. It is inspectable and diffable. The body is binary pages. This satisfies human-readable before fast without weakening the hot path.

**Version arithmetic.** A major change alters the meaning of earlier bytes. A minor change adds optional or skippable fields. A reader rejects an unknown major version. It accepts an unknown minor one only when the feature bitset holds no unknown bit, and the header records the page size.

## Dependencies

- **A1 (key encoding).** Pages store encoded keys. The ordering invariant depends on canonical key bytes.
- **C1 (manifest schema).** The manifest declares the table, codecs, and index configuration that the header records.
- **C3 (sync wire protocol).** Transfer needs page addressing and offset resume. The `content_digest` is the transfer identifier.
- **16 (hydration).** Hydration consumes the directory, verifies pages, and hands off to the diff tail at one `commit_version`.
- **28 (operations).** Backup, restore, and migration use the D12 framework and the digest.

## Acceptance criteria

1. A v1 reader reads a valid v1 ShardSnapshot offline, with no external schema.
2. A v1 reader rejects a v2 major ShardSnapshot with `format.unsupported_version` and materializes no page.
3. A change to one byte in any page produces `format.corrupt_page` with `page_id` and byte offset.
4. A page whose key falls outside the prefix range produces `format.page_out_of_bounds`.
5. A ShardSnapshot cut at any offset produces `format.truncated_snapshot`, never a crash or a short read.
6. Two writes of the same ordered input produce identical bytes and digests on native and wasm32.
7. Hydration of a 1 GiB ShardSnapshot stays below the page budget, which does not grow with ShardSnapshot size.
8. One unknown skippable field reads without error. One unknown required-feature bit fails with `format.missing_required_feature`.
9. The D12 migration framework rewrites a v1 ShardSnapshot to v2 and lists each digest and layout change.
10. A ShardSnapshot with a missing `manifest_digest` fails with `format.missing_provenance`.
11. A sealed ShardSnapshot remains readable after a `lease_term` change.

## What would falsify this design

- If one corrupt page forces a full resync, per-page integrity is decorative.
- If two writers produce different bytes for the same ordered input, the digest cannot be a transfer identifier.
- If a reader must read the body before it can parse the header, hydration cannot resume.
- If every minor version forces a reject, the feature-bitset rule is too coarse.
- If a valid-looking length field forces allocation past the page budget, the resource bound is unproven.

## Open questions

1. **Canonical strictness.** Must a reader reject every non-canonical encoding, or only those that change meaning? Strict rejection makes digests trustworthy and turns writer bugs into hard failures.
2. **Index placement.** Is the inverted index a page class inside the ShardSnapshot, or a sibling object with the same envelope? This decides whether search and get share one verification path.
3. **Digest algorithm.** BLAKE3 keeps the wasm binary small. SHA-256 has wider platform support. The header records the choice, so a later major version may change it.
4. **Policy version pin.** Does the header record the policy version that produced the scope, or reference the manifest? A replica user must know which rules shaped the local data.
5. **Reserved fields.** Must the header reserve bytes for encryption and compression metadata, so a later minor version adds them without a major bump?
