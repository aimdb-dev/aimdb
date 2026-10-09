# AimDB and ROS 2

AimDB records can be ROS 2 topics. `aimdb-zenoh-connector` speaks what
`rmw_zenoh` puts on the wire, so a ROS 2 system that uses rmw_zenoh sees an
AimDB process as an ordinary node: `ros2 node list` lists it, `ros2 topic echo`
reads its records, and `ros2 topic pub` writes into them. AimDB needs no ROS
installation, only a Zenoh router both sides reach.

Tested with Jazzy and Lyrical. A robot on a DDS middleware needs
`zenoh-bridge-ros2dds`; Humble has no type hashes and is not supported.

## The ROS side

Every ROS node and AimDB connect to one Zenoh router:

```text
export RMW_IMPLEMENTATION=rmw_zenoh_cpp
ros2 run rmw_zenoh_cpp rmw_zenohd          # the router, on tcp/…:7447
```

## Message types

A ROS message is a Rust struct with the `.msg`'s fields **in order**, since
CDR is positional, and one derive:

```rust,ignore
use aimdb_data_contracts::RosMessage;

/// cell_msgs/msg/SpindleCommand.msg: float64 rpm, bool enabled, string<=32 tool
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, RosMessage)]
#[ros(type = "cell_msgs/msg/SpindleCommand", hash = "RIHS01_<64 hex>")]
pub struct SpindleCommand {
    pub rpm: f64,
    pub enabled: bool,
    #[ros(max_len = 32)]
    pub tool: String,
}
```

**The hash** identifies the type on the wire. A wrong one fails silently, as
the two sides never meet, so copy it from the robot:

```text
$ ros2 topic info -v /cell4/spindle_cmd | grep hash
Topic type hash: RIHS01_…
```

| `.msg` | Rust |
|---|---|
| `bool` | `bool` |
| `byte`, `char`, `uint8`, `int8` | `u8`, `i8` |
| `uint16` … `int64` | `u16` … `i64` |
| `float32`, `float64` | `f32`, `f64` |
| `string`; `string<=N` | `String`; add `#[ros(max_len = N)]` |
| `T[N]` | `[T; N]` |
| `T[]`; `T[<=N]` | `Vec<T>`; add `#[ros(max_len = N)]` |
| `pkg/Type` | another `RosMessage` struct |
| `wstring` | not supported |

Constants in a `.msg` are not on the wire and are left out. An empty message
carries `structure_needs_at_least_one_member: u8`, as ROS does. Standard types
such as `std_msgs/Header` and `builtin_interfaces/Time` are written the same
way, once per project.

## The connector

```rust,ignore
use aimdb_zenoh_connector::{Ros2Connector, Ros2Node};

let ros2 = Ros2Connector::new("tcp/192.168.10.5:7447",
        Ros2Node::new("cell4_gateway").namespace("/cell4"))
    .domain_id(7)
    .register::<SpindleCommand>();
```

- **Domain:** `domain_id(n)`; without it `ROS_DOMAIN_ID`, as `rcl` reads it;
  without that 0. It must be 0 to 232. Like a wrong hash, a wrong domain fails
  silently.
- **Namespace:** the node's identity in `ros2 node list`. It does not prefix
  topics: `ros2://cell4/spindle_cmd` is always `/cell4/spindle_cmd`.
- **Links:** `linked_to("ros2://…")` makes a record a ROS publisher,
  `linked_from("ros2://…")` a subscription. Each advertises `KEEP_LAST` 10,
  `RELIABLE`, `VOLATILE`; `Ros2LinkExt::with_depth` and `with_reliability`
  override it. Reliability is advertised only: the data travels on Zenoh's
  reliable default.
- **Mistakes fail `build()`:** an unregistered type, a codec other than CDR, a
  topic writer, a `{…}` pattern, two types on one topic, an invalid ROS name or
  domain. A custom serializer is allowed and logs a warning, since nothing can
  check that it writes CDR.

## Microcontrollers: the gateway

A microcontroller publishes plain `zenoh://` to the same router, and a gateway
re-publishes the same contract as a ROS topic. The gateway derives its ROS
connector from its Zenoh connector, so it keeps one router connection:

```rust,ignore
let zenoh = ZenohConnector::new("tcp/192.168.10.5:7447");
let ros2 = zenoh.ros2(Ros2Node::new("cell4_gateway")).register::<Temperature>();
builder.with_connector(zenoh).with_connector(ros2);

builder.configure::<Temperature>("cell4.temperature", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
        .linked_from("zenoh://cell4/probe/temperature")
        .linked_to("ros2://cell4/temperature");
});
```

ROS sees `/cell4/temperature` published by `/cell4_gateway`. A type that is a
`RosMessage` encodes as CDR by default, so both links need no codec. The
attachment's timestamp comes from the gateway; a microcontroller without a
clock leaves `header.stamp` to a transform on the gateway record. The
microcontroller side, on `zenoh-nostd`, follows once zenoh-nostd is published
on crates.io.

## Checking it from ROS

```text
$ ros2 node list
/cell4/cell4_gateway
$ ros2 topic info -v /cell4/temperature     # type, hash, QoS, GID
$ ros2 topic echo /cell4/temperature
$ ros2 topic pub --once /cell4/spindle_cmd cell_msgs/msg/SpindleCommand "{rpm: 1200.0, enabled: true, tool: t1}"
```

`make ros2-interop DISTRO=lyrical` runs these checks against a ROS 2
distribution in Docker, with the connector's `ros2_talker` and `ros2_listener`
examples.

## Not yet

Services, actions, `TRANSIENT_LOCAL` (latched topics), Humble, and a
microcontroller that joins the ROS graph itself rather than through a gateway.

Design: [053 — Zenoh connector](design/053-zenoh-connector.md).
