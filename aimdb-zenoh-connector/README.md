# aimdb-zenoh-connector

Zenoh connector for AimDB, with an rmw_zenoh-compatible ROS 2 profile:
`zenoh://` links for plain Zenoh, and `ros2://` links that make AimDB records
ROS 2 topics. No ROS installation is needed on the AimDB side, only a Zenoh
router such as `rmw_zenohd`.

## Installation

```toml
[dependencies]
aimdb-zenoh-connector = { version = "0.1", features = ["std"] }
# ROS 2 message types: #[derive(RosMessage)]
aimdb-data-contracts = { version = "0.2", features = ["ros2"] }
```

| Feature | What it adds |
|---|---|
| `std` | The `zenoh` crate backend: `zenoh://` and `ros2://`, over TCP and UDP |
| `transport-tls`, `transport-quic`, `transport-ws` | Further Zenoh transports for `with_zenoh_config` |
| `tracing`, `log` | Logging through aimdb-core's facade |
| `embedded` | Reserved for the `zenoh-nostd` backend, which follows once zenoh-nostd is on crates.io |

## ROS 2 topics

A ROS message is a struct with the `.msg`'s fields, in order, and one derive.
Copy the type hash from `ros2 topic info -v` on the robot.

```rust,ignore
use aimdb_data_contracts::{LinkableRegistrarExt, RosMessage};
use aimdb_zenoh_connector::{Ros2Connector, Ros2Node};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, RosMessage)]
#[ros(type = "cell_msgs/msg/SpindleCommand", hash = "RIHS01_<64 hex>")]
pub struct SpindleCommand {
    pub rpm: f64,
    pub enabled: bool,
}

let ros2 = Ros2Connector::new("tcp/192.168.10.5:7447",
        Ros2Node::new("cell4_gateway").namespace("/cell4"))
    .domain_id(7)                              // else ROS_DOMAIN_ID, else 0
    .register::<Temperature>()
    .register::<SpindleCommand>();

let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(ros2);
builder.configure::<Temperature>("cell4.temperature", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
        .linked_to("ros2://cell4/temperature");     // a ROS publisher
});
builder.configure::<SpindleCommand>("cell4.spindle.cmd", |reg| {
    reg.buffer(BufferCfg::Mailbox)
        .linked_from("ros2://cell4/spindle_cmd");   // a ROS subscription
});
```

- A `ros2://` URL names a fully qualified topic: `ros2://cell4/temperature` is
  `/cell4/temperature`. The node's namespace sets its identity, not its topics.
- Each link advertises `KEEP_LAST` 10, `RELIABLE`, `VOLATILE`. Override per link
  with `Ros2LinkExt`:

  ```rust,ignore
  reg.link_to("ros2://cell4/temperature")
      .with_link_codec(link_codecs::Default)
      .with_depth(1)
      .with_reliability(Reliability::BestEffort)
      .finish();
  ```

- `build()` fails for a type that is not registered, a codec other than CDR,
  a topic writer, a `{…}` pattern, two types on one topic, an invalid ROS name
  and a domain above 232.

See [AimDB and ROS 2](https://github.com/aimdb-dev/aimdb/blob/main/docs/aimdb-and-ros2.md) for the type mapping, the
gateway pattern and how to check the result with the ROS 2 CLI.

## Plain Zenoh

```rust,ignore
use aimdb_zenoh_connector::ZenohConnector;

let zenoh = ZenohConnector::new("tcp/192.168.10.5:7447");
builder.with_connector(zenoh);

builder.configure::<MachineState>("cell4.state", |reg| {
    reg.buffer(BufferCfg::SingleLatest)
        .linked_to_with("zenoh://aimdb/cell4/state", Postcard::<128>);
});
builder.configure::<MachineState>("cells.state", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 64 })
        .link_from("zenoh://aimdb/{cell}/state")      // every cell
        .key("cell", 64)                              // a KeyId per cell
        .with_match_deserializer(|_ctx, m, bytes| decode(m.get("cell"), bytes))
        .finish();
});
```

- Inbound keys follow Zenoh's rules: `*` one chunk, `**` any number, `$*` within
  a chunk; `{name}` and `{name..}` capture whole chunks. A sample is ingested
  once per route, even when several subscriptions receive it.
- Subscriptions and publications are remote-only: the connector never receives
  its own puts. Deletes and samples on wildcard keys are dropped.
- `with_zenoh_config(zenoh::Config)` starts from your own Zenoh configuration:
  peer mode, TLS, QUIC, access control.

## Both on one session

A gateway bridges devices on plain Zenoh to ROS 2. Derive the ROS view from
the Zenoh connector so both share one router connection:

```rust,ignore
let zenoh = ZenohConnector::new("tcp/192.168.10.5:7447");
let ros2 = zenoh.ros2(Ros2Node::new("cell4_gateway")).register::<Temperature>();
let mut builder = AimDbBuilder::new().runtime(runtime)
    .with_connector(zenoh)
    .with_connector(ros2);

builder.configure::<Temperature>("cell4.temperature", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
        .linked_from("zenoh://cell4/probe/temperature")
        .linked_to("ros2://cell4/temperature");
});
```

## Limits

- **Reliability is advertised, not applied.** A `BEST_EFFORT` link says so in
  its token but is sent on Zenoh's default reliable path, because zenoh 1.10's
  publisher reliability is still an unstable API.
- **No services, actions or `TRANSIENT_LOCAL`** yet.
- **Transport compression stays off.** zenoh 1.10.1 decompresses through
  `lz4_flex` 0.10.0, which has RUSTSEC-2026-0041; this crate never enables it.
  An application whose own dependencies enable `zenoh`'s compression, for
  example `zenoh` with default features, compiles that path in.

## Testing

```text
cargo test -p aimdb-zenoh-connector --features std   # includes an in-process router
make ros2-interop DISTRO=lyrical                      # against real ROS 2, in Docker
```

The examples `ros2_talker` and `ros2_listener` are a publisher and a
subscription for trying the connector against a ROS 2 host.

Design: [053 — Zenoh connector](https://github.com/aimdb-dev/aimdb/blob/main/docs/design/053-zenoh-connector.md).
