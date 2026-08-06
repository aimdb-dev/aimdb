# 047 — Retire `aimdb-ws-protocol`: every transport speaks AimX

**Status:** ✅ Implemented (mapping-table gate from 036 A2; shipped on
`feat/retire-aimdb-ws-protocol`)

**Scope:** delete the `aimdb-ws-protocol` crate and the WS-JSON envelope it
defines, porting the WebSocket connector (`aimdb-websocket-connector`), the
browser bridge (`aimdb-wasm-adapter::ws_bridge`), and the UI's raw discovery
path onto the AimX envelope (`aimdb-core::session::aimx`). Extends AimX
with the two features that originally justified the WS fork — wildcard /
multi-record live subscribe and a query passthrough result shape — as
additive core changes.

**Origin:** design 038 §3.9 (estimated ~1,200–1,800 lines saved), carrying
036 A2 and 034 §3.10 root cause 10.

**Consumers:** `aimdb-websocket-connector` (server + `ws-client`),
`aimdb-wasm-adapter` (`WsBridge`, `WasmDb.discover`), `_external/aimdb-ui`
(`useAimDb.tsx` raw discovery), `aimdb-client`/CLI/MCP (unchanged wire,
extended result shapes), `aimdb-pro` weather hub (WS builder knobs, query
handler).

---

## 1. Context

The workspace ships two wire protocols for one feature. AimX
(`aimdb-core/src/session/aimx/`, NDJSON tagged frames) is ridden by the
UDS/serial/TCP connectors and `aimdb-client`; `aimdb-ws-protocol`
(`ServerMessage`/`ClientMessage`, `type`-tagged JSON) is ridden by the
WebSocket stack and the browser `WsBridge`. Every protocol feature —
snapshots, acks, writes, queries, subscribe — exists twice, with two codecs
and two error mappings.

Crucially, both protocols already ride the same session engine: the WS codec
implements the same `EnvelopeCodec` trait as `AimxCodec` and is driven by
the same `run_session`/`serve`/`run_client`. The genuinely WS-specific code
is the wire types, the WS codec, the query/list halves of the WS dispatch,
and the hand-rolled browser bridge. Everything below the codec (transport,
HTTP upgrade auth, fan-out bus, pumps) is engine-generic and stays.

### Corrections to the originally scoped work (verified against the code)

1. **"Fold `aimdb-client`'s hand-rolled demux onto `run_client`" is done.**
   `AimxClient` was deleted in PR #124; `aimdb-client::AimxConnection` rides
   `run_client` and holds zero correlation logic. The only residue is dead
   re-exports in `aimdb-client/src/protocol.rs` (`RequestExt`/`ResponseExt`/
   `serialize_message`/`parse_message`/`EventMessage`/`cli_hello` — zero
   consumers). That is a cleanup item here, not a work package.
2. **Query passthrough already exists in AimX.** `record.query` +
   `QueryHandlerFn` (`aimdb-core/src/remote/query.rs`, registered by
   `aimdb-persistence::with_persistence`) already delegates a pattern query
   whose `name` accepts `*`. What's missing is the *result shape* (per-record
   `ts`, `total`), not the passthrough.

The one genuinely missing AimX feature is **wildcard / multi-record live
subscribe**: `Inbound::Subscribe` carries a single exact `topic` resolved by
exact key.

## 2. Mapping table

### 2.1 Frames

AimX wire tags from `session/aimx/codec.rs`; ws-protocol messages from
`aimdb-ws-protocol/src/lib.rs`. "→" marks the surviving form.

| Semantics | ws-protocol | AimX (surviving) | Notes |
|---|---|---|---|
| RPC request | `Query{id,pattern,from,to,limit}`, `ListTopics{id}` | `{"t":"req","id":N,"method":M,"params":P}` | String ids → engine-owned `u64` ids. `query`→`record.query` `{name,limit,start,end}`; `list_topics`→`record.list`. |
| RPC reply | `QueryResult{id,records,total}`, `TopicList{id,topics}` | `{"t":"reply","id":N,"ok":V}` | Result shapes: §3.4. |
| RPC error | `Error{code,topic,message}` | `{"t":"reply","id":N,"err":CODE}` | 6→3 code collapse: §3.5. |
| Subscribe | `Subscribe{topics:[..]}` (multi-topic, wildcards) | `{"t":"sub","id":N,"topic":T}` | One frame per pattern; a wildcard pattern is **one** subscription fanning in many records (§3.1). |
| Subscribe ack | `Subscribed{topics}` | `{"t":"subscribed","sub":S}` (**new**) | §3.2. |
| Unsubscribe | `Unsubscribe{topics}` | `{"t":"unsub","sub":S}` | By sub id, not topic. |
| Live data | `Data{topic,payload,ts}` | `{"t":"event","sub":S,"seq":N,"topic":T,"data":V}` | `topic` is **new**, present when the server tags it (always on WS; on wildcard subs elsewhere). Server-side `ts` is dropped — timestamps belong to the record layer / query results. `seq` is new for WS clients (drop detection). |
| Late-join snapshot | `Snapshot{topic,payload}` | `{"t":"snap","sub":S,"seq":N,"topic":T,"data":V}`, final frame adds `"last":true` | `sub`, `seq` and `last` are **new** (§3.3). `seq` shares the subscription's event sequence space: the burst is `1..=N` and the first `event` continues at `N+1`, so a dropped snapshot is a detectable gap. `last` terminates the burst so the gap lands even with no event. |
| Client write | `Write{topic,payload}` | `{"t":"write","topic":T,"payload":V}` | Identical semantics (fire-and-forget, producer/arbiter path). |
| Keepalive | `Ping`/`Pong` | `{"t":"ping"}` / `{"t":"pong"}` | Identical. |
| Discovery | `ListTopics` over a raw socket | `record.list` req over a raw socket | UI's `discoverTopics` and `WasmDb.discover` reissue as AimX. |

### 2.2 Deleted with the ws wire

- `topic_matches` — moves into `aimdb-core::session::topic_match` (same
  MQTT-style semantics, same tests). `now_ms()` does **not** move; nothing
  event-side needs a server clock.
- `WsCodec` and its per-connection id↔topic maps — the browser now speaks
  engine ids natively.
- The transport's multi-topic `split_multi_topic` — the bridge issues one
  `sub` frame per pattern.
- `ClientManager`'s pre-serialized `Data`-frame broadcast — the bus carries
  `(topic, payload)`; the per-connection codec envelopes each event (payload
  bytes stay `Arc`-shared; only the small envelope is per-subscriber).
- `WebSocketConnectorBuilder::with_raw_payload` — its purpose was bypassing
  the ws `Data` envelope; under AimX the envelope *is* the protocol. No
  in-tree or aimdb-pro users.
- `ClientConfig::topic_routed_subs` — existed solely because the ws wire
  pushed `Data{topic}` without ids. All subscriptions are id-routed now.

## 3. Gap analysis and decisions

### 3.1 Wildcard subscribe (DECIDED: optional `topic` on `Event`)

One wildcard subscription fans in many records. The server matches the
pattern against the registry **once at subscribe time** — the record set is
frozen at builder time (`configure<T>` exists only on `AimDbBuilder`), so
subscribe-time enumeration is complete. MQTT-style dynamic membership
(records registered later) is deferred until core grows runtime
registration, which would be its own design.

- `AimxSession::subscribe`: a topic containing `#`/`*` matches all current
  records via `topic_matches`, merges their update streams
  (`stream::select_all`) under the one subscription id, and emits one
  `Snapshot` per matched record on open. Records without remote access are
  skipped (logged), matching "subscribe to what can stream". A wildcard that
  matches nothing yields an empty stream that ends immediately — with a
  frozen record set, "no matches now" is "no matches ever". Exact-topic
  subscribe keeps its existing fast path and (unchanged) emits no snapshot
  on the UDS/serial/TCP paths.
- Core stays pull-per-record (no fan-out bus — no_std targets must not pay a
  per-record pump); the WS server's `ClientManager` bus keeps per-event
  pattern matching for many-client fan-out.
- **Engine plumbing:** subscription streams (server `Session::subscribe`,
  client `ClientHandle::subscribe`) change item type from `Payload` to
  `SubUpdate { topic: Option<Arc<str>>, data: Payload }` so each event can
  carry which record fired. This is the one place the implementation extends
  the message set beyond "add `topic` to `Outbound::Event`": without an item
  type carrying the topic, the engine pump has nothing to thread into the
  frame.

### 3.2 Subscribe ack (DECIDED: teach `AimxCodec` the frame)

ws-protocol emits `Subscribed{topics}` and the browser relies on it; AimX
runs `acks_subscribe:false` and `AimxCodec` rejects `Outbound::Subscribed`.
Decision: `AimxCodec` encodes/decodes `{"t":"subscribed","sub":S}`; the WS
server keeps `acks_subscribe:true`. UDS/serial/TCP stay
`acks_subscribe:false` (unchanged wire).

### 3.3 Snapshot routing (DECIDED: `sub` on `Snapshot`)

A deviation the plan's phase list didn't spell out, forced by the client
demux: `run_client` routes frames by subscription id, and a wildcard
subscription's snapshots arrive tagged with concrete record topics that do
not string-match the pattern key. Rather than teaching the client demux
pattern matching (state + O(subs) per snapshot), `Outbound::Snapshot` gains
the `sub` of the subscription that triggered it — the server knows it at
emission (snapshots are only emitted inside the subscribe handshake). The
engine hook `Session::snapshot(topic) -> Option<Payload>` becomes
`Session::snapshots(topic) -> Vec<(String, Payload)>` ("one snapshot per
matched record"); the default stays "none".

"One snapshot per matched record" is a claim the subscriber must be able to
*check*, because the burst is the one place a subscription can exceed its
bounded sink by a wide margin: a wildcard over N records emits N updates
before the consumer has run at all, and N is unbounded while the sink is
`SUBSCRIBE_CHANNEL_CAP` (256). Blocking the client demux until the consumer
drains is not an option — that loop also carries every other subscription and
pending RPC on the connection, so a subscriber that (say) awaits an RPC before
draining would deadlock itself. So snapshots take the same route as events:
they are numbered in the subscription's sequence space (`1..=N`, events
continuing at `N+1`), dropped on a full sink, and the shortfall folds into the
next delivered update's `SubUpdate::skipped`.

Numbering alone is not enough, because a gap only *reaches* the subscriber on an
update that is actually delivered. If the burst is truncated at its tail, the
next delivered update is the first live event — and a **static** subscription
may never produce one, leaving the consumer unable to tell a complete initial
state from a truncated one. So the burst is explicitly terminated: the server
flags its final snapshot (`"last":true`), and the client holds one sink slot in
reserve through the burst so that frame always has somewhere to land. Against
sink pressure this is infallible rather than merely likely: only the demux loop
sends on that channel and the consumer only removes, so a slot observed free
stays free; each subscription has its own channel; and the server emits the
whole burst inline in the Subscribe handler before starting the event pump, so
none of that subscription's events can interleave and take the slot.

The subscriber therefore sees exactly one update with `SubUpdate::snapshot_end`
set, carrying the burst's whole loss count in `skipped`, and can audit "one
snapshot per matched record" the moment the burst ends. Backpressure was
rejected as the alternative: the client demux loop also carries every other
subscription and every pending RPC on the connection, so blocking it on a full
sink would deadlock any consumer that awaits an RPC before draining.

The reserve bounds *sink overflow*, which is the failure this design exists to
fix, and nothing else. The flag rides the final snapshot's frame, so it is lost
whenever that frame is: if the payload fails to encode (`log_warn` + skip) or
the client rejects the frame as malformed, no `snapshot_end` arrives — and a
pattern matching *no* records emits no burst at all. Loss accounting survives
all three (the shortfall still folds into the next delivered update's
`skipped`); only the end-of-burst signal goes missing, so consumers must drive
the stream normally rather than looping until the flag. Hardening these was
weighed and declined: requiring `"last"` on every frame would tax every
mid-burst snapshot to guard a version skew the release process already
prevents, and a one-frame server-side lookahead would buy an encode failure
that must *also* land on the burst's last record of a *static* subscription
before it differs from an ordinary broken record. Closing them properly needs a
terminator independent of the snapshot payloads — the burst size `N` in the
subscribe ack, or a dedicated `snapend` frame — deferred until something
demands it.

### 3.4 Query / list result shapes (DECIDED)

- `record.query` params stay `{name, limit, start, end}` (`name` accepts
  MQTT wildcards and `*`; `start`/`end` are the time range — units are the
  handler's contract, ms for the persistence backend). The **result**
  becomes `{"records":[{"topic":T,"payload":V,"ts":MS}, …], "total":N}` —
  the old `QueryResult` shape minus the ws envelope. `QueryRecord` moves to
  `aimdb-core::remote` as the canonical row type;
  `aimdb-persistence::with_persistence` registration is updated to produce
  it (was `{values:[{record,value,stored_at}],count}`).
- `record.list`: `RecordMetadata` gains optional `schema_type` and `entity`
  fields. Core populates `entity` from the record key's leaf segment after the
  last `.` or `/` (the server owns the naming convention; clients must not
  parse topics). `schema_type` is populated by dispatches that own a schema
  registry — the WS dispatch stamps in the name its `StreamableRegistry`
  resolves. Core alone cannot resolve contract schema names and leaves it
  `None`.

  The rows are **full `RecordMetadata`**, not the old topic-scoped
  `TopicInfo` shape: the topic is `record_key`, and `name` is the Rust type
  name — a migrating client must read `record_key`, never `name`, as the
  topic. `TopicInfo` is deleted.

### 3.5 Error vocabulary (DECIDED: keep the 3-code core)

ws-protocol has 6 `ErrorCode`s + per-error `topic`/`message`; AimX collapses
to `not_found` / `denied` / `internal` as a bare `err` string. Decision:
accept the 6→3 collapse rather than widening `RpcError`.
`UNAUTHORIZED` stays out-of-band (`AuthError` at the HTTP upgrade, exactly
as today); `FORBIDDEN`→`denied`; `UNKNOWN_TOPIC`→`not_found`;
`SERIALIZATION_ERROR`/`WRITE_ERROR`/`SERVER_ERROR`→`internal`. The wire
tolerates an optional human-readable `msg` field alongside `err` (decoders
ignore unknown fields), reserved as headroom; plumbing messages through
`RpcError` is deliberately out of scope.

### 3.6 Auto-subscribe under engine demux (accepted limitation)

`with_auto_subscribe` seeds server-side subscriptions whose events carry
server-chosen sub ids (synthesized from `u64::MAX` downward so they cannot
collide with client-chosen ids). A raw WS consumer sees them fine; a
`run_client`-based consumer drops events for sub ids it never issued —
engine clients (including the new `WsBridge`) must subscribe explicitly.
The aimdb-pro hub's UI already does (`subscribeTopics` = exact names from
discovery), so nothing user-visible changes.

## 4. What lands where

- **Phase 1 (core, additive):** `topic_match.rs`, `SubUpdate` item type,
  `Event.topic` + `Snapshot.sub`, `subscribed` frame, wildcard subscribe +
  per-match snapshots in `AimxDispatch`, `QueryRecord` + persistence result
  shape, `RecordMetadata.schema_type`/`entity`, `AimxCodec` roundtrip suite.
- **Phase 2 (WS connector):** `AimxCodec` on the WS transport (one codec
  blob per WS text frame — no extra framing needed), dispatch converged on
  `record.query`/`record.list`, bus carries `(topic, payload)`, tests
  rewritten to drive AimX frames.
- **Phase 3 (browser):** `WsBridge` rewritten on `run_client` +
  `ClientHandle` over a `web_sys::WebSocket`-backed `Connection`/`Dialer`
  (single-threaded wasm `Send` wrappers, same pattern as `SendFuture`);
  reply/subscription correlation then exists exactly once
  (`aimdb-core/src/session/client.rs`). JS API (`write`, `query`,
  `listTopics`, `onStatusChange`, offline queue, reconnect) is preserved.
  `useAimDb.tsx` discovery reissued as a raw `record.list` req.
- **Phase 4 (delete):** `aimdb-ws-protocol` crate, `WsCodec`,
  `protocol.rs` shims, `aimdb-client` dead helpers; design 038 §2.5/§3.9
  annotated; CHANGELOGs.

Breaking for browser clients ⇒ Phases 1–4 land in the next
protocol-breaking release as one branch, one commit per work item (no
stacked PRs — house invariant). This document may land ahead of the code.

## 5. Go / no-go

Convergence pays: the fork costs a 353-line protocol crate, a 507-line
codec, two error mappings, a hand-rolled 835-line browser demux duplicating
`run_client`, and a WS-only query/list vocabulary — against a one-page
additive AimX extension (wildcard subscribe + two result-shape changes)
whose features every other transport inherits (CLI `watch 'temp.#'`, MCP
wildcard reads, serial/TCP browsers-of-the-future). The browser break is
contained to the bridge's wire (JS API preserved) and lands in a release
already flagged protocol-breaking. **Go.**
