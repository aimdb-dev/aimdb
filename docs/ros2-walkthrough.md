# Walkthrough: observe and command a ROS 2 robot from AimDB

Two examples in `aimdb-zenoh-connector/examples/` run against a real ROS 2
system:

- **`ros2_gateway`** observes. It subscribes to `geometry_msgs/msg/Twist` on
  `/cmd_vel`, keeps each message as a typed record, and republishes it as JSON
  on a plain Zenoh key for consumers that don't speak ROS.
- **`turtlesim_mcp`** commands. A caller, for example Claude through
  `aimdb-mcp`, writes a *goal*. A controller in AimDB turns goal and pose into
  velocity commands for turtlesim. The caller decides where to go, AimDB
  decides how to move, and ROS 2 moves the turtle.

Neither example needs ROS installed on the AimDB side, only a Zenoh router
both sides reach. `make ros2-interop` runs every command on this page against
Jazzy and Lyrical in CI, so the outputs below are the ones the test checks.

## Prerequisites

- ROS 2 **Jazzy** or **Lyrical** with `rmw_zenoh_cpp`, `geometry_msgs` and
  `turtlesim`:

  ```text
  sudo apt install ros-jazzy-rmw-zenoh-cpp ros-jazzy-turtlesim
  ```

  Humble has no type hashes and is not supported.
- A Rust toolchain and this repository. Build the examples and install the
  CLI and the MCP server:

  ```text
  cargo build -p aimdb-zenoh-connector --features std --examples
  cargo install --path tools/aimdb-cli     # the `aimdb` command
  cargo install --path tools/aimdb-mcp     # the `aimdb-mcp` command
  ```

The commands below run the examples from `target/debug/examples/`;
`cargo run -p aimdb-zenoh-connector --features std --example <name>` works too.

## 1. Start the router

Every ROS node and every AimDB process connects to one Zenoh router. In each
ROS terminal:

```text
source /opt/ros/jazzy/setup.bash
export RMW_IMPLEMENTATION=rmw_zenoh_cpp
```

Then start the router and leave it running:

```text
ros2 run rmw_zenoh_cpp rmw_zenohd
```

It listens on `tcp/[::]:7447`. On a host without IPv6, point
`ZENOH_ROUTER_CONFIG_URI` at a copy of
`$(ros2 pkg prefix rmw_zenoh_cpp)/share/rmw_zenoh_cpp/config/DEFAULT_RMW_ZENOH_ROUTER_CONFIG.json5`
that listens on `tcp/0.0.0.0:7447`.

## 2. Configuration

Both examples read two variables:

| Variable | Default | Meaning |
|---|---|---|
| `ZENOH_ENDPOINT` | `tcp/127.0.0.1:7447` | The router. Set it when AimDB runs on another machine, e.g. `tcp/192.168.1.20:7447` |
| `ROS_DOMAIN_ID` | `0` | The ROS domain, read the way `rcl` reads it. It must match the ROS side |

**A wrong domain or type hash fails silently**: the two sides never meet, and
nothing reports an error. If nothing arrives, check `ROS_DOMAIN_ID` on both
sides first, then compare the hash in the example with
`ros2 topic info -v <topic>`.

`AIMX_SOCKET` sets the Unix socket the `aimdb` CLI and `aimdb-mcp` connect
to. Its default is `/tmp/aimdb-ros2-gateway.sock` for the gateway and
`/tmp/aimdb-turtlesim.sock` for the turtle. The CLI finds sockets in `/tmp`
on its own; pass `--connect unix://<path>` when more than one is running.

## 3. Observe: the gateway

```text
/cmd_vel ──ros2://──▶ robot.cmd_vel ──zenoh:// (JSON)──▶ telemetry/cmd_vel
                           │
                           └──AimX──▶ aimdb watch
```

The whole configuration is one record with two links. Both links share one
Zenoh session, because the ROS view is derived from the plain connector:

```rust
let zenoh = ZenohConnector::new(endpoint.as_str());
let ros2 = zenoh
    .ros2(Ros2Node::new("telemetry_gateway").namespace("/aimdb"))
    .register::<Twist>();

builder.configure::<Twist>("robot.cmd_vel", |reg| {
    reg.buffer(BufferCfg::SingleLatest)
        .with_remote_access()
        .linked_from("ros2://cmd_vel") // CDR, as a ROS subscription
        .linked_to_with("zenoh://telemetry/cmd_vel", Json); // JSON, plain Zenoh
});
```

`Twist` is a plain Rust struct with `#[derive(RosMessage)]` and the type hash
ROS publishes. Its nested `Vector3` is another derived struct. The
`SingleLatest` buffer makes the record the robot's *current* command: the
newest value wins, `aimdb record get` can read it at any time, and a late
subscriber receives it first. An `SpmcRing` would keep a backlog instead, and
has no current value to serve.

Start the gateway, a plain Zenoh subscriber and a watch, each in its own
terminal:

```text
$ target/debug/examples/ros2_gateway
ros2_gateway: /cmd_vel -> robot.cmd_vel -> telemetry/cmd_vel
ros2_gateway: router tcp/127.0.0.1:7447, AimX unix:///tmp/aimdb-ros2-gateway.sock

$ target/debug/examples/zenoh_sub 'telemetry/**'
zenoh_sub: telemetry/** on tcp/127.0.0.1:7447

$ aimdb watch robot.cmd_vel
📡 Watching record: robot.cmd_vel
```

`zenoh_sub` is a stand-in for Zenoh's `z_sub`; `z_sub -k 'telemetry/**'`
shows the same payloads. Now publish two Twists from ROS:

```text
ros2 topic pub --times 2 -r 1 /cmd_vel geometry_msgs/msg/Twist \
    "{linear: {x: 0.5}, angular: {z: 0.3}}"
```

`aimdb watch` prints each update as the record holds it:

```text
2026-11-07 21:40:32.223 | seq:1 | ts:8.379694 | {"angular":{"x":0.0,"y":0.0,"z":0.3},"linear":{"x":0.5,"y":0.0,"z":0.0}}
2026-11-07 21:40:33.223 | seq:2 | ts:9.380052 | {"angular":{"x":0.0,"y":0.0,"z":0.3},"linear":{"x":0.5,"y":0.0,"z":0.0}}
```

The Zenoh subscriber receives the JSON payload, with the fields in `.msg`
order:

```text
telemetry/cmd_vel: {"linear":{"x":0.5,"y":0.0,"z":0.0},"angular":{"x":0.0,"y":0.0,"z":0.3}}
telemetry/cmd_vel: {"linear":{"x":0.5,"y":0.0,"z":0.0},"angular":{"x":0.0,"y":0.0,"z":0.3}}
```

A watch started after the last message still sees it, because AimX sends the
record's current value as a snapshot when a subscriber joins:

```text
$ aimdb watch robot.cmd_vel --count 1
📡 Watching record: robot.cmd_vel
2026-11-07 21:47:28.626 | seq:1 | ts:5.334474 | {"angular":{"x":0.0,"y":0.0,"z":0.3},"linear":{"x":0.5,"y":0.0,"z":0.0}}
📸 snapshot complete (1 record)
```

## 4. Command: turtlesim through goals

```text
caller ──AimX──▶ turtle.goal   "Stop" | {"GoTo": {"x": 8.0, "y": 8.0}}
/turtle1/pose ──▶ turtle.pose
turtle.goal + turtle.pose ──controller──▶ turtle.cmd_vel ──▶ /turtle1/cmd_vel
```

| Record | Type | Written by | Over AimX |
|---|---|---|---|
| `turtle.goal` | `Goal`: `Stop` or `GoTo { x, y }` | the caller | read and write |
| `turtle.pose` | turtlesim's `Pose` | turtlesim | read only |
| `turtle.cmd_vel` | `geometry_msgs/msg/Twist` | the controller | read only |

The controller is a `transform_join` over the goal and the pose. It runs on
every pose turtlesim publishes, about 60 times a second. It turns toward the
target, drives once it roughly faces it, and sends one zero command when it
arrives within 0.05 or is told to `Stop`. Speeds are clamped to 2 units/s
and 2 rad/s, and targets to the arena. None of that is up to the caller.

There are two write barriers between a caller and the motors:

- **AimX policy.** Only `turtle.goal` is in the writable set.
- **AimDB's single-writer rule.** `turtle.cmd_vel` is a transform's output,
  so AimDB refuses remote writes to it whatever the policy says.

turtlesim's messages moved packages after Jazzy: Jazzy publishes
`turtlesim/msg/Pose`, Lyrical `turtlesim_msgs/msg/Pose`. The fields are the
same, but the type name, and with it the hash, are not, and a mismatch fails
silently. The example carries both types, chooses by `ROS_DISTRO` (Jazzy when
it is unset), and prints its choice.

Start turtlesim and the pilot:

```text
$ ros2 run turtlesim turtlesim_node
$ ROS_DISTRO=jazzy target/debug/examples/turtlesim_mcp
turtlesim_mcp: /turtle1/pose (turtlesim::msg::dds_::Pose_) -> controller -> /turtle1/cmd_vel
turtlesim_mcp: router tcp/127.0.0.1:7447, AimX unix:///tmp/aimdb-turtlesim.sock (turtle.goal writable)
```

### With the CLI

```text
$ aimdb record set turtle.goal '{"GoTo": {"x": 8.0, "y": 8.0}}'
✅ Successfully set record: turtle.goal
```

The turtle turns, drives and stops. The pilot logs:

```text
goal: GoTo { x: 8.0, y: 8.0 }
stopped at (7.97, 7.96)
```

`aimdb watch turtle.cmd_vel` shows the controller's commands as it works,
and `aimdb record get turtle.pose` shows where the turtle ended up:

```text
$ aimdb record get turtle.pose
{
  "ts": 14011178691,
  "value": {
    "angular_velocity": 0.0,
    "linear_velocity": 0.0,
    "theta": 0.9307765,
    "x": 7.9706445,
    "y": 7.960569
  }
}
```

`aimdb record set turtle.goal '"Stop"'` stops it mid-route. A direct velocity
command is refused:

```text
$ aimdb record set turtle.cmd_vel '{"linear":{"x":9.0,"y":0.0,"z":0.0},"angular":{"x":0.0,"y":0.0,"z":0.0}}'
Error: Server error: denied
  Message: permission denied
```

### With Claude

`aimdb-mcp` offers `set_record` only when started with `--allow-writes`. In
Claude Code:

```text
claude mcp add aimdb -- aimdb-mcp --connect unix:///tmp/aimdb-turtlesim.sock --allow-writes
```

In Claude Desktop's `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "aimdb": {
      "command": "aimdb-mcp",
      "args": ["--connect", "unix:///tmp/aimdb-turtlesim.sock", "--allow-writes"]
    }
  }
}
```

Then ask, for example, *"Where is the turtle? Drive it to the top-left
corner, then trace a square with side 3."* Claude reads `turtle.pose` with
`get_record`, writes one `GoTo` per corner with `set_record`, and checks the
pose between corners. If it tries `turtle.cmd_vel`, the write comes back as
`permission denied`.

## How it is tested

`make ros2-interop DISTRO=jazzy` (and `lyrical`) builds an image with
`rmw_zenoh_cpp`, `geometry_msgs` and `turtlesim`, starts `rmw_zenohd`, and
checks:

- the gateway republishes 3 of 3 `ros2 topic pub` messages as the exact JSON
  above, in domain 42, `aimdb watch` sees all three, and a later watch gets
  the last one as a snapshot;
- `turtlesim_mcp` drives the turtle from its spawn point to within 0.1 of
  `(8, 8)` after one `GoTo`, and refuses a write to `turtle.cmd_vel`.

See [AimDB and ROS 2](aimdb-and-ros2.md) for the type mapping and the
connector's options.
