# Snapshot format v1

A snapshot is one shard range or one scope at one version. The format has a
canonical text header and a binary body. A reader verifies the body before it
serves one page.

## Layout

```text
offset 0
+------------------------------+
| text header                  |
+------------------------------+
| page 0                       |
| page 1                       |
| ...                          |
+------------------------------+  directory_offset
| page directory               |
+------------------------------+  directory_offset + directory_length
```

The header ends with the `header_end` line. The body starts on the next byte.
The directory is the last object in the file.

## Header

The first line is `oxkv-snapshot/v1`. Each later line is `name=value`. The
field order is fixed. Decimal fields use a fixed width. Hash fields use
lowercase hex. The header is ASCII text.

| Field | Required | Notes |
| --- | --- | --- |
| `format_version` | Yes | Major and minor. |
| `key_encoding_version` | Yes | The key encoding annex version. |
| `required_features` | Yes | A 64-bit hex bitset. |
| `artifact` | Yes | `ShardSnapshot` or `ScopeSnapshot`. |
| `manifest_digest` | Yes | A 32-byte hash. |
| `map_version` | Yes | The shard map version. |
| `map_digest` | Yes | A 32-byte hash. |
| `policy_version` | Scope | A 32-byte content address. |
| `shard_id` | Shard | Opaque identifier bytes. |
| `prefix_lower` | Shard | The inclusive lower bound. |
| `prefix_upper` | Shard | The exclusive upper bound. |
| `commit_version` | Shard | The shard commit version. |
| `scope_identity` | Scope | A 32-byte hash. |
| `version_vector` | Scope | One commit version per shard. |
| `projection_epoch` | Scope | The needed fact projection. |
| `projection_watermarks` | Scope | One watermark per fact source. |
| `page_size` | Yes | The target page payload size. |
| `page_count` | Yes | The directory entry count. |
| `codec` | Yes | The page codec. 1.0 uses `0`. |
| `directory_codec` | Yes | The directory codec. 1.0 uses `0`. |
| `directory_offset` | Yes | The absolute directory offset. |
| `directory_length` | Yes | The directory length. |
| `content_digest` | Yes | The content digest. |
| `lease_term` | Yes | The authority lease term at seal time. |
| `content_seal` | Yes | The digest, lease term, and optional signature. |

The required feature bits are:

| Bit | Feature |
| --- | --- |
| 0 | Text header |
| 1 | BLAKE3 content digest |
| 2 | CRC32C page integrity |

A reader rejects an unknown major version with
`format.unsupported_version`. A reader rejects an unknown required bit with
`format.missing_required_feature`. A reader accepts a higher minor version
only when every required bit is known.

A reader skips an unknown field. A required field stays required.

## Page

A page is a binary frame.

| Field | Width | Notes |
| --- | --- | --- |
| magic | 4 | `OXPG` |
| format major | 2 | Big-endian. |
| format minor | 2 | Big-endian. |
| `page_id` | 32 | A BLAKE3 hash. |
| `page_sequence` | 8 | Strictly increasing in one snapshot. |
| shard length | 4 | Big-endian. |
| `shard_id` | variable | Opaque bytes. |
| `commit_version` | 8 | Big-endian. |
| first key length | 4 | Big-endian. |
| first key | variable | Bytes. |
| last key length | 4 | Big-endian. |
| last key | variable | Bytes. |
| record count | 4 | Big-endian. |
| codec | 2 | Big-endian. |
| uncompressed length | 8 | Big-endian. |
| payload length | 8 | Big-endian. |
| payload | variable | Canonical records. |
| CRC32C | 4 | Over the frame without this field. |

A payload record is `key_length` (4 bytes), key bytes, `value_length`
(4 bytes), and value bytes. Keys in one page use strictly ascending byte
order. Every key is inside the declared range.

`page_id` is a BLAKE3 hash of the domain tag, shard identifier, commit
version, page sequence, first key, and last key. It does not depend on a byte
offset.

## Directory

| Field | Width | Notes |
| --- | --- | --- |
| magic | 4 | `OXDR` |
| format major | 2 | Big-endian. |
| format minor | 2 | Big-endian. |
| entry count | 4 | Big-endian. |
| entries | variable | One entry per page. |
| integrity | 32 | BLAKE3 over the directory body. |

One entry has `page_id`, `page_sequence`, offset, length, first key, last
key, and the page CRC32C value. The entries follow `page_sequence` order.

## Integrity

A page carries CRC32C. A reader refetches one corrupt page.

The directory carries one BLAKE3 value. A reader checks it before it trusts
an entry.

The content digest is a BLAKE3 hash of the domain tag, the directory
integrity value, and every page integrity value in directory order. The
content digest does not cover the header.

The content seal covers the content digest and the authority lease term. The
lease term never enters the content digest. A new authority may serve the
same sealed snapshot with a new seal.

A reader checks the content digest before the content seal. A reader checks
the seal before it serves the artifact.

## Errors

| Code | Cause |
| --- | --- |
| `format.unsupported_version` | Unknown major version or malformed header. |
| `format.missing_required_feature` | Unknown required feature bit. |
| `format.corrupt_page` | Bad CRC, bad page identity, unsorted key, bad directory, or forbidden scope namespace. |
| `format.page_out_of_bounds` | A page key is outside the shard prefix bounds. |
| `format.truncated_snapshot` | The input ends before a required byte range. |
| `format.missing_provenance` | A required provenance field is absent. |
| `format.page_too_large` | A length field is over its limit. |
| `replication.stale_epoch` | The content seal does not cover the content digest or the lease term. |

## Limits

| Limit | Value |
| --- | --- |
| Header | 64 KiB |
| Page frame | 16 MiB |
| Key | 4 KiB |
| Value | 16 MiB |
| Shard identifier | 256 bytes |
| Pages in one snapshot | 1,000,000 |
| Directory | 128 MiB |
| Records in one page | 1,000,000 |

A reader checks every length before it allocates.

## Determinism

The writer uses no clock, randomness, map order, floating point, or I/O. The
same ordered records and parameters produce the same bytes and digests on
native and wasm32.

The page sequence starts at 1. The writer splits pages at record boundaries.
A record that is larger than the page budget is an error.

## ScopeSnapshot

A ScopeSnapshot carries admitted user rows and the projection facts that the
scope needs. It must not carry receipt, event, audit, or lease data. The
`ScopeProjectionBuilder` orders shards by shard identifier and keys by byte
order. Insert order cannot change the output bytes.

The caller decides row admission. The builder enforces the namespace rule and
the version vector.

## Golden vectors

The files under `tests/vectors` pin these cases:

- A valid v1 snapshot.
- A v2 major rejection.
- One-byte page corruption.
- A page outside the shard bounds.
- Truncation at every byte offset.
- An unknown required feature.
- A lease term change.
- Native and wasm identity.

## Notes

- The page CRC32C covers the frame without the CRC field. This is stronger
  than a payload-only checksum and catches a corrupt page header.
- The content seal carries an optional Ed25519 signature. This crate does not
  verify the signature because it holds no key material.
- The page and directory frames reject an unknown minor version. A later
  minor with a new page layout needs a new major version or a required
  feature bit.
