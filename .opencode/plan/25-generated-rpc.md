# 25. Generated RPC

A declarative surface that stops at the process boundary is not declarative. If
native, browser, and test clients each learn the API from a hand-written stub,
the manifest is one source among several, and the claim of one shared ABI is
false. Generated RPC carries the manifest to every caller: one descriptor, four
consumers, one negotiation step, one error mapping, one transport seam. The RPC
surface is derived, never authored.

## What the greater goal demands

The unit of ownership is a policy-filtered query. A replica is read-only. A
write is a request with an idempotency key and a base version. A receipt is
`Queued`, `Committed`, `Rejected`, or `Conflict`. Every caller must observe
these contracts the same way.

C1-C5 and A1-A12 freeze at 1.0: manifest, snapshot and page format, sync wire protocol, policy language, receipt and error semantics, and the annexes. A hand-maintained
RPC surface adds an unversioned surface. It can drift from the manifest and between
languages. The qualification suite then proves parity for the tested clients,
not for the ABI.

The manifest declares tables, fields, indexes, rules, and exposed operations.
The goal states that RPC is generated from it. This essay treats that as
required.

## Required capabilities

1. **Descriptor emission.** The manifest compiles to one versioned, deterministic
   descriptor. It lists every method, message, field number, error code, and
   streaming shape. The same manifest must produce byte-identical descriptors in
   every build.
2. **Server router.** The router decodes a request, checks the negotiated
   version, dispatches to a generated handler, and encodes the response. An
   unknown method returns a named protocol error.
3. **Native client.** The client is typed and owns retry policy. A write cannot
   be expressed without an idempotency key and a base version.
4. **Browser client.** The same operations are reachable from a browser with no
   translation proxy. The browser is a first-class caller.
5. **In-process client.** Tests call the same protocol core over an in-memory
   transport. A direct function-call shortcut is a separate mode the
   qualification suite must prove equivalent, never the default.
6. **SessionHello before the first call.** `SessionHello` is a side-effect
   free unary call. It returns the RPC descriptor, `sync_wire_version`, `manifest_digest`, minimum
   client version, `map_version`, capabilities, streaming modes, and size limits. It must not negotiate a `policy_version`. A data call cannot
   precede it.
7. **Streaming shapes for sync and receipts.** Sync diffs, snapshot pages,
   receipt transitions, and events are server-streaming calls with a cursor.
   Bidi is out of scope for 1.0.
8. **A transport seam.** One sans-IO protocol core, at least two in-tree
   transports, and no transport-specific branch in the core.
9. **Generated error and receipt mapping.** Every taxonomy variant maps to one
   code. An unknown code from a newer server maps to a named error that
   preserves the raw value.

## Invariants that must hold

- **Single source.** Every method, field, and code is a pure function of the
  manifest. No hand-written method may appear in a generated surface.
- **Determinism.** Descriptor bytes are a function of manifest bytes. Negotiation
  carries the descriptor digest, and a client must refuse a digest it did not
  generate.
- **No data before negotiation.** A request that skips negotiation fails with a
  named error, not a partial read.
- **The envelope does not rewrite the payload.** C3 (sync wire protocol)
  produces a diff frame, and RPC carries it as opaque bytes. RPC must not
  re-encode diff entries into a second schema.
- **Errors are total.** Every receipt and error variant has one generated code.
  A new server code cannot crash a client.
- **Resume is by cursor, never by connection.** A dropped stream is a new call
  with the last acknowledged cursor. No stream state lives only in a socket.
- **A slow consumer cannot stall a shard.** Each stream has a bounded buffer and
  a named `consumer.too_slow` error. Backpressure is explicit.
- **Writes stay requests.** No generated method accepts an unversioned write.
  Cross-shard mutation surfaces as the named rejection, not a transport error.
- **Parity is observable.** The same script produces the same responses and
  errors on both transports.

## Decisions and technical choices

**A neutral descriptor IR, with a Protobuf emitter at the boundary.** Protobuf
gives field numbering, presence, and tooling. It cannot express the policy
language, and need not. The manifest compiles to a neutral IR, and the IR emits
a Protobuf descriptor set for the RPC boundary. A policy payload crosses RPC only as opaque bytes, so the
lossy corners of proto3 do not matter. A10 (descriptor) freezes the IR, and the emitter is
replaceable.

**Connect is the RPC envelope, not the sync frame format.** The browser decides
this. Native gRPC uses HTTP/2 trailers for status, and a browser cannot read
trailers. The workaround is gRPC-Web plus a translating proxy: two protocols and
an extra deployment component. Connect carries status and end-of-stream in the
response body, so one generated browser client works over HTTP/1.1 and HTTP/2
with no proxy. Connect also permits JSON, which matches human-readable-before-fast.

The sync protocol remains custom and sans-IO. Connect carries sync frames as opaque bytes. A Connect server stream may carry those frames. RPC must not re-encode a sync diff entry into a second schema. A custom RPC framework would make the project own framing, flow control, multiplexing, proxy behavior, and every binding. Reject it for 1.0.

**Server-streaming only.** Connect supports unary and server-streaming on both
HTTP versions. Bidi requires HTTP/2 and is unavailable in browsers. Sync needs a
server push and a client cursor, not a symmetric channel. Acks and cursor
advances are small unary calls. Resume logic stays in the protocol core, not in
a socket state machine.

**JSON by default, binary by negotiation.** The default codec is JSON with
stable field order and names. Negotiation advertises a binary codec, and a
client may switch after a benchmark. Human-readable output wins until a
measurement says otherwise.

**Two in-tree transports.** An in-memory duplex transport serves tests and
embedded callers. An HTTP transport serves native and browser callers. Both pass
bytes through the same protocol core. A WebSocket adapter is optional and must
use the same frames.

**The cost of Connect.** The project depends on an external protocol
specification and must pin its version. Streaming over HTTP/1.1 depends on
chunked transfer, and some proxies buffer it, so the project must publish a
proxy matrix and a timeout policy. No bidi means future push-pull features need
a redesign. JSON costs bytes and CPU. Protobuf shapes the descriptor, though the
neutral IR remains authoritative.

## Dependencies

- C1 (manifest schema) (item 7) must express operations, streaming shapes, and error
  associations before generation is possible.
- C3 (sync wire protocol) (item 14) supplies the opaque diff frames and `SyncCursor`
  semantics that the streaming methods carry.
- Versioned writes (item 9) and C5 (receipt and error semantics) (item 10) supply the write request and
  the receipt state taxonomy.
- The business event stream (item 24) supplies its envelope and `EventCursor`.
- A4 (shard map) (item 22) supplies the named cross-shard rejection.
- A10 (descriptor) freezes the neutral IR.
- The qualification suite (item 30) supplies the golden vectors and the
  native-browser-native parity run.

## Acceptance criteria

1. Two clean builds from one manifest produce byte-identical descriptors, and
   `SessionHello` carries the `descriptor_digest`.
2. A native client and a browser client complete `SessionHello`, subscribe, resume
   after disconnect, and observe a terminal receipt against one server, with no
   proxy.
3. The in-process and HTTP transports return identical responses and errors for
   one scripted scenario.
4. A compile-fail test shows that a write without an idempotency key or `base_version`
   does not build.
5. An unknown server error code produces a named, non-panicking client error
   that preserves the raw code.
6. An unnegotiated data call returns `sync.negotiation_required`. A client below the
   minimum `manifest_digest` returns `sync.incompatible_manifest` before any data is
   read.
7. A consumer that stops reading triggers `consumer.too_slow` without stalling a
   shard.
8. Every receipt and error variant has exactly one generated code, checked
   exhaustively at build time.
9. An unknown method returns `rpc.unknown_method`, not a framework default.
10. A control operation uses the `control` namespace and the same generator, error mapping, and authentication as a data operation.

## What would falsify this design

- A required operation needs a hand-written method outside the manifest.
- The manifest cannot express an operation without a codegen special case.
- A browser cannot complete sync without a proxy, which would falsify the
  Connect recommendation.
- The documented proxy set buffers server-streams, which would force a
  transport change or a WebSocket fallback.
- Generated clients from one manifest disagree on a golden vector.
- A new error code cannot be added without breaking old generated clients.
- Descriptor output differs between builds, which makes digest negotiation
  meaningless.
- Bidi becomes necessary for a 1.0 feature, which would break the browser path.

## Open questions

1. Is the neutral IR part of A10 (descriptor), or is the Protobuf descriptor the
   only published form?
2. How are method paths versioned when an operation is renamed inside a
   compatible manifest version? Aliases, or a major-version break?
3. Do event-stream frames share the sync envelope, or get a separate method
   family and cursor?
4. Should the binary codec be selected per call or per connection?
5. Is a WebSocket adapter worth a third transport, or does fetch streaming cover
   every browser?
6. Where do compression and size limits live: transport, codec, or manifest?
