# 053 — Zenoh connector, with an rmw_zenoh-compatible ROS 2 profile

**Status:** RFC.
- **rev 1, 2026-09-23.** First draft.
- **rev 2, 2026-09-23.** The API moves from a per-link method (`.ros2()`) to two
  URL schemes on one shared session: `zenoh://` for plain Zenoh and `ros2://` for
  ROS topics. The changes are in §4.2–4.7, §8 and §9.
- **rev 3, 2026-09-23.** The `.msg` codegen importer is dropped. ROS types are
  hand-written structs with `#[derive(RosMessage)]`, and the common interfaces ship
  prebuilt in a new `aimdb-ros2-msgs` crate. The changes are in §2, §4.2–4.5, §8,
  §9, §10 and §11.
- **rev 4, 2026-09-23.** v1 is cut back in two ways:
  - The `ros2://` profile is **std-only** (Native backend).
  - The embedded backend speaks **plain `zenoh://` only**. MCU data reaches ROS
    through a std gateway on the same router (§4.8).

  Direct-from-MCU ROS moves to v2 (§4.9), which takes zenoh-nostd's missing
  liveliness API off the critical path. `Ros2Connector::new` now stands alone for
  ROS-only deployments, and the shared form is used only when both schemes are
  needed. A worked Tokio example is added (§4.10). The changes are in §1, §2, §4,
  §5, §7, §8, §9, §10 and §11.
- **rev 5, 2026-09-23.** Checked against AimDB @ `e759cbe`; each finding is
  folded in where it applies, marked **[verified]** when a claim was confirmed
  by reading or running code. The changes:
  - Core's additions become new accessors returning a new `#[non_exhaustive]`
    struct. Adding a field to the public `OutboundRoute` would have broken
    2.x (§4.6).
  - The `WIRE_FORMAT` const-assert fires at `cargo build`, not `cargo check`
    (§4.3, §9).
  - Per-link codecs and topic providers join the guarantee table (§4.3, §4.4).
  - The advertised QoS is a fixed default plus overrides, no longer derived
    from the buffer (§6).
  - Inbound `zenoh://` declares one subscriber per distinct key, not per
    link (§4.3).
  - The force-`Send` precedent and citations are corrected (§4.4, §5.2, §10).
  - Criterion 3 runs on a `current_thread` runtime (§9).
  - The native `ros2://` path ships without waiting for the embedded backend
    (§11).
- **rev 6, 2026-09-23.** The upstream claims are checked too: rmw_zenoh,
  zenoh-cpp, zenoh-nostd, `zenoh`, `cdr-encoding`, `xxhash-rust`, ros2cli,
  rclpy and REP 2000, at the commits in §13. Where running code settled a
  question, the claim is marked **[verified]**. The changes:
  - Spikes S3, S4 and S5 are answered offline (§10).
  - §5.2 is corrected on two points that would not have compiled or worked:
    - one leaked `Resources` cannot be reused across reconnects;
    - the shim's link must own its socket (issue #11).
  - The `Send` gap upstream is wider than a mutex swap (§5.2).
  - Serial is issue #9 and still open; #11 is a lifetime bug (§5.2).
  - rmw_zenoh's default depth is 42, which corrects rev 5's §6 wording.
    Rolling vendors zenoh-c 1.10.1, not 1.8.0 (§3).
  - REP 2000 lists `rmw_zenoh_cpp` as Tier 1 for Kilted and has no Lyrical
    section yet (§1).
  - Humble keys carry a placeholder hash, not none (§2).
  - The native publisher mirrors rmw_zenoh's congestion control (§5.1).
- **rev 7, 2026-09-26.** The domain ID and node namespace get an API, following
  ROS's layering: the domain is set per session on `Ros2Connector`, and the
  namespace per node on `Ros2Node` (§4.2). Q6 is answered: topics stay fully
  qualified (§4.3, §10). Review findings are folded in too:
  - Core's serializer setters clear the recorded wire format, so a custom
    serializer after a codec verb now warns (§4.3, §4.4, §4.6).
  - The `WIRE_FORMAT` const-assert becomes a `build()` check (§4.3, §9).
  - `aimdb-cdr` decodes both byte orders (§4.4).
  - The wildcard, `SchemaType::NAME` and `RouteMeta` duplication consequences
    are written down (§4.3, §4.4, §4.6).

  - `aimdb-ros2-msgs` is postponed to a later phase. In v1 every ROS type,
    standard or custom, is hand-written with the derive (§4.5).

  The changes are in the scope list, §2, §4.2–4.6, §4.10, §8, §9, §10, §11 and
  §12.

Nothing is implemented. **[checked]** marks a claim read from upstream source at
the commits listed in §13; **[verified]** marks one confirmed by reading or
running code at those commits. **[spike]** marks a question only a running system can
answer; §10 collects them.
**Predecessors:** [052 — Runtime-neutral connectors](052-runtime-neutral-connectors.md)
(this connector is written in 052's shape from the start),
[045 — Per-link codec selection](045-per-link-codec-selection.md),
[041 — Data contracts as first-class capabilities](041-data-contracts-integration.md)
(`RosMessage` follows the one-verb-per-contract rule; its connector-side
registration follows `Streamable`'s precedent).
**Scope:**
- New crates `aimdb-zenoh-connector` and `aimdb-cdr`. A prebuilt
  `aimdb-ros2-msgs` is a later phase (§4.5).
- Additive changes to `aimdb-data-contracts`: `RosMessage`, `Linkable::WIRE_FORMAT`,
  `LinkCodec::WIRE_FORMAT` (recorded on each link), and `link_codecs::Cdr`.
- A new `#[derive(RosMessage)]` in `aimdb-derive`.
- Additive changes to `aimdb-core`: two new route accessors that return
  a new `#[non_exhaustive]` `RouteMeta` (the record's `TypeId` and the link
  config), plus a reserved `aimdb.topic_provider` config key, and serializer
  setters that clear the reserved `aimdb.wire_format` key. The existing
  `OutboundRoute`, `ConnectorLink` and `collect_*_routes` are left alone (§4.6).
- `aimdb-codegen` is not touched.

Nothing breaks. No existing public struct gains a field.

---

## 1. The question

> Can AimDB records appear as ROS 2 topics, including from a `no_std + alloc`
> microcontroller, without a ROS install on the AimDB side?

Yes. There are two conditions: speak Zenoh, and match what `rmw_zenoh` puts on the wire.
In v1 the ROS conventions live on std devices. A microcontroller publishes plain
Zenoh to the same router, and a std gateway re-publishes the same contract as a ROS
topic (§4.8). That needs no new code on the MCU beyond the `zenoh://` link itself.
Direct-from-MCU ROS is v2.

Two upstream facts make this worth doing now:

- `rmw_zenoh_cpp` is a **Tier 1** middleware on all platforms and architectures,
  alongside Fast DDS and Cyclone. **[verified]** REP 2000 lists this for
  **Kilted** (`rep-2000.rst:1239`, master @ `11ca24a`). REP 2000 has no Lyrical
  section yet, and the Lyrical release page states no tier. Lyrical still
  ships `rmw_zenoh` with zenoh 1.8.0 (its changelog). Jazzy's REP 2000 table
  has no Zenoh row, although `rmw_zenoh` has a `jazzy` branch. A robot that
  runs it needs only a Zenoh router between it and us. We don't need a bridge process,
  a DDS stack or an XRCE agent.
- Zenoh is the same protocol on a microcontroller and in the cloud. DDS splits
  into DDS-XRCE on constrained devices, and MQTT splits in its own way. A pure-Rust
  `no_std` implementation, `zenoh-nostd`, now exists under the Eclipse org.

Plain Zenoh is also useful without ROS: an AimDB-to-AimDB transport with
routing, wildcards and peer mode that MQTT does not have. The connector therefore
serves two schemes, which can share **one** Zenoh session:
- `zenoh://` is plain Zenoh with any codec;
- `ros2://` is a ROS topic, following every rmw_zenoh convention.

## 2. Goals and non-goals

**Goals**

- **G1.** `zenoh://` links for outbound and inbound, with any codec.
- **G2.** `ros2://` links for outbound and inbound, **on std**. The connector uses
  rmw_zenoh-compatible key expressions, CDR payloads, the publication attachment,
  and liveliness tokens, so AimDB entities appear in `ros2 node list` and
  `ros2 topic info -v`. It targets Jazzy, Kilted, Lyrical and Rolling.
- **G3.** Two backends in 052's shape:
  - `Native` uses the `zenoh` crate on std and serves both schemes.
  - `Embedded` uses `zenoh-nostd` on `no_std + alloc`, over any `StreamDialer`,
    and serves `zenoh://` only.
- **G4.** ROS type identity is a compile-time property of the Rust contract type.
  A wrong combination of type and scheme fails at compile time or at `build()`,
  not as silence on the wire. §4.3 names the two exceptions that remain.
- **G5.** Where a device uses both schemes, they share one session: one router
  connection per device.

**Non-goals for v1**

- `ros2://` on the embedded backend, meaning an MCU as a ROS node in its own right.
  This is v2 (§4.9). Until then MCU data reaches ROS through a gateway (§4.8).
- Services and actions (liveliness entity kinds `SS`/`SC`, Zenoh queryables).
- `TRANSIENT_LOCAL`. The native backend gets it in v1.1 (§6).
- Humble. It has no REP-2016 type hashes. **[verified]** rmw_zenoh's `humble`
  branch still builds the same key shape, with the literal
  `TypeHashNotSupported` in the hash segment (`rmw_publisher_data.cpp:75`). So
  support would mostly be a per-distro constant, and it is left out for scope,
  not feasibility.
- Consuming the ROS graph. We announce our own entities and never build a graph
  cache.
- A `.msg` importer or any other code generation. ROS types are written by hand
  with a derive (§4.5).
- A prebuilt message crate (`aimdb-ros2-msgs`). It is postponed to a later phase
  (§4.5).
- Auto-exposing every record of a type. Topics belong to instances, not types, and
  writes into the plant stay explicit (§8).
- DDS robots. A robot on the default `rmw_fastrtps_cpp` needs
  `zenoh-bridge-ros2dds`, which is out of our hands.
- Shared memory, Zenoh storages as an AimDB persistence backend, and AimX sessions
  over Zenoh.

## 3. What rmw_zenoh expects on the wire **[verified]**

Read at `rolling` @ `1f7c62a` and on the `jazzy`, `kilted` and `lyrical`
branches. The GID hash was also run (S3, §10).

| Element | Format |
|---|---|
| Data key | `<domain_id>/<fully_qualified_name>/<type_name>/<type_hash>`, e.g. `0/chatter/std_msgs::msg::dds_::String_/RIHS01_df668c74…`. Leading and trailing slashes are stripped from the name (`liveliness_utils.cpp:95`). The type name is `<pkg>::msg::dds_::<Name>_` (`type_support_common.cpp:46`) |
| Payload | CDR (`DDS_CDR`, i.e. XCDR1) with the 4-byte encapsulation header, in host byte order (Fast-CDR `DEFAULT_ENDIAN`, `cdr.cpp:27`). That is little-endian on every target AimDB ships |
| Attachment | **Required.** A subscriber that receives a sample without one logs `Unable to obtain attachment` and drops it (`rmw_subscription_data.cpp:417`). 33 bytes, written by zenoh-cpp's `ext::Serializer`: sequence number `i64` LE (per publisher, starting at 1), then source timestamp `i64` LE (ns since Unix epoch). Next comes `0x10`: the serializer writes `std::array` as a sequence, so a LEB128 length precedes it (zenoh-cpp `1.8.0-4-gaf381b4`, `serialization.hxx:237`). Last is the 16-byte GID |
| GID | XXH3-128 of the entity's full liveliness key expression; `low64` then `high64`, each LE |
| Node token | `@ros2_lv/<domain>/<zid>/<node_id>/<node_id>/NN/<enclave>/<namespace>/<node_name>` |
| Pub/sub token | `@ros2_lv/<domain>/<zid>/<node_id>/<entity_id>/MP\|MS/<enclave>/<namespace>/<node_name>/<topic>/<type_name>/<type_hash>/<qos>` |
| Name mangling | `/` becomes `%`; an empty enclave or namespace is `%` |
| QoS field | `<rel>:<dur>:<hist>,<depth>:<dl_s>,<dl_ns>:<ls_s>,<ls_ns>:<lv>,<lv_s>,<lv_ns>`. Each component is empty when it equals rmw_zenoh's default: `KEEP_LAST`, depth **42**, `RELIABLE`, `VOLATILE`, `AUTOMATIC` (`qos.cpp`, the same on all three distros). So depth 10 with everything else default is `::,10:,:,:,,`, and only depth 42 gives the all-empty `::,:,:,:,,` |

Differences between distros and dependencies:
- Jazzy, Kilted and Lyrical produce identical keys and tokens for plain message
  types. Lyrical adds an optional trailing `/backends:…` token segment, which
  appears only for `rosidl::Buffer`-carrying types. We never emit it.
  **[verified]**: Jazzy and Kilted have no backends code, and the Jazzy→Lyrical
  diff of `liveliness_utils.cpp` is otherwise only the backend escaping.
- Jazzy, Kilted and Lyrical vendor zenoh-c 1.8.0 plus fixes (commit `05bd370`,
  zenoh `2687c51`). **Rolling has moved to zenoh-c 1.10.1** plus fixes
  (`07b0d43`), so S2 has to cover both. All four build the router with
  `zenoh/transport_serial`, which matters once zenoh-nostd gains serial (§5.2).
- **Subscribers are zenoh-ext `AdvancedSubscriber`s** (`rmw_subscription_data.cpp:434`).
  For a `VOLATILE` subscriber they use default options and receive a plain
  `put` like any subscriber. A `TRANSIENT_LOCAL` subscriber also asks
  publishers it detects through liveliness for history. Our v1 publishers
  have no cache and are not detectable, so such a subscriber, rviz on `/map`
  for example, gets live samples but no history until v1.1 (§6).

Two consequences shape the design.

1. **Every publish carries a wall-clock timestamp.** `RuntimeOps::unix_time()`
   returns `Option<(u64, u32)>` and is already runtime-neutral. On std, where v1's
   `ros2://` runs, it is always available. For v2's direct-from-MCU ROS, a `None`
   (a bare MCU with no time sync) means the attachment carries `0` and the connector
   warns once at build. rmw_zenoh accepts that, and `source_timestamp` is simply
   meaningless for that publisher. The message's own `header.stamp` is a separate
   matter; §4.8 covers it for the gateway pattern.
2. **Type identity lives in the key.** A wrong type hash does not produce an error.
   Zenoh routes on exact keys, so the two sides simply never meet. §4.3's checks
   guarantee that the *right* hash is used for the Rust type on the link. Only the
   interop test (§9) can prove that the hash matches what the robot runs.

## 4. Architecture

### 4.1 Crate layout

```text
aimdb-zenoh-connector
├── connector.rs    ZenohConnector<B> (scheme `zenoh`), sealed Backend trait
├── shared.rs       the session state connectors on one session hold (§4.7)
├── ros2.rs         `std`: Ros2Connector (scheme `ros2`)
├── registry.rs     `std`: TypeId → ROS type name + hash, filled by `.register::<T>()`
├── link_ext.rs     `std`: Ros2LinkExt, QoS overrides for the long form (§4.3)
├── profile/        the rmw_zenoh profile: pure, alloc-only, host-tested once
│   ├── keys.rs         data keys, liveliness tokens, name mangling, QoS strings
│   ├── gid.rs          XXH3-128 over the token (xxhash-rust, no_std)
│   └── attachment.rs   the 33-byte attachment, encoded into a stack array
├── native.rs       `std`: the zenoh crate, both schemes
└── embedded/       `embedded`: zenoh-nostd, no_std + alloc, `zenoh://` only
    ├── link.rs         ZLinkManager/ZLink shim over StreamDialer/ByteStream
    └── session.rs      connect → declare → run ∥ publish → back off → reconnect
```

In v1 only the native backend uses the profile. It is written `no_std + alloc`
anyway, so v2 (§4.9) can reuse it on the MCU unchanged. MQTT's `Native`/`Embedded`
split is the precedent (`aimdb-mqtt-connector/src/connector.rs`).

### 4.2 User-facing API

In v1 every ROS type is a hand-written struct with one derive. That covers
standard interfaces such as `sensor_msgs/Temperature` and custom types from the
robot team's package alike (§4.5):

```rust
#[derive(Clone, Debug, Serialize, Deserialize, RosMessage)]
#[ros(type = "cell_msgs/msg/SpindleCommand", hash = "RIHS01_<64 hex, from `ros2 topic info -v`>")]
pub struct SpindleCommand {            // field order and types mirror the .msg (§4.5)
    pub rpm: f64,
    pub enabled: bool,
    pub tool_id: String,
}
```

There are three shapes of deployment, one connector setup each.

**ROS only (Tokio).** This is the common case: one connector, which owns its
session.

```rust
use aimdb_zenoh_connector::{Ros2Connector, Ros2Node};

let ros2 = Ros2Connector::new("tcp/192.168.10.5:7447", Ros2Node::new("cell4_gateway"))
    .register::<Temperature>()
    .register::<SpindleCommand>();

let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(ros2);

// ROS publisher: CDR, rmw_zenoh key, attachment, MP token.
builder.configure::<Temperature>("cell4.temperature", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
        .source(read_probe)
        .linked_to("ros2://cell4/temperature");
});

// ROS subscriber: the typed key, MS token.
builder.configure::<SpindleCommand>("cell4.spindle.cmd", |reg| {
    reg.buffer(BufferCfg::Mailbox)
        .linked_from("ros2://cell4/spindle_cmd");
});
```

**Plain Zenoh only (Tokio or Embassy).** On Embassy the transport comes from the
adapter, exactly as `MqttConnector::transport` does:

```rust
let zenoh = ZenohConnector::new("tcp/192.168.10.5:7447")
    .transport(EmbassyNet::tcp(stack, ZENOH_RX.init([0; 2048]), ZENOH_TX.init([0; 2048])));

builder.configure::<MachineState>("cell4.state", |reg| {
    reg.buffer(BufferCfg::SingleLatest)
        .linked_to_with("zenoh://aimdb/cell4/state", Postcard::<128>);
});
```

**Both schemes on one session (Tokio).** This is the gateway case (§4.8): the ROS
connector is derived from the Zenoh one, so they share a router connection.

```rust
let zenoh = ZenohConnector::new("tcp/192.168.10.5:7447");
let ros2  = zenoh.ros2(Ros2Node::new("cell4_gateway")).register::<Temperature>();

let mut builder = AimDbBuilder::new()
    .runtime(runtime)
    .with_connector(zenoh)
    .with_connector(ros2);
```

**Domain ID and namespace.** These follow ROS's own layering. The domain belongs to
the context, which is one session here. The namespace belongs to the node.

```rust
let ros2 = Ros2Connector::new("tcp/192.168.10.5:7447",
        Ros2Node::new("cell4_gateway").namespace("/cell4"))
    .domain_id(7)
    .register::<Temperature>();

// Shared session: the domain goes on the ROS view, since `zenoh://` has none.
let ros2 = zenoh.ros2(Ros2Node::new("cell4_gateway").namespace("/cell4")).domain_id(7);
```

- **The domain resolves the way rcl resolves it.** An explicit `.domain_id(n)` wins.
  Without one, the connector reads `ROS_DOMAIN_ID`, and falls back to `0` when that
  is unset. **[checked]**: rcl reads the variable only when no domain was set
  (`rcl/src/rcl/init.c:152`, `domain_id.c:25`), and rmw_zenoh stores one domain per
  context (`rmw_init.cpp:66`). Reading the environment matters because the domain is
  the first segment of every key and token. A gateway pinned to `0` while the robot
  runs on `7` fails silently, like a wrong type hash (§3). A `ROS_DOMAIN_ID` that is
  not an integer fails `build()`, as it fails `rcl_init`.
- **The domain is on the connector, not on `Ros2Node`.** If several nodes per
  session arrive later (§4.7), they cannot disagree on a domain that rmw_zenoh keeps
  per context anyway.
- **The namespace is set in code** or from the application's config. AimDB is not
  started by `ros2 launch`, so `--ros-args -r __ns:=…` is not parsed. The default
  is the root namespace. The namespace changes the node's identity (its tokens and
  `ros2 node list`), not its topics (§4.3).
- **The enclave** is always the default (`%` in tokens) in v1.

`Ros2Connector` and `.ros2(..)` exist only with the `std` feature. On an embedded
build, a `ros2://` link fails `build()` with core's unregistered-scheme error, and
the connector docs point to §4.8.

The endpoint uses Zenoh's own locator syntax (`tcp/host:port`), so strings copied
from a Zenoh or rmw_zenoh config work unchanged. The native backend also accepts
`.with_zenoh_config(zenoh::Config)` for peer mode, TLS, QUIC and access control.

### 4.3 Link semantics

**`zenoh://<keyexpr>`: plain Zenoh**

- The resource is the Zenoh key expression verbatim. AimDB 2.0 keeps `/` inside
  external addresses, so `zenoh://a/b/c` is `a/b/c`.
- Outbound: one `put` per value, with whatever codec the link carries.
- Inbound: one declared subscriber per **distinct** route topic, not per link.
  Wildcards (`*`, `**`) are allowed. The connector's `Source` yields the
  *subscriber's* key expression as the route topic, not the sample's key, so
  `pump_source`'s exact-match router still works. **[verified]**: the router
  compares by string equality, and it already fans one topic out to every
  route that shares it (`router.rs:104`). With one subscriber per link, N
  records sharing a key would each receive every sample N times. MQTT dedupes the same way through
  `RouterBuilder::resource_ids()`. The route topic is the one
  `collect_inbound_routes` resolves, so an inbound `with_topic_resolver` is
  honoured. The cost: a record linked to `aimdb/*/state` gets the payload but not
  the concrete key it arrived on, so it cannot tell `cell4` from `cell5`. A record
  that needs to know links one concrete key per record.
- No tokens, no attachment, no type in the key. CDR over `zenoh://` is allowed
  (`linked_to_with(url, Cdr::<256>)`) for non-ROS consumers that want it. It will
  not reach ROS, and the docs say so.

**`ros2://<topic>`: a ROS topic**

- The resource is the fully qualified topic name without its leading slash, so
  `ros2://cell4/temperature` is `/cell4/temperature`. Topics are never resolved
  against the node namespace: a node in `/cell4` linked to `ros2://temperature`
  publishes `/temperature`. The URL could not express the difference anyway.
  **[verified]**: `LinkAddress::parse` keeps `/` and strips leading slashes, so
  `ros2:///cell4/temperature` is the same topic, and an absolute name cannot be
  told from a relative one. The connector docs say this, since ROS users will
  expect a namespace to prefix topics.
- Direction comes from the link. `linked_to` makes AimDB a ROS publisher (`MP`
  token); `linked_from` makes it a subscriber (`MS` token). No method names the
  role. The scheme names the protocol, as `mqtt://` and `knx://` already do.
- The payload is the type's `Linkable` encoding, which must be CDR (see the
  guarantees below).
- At build, the connector looks up each route's `TypeId` in its registry to get
  the ROS type name and hash. It then derives the data key, token and GID once and
  holds them for the life of the session.

**Guarantees: every mismatch fails before the wire**

| Mistake | Caught | How |
|---|---|---|
| `.register::<T>()` on a type that isn't a `RosMessage` | compile time | `register` is bounded on `T: RosMessage` |
| `.register::<T>()` on a hand-implemented `RosMessage` whose `Linkable` is not CDR | `build()` | `register` checks `T::WIRE_FORMAT` (§4.4), and `build()` reports the mismatch with the type's name. The derive always emits CDR, so only hand-written impls can hit this. rev 5's inline-const assert was dropped in rev 7. It fired on `cargo build` but not on `cargo check` or in rust-analyzer, and it needed a special trybuild setup, all for a rare mistake |
| A malformed `type` or `hash` in `#[ros(..)]` | compile time | The derive validates `pkg/msg/Name` and `RIHS01_` + 64 hex |
| A malformed type name or hash in a hand-written impl | `build()` | Checked at registration, so the error names the type |
| A `ros2://` link on a type that was never registered | `build()` | The route's `TypeId` (from `RouteMeta`, §4.6) is missing from the registry |
| A `ros2://` link with a non-CDR codec: `linked_to_with(url, Json)`, or `.with_link_codec(Postcard::<N>)` on the long form | `build()` | Every codec verb records its `LinkCodec::WIRE_FORMAT` in the link config (§4.4), and the connector refuses anything other than `cdr`, read from `RouteMeta::config` |
| A `ros2://` outbound link with `.with_topic_provider(..)` | `build()` | A provider can send a value to a topic that has no precomputed key, token or GID. `finish()` records `aimdb.topic_provider` in the link config (§4.6), so the connector refuses it |
| A topic, node name or namespace that breaks ROS name rules | `build()` | The profile's name validator |
| A `ROS_DOMAIN_ID` that is not an integer | `build()` | Parsed once at build, as `rcl_init` does (§4.2) |
| `ros2://` links with no `Ros2Connector` registered | `build()` | Core's existing refusal for a scheme no connector claims. **[verified]**: it is recorded when the link is finished (`typed_api.rs:930`) and reported by `build()`, so `with_connector` must come before `configure`, as every example here does |

Two gaps remain that the design cannot close.

1. **A custom serializer.** The long form
   `reg.link_to("ros2://…").with_serializer(|..| ..)` installs an arbitrary
   serializer, and a connector cannot inspect a closure. Such a link carries no
   recorded wire format, so the connector **warns** at `build()` rather than
   refusing it. That keeps it the documented escape hatch that voids the
   guarantee. The warning also covers a `with_serializer` placed *after* a codec
   verb, because the serializer setters clear the recorded format (§4.4).
2. **Hand-written field layout.** CDR is positional. A struct whose fields are out
   of order or wrongly typed relative to the `.msg` (`f32` for `float64`) decodes
   garbage or fails to decode at runtime. Nothing catches that at compile time in
   v1. §4.5's mapping table is the mitigation, and §4.5's future hash check would
   turn it into a compile error.

The interop test (§9) is the backstop for both.

**QoS.** The advertised QoS is a fixed default (§6). To override it, use the
long form with the type's own codec. `with_link_codec` comes from
`LinkCodecBuilderExt`:

```rust
reg.link_to("ros2://cell4/temperature")
    .with_link_codec(link_codecs::Default)     // T's Linkable, i.e. CDR
    .with_depth(1)                             // Ros2LinkExt
    .with_reliability(Reliability::BestEffort)
    .finish();
```

`Ros2LinkExt` pushes `ros2.depth` / `ros2.reliability` through `with_config`, the
same way `MqttLinkExt` pushes `qos` **[verified]** (`link_ext.rs`). The same
extension exists on inbound builders, and the connector reads it through
`RouteMeta::config` (§4.6).

### 4.4 Contracts: `RosMessage`, `WIRE_FORMAT` and CDR

**`RosMessage`**, in `aimdb-data-contracts` behind a new `ros2` feature:

```rust
/// The type is a ROS 2 interface. Unlocks `ros2://` links through
/// `Ros2Connector::register::<T>()` (design 041's one-verb rule).
pub trait RosMessage: Linkable {
    /// DDS-mangled name, as rmw_zenoh puts it in keys and tokens.
    const ROS_TYPE_NAME: &'static str;
    /// REP-2016 hash: `RIHS01_` + 64 lowercase hex.
    const ROS_TYPE_HASH: &'static str;
}
```

The registration mirrors `StreamableRegistry` in the WebSocket connector: a type
opts in once on the connector, and links carry the data.

**`Linkable::WIRE_FORMAT`** is an additive associated const with a default, so
every existing impl compiles unchanged:

```rust
pub trait Linkable: SchemaType + Sized {
    /// What `to_bytes` / `from_bytes` speak. Informational for most
    /// connectors; `Ros2Connector::register` requires `Cdr`.
    const WIRE_FORMAT: WireFormat = WireFormat::Unspecified;
    // … unchanged …
}

#[non_exhaustive]
pub enum WireFormat { Unspecified, Json, Postcard, Cdr }
```

**`#[derive(RosMessage)]`**, in `aimdb-derive` and re-exported by
`aimdb-data-contracts` under `ros2`, takes `#[ros(type = "pkg/msg/Name", hash = "RIHS01_…")]`
and emits three impls, so the user writes none by hand:
- `SchemaType`, with `NAME` set to the ROS type (`cell_msgs/msg/SpindleCommand`).
  Existing `NAME`s are plain identifiers such as `"temperature"`. **[verified]**
  that its consumers treat it as an opaque string, so the slashes are harmless:
  the WebSocket `StreamableRegistry` and the WASM schema registry key on it
  (`registry.rs:35`, `schema_registry.rs:114`), and it is `Observable::SIGNAL`'s
  default (`lib.rs:218`). ROS type names are globally unique, which suits the
  WebSocket registry's refusal of two types under one name;
- `Linkable` as CDR through `aimdb-cdr`, with `WIRE_FORMAT = Cdr`, a bounded
  `encode_into`, and `ENCODE_BUFFER_CAPACITY` (overridable with
  `#[ros(encode_capacity = N)]`);
- `RosMessage`, with the DDS name derived from the ROS name
  (`cell_msgs/msg/SpindleCommand` becomes `cell_msgs::msg::dds_::SpindleCommand_`)
  and the hash validated.

The existing `#[derive(Linkable)]` is JSON-only **[verified]**
(`aimdb-derive/src/lib.rs:69`), and it sets `WIRE_FORMAT = Json` too. Postcard
`Linkable` impls do not come from a derive: `aimdb-codegen` emits them
(`rust.rs:1052`). v1 leaves those at `Unspecified` to keep codegen untouched.
The const only has to be truthful for CDR, which is all this connector reads.
Q9 revisits it if other tools start reporting formats.

**`LinkCodec::WIRE_FORMAT`** is the per-link counterpart, added as the same kind
of defaulted associated const. The built-in codecs set theirs:
- `Json` and `Postcard` set their own variant;
- `Cdr` sets `Cdr`;
- `Default` forwards `T::WIRE_FORMAT`.

The codec verbs record it on the link through the existing `with_config`,
under the reserved key `aimdb.wire_format` (§4.6). The verbs are
`linked_to` / `linked_from` (through `Default`), `linked_*_with` and
`with_link_codec`. A per-link codec is
the blessed design-045 path, not an escape hatch. Without this record,
`linked_to_with("ros2://…", Json)` would put JSON on a ROS topic silently.
**[verified]**: `with_config` appends to the link's `Vec<(String, String)>`,
and no connector in the tree rejects unknown keys, so the extra entry is inert
everywhere else.

**The record follows the serializer.** Core's `with_serializer`,
`with_serializer_into` and `with_deserializer` remove any recorded
`aimdb.wire_format`. The codec verbs install their serializer through those same
setters (**[verified]** `linkable.rs:44`, `link_codec.rs:184`), then record the
format. So a link carries a format exactly when a codec verb was the last to set
its serializer. A custom serializer anywhere after it leaves no format, and the
connector warns (§4.3). Removing the entry, rather than appending a second one,
means no reader has to know which duplicate wins.

**CDR becomes the type's default format everywhere.** An MQTT link on a
`RosMessage` type also sends CDR unless it names another codec
(`linked_to_with(url, Json)`, design 045). For a type that exists to mirror a ROS
interface, that is the right default, and it is documented.

**`link_codecs::Cdr<const N: usize = 256>`**, behind `linkable-cdr`, is for the
opposite case: CDR on a link whose type's default is something else. It advertises
`ENCODE_BUFFER_CAPACITY = Some(N)`, so it takes 045's bounded, allocation-free path.

**The CDR implementation needs its own crate.** `cdr-encoding` 0.11, the serde CDR
crate behind RustDDS and `ros2-client`, is std-only **[checked]**: it imports
`std::io` and `std::marker`. The new `aimdb-cdr` crate provides serde for XCDR1:
- `no_std`, with `alloc` optional;
- encodes little-endian, as rmw_zenoh does on every target AimDB ships (§3);
- decodes both byte orders, taken from the encapsulation header (`CDR_LE` or
  `CDR_BE`), since a big-endian ROS host sends big-endian CDR under the same
  type. Any other representation ID is a decode error;
- the encapsulation header;
- alignment measured from the end of that header;
- strings as a `u32` length that includes the NUL;
- sequences with a `u32` count; fixed arrays without one.

It is a few hundred lines, tested against golden bytes captured from
`ros2 topic pub`. An upstream `no_std` PR to `cdr-encoding` is worth offering, but
not worth waiting for.

### 4.5 Getting ROS types: hand-written, no codegen

In v1 there is one path, and it runs no generator. A cell usually touches a
handful of interfaces, standard and custom, and each is a short struct. The user
writes it with `#[derive(RosMessage)]` (§4.4):
- **Field order** follows the `.msg` exactly. CDR is positional.
- **Field names** should match the `.msg` too. They do not affect the wire, but they
  keep the door open for the hash check below.
- **Constants** in the `.msg` are not on the wire and are left out.
- **Nested types** are other hand-written `RosMessage` types. A stamped message
  therefore also needs `std_msgs/Header` and `builtin_interfaces/Time`, two
  short structs written once per project (§4.10 shows them).
- **The hash** is copied once from the robot. `ros2 topic info -v` prints it for
  any active topic as `Topic type hash: RIHS01_…` on Jazzy, Kilted, Lyrical and
  Rolling (**[verified]**, S4 in §10). rmw_zenoh also carries it in every data key.

The type mapping, as the docs will print it:

| ROS (`.msg`) | Rust |
|---|---|
| `bool` | `bool` |
| `byte`, `char`, `uint8` / `int8` | `u8` / `i8` |
| `uint16` … `int64` | `u16` … `i64` |
| `float32` / `float64` | `f32` / `f64` |
| `string`, `string<=N` | `String` (the bound is checked on encode) |
| `T[N]` | `[T; N]` |
| `T[]`, `T[<=N]` | `Vec<T>` (the bound is checked on encode) |
| `pkg/Type` | a `RosMessage` struct |
| `wstring` | unsupported in v1 |

**Later, if hand-writing becomes painful.** The derive can compute the REP-2016
hash itself: SHA-256 over ROS's canonical type description, built from the
struct's field names and mapped types, and recursing through nested `RosMessage`
types via associated consts. If it computes the hash and compares it with the
pasted one, a wrong field order, a wrong type or a stale hash all become compile
errors. That closes both gaps from §4.3. It is deferred until usage shows it is
needed.

**Later phase: a prebuilt `aimdb-ros2-msgs`.** A crate of the common interfaces
(`builtin_interfaces`, `std_msgs`, `sensor_msgs`, `geometry_msgs`) would spare
users the hand-written standard types. It is postponed because it brings its own
upkeep: an internal generation script run against a real ROS install, per-package
features, and a CI job in a ROS image that checks every shipped type's hash and
decoding per distro. That is worth doing once v1 shows which interfaces users
actually hand-write. Nothing in v1 has to change for it: the crate would contain
only `#[derive(RosMessage)]` types.

### 4.6 Changes to core (all additive)

The connector needs three facts per route that core's route accessors do not
give it today:
- the record's `TypeId`, for the registry lookup;
- the inbound link config, for QoS overrides and the recorded wire format;
- whether an outbound link has a topic provider.

**[verified]**: `RecordEntry` already stores `type_id` (`builder.rs:51`).
`collect_inbound_routes(scheme)` returns `Vec<(String, IngestFn)>` and drops
`InboundConnectorLink::config` (`builder.rs:1174`). Outbound routes carry
`config` but no type.

**Not a new field on `OutboundRoute`.** It is a public struct with all-public
fields and no `#[non_exhaustive]` (`builder.rs:38`), in a crate released as
2.0.0. Core itself destructures it exhaustively (`session/pump.rs:44`,
`session/client.rs:865`), and so may any third-party connector. A new field
there is a semver break. The same holds for `ConnectorLink` (`connector.rs:453`).

The additions instead:

1. **A new route-metadata struct.**

   ```rust
   #[non_exhaustive]
   pub struct RouteMeta {
       pub topic: String,
       pub type_id: TypeId,
       pub config: Vec<(String, String)>,
   }
   ```

   For outbound routes, `topic` and `config` repeat what `OutboundRoute` already
   carries. That is deliberate: one shape serves both directions, and a connector
   reads route facts from `RouteMeta` alone.

2. **Two new accessors** that pair each route with its `RouteMeta`:
   - `collect_outbound_routes_with_meta(scheme) -> Vec<(OutboundRoute, RouteMeta)>`;
   - `collect_inbound_routes_with_meta(scheme) -> Vec<(IngestFn, RouteMeta)>`.

   The existing `collect_*_routes` are left in place and delegate to them. The
   `#[non_exhaustive]` on `RouteMeta` means later facts need no third accessor.

3. **A reserved config key for topic providers.** `OutboundConnectorBuilder::finish`
   appends `("aimdb.topic_provider", "true")` when `.with_topic_provider(..)` was
   called. The provider itself is fused into the route's source at `finish()`
   (`typed_api.rs:979`), so nothing else downstream can see it. This rides the
   existing `with_config` vector, so no struct changes. The per-link wire format
   from §4.4 uses the same mechanism under `aimdb.wire_format`. Both keys live
   in the `aimdb.` namespace, which connectors must not use for their own options.

4. **Serializer setters clear the recorded wire format.** `with_serializer`,
   `with_serializer_into` and `with_deserializer` remove any `aimdb.wire_format`
   entry (§4.4). No signature changes, and links without the key behave as
   before.

`Ros2Connector` therefore does not use core's `pump_sink` / `pump_source`,
because those call the old accessors. It drives its own pumps over the `_with_meta`
accessors, which is also what lets it carry a route index instead of a topic
`String` (§7). `ZenohConnector` for plain `zenoh://` uses the new accessors too,
for the recorded wire format and for inbound de-duplication (§4.3).

**Side note, confirmed:** inbound `with_qos` is a blind spot on **both** MQTT
backends today. The native backend subscribes at a hard-coded `AtLeastOnce`
(`native.rs:188`). The embedded `warn_unsupported_qos` reads outbound routes
only (`embedded/mod.rs:572`). The inbound accessor makes the fix possible; the
fix itself is a separate MQTT change.

### 4.7 One session, two schemes

A connector registers exactly one scheme (`ConnectorBuilder::scheme`), and a device
must not open two router connections. So `ZenohConnector` and `Ros2Connector` are
two `ConnectorBuilder`s that can hold the same `Arc<Shared>`:
- `Ros2Connector::new(endpoint, node)` creates a fresh `Shared`, owning its
  session alone;
- `zenoh.ros2(node)` clones the handle of an existing `ZenohConnector`, so both
  schemes ride one session.

- **Whichever builds first drives the session.** `Shared` holds a one-shot
  (core's `session::OneShot`, design 052 §5.5) for the session task. The first
  `build()` to run takes it and returns the session future, along with its own
  pump futures. The second `build()` returns only its pumps. So either connector
  works alone, and registration order does not matter.
- **Declarations are complete before the first connect.** `AimDbBuilder` calls
  every `build()` before `AimDbRunner::run()` polls anything. Both connectors
  therefore record their subscribers, tokens and publishers in `Shared` before the
  session connects. After a reconnect the session re-declares everything, since
  the router withdraws tokens when a session drops.
- **Each connector owns its scheme** (`owns_scheme() = true`). Registering two
  `ZenohConnector`s, or two views of different sessions, under one scheme is
  refused at build, as it is for MQTT.
- **One channel pair per session.** Both connectors' pumps feed the same action
  channel and read from the same event channel. These are `embassy_sync` channels
  with `CriticalSectionRawMutex` on both backends, so `Shared` is `Send + Sync`
  without `unsafe`, and the same code carries over to the MCU in v2.
- **Refused:** `.ros2(..)` twice on one session. There is one ROS node per session
  in v1. Several nodes on one session is a later extension if a use case needs it.

### 4.8 MCU data into ROS: the gateway pattern

In v1 an MCU never speaks ROS itself. It publishes plain Zenoh to the robot's
router, and a std gateway on the same router re-publishes the same contract as a
ROS topic:

```text
MCU ──zenoh://cell4/probe/temperature──► router ──► gateway ──ros2://cell4/temperature──► router ──► ROS
```

On the MCU (Embassy), `zenoh://` only:

```rust
let zenoh = ZenohConnector::new("tcp/192.168.10.5:7447")
    .transport(EmbassyNet::tcp(stack, rx, tx));

builder.configure::<Temperature>("cell4.temperature", |reg| {
    reg.buffer_sized::<8, 2>(EmbassyBufferType::SpmcRing)
        .source(read_probe)
        .linked_to("zenoh://cell4/probe/temperature");
});
```

On the gateway (Tokio), both schemes on one session (§4.7):

```rust
let zenoh = ZenohConnector::new("tcp/192.168.10.5:7447");
let ros2  = zenoh.ros2(Ros2Node::new("cell4_gateway")).register::<Temperature>();

builder.configure::<Temperature>("cell4.temperature", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
        .linked_from("zenoh://cell4/probe/temperature")
        .linked_to("ros2://cell4/temperature");
});
```

What this gives and costs:

- **ROS sees** `/cell4/temperature`, published by `/cell4_gateway`. The MCU itself
  is not a node in the graph, which rarely matters in practice.
- **The codec matches automatically.** `Temperature` is a `RosMessage`, so its
  default `Linkable` format is CDR on both ends and no codec is named. A deployment
  can move both links to Postcard to save bytes on the MCU; the two ends then change
  together.
- **Commands run in reverse** on the same pattern. The gateway has
  `linked_from("ros2://…")` feeding a record that is `linked_to("zenoh://…")`, and the
  MCU subscribes to it.
- **Timestamps.** The ROS attachment is stamped by the gateway, which always has a
  wall clock. The message's own `header.stamp` is whatever the MCU wrote. An MCU
  without time sync writes zero, so tools that use stamps (tf, message filters)
  need the gateway to fill it in. A transform on the gateway record does that
  (design 020). The docs show it.
- **Latency.** Two passes through the router add one LAN round trip, negligible for
  plant telemetry.
- **The MCU needs nothing ROS-specific:** no liveliness tokens, no attachment, no
  wall clock, and CDR only because it is the type's default. Every open upstream
  gap that matters only to ROS (§5.2) stays off the MCU.

### 4.9 v2: ROS directly from the MCU

`ros2://` on the embedded backend makes the MCU a ROS node of its own. It is the PX4
topology: a Zenoh client on the flight controller and a router on the companion
computer. It is deferred until these three are true:

1. zenoh-nostd has a liveliness-token API (§5.2), upstream or in our published fork.
2. The session's own ZID is reachable, since the token needs it. S5 (§10) found
   it is not, and names the one-line upstream fix.
3. The MCU has a wall-clock source for the attachment timestamp, or `0` is accepted
   as documented degradation (§3).

Nothing in v1 has to be redone for v2:
- the profile module is already `no_std + alloc` (§4.1);
- `Shared` already uses MCU-safe channels (§4.7);
- `Ros2Connector` gains a `.transport(dialer)` path, the way `ZenohConnector`
  already has one.

### 4.10 Worked example (Tokio)

This is a cell gateway that:
- publishes a probe as a ROS topic;
- mirrors the same reading to the MES over MQTT as JSON;
- takes spindle commands from ROS.

It is target API; the import paths of the registrar extension traits are confirmed
at implementation time.

```rust
use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_data_contracts::{link_codecs::Json, LinkCodecRegistrarExt, LinkableRegistrarExt, RosMessage};
use aimdb_mqtt_connector::MqttConnector;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_zenoh_connector::{Ros2Connector, Ros2Node};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// Standard interfaces, mirrored by hand like any other (§4.5).
#[derive(Clone, Debug, Serialize, Deserialize, RosMessage)]
#[ros(type = "builtin_interfaces/msg/Time", hash = "RIHS01_<…>")]
pub struct Time { pub sec: i32, pub nanosec: u32 }

#[derive(Clone, Debug, Serialize, Deserialize, RosMessage)]
#[ros(type = "std_msgs/msg/Header", hash = "RIHS01_<…>")]
pub struct Header { pub stamp: Time, pub frame_id: String }

#[derive(Clone, Debug, Serialize, Deserialize, RosMessage)]
#[ros(type = "sensor_msgs/msg/Temperature", hash = "RIHS01_<…>")]
pub struct Temperature { pub header: Header, pub temperature: f64, pub variance: f64 }

/// Mirrored by hand from cell_msgs/msg/SpindleCommand.msg (§4.5).
#[derive(Clone, Debug, Serialize, Deserialize, RosMessage)]
#[ros(type = "cell_msgs/msg/SpindleCommand", hash = "RIHS01_<from `ros2 topic info -v`>")]
pub struct SpindleCommand {
    pub rpm: f64,
    pub enabled: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(TokioAdapter::new()?);

    let ros2 = Ros2Connector::new("tcp/192.168.10.5:7447", Ros2Node::new("cell4_gateway"))
        .register::<Temperature>()
        .register::<SpindleCommand>();

    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(ros2)
        .with_connector(MqttConnector::new("mqtt://mes.plant.local:1883"));

    // Probe → ROS topic /cell4/temperature (CDR) and → MES over MQTT (JSON).
    builder.configure::<Temperature>("cell4.temperature", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .source(|ctx, producer| async move {
                loop {
                    let (sec, nanosec) = ctx.unix_time().unwrap_or((0, 0));
                    producer.produce(Temperature {
                        header: Header {
                            stamp: Time { sec: sec as i32, nanosec },
                            frame_id: "cell4_probe".into(),
                        },
                        temperature: read_probe(),
                        variance: 0.0,
                    });
                    ctx.time().sleep_secs(1).await;
                }
            })
            .linked_to("ros2://cell4/temperature")
            .linked_to_with("mqtt://plant/cell4/temperature", Json);
    });

    // ROS topic /cell4/spindle_cmd → latest command wins.
    builder.configure::<SpindleCommand>("cell4.spindle.cmd", |reg| {
        reg.buffer(BufferCfg::Mailbox)
            .linked_from("ros2://cell4/spindle_cmd")
            .tap(|ctx, consumer| async move {
                let mut reader = consumer.subscribe();
                while let Ok(cmd) = reader.recv().await {
                    ctx.log().info(&format!("spindle: {} rpm, enabled={}", cmd.rpm, cmd.enabled));
                }
            });
    });

    builder.run().await?;
    Ok(())
}

fn read_probe() -> f64 { 23.4 }
```

On the robot side, with rmw_zenoh and its router running:

```text
$ ros2 node list
/cell4_gateway

$ ros2 topic echo /cell4/temperature
header:
  stamp: {sec: 1790193600, nanosec: 120000000}
  frame_id: cell4_probe
temperature: 23.4
variance: 0.0

$ ros2 topic pub --once /cell4/spindle_cmd cell_msgs/msg/SpindleCommand "{rpm: 1200.0, enabled: true}"
```

The last command logs `spindle: 1200 rpm, enabled=true` in the gateway.

## 5. Backends

### 5.1 Native (`std`): the `zenoh` crate

- Client mode against the router by default. Peer mode and the rest come through
  `with_zenoh_config`.
- Uses `session.liveliness().declare_token(..)` for tokens and `put(..).attachment(..)`
  for data. Tokens are declared before the first publish. On session loss the
  router withdraws them, so the node leaves `ros2 node list` by itself.
- **[verified]** `session.liveliness().declare_token(..)` and the put
  builder's `.attachment(..)` exist in `zenoh` 1.8.0 (`api/liveliness.rs:114`,
  `api/builders/sample.rs:56`).
- Publisher options mirror rmw_zenoh's (`rmw_publisher_data.cpp:942`):
  - Zenoh `reliability` follows the QoS reliability.
  - Congestion control is `DROP`, or `BLOCK` for `RELIABLE` with `KEEP_ALL`.
- Pin a zenoh release that is wire-compatible with the router rmw_zenoh vendors:
  zenoh-c 1.8.0 in Jazzy, Kilted and Lyrical, and 1.10.1 in Rolling (§3). The
  latest `zenoh` on crates.io is 1.10.1. Zenoh promises 1.x wire
  compatibility, and the interop test must prove it for both routers
  (**[spike S2]**).
- Cost: a large dependency tree, kept behind the `std` feature and never reachable
  from `embedded`. The same CI guard pattern as design 041's `rand` tracer applies.

### 5.2 Embedded (`no_std + alloc`): `zenoh-nostd`

`zenoh-nostd` itself is `no_std` and allocation-free. The connector needs `alloc`
for route tables and payload handoff, like every AimDB connector. In v1 this
backend serves `zenoh://` only (§4.8, §4.9).

**Transport.** zenoh-nostd abstracts I/O behind `ZLinkManager::connect(Endpoint)`
and `ZLink::split(&mut self) -> (Tx<'_>, Rx<'_>)` **[verified]** (`io/link.rs`).
Core's `ByteStream::split(&mut self)` also yields borrowed, concurrently
pollable halves (design 052). The shapes line up, so the shim is thin:
- `connect` reads `Endpoint::protocol()` / `address()` for `tcp/host:port` and
  calls `StreamDialer::connect(host, port)`;
- `read_exact` loops over `ByteRead::read`;
- `is_streamed = true`;
- `mtu()`, which `ZLinkInfo` also requires, returns the configured buffer size;
- `listen` is refused (client mode only).

**The shim's link must own its socket.** `ZLinkManager::Link<'a>` may borrow the
manager. If it does, and the borrowed type has a `Drop` impl, a per-connection
`Resources` fails the drop check. **[verified]** by two mock links compiled at
`e88f73a`. A link that borrows the manager and implements `Drop` reproduces
zenoh-nostd **issue #11** exactly: `E0597: resources does not live long
enough`. Upstream's own Embassy link does that by wrapping `TcpSocket<'a>`, so
its examples fall back to `StaticCell` macros that connect only once. A link
that *owns* its socket compiles. AimDB's `StreamDialer::Stream` is owned, with
no lifetime (`session/io.rs:103`), so the shim wraps it by value and never
borrows the dialer.

The backend is therefore generic over any 052 dialer: `EmbassyNet::tcp` today,
`TokioNet::tcp()` for host tests, and a FreeRTOS `LwipNet` later.

**Task shape.** There is one session future:
1. Connect.
2. Declare the subscribers recorded in `Shared` (§4.7).
3. `select(session.run(), drain_actions)`, where `drain_actions` turns queued
   publishes into `put`.
4. On error, back off through `Delay`, reconnect and re-declare.

**`Resources` is per connection, not leaked once.** `session_connect` takes
`&'res mut Resources<'res, Config>` (`api/session.rs:84`). That borrows the value
for its whole lifetime, so a leaked `&'static mut Resources` can back exactly one
session. **[verified]**: reusing it in a reconnect loop fails with `E0499`.
Instead, `Resources` is a local of the loop body. Each iteration creates it,
connects, runs, and drops it. This compiles with an owned link, with and without
`alloc`, **[verified]**. It stays inside the one session future, which is boxed
once at build, so nothing is leaked. With an array `Buff`, a reconnect costs no
heap allocation. With a `Vec` `Buff`, it costs one per connect.

**Subscriber keys must be `'static`.** `declare_subscriber` takes
`&'static keyexpr` (`api/session/sub.rs:226`). The connector leaks each distinct
inbound key once at build with `Box::leak`, as the MQTT embedded backend already
leaks its build-time strings (`embedded/mod.rs:373`), and re-declares the same
keys after every reconnect. `put` takes a borrowed key, so outbound needs no leak.

**What upstream constrains** (**[checked]** at `main` e88f73a, tag 0.2.0, and
branch `dev/0.3.0`):

| Finding | Consequence | Plan |
|---|---|---|
| Not on crates.io (`zenoh-nostd`, `zenoh-proto` and `zenoh-sansio` all 404); git tag `0.2.0` = `main` @ `e88f73a`, whose workspace `Cargo.toml` still says `0.1.0`; no commits on `main` since 2026-06-26; `dev/0.3.0` is 31 commits and roughly +9.5k lines ahead | A published AimDB crate cannot take a git dependency. **Blocks v1** | Ask upstream to publish. Meanwhile, do what `aimdb-mountain-mqtt` did: publish a zero-delta fork `aimdb-zenoh-nostd` pinned to a tag, and retire it when upstream releases |
| No liveliness-token API on `main` or `dev/0.3.0`. `DeclareToken`/`UndeclareToken` exist in `zenoh-proto` only (`msgs/declare.rs:27`) | An MCU cannot announce itself to the ROS graph. **Blocks v2 only** (§4.9) | A small upstream PR: `session.liveliness().declare_token(ke)`, modelled on `put`. Off the v1 critical path |
| The session future is `!Send` for three separate reasons, **[verified]** with an `assert_send` probe: <ul><li>session and driver state sit behind `embassy_sync` `NoopRawMutex` (`api/session.rs:59`, `io/driver.rs:22`);</li><li>the link and transport traits return `impl Future` / `impl Iterator` with no `+ Send`, for example `ZTransportLinkRx::recv` (`io/transport/traits.rs:63`);</li><li>stored callbacks are `dyn ZDynCallback` without `+ Send`.</li></ul> | The session future is `!Send`, so the runner cannot box it without force-`Send` | Short term, use `aimdb-embassy-adapter::connectors::into_box_future`. That keeps the connector crate free of `unsafe`, but it makes `embedded` Embassy-bound for now: force-`Send` is sound only on a single-core cooperative executor. **[verified]** MQTT is not quite the precedent. It uses `into_box_future` only for its SNTP task (`embedded/mod.rs:533`). Its session tasks go through the connector's own `unsafe { SendSession::new(..) }` (`embedded/session.rs:21`, plus `AssertSend` in `tls.rs:189`). That is sound for MQTT, whose streams really are `Send`, and it keeps MQTT's backend runtime-neutral. It would not be sound over zenoh-nostd's `NoopRawMutex` state, and it breaks criterion 7. Upstream, all three have to change before the backend becomes runtime-neutral: `CriticalSectionRawMutex`, `+ Send` on every return-position `impl Trait` in the link and transport traits (052 §5.1's rule), and `+ Send` on the callback objects. A mutex swap alone is not enough |
| Uses `embassy-time` directly (`Timer`, `Instant`) | Every target needs an `embassy-time` driver. Host tests need one too, which the MQTT tests already supply | Accept. A FreeRTOS adapter must ship a driver |
| `embassy-sync` 0.7.2, while the workspace is patched to 0.8.0 | Two copies in the firmware image | Measure flash; offer upstream a version bump |
| `Interest` not implemented on `main`: the publisher's interest is a `TODO` (`api/session/pub.rs:98`). Publisher interest is declared on `dev/0.3.0` (`pub.rs:125`). Tracked upstream as issue #10 (open) | The client sends every put to the router whether or not anyone subscribes. That costs bandwidth, not correctness | Whether the router accepts token declarations without it matters for v2 only: **[spike S1]** |
| No serial link. Upstream tracks it as **issue #9, still open**. No branch implements it: the `Endpoint` parser knows `serial`, but no platform crate has a serial link. rev 4's "issue #11, fixed on a branch" was wrong. #11 is the `Resources` lifetime bug above, and the `issue/11` branch merged as PR #13 with example workarounds only | TCP only, so no UART to `rmw_zenohd` yet, even though that router vendors the serial transport | Follow #9. A serial link is a `ZLinkManager` impl, which AimDB's serial connector could supply over its own `ByteStream` without waiting for upstream |
| The inbound `Sample` drops the attachment: it holds only key and payload (`api/sample.rs:6`) | An MCU could not read sequence number, timestamp or GID from ROS publishers | Irrelevant in v1, where the MCU never subscribes to ROS directly |
| MSRV 1.91, edition 2024; license EPL-2.0 OR Apache-2.0 | Compatible with our pinned 1.98 and Apache-2.0 | — |

**Fallback.** If zenoh-nostd stalls, zenoh-pico over FFI is the known-good path.
It is C, it is mature, and `rmw_zenoh_pico` reportedly runs micro-ROS on it (not
checked for rev 6: the repository was not found under the names tried). It costs a
C toolchain in the firmware build and `unsafe` FFI in a connector, and it
contradicts the pure-Rust story. Keep it as the plan B, not the plan.

### 5.3 Topology

```text
 STM32 (Embassy)                Linux edge (Tokio)                 Robot / ROS 2
┌─────────────────┐  tcp/7447  ┌───────────────────────┐          ┌──────────────┐
│ AimDB           │───────────►│ rmw_zenohd (router)   │◄────────►│ rmw_zenoh_cpp│
│ zenoh:// only   │            │ AimDB gateway         │          │ nodes        │
│ (Embedded)      │            │ zenoh:// + ros2://    │          │              │
└─────────────────┘            │ (Native, one session) │          └──────────────┘
                               └───────────────────────┘
```

The MCU is a Zenoh client of the same router the robot already needs, and the
gateway is the only ROS participant on the AimDB side (§4.8). The router and the
gateway can share one Linux box. In v2 the MCU can also join the ROS graph
directly, which is the PX4 topology with AimDB in place of uORB (§4.9).

## 6. Mapping AimDB semantics to ROS QoS

In Zenoh, QoS settings are never incompatible: any publisher matches any
subscriber **[checked]**. The advertised QoS in a token is therefore informational.
`ros2 topic info -v` shows it, and nothing negotiates on it.

**Default: rmw's own default profile.** Every `ros2://` link advertises
`KEEP_LAST`, depth 10, `RELIABLE`, `VOLATILE`. That is the same as a
default-constructed `rclcpp` publisher. rmw_zenoh's own default depth is 42, so
the token's QoS field is `::,10:,:,:,,` (§3), not all-empty. rev 4 derived
the default from the record's buffer instead. That was dropped because
**[verified]** routes carry no buffer config: `OutboundRoute`, `RouteMeta`
(§4.6) and the router know nothing about the record behind them. Adding the
buffer config would mean a third core fact whose only use is an informational
string.

**Recommended overrides**, which the connector docs print. Each is set per link
with `Ros2LinkExt` (§4.3):

| AimDB buffer | Recommended ROS QoS | Note |
|---|---|---|
| `SpmcRing { capacity }` | `KEEP_LAST`, depth = capacity, `VOLATILE` | Telemetry |
| `SingleLatest` | `KEEP_LAST`, depth 1, `VOLATILE` in v1 | The natural home for latched topics (`/robot_description`, `/map`). `TRANSIENT_LOCAL` needs zenoh-ext's `AdvancedPublisher` cache, so it comes in v1.1 |
| `Mailbox` (inbound) | `KEEP_LAST`, depth 1 | Command topics: the latest instruction wins |
| Reliability | `RELIABLE` by default; `BEST_EFFORT` as an override | rmw_zenoh maps it to the Zenoh publisher's `reliability` (§5.1). A best-effort publication only travels over a best-effort link, such as UDP, if the session has one configured |

If the buffer-derived default is still wanted later, it is one more field
on the `#[non_exhaustive]` `RouteMeta`, with no new accessor.

## 7. Cost on the MCU (to measure, not assumed)

- **Outbound, per message:** a codec with `ENCODE_BUFFER_CAPACITY` set (Postcard,
  or CDR for `RosMessage` types) encodes into 045's reusable scratch, with no
  allocation. The handoff to the session task copies the payload into the action
  channel, which is one allocation, the same as the MQTT embedded backend today.
  Carrying a route index instead of a topic `String` removes a second allocation
  MQTT still pays. That is possible only because the connector drives its own
  pumps over the `_with_meta` accessors (§4.6). **[verified]** core's `pump_sink`
  hands a connector `destination: &str` (`session/pump.rs:40`), and MQTT's
  action carries it as an owned `String` (`embedded/mod.rs:82`).
- **Inbound, per message:** one `Payload` allocation, as in MQTT.
- **Shared state:** allocated once at build.
- **Nothing ROS-specific** in v1: no tokens, no attachment, no registry and no
  wall clock on the MCU (§4.8).
- **Flash and RAM:** unknown. Acceptance criterion 5 measures zenoh-nostd plus the
  connector on the STM32H5 against the MQTT embedded build. The duplicate
  `embassy-sync` (§5.2) shows up here.

## 8. Alternatives considered

- **A per-link method: `.ros2()`, `.as_ros_topic()`, `.as_ros_publisher()` /
  `.as_ros_subscriber()`** (rev 1). It works, but every name we tried read wrongly
  in one direction or hid what it did. It also needed a `RosMessage`-bounded
  extension on core's builders. The scheme names the protocol the way AimDB
  already does, and direction comes from `linked_to` / `linked_from`.
- **One connector claiming several schemes.** Cleaner in principle, but it
  changes `ConnectorBuilder` and connector registration in core. Two builders on a
  shared session (§4.7) need no such change.
- **Type-level auto-exposure (`expose_ros2::<T>()` publishes every record of `T`).**
  Rejected for v1. Topics belong to instances, not types: one `Temperature` backs
  `/cell4/temperature` and `/cell5/temperature`, often under names an existing
  robot dictates. Inbound auto-exposure would let any ROS node write AimDB records,
  including `Mailbox` actuation paths. Registration therefore opts a *type* in, and
  a link still opts each *record* in.
- **A `.msg` codegen importer** (rev 2). It gets field layout and hashes right
  mechanically, but it adds a tool, vendored inputs, provenance files and a
  drift-check CI job to every user's repo. That is a lot of ceremony for the
  handful of messages a cell usually has. The derive covers all of them in v1,
  and a prebuilt crate for the common ones can follow (§4.5). The derive's future hash check
  (§4.5) recovers most of codegen's safety without any of its workflow.
- **`ros2://` on the MCU in v1** (rev 1–3). It would make the MCU a ROS node
  directly, but it put three open upstream items on the critical path:
  zenoh-nostd's missing liveliness API, the session's own ZID, and a wall clock
  for the attachment. It also added the most MCU footprint. The gateway pattern
  (§4.8) gets the same data into ROS with none of that, using only AimDB's
  existing cross-tier model. It moves to v2 (§4.9) rather than being dropped.
- **An `rclrs`-based connector.** It gives full DDS reach, but it is std-only, needs
  a sourced ROS workspace and links ROS's C libraries. It is worth building later
  as a separate crate, for DDS robots. It is not the MCU story.
- **Plain Zenoh only, and let users run `zenoh-bridge-ros2dds`.** This works with
  DDS robots, but AimDB entities stay invisible to the ROS graph and every user maps
  types and keys by hand.
- **micro-ROS.** An XRCE agent and a C stack. Not our architecture.
- **An `rmw_aimdb` plugin.** A huge C surface that would make AimDB ROS middleware
  instead of the typed layer beside it.

## 9. Acceptance criteria

1. **Golden profile tests.** Data keys, node and entity tokens, QoS strings and
   GIDs are byte-equal to values captured from a real rmw_zenoh on Jazzy and Lyrical
   (`ros2 topic info -v`, and the router's admin space for tokens). The captures
   include a non-zero domain and a namespaced node.
2. **Interop CI (native).** In Docker (`ros:lyrical` plus rmw_zenoh), a `ros2://`
   outbound link arrives in `ros2 topic echo` with correct values.
   `ros2 topic info -v` lists the publisher with the right node, type, QoS and GID.
   In the reverse direction, `ros2 topic pub` lands in a `ros2://` inbound record.
3. **Gateway interop CI (embedded backend, on host).** The embedded backend over
   `TokioNet::tcp()` publishes on `zenoh://`. A native gateway in the same test
   re-publishes it on `ros2://` (§4.8), and it arrives in `ros2 topic echo`. The
   reverse path works for a command. This is 052 acceptance 6's pattern: host
   coverage before any hardware. The embedded backend runs on a
   **`current_thread`** runtime (or a `LocalSet`). MQTT's host tests use
   `multi_thread` (`tests/tokio_broker.rs:60`), which is sound for MQTT but not
   here: `into_box_future` force-`Send`s zenoh-nostd's `NoopRawMutex` state
   (§5.2), so a work-stealing runtime could move it across threads. The
   `embassy-time` driver comes from the test binary, as in the MQTT tests.
4. **Hardware.** An STM32H5 on Embassy publishes `sensor_msgs/Temperature` over
   `zenoh://`, and through the gateway it appears in `ros2 topic echo`. After a
   router restart, data flows again without a device reset.
5. **Builds and footprint.**
   `cargo check -p aimdb-zenoh-connector --no-default-features --features embedded --target thumbv7em-none-eabihf`
   passes. Flash and RAM are recorded against the MQTT embedded build.
6. **`aimdb-cdr`.** Round-trip and golden-byte tests pass for primitives, strings,
   sequences, fixed arrays and nested types, and big-endian golden bytes decode to
   the same values. A fuzzed decode never panics.
7. **No `unsafe` in the connector.** The connector crate has no `unsafe impl` (052
   criterion 2). Until zenoh-nostd is `Send`-clean, the only force-`Send` is the
   adapter's `into_box_future`. The MQTT connector does not meet that criterion
   yet (§5.2), so it is not the model to copy here.
8. **One session.** A process with `ZenohConnector` and a derived `Ros2Connector`
   registered opens exactly one TCP connection to the router, whichever order they
   were registered in. `Ros2Connector::new` alone and `ZenohConnector` alone also
   work. On an embedded build, a `ros2://` link fails `build()` with the
   unregistered-scheme error.
9. **Refusals.** Every row of §4.3's guarantee table has a test:
   - The two compile-time rows are `trybuild` UI tests.
   - The eight build-time rows assert the `build()` error.
   - The domain's precedence (explicit, then `ROS_DOMAIN_ID`, then `0`) has a test
     per step.
   - The custom-serializer gap has a test asserting the warning, both alone and
     with `with_serializer` placed after a codec verb.

   A *wrong* but well-formed hash is covered by a test that documents it as
   silence, so nobody mistakes that behaviour for a connector bug.
10. **The derive.** `trybuild` tests cover malformed `type` and `hash` attributes,
    and golden tests cover the DDS-name mangling and every row of §4.5's mapping
    table.

## 10. Open questions

rev 6 answered S3, S4 and S5 from source and offline runs. The two-day spike
(§11 step 1) now has to answer **S2 only** for v1. S1 matters only for v2.

- **S1 (v2).** Does `rmw_zenohd` accept `DeclareToken` from a zenoh-nostd client
  on `main`, without Interest? Does the node then appear in `ros2 node list`?
- **S2.** Is zenoh-nostd 0.2 wire-compatible with the zenoh-c 1.8 router, and is the
  pinned `zenoh` crate? It also has to be checked against Rolling's 1.10.1
  router (§3).
- **S3. Answered: yes.** rmw_zenoh's `simplified_xxhash3.cpp`, compiled
  unchanged, and `xxhash-rust` 0.8.18's `xxh3_128` give identical
  `(low64, high64)` for 603 inputs. Those cover every length from 0 to 600
  bytes, which spans all of XXH3's size classes, plus realistic node and
  publisher tokens. A GID read from a live `ros2 topic info -v` stays in
  criterion 1 as the end-to-end check.
- **S4. Answered: yes, on Jazzy, Kilted, Lyrical and Rolling.**
  `ros2 topic info -v` prints each endpoint through rclpy's endpoint-info
  `__str__`, which includes `Topic type hash: RIHS01_<hex>`. That is
  `topic_endpoint_info.py:197` on Kilted, and `endpoint_info.py:203` on
  Lyrical, where the file moved. Jazzy's `type_hash.py` formats the value as
  `RIHS01_`. rmw_zenoh's data key is the fallback, since it carries the hash on
  every distro (§3).
- **S5 (v2). Answered: not reachable today.** The session's own ZID is
  generated with `uhlc::ID::rand()` inside `TransportLinkManager`
  (`io/transport.rs:93`). The constructor that takes a ZID is `pub(crate)`, and
  no getter exists on `main` or `dev/0.3.0`. `Driver::zid()` returns the
  *peer's* (`other_zid`). The fix is a one-line upstream accessor, or a
  `with_zid` constructor that lets AimDB choose the ZID it puts in its tokens.
  It joins the liveliness PR in §11 step 6.
- **Q6. Answered (rev 7): fully qualified only.** The namespace sets the node's
  identity, not its topics (§4.2, §4.3). The link URL cannot tell an absolute
  name from a relative one.
- **Q7.** Publish the `aimdb-zenoh-nostd` fork now, or ship the embedded backend as
  unpublished (git-only) until upstream releases?
- **Q8.** Should plain `zenoh://` links carry a contract fingerprint in Zenoh's
  `encoding` field, to catch schema skew between AimDB peers? There is no
  precedent to copy. **[verified]** the WASM schema registry keys on
  `SchemaType::NAME` alone, with no version or fingerprint
  (`aimdb-wasm-adapter/src/schema_registry.rs:82`), so this would be new
  machinery.
- **Q9.** Is `WireFormat` worth exposing beyond this connector, for example so
  introspection and the MCP tools can report each link's wire format?

## 11. Sequencing

| # | Step | Depends on |
|---|---|---|
| 1 | **Spike (about 2 days).** Two parts: (a) with the `zenoh` crate, a hand-built token plus a `put` with an attachment, seen by `ros2 topic echo`, `ros2 node list` and `ros2 topic info -v`; (b) a zenoh-nostd `put` on the host reaching a zenoh 1.8 subscriber through `rmw_zenohd`, and the same against Rolling's 1.10.1 router. Answers S2. S3 and S4 were answered in rev 6, and part (a) now only confirms them live | — |
| 2 | `aimdb-cdr`; `Linkable::WIRE_FORMAT` and `LinkCodec::WIRE_FORMAT` with the recorded `aimdb.wire_format`; `RosMessage` and `#[derive(RosMessage)]`; `link_codecs::Cdr` (all additive, in data-contracts and aimdb-derive) | — |
| 3 | `profile/` module and its golden tests (criterion 1) | 1 |
| 4 | Core `RouteMeta`, the `_with_meta` accessors and `aimdb.topic_provider` (§4.6); `Shared`, `ZenohConnector`, `Ros2Connector` and the registry on the native backend; interop CI (criteria 2, 8, 9). **This ships v1's ROS feature on its own** | 2, 3 |
| 5 | Embedded backend (`zenoh://` only): gateway interop on host first (criterion 3), then STM32H5 (criteria 4, 5) | 1, 4 |
| 6 | Upstream. For v1, a crates.io publish or the fork. The rest is v2 prep and off the critical path: `Send` cleanliness (all three causes in §5.2), the liveliness API PR, and an own-ZID accessor (S5) | 1 |
| 7 | Docs: design 012 connector-guide section, a BYOC tutorial built on this connector, an "AimDB and ROS 2" page, and the manufacturing-cell demo | 4, 5 |

Steps 2 and 3 are pure and additive and can land early. `aimdb-ros2-msgs` is
not in the table: it is a later phase (§4.5). The effort estimate is unverified until the spike: roughly 4–6 weeks for
one engineer, and the rev 4 scope cut should put it toward the low end. Most of the
remaining variance is in steps 5 and 6, because both depend on zenoh-nostd being
publishable.

**Native first, embedded when unblocked.** Nothing in steps 1–4 depends on
zenoh-nostd beyond spike part (b). A std gateway speaking `ros2://`, plus
`zenoh://` between std AimDB peers, is a complete release. Steps 5 and 6 then
gate only the MCU half, and zenoh-nostd's publishing question (Q7) never holds
up the ROS feature. After rev 6, the only known blockers are two upstream
items: S2 (wire compatibility with both vendored routers) and the zenoh-nostd
publish. The rev 5 and rev 6 corrections each have an in-design fix.

On the roadmap, v1 lands after the conformance suite and doubles as BYOC tutorial
material. v2 (§4.9) is sequenced separately once its three preconditions hold.

## 12. What this does not change

- Existing connectors, the Tokio and Embassy adapters, and the 052 traits are
  untouched.
- `aimdb-core` gains one `#[non_exhaustive]` struct (`RouteMeta`), two accessors
  and one reserved config key. Its serializer setters also clear the reserved
  `aimdb.wire_format` key. No existing public struct gains a field.
- `aimdb-data-contracts` gains two defaulted associated consts (`Linkable` and
  `LinkCodec`) and two features, both off by default. The codec verbs also
  record `aimdb.wire_format` on each link, which connectors that do not read
  it ignore. `aimdb-derive` gains one derive, and `#[derive(Linkable)]` sets
  `WIRE_FORMAT = Json`.
- `aimdb-codegen` is untouched.
- Existing `Linkable` impls compile unchanged.

## 13. Sources read

- `ros2/rmw_zenoh`: `rolling` @ `1f7c62a` (2026-09-23), `jazzy` @ `224e568`,
  `kilted` @ `0ceb68a`, `lyrical` @ `4e17ccf`, and `humble`. Files: `docs/design.md`;
  `rmw_zenoh_cpp/src/detail/{liveliness_utils,attachment_helpers,rmw_publisher_data,rmw_subscription_data,cdr,type_support,type_support_common,qos,simplified_xxhash3}.cpp`;
  `zenoh_cpp_vendor/CMakeLists.txt`.
- `eclipse-zenoh/zenoh-cpp` @ `af381b4` (`1.8.0-4`, the Jazzy/Lyrical vendored
  commit): `include/zenoh/api/ext/serialization.hxx`.
- `eclipse-zenoh/zenoh-nostd`: `main` @ `e88f73a` (tag `0.2.0`, 2026-06-26),
  branches `dev/0.3.0` @ `912882e` and `issue/11` (merged as PR #13). Files:
  `crates/zenoh-nostd/src/{io/link.rs,io/driver.rs,io/transport.rs,io/transport/traits.rs,resources.rs,api/session.rs,api/sample.rs}`,
  `src/api/session/{run,put,pub,sub}.rs`, `crates/zenoh-proto/src/{endpoint.rs,msgs/declare.rs,msgs/fields.rs}`,
  and the upstream issue list (#9 serial, #10 Interest, #11 lifetimes).
- `zenoh` 1.8.0, `cdr-encoding` 0.11.0 (the latest; its `std::io` /
  `std::marker` imports are unconditional) and `xxhash-rust` 0.8.18
  (`#![no_std]`), all crates.io sources.
- `ros2/ros2cli` (`ros2topic/verb/info.py`) and `ros2/rclpy`
  (`topic_endpoint_info.py`, `endpoint_info.py`, `type_hash.py`) on `jazzy`,
  `kilted`, `lyrical` and `rolling`.
- `ros2/rcl` `rolling` (`rcl/src/rcl/{init,domain_id}.c`) and rmw_zenoh's
  `rmw_init.cpp`, for domain resolution (rev 7, §4.2).
- `ros-infrastructure/rep` master @ `11ca24a` (`rep-2000.rst`) and
  `ros2/ros2_documentation` @ `617ded6` (`Release-Lyrical-Luth.rst` and the
  Lyrical changelog).
- rev 6 ran four offline checks:
  - rmw_zenoh's XXH3 compiled against `xxhash-rust` (S3);
  - a leaked-`Resources` reconnect loop, which fails with `E0499`;
  - owned and borrowing mock links, where the borrowing one reproduces issue
    #11;
  - `assert_send` on the session future.

  The last three were compiled inside zenoh-nostd @ `e88f73a` on rustc 1.98.0.
- AimDB @ `e759cbe`: `aimdb-core/src/{builder.rs,connector.rs,typed_api.rs,router.rs,session/io.rs,session/pump.rs,session/client.rs,executor.rs}`,
  `aimdb-data-contracts/src/{lib.rs,linkable.rs,link_codec.rs,streamable.rs}`,
  `aimdb-derive/src/lib.rs`, `aimdb-codegen/src/rust.rs`,
  `aimdb-websocket-connector/src/server/registry.rs`,
  `aimdb-wasm-adapter/src/schema_registry.rs`,
  `aimdb-embassy-adapter/src/{connectors.rs,net.rs}`, and
  `aimdb-mqtt-connector/src/{connector.rs,native.rs,link_ext.rs,embedded/{mod,session,tls}.rs}`
  and `tests/tokio_broker.rs`.
- rev 5 also ran two checks on the pinned rustc 1.98.0. One confirmed that a
  generic inline-const assert passes `cargo check` and fails `cargo build`. The
  other read trybuild 1.0.121's mode selection (`src/cargo.rs:97`).
