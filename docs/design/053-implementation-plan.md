# 053 — Implementation plan

**Design:** [053 — Zenoh connector, with an rmw_zenoh-compatible ROS 2 profile](053-zenoh-connector.md), rev 9.
**Feature branch:** `feat/aimdb-zenoh-connector`.

The design's sequencing (053 §11) is split into 16 stages. Each stage gets
its own branch, `feat/053-sNN-<slug>`, and ends with a PR into the feature
branch. The feature branch merges into `main` once, after s16.

CI does not run on PRs into the feature branch. Each PR states the checks
run locally: the Makefile test legs of the crates it touches and
`make clippy`. The s12 checkpoint enables CI on the feature branch for one
run and reverts the trigger in the same PR.

Already on the feature branch: `aimdb-cdr`, `WireFormat` on `Linkable` and
`LinkCodec`, and the recorded `aimdb.wire_format` key.

## Stages

### Data contracts

| # | Branch | Scope | Done when |
|---|---|---|---|
| s01 | `ros-message` | `RosMessage` trait behind a new `ros2` feature in `aimdb-data-contracts`; the DDS type-name helper; type-name and hash validators that the registry reuses | Unit tests for name building and validation |
| s02 | `cdr-codec` | `link_codecs::Cdr<N>` behind `linkable-cdr`, over `aimdb-cdr::to_slice`; records `cdr` as the wire format | Takes the bounded encode path; wire-format test |
| s03 | `derive` | `#[derive(RosMessage)]`: `SchemaType`, CDR `Linkable` and `RosMessage`; validates `type` and `hash`; `encode_capacity`; `max_len` for bounded strings and sequences | Criterion 10; criterion 9's malformed-attribute row (trybuild) |

### Core and grammar

| # | Branch | Scope | Done when |
|---|---|---|---|
| s04 | `core-route-facts` | `RouteInfo::type_id`; `InboundDispatch::routes()` and `InboundRouteInfo`; remove `TOPIC_WRITER_KEY`; update MQTT's `PublishOpts` tests (053 §4.6) | Core tests pass; the inbound rows of the data-contracts wire-format test are restored |
| s05 | `crate-grammar` | `aimdb-zenoh-connector` crate with `std` and `embedded` features, Makefile legs, and a guard that `embedded` never pulls in `zenoh`; `ZenohGrammar` (053 §4.3) | Unit half of criterion 11, against `zenoh-keyexpr` |

### Spike and ROS profile

| # | Branch | Scope | Done when |
|---|---|---|---|
| s06 | `spike` | Answer S2 (wire compatibility with the 1.8 and 1.10.1 routers) and S6 (`put` while `session.run()` is pending). Results go into 053 as rev 10; spike code stays out of the tree | 053 §10 answers S2 and S6. Needs Docker with ROS and rmw_zenoh |
| s07 | `profile` | `profile/`: data keys, liveliness tokens, name mangling, QoS strings, GID, attachment, name validator (053 §3) | Criterion 1, against values captured from rmw_zenoh on Jazzy and Lyrical |

### Native backend

These stages ship v1's ROS feature.

| # | Branch | Scope | Done when |
|---|---|---|---|
| s08 | `native-zenoh` | `Shared` and `ZenohConnector` on the `zenoh` crate, `zenoh://` only: build-time slots, session task, `put`, subscriptions, `dispatch` (053 §4.7, §5.1) | End-to-end half of criterion 11 against an in-process Zenoh peer; `ZenohConnector` works alone |
| s09 | `ros2-outbound` | `Ros2Connector`, registry, `Ros2Node`, domain resolution, `Ros2LinkExt`; outbound `ros2://` with node and publisher tokens, attachment, GID and sequence number | Criterion 9's `register` bound row (trybuild) and outbound build-time rows; domain-precedence tests |
| s10 | `ros2-inbound` | Inbound `ros2://`: one subscriber per topic, subscriber tokens, dispatch under the link topic; refusals for patterns and for two types on one topic | Criterion 9's remaining rows; custom-serializer warning tests |
| s11 | `shared-session` | `zenoh.ros2(..)` on one session: one-shot session task; a view never registered leaves an empty slot | Criterion 8 |
| s12 | `interop-ci` | Docker interop job (`ros:lyrical` + rmw_zenoh); golden values captured from a real rmw_zenoh. Checkpoint with one CI run | Criteria 1 and 2 |

### Embedded backend

| # | Branch | Scope | Done when |
|---|---|---|---|
| s13 | `embedded-shim` | zenoh-nostd dependency; `ZLink` shim over `StreamDialer` that owns its socket; `Resources` per connection (053 §5.2) | Builds for `thumbv7em-none-eabihf`; loopback tests |
| s14 | `embedded-session` | Embedded session task; gateway interop on the host over `TokioNet` on a `current_thread` runtime; allocation bench rows | Criteria 3, 7 and 12 |
| s15 | `hardware` | STM32H5 example; flash and RAM against the MQTT embedded build | Criteria 4 and 5 |
| s16 | `docs` | Connector guide section, "AimDB and ROS 2" page, CHANGELOGs | The feature branch is ready to merge into `main` |

## Notes for s08

From the review of `ZenohGrammar` (PR #307), checked against a live
zenoh 1.10.1 session:

- **Wildcard-keyed samples.** A `put` on `a/*` reaches every intersecting
  subscriber with `a/*` as its key. The grammar already makes such keys match
  no route; 053 §4.3 also promises a counter. Core has no inbound route
  statistics (`RouteStats` is outbound only), so s08 decides where it lives.
- **Declare subscribers with `filter()`,** which is already canonical, through
  `keyexpr::new` / `KeyExpr::try_from`. Never run a raw pattern through
  `autocanonize`: in zenoh-keyexpr 1.10.1 it panics on `a$*$*` and turns
  `a$*$*$*` into an invalid `a$*$*`.
- **Partly overlapping filters** (`a/*/c` with `a/b/*`) still deliver a sample
  once per subscriber, 055 §5.7's known difference. A possible fix: subscriber
  `i`'s callback dispatches only if no earlier subscription `j < i` also
  matches the key, using `ZenohGrammar` on the subscription strings. This is
  the same idea as MQTT 5's subscription identifiers.
- **Criterion 11 needs a real session.** `tests/inbound_routes.rs` emulates
  Zenoh's delivery; s08 adds the same cases over a live peer.

## Dependencies

| Stage | Needs |
|---|---|
| s02 | s01 |
| s03 | s01, s02 |
| s05 | s04 |
| s07 | s01 |
| s08 | s04, s05 |
| s09 | s03, s07, s08 |
| s10 | s09 |
| s11 | s10 |
| s12 | s06 (S2), s11 |
| s13 | s06 (S6), a zenoh-nostd release, s08 |
| s14 | s13 |
| s15 | s14 |
| s16 | s12; s15 once the embedded backend ships |

- s01–s03 and s04–s05 do not touch each other and can run side by side.
- s07 is checked against captures from live rmw_zenoh nodes, not `hiroz-protocol`.
- Q7 (053 §10) is answered: no fork. s13–s15 wait for a zenoh-nostd
  release on crates.io; until then the feature branch can merge into `main`
  with the native backend only.
- Upstream work (a zenoh-nostd release, `Send` cleanliness, the liveliness
  API, an own-ZID accessor) happens outside this repository. Only the
  release blocks v1's embedded half, from s13.
