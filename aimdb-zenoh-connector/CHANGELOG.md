# Changelog - aimdb-zenoh-connector

All notable changes to the `aimdb-zenoh-connector` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **First release: Zenoh links, and AimDB records as ROS 2 topics** through
  rmw_zenoh, on the `zenoh` crate (`std` feature). The `zenoh-nostd` backend
  for microcontrollers follows once zenoh-nostd is on crates.io.
- **`ZenohConnector`** for `zenoh://` links.
  - Outbound: one publisher per fixed key; a topic writer's keys are checked
    per message. A key with a wildcard fails the build.
  - Inbound: `ZenohGrammar` (`*`, `**` anywhere, `$*` within a chunk,
    verbatim `@` chunks, `{name}`/`{name..}` captures and keys) and one
    subscriber per filter of the covering set. A sample that several
    subscribers receive is ingested once per route.
  - Remote-only: the connector never receives its own publications. Deletes
    and samples on wildcard keys are dropped.
  - `with_zenoh_config` for peer mode, TLS, QUIC and the rest
    (`transport-tls`, `transport-quic`, `transport-ws`). Transport
    compression stays off; see the crate docs.
- **`Ros2Connector`** for `ros2://` links, as a ROS 2 node (`Ros2Node`, with
  a namespace) in a domain (`domain_id`, else `ROS_DOMAIN_ID`, else 0; 0 to
  232). Types are registered with `.register::<T: RosMessage>()`.
  - Outbound links are publishers: rmw_zenoh data keys, node and publisher
    liveliness tokens, and the attachment (sequence, timestamp, GID).
  - Inbound links are subscriptions: one subscriber per topic, dispatched to
    every record linked to it.
  - `Ros2LinkExt::with_depth` and `with_reliability` set the advertised QoS
    (default `KEEP_LAST` 10, `RELIABLE`, `VOLATILE`); reliability is not yet
    applied on the wire.
  - `build()` refuses an unregistered type, a non-CDR codec, a topic writer,
    a pattern, two types on one topic, invalid ROS names and a bad domain; a
    custom (de)serializer warns.
- **`zenoh.ros2(node)`**: a `Ros2Connector` on the `ZenohConnector`'s session,
  so a gateway (`zenoh://` from devices, `ros2://` to robots) keeps one
  router connection.
- Tested against an in-process router, and against Jazzy's and Lyrical's
  `rmw_zenohd` with the ROS 2 CLI in CI (`make ros2-interop`).
