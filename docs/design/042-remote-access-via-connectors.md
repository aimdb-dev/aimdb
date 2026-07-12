# Remote access via connectors

**Issue:** aimdb-dev/aimdb#39 — *Enable remote access for embedded / `no_std` environments*
**Status:** 🟢 Architecture settled; Phases 0–6 landed in code, Phase 7 (on-target validation) open.
**Scope:** This is the single design doc for the connector-convergence initiative. It captures the *what* and *why* and the load-bearing decisions — not implementation detail (read the code under [`aimdb-core/src/session/`](../../aimdb-core/src/session/) for that).

---

## The problem

Issue #39 wants AimX remote access to run on **embedded** (`no_std`/Embassy). The naive path — a new `RemoteTransport` trait with UDS/serial/TCP impls — would bolt a second I/O abstraction next to the connector framework that *already* crosses the std/Embassy boundary, and leave AimDB maintaining **four hand-rolled networking stacks**: an AimX server + client and a WebSocket server + client, each re-implementing bind/accept/connect, sessions, framing, and RPC.

So the real task is **not** "add a transport." It is: **converge those four stacks onto one shared, runtime-agnostic session abstraction in the connector layer**, after which embedded remote access — and any future transport — falls out for free.

## The decision

Pursue **convergence**, not a parallel transport trait. The connector layer already solves cross-runtime I/O and spawn-free execution; remote access rides it.

A connector is a **composition of capabilities** over the existing, kind-agnostic [`ConnectorBuilder::build -> Vec<BoxFuture>`](../../aimdb-core/src/connector.rs) spine (driven by one `FuturesUnordered`, no `tokio::spawn`). The framework owns every loop; an author implements only a small I/O adapter per capability and composes them in `build()`.

| capability | role | framework provides | author writes |
|---|---|---|---|
| **Sink** | data out (MQTT-pub, HTTP) | consume loop (`pump_sink`) | `publish()` — today's `Connector` |
| **Source** | data in (MQTT-sub) | read+route loop (`pump_source`) | `next() -> (topic, bytes)` |
| **Server** | accept sessions (AimX, WS) | accept + `run_session`/`serve` (reactive) | `Listener` + `Dispatch` + codec |
| **Client** | dial sessions (AimX, WS, sensor MCU) | `run_client`/`pump_client` (proactive: handshake, RPC demux, reconnect) | `Dialer` + codec |

**Server and Client share one substrate** — a framed `Connection`, an `EnvelopeCodec` (outer protocol frame), and one role-neutral logical message set (`Inbound`/`Outbound`) — so they are two engines over shared parts, not two stacks. The substrate is split across three layers: **transport** (`Connection`/`Listener`/`Dialer`), **codec** (`EnvelopeCodec`), and **dispatch** (`Dispatch` factory → per-connection `Session` with `call`/`subscribe`/`write`). The `EnvelopeCodec` *nests* the existing [M16 record-value `JsonCodec`](032-M16-aimx-json-codec.md) rather than replacing it.

## Load-bearing decisions

1. **`Payload = Arc<[u8]>` (raw bytes)**, not `serde_json::Value`. Cheap-clone for fan-out, `no_std+alloc`-native; one serde pass on hot paths — a JSON tree materializes only inside RPC handlers that inspect structure.
2. **One `Dispatch` trait** carries all reply cardinalities: `call` (one) / `subscribe` (stream) / `write` (none). A subscription is just a method whose reply is a `Stream`; both existing stacks already interleave RPC + streaming over one connection.
3. **`publish` is a sibling capability**, not absorbed into the session trait. `Sink` is today's `Connector` verbatim (Phase 1 collapsed the skeleton onto it — `pump_sink` takes `Arc<dyn Connector>`).
4. **Client-first embedded order.** A sensor MCU *dials a gateway and pushes records*, so `Dialer` + `run_client` is the embedded-critical, smallest-footprint path (one connection, no accept/fan-out) against #39's ~60–100 KB / ≥256 KB RAM budget. The substrate stays role-neutral so server and client share it.

## Invariants (hold across every phase)

- **Behavior-preserving ports** — porting a stack changes *no* wire protocol; wire-capture/golden tests gate each.
- **One engine per role** — server (reactive) and client (proactive) are distinct, each proven in `std` then `no_std`-ed once; never fork a role into parallel std/no_std engines or re-implement the shared substrate per role.
- **Toolkit is additive** — the `ConnectorBuilder -> Vec<BoxFuture>` escape hatch always works; no connector is forced through a capability that doesn't fit.
- **Single-writer-per-key** stays intact — `Session::write` routes through the existing producer/arbiter path; remote clients are request streams, never direct co-writers.
- **Spawn-free** — every phase only appends `BoxFuture`s to the runner.

## Roadmap & status

Std-first, behavior-preserving, incremental: build the abstraction in `std`, port the four stacks onto it wire-identically, *then* `no_std` the engines and add embedded transports. Each phase ships standalone value.

| # | Phase | Ships | Status |
|---|---|---|---|
| 0 | Freeze `dyn`-safe contracts (the decisions above + trait skeletons) | signature record | ✅ |
| 1 | Data-plane toolkit: `Source` + `pump_sink`/`pump_source`; migrate MQTT | easier third-party connectors | ✅ |
| 2 | Session substrate + server (`run_session`/`serve`) & client (`run_client`/`pump_client`) engines, std | the shared machinery | ✅ |
| 3 | Port AimX (server + client) onto the engines | AimX on shared engine; legacy loops deleted | ✅ |
| 4 | Port WebSocket (server + client) onto the engines | four stacks → two engines | ✅ landed (on-socket validation ongoing) |
| 5 | Make the engines runtime-neutral (`futures` channels + adapter `TimeOps`, no tokio/embassy) | engines cross-compile to `thumbv7em` | ✅ |
| 6 | Embedded transport crates (`Listener`+`Dialer`) | remote access over real links | 🟡 `aimdb-uds-connector` landed (UDS, both halves); serial/TCP + the no_std AimX *server* port are tracked follow-ups (below) |
| 7 | Validate on target MCU — memory budget, #39 acceptance | **#39 delivered** | ⬜ open — tracked by #13 under umbrella #39 |

**Value milestones:** after Phase 1, third-party connectors are a few lines; after Phase 4, all four hand-rolled stacks collapse onto two shared engines (shippable end state even if #39 is deprioritized); after Phase 7, embedded remote access on the connector framework.

**Tracked follow-ups (open issues):**
- **#120** — no_std AimX *server* (dispatch) port: cross-cutting `AnyRecord` + `RecordMetadataTracker` de-std. The AimX codec is already `no_std+alloc`; only `AimxDispatch` is std-gated.
- **#121** — `aimdb-tcp-connector` (tokio + embassy-net); **#122** — `aimdb-serial-connector` (COBS over `tokio-serial` + `embedded-io-async`). The two remaining Phase-6 transports.
- **#123** — transport-agnostic host client + `cli`/`mcp` `--connect <url>` resolver.
- **#41** — dropped-event tracking for remote-access subscriptions.
- **#13** — performance validation & benchmarking (the Phase-7 gate).

## Foundations reused (not rebuilt)

- Spawn-free runner + `ConnectorBuilder` spine ([028](028-M13-remove-spawn-trait.md)) and the spawn-free AimX supervisor ([030](030-M13-aimx-remote-spawn-free.md)).
- The `no_std`-capable record-value codec ([032](032-M16-aimx-json-codec.md)) — nested by the `EnvelopeCodec`.
- Record binding: `collect_inbound_routes` / `collect_outbound_routes` + `ProducerTrait` / `ConsumerTrait`.
