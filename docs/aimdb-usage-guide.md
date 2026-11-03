# AimDB External Usage Guide

This guide shows how to use AimDB in your own projects, whether you're building for standard platforms (Linux, macOS) or embedded systems.

## Runtime Selection

AimDB supports two async runtime environments:

### **Tokio** (Standard Platforms)
For desktops, servers, and cloud deployments. Uses the standard library and provides the full Rust ecosystem.

**Use when:**
- Building for Linux, macOS, or Windows
- Need access to std library features
- Working with existing Tokio-based codebases
- Want maximum performance on platforms with OS support

### **Embassy** (Embedded Systems)
For microcontrollers and no_std environments. Designed for resource-constrained devices.

**Use when:**
- Targeting MCUs (ARM Cortex-M, RISC-V, etc.)
- Working in no_std environments
- Building IoT edge devices
- Need minimal memory footprint and deterministic behavior

**Creating Embassy projects:** See the [Embassy Project Documentation](https://embassy.dev/book/#_starting_a_new_project) for detailed setup instructions.

---

## Quick Start: Tokio (Standard Platforms)

### 1. Create a New Project

```bash
cargo new my-aimdb-project
cd my-aimdb-project
```

### 2. Configure Dependencies (Cargo.toml)

```toml
[package]
name = "my-aimdb-project"
version = "0.1.0"
edition = "2021"

[dependencies]
# AimDB core and Tokio runtime adapter
aimdb-core = "3.0"
aimdb-tokio-adapter = { version = "0.8", features = ["tokio-runtime"] }

# Optional: KNX connector
# aimdb-knx-connector = { version = "0.6", features = ["std", "critical-section-std-impl"] }
# aimdb-tokio-adapter's "net" feature is also required — see the KNX section.

# Optional: MQTT connector
# aimdb-mqtt-connector = { version = "0.8", features = ["tokio-runtime"] }

# Optional: Sync API wrapper (for blocking code)
# aimdb-sync = "0.7"

# Tokio runtime
tokio = { version = "1.0", features = ["full"] }

# CRITICAL: Patch dependencies for compatibility (see notes below)
[patch.crates-io]
# Required if using KNX connector - bug fixes not yet on crates.io
# knx-pico = { git = "https://github.com/aimdb-dev/knx-pico.git", branch = "master" }

# Not needed for Tokio-only projects (Embassy patches only)
# mountain-mqtt = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
# mountain-mqtt-embassy = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
```

### 3. Basic Example (main.rs)

```rust
use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbResult};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;

#[derive(Debug, Clone)]
struct SensorData {
    temperature: f32,
}

#[tokio::main]
async fn main() -> DbResult<()> {
    // Create Tokio runtime adapter
    let runtime = Arc::new(TokioAdapter::new()?);

    let mut builder = AimDbBuilder::new().runtime(runtime);
    builder.configure::<SensorData>("sensor.data", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            // Simulate a temperature sensor reading every second
            .source(|ctx, producer| async move {
                for i in 0u32.. {
                    let temperature = 20.0 + (i % 50) as f32 * 0.1; // 20-25°C
                    producer.produce(SensorData { temperature });
                    ctx.time().sleep_secs(1).await;
                }
            })
            // React to sensor data changes
            .tap(|_, consumer| async move {
                let mut reader = consumer.subscribe();
                while let Ok(data) = reader.recv().await {
                    // Every value carries the time it was written.
                    println!(
                        "🌡️  Temperature: {:.1}°C at {:?}",
                        data.value.temperature,
                        data.stamp()
                    );
                }
            });
    });

    println!("🚀 AimDB running with Tokio...");
    builder.run().await
}
```

### 4. Run Your Project

```bash
cargo run
```

---

## Quick Start: Embassy (Embedded Systems)

For creating new Embassy projects, follow the official **[Embassy Getting Started Guide](https://embassy.dev/book/#_starting_a_new_project)**.

Once you have an Embassy project set up, add AimDB:

### Configure Dependencies (Cargo.toml)

```toml
[dependencies]
# AimDB core and Embassy runtime adapter
aimdb-core = { version = "3.0", default-features = false }
aimdb-embassy-adapter = { version = "0.8", features = ["embassy-runtime"] }

# Optional: KNX connector
# aimdb-knx-connector = { version = "0.6", features = ["connector"], default-features = false }

# Optional: MQTT connector
# aimdb-mqtt-connector = { version = "0.8", features = ["embassy-runtime"], default-features = false }

# Embassy runtime (example for RP2040)
embassy-executor = { version = "0.10", features = ["platform-cortex-m", "executor-thread"] }
embassy-time = { version = "0.5", features = ["generic-queue-16"] } # REQUIRED — see note below
embassy-rp = { version = "0.2", features = ["time-driver"] }

# CRITICAL: Patch dependencies for compatibility
[patch.crates-io]
# Required: Embassy fork with latest upstream main
embassy-executor = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-time = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-sync = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-futures = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
# Add other embassy-* crates as needed (embassy-rp, embassy-stm32, etc.)

# Required: KNX connector dependencies
knx-pico = { git = "https://github.com/aimdb-dev/knx-pico.git", branch = "master" }

# Required: MQTT connector dependencies
mountain-mqtt = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
mountain-mqtt-embassy = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
```

> **Required: `embassy-time` generic timer queue.** AimDB drives every
> registered future from a single Embassy task via `FuturesUnordered`, so
> `embassy_time::Timer` and `Ticker` are polled with `FuturesUnordered`'s
> waker rather than an Embassy-executor waker. The default
> executor-integrated timer queue rejects that waker and panics with
> *"Found waker not created by the Embassy executor…"* at the first timer
> await. Enable one of the `generic-queue-N` features on `embassy-time` in
> your binary — `generic-queue-16` is a reasonable starting point; raise it
> if you have many concurrent timers in flight. This is a binary-level
> choice: AimDB's own crates deliberately do **not** enable a queue size,
> following [embassy-time's guidance for libraries](https://docs.embassy.dev/embassy-time/git/default/index.html#generic-queue).

### Basic Example (main.rs)

```rust
#![no_std]
#![no_main]

use aimdb_core::{AimDbBuilder, Input, RecordRegistrar};
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyBufferType, EmbassyRecordRegistrarExtCustom};
use core::fmt::Debug;
use embassy_executor::Spawner;
use embassy_rp as hal;
use embassy_rp::gpio::{Input as Pin, Pull};

/// What the button yields: one press.
#[derive(Debug, Clone, Copy)]
struct ButtonEvent {
    press: u32,
}

/// The record, built from the event.
#[derive(Debug, Clone)]
struct ButtonPress {
    count: u32,
}

impl From<ButtonEvent> for ButtonPress {
    fn from(event: ButtonEvent) -> Self {
        ButtonPress { count: event.press }
    }
}

/// A push button input: one event per debounced press, for every record
/// type built `From<ButtonEvent>`. It owns its pin.
struct PushButton(Pin<'static>);

impl<T> Input<T> for PushButton
where
    T: From<ButtonEvent> + Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T>) {
        let PushButton(mut pin) = self;
        reg.source(move |ctx, producer| async move {
            let mut press = 0;
            loop {
                pin.wait_for_falling_edge().await; // active low
                ctx.time().sleep_millis(50).await; // debounce
                press += 1;
                producer.produce(T::from(ButtonEvent { press }));
                pin.wait_for_rising_edge().await;
                ctx.time().sleep_millis(50).await;
            }
        });
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = hal::init(Default::default());

    // The pin is moved into the input: it cannot feed a second record.
    let button = PushButton(Pin::new(p.PIN_15, Pull::Up));

    let runtime = alloc::sync::Arc::new(EmbassyAdapter::new());
    let mut builder = AimDbBuilder::new().runtime(runtime);
    builder.configure::<ButtonPress>("button.press", |reg| {
        reg.buffer_sized::<1, 1>(EmbassyBufferType::SingleLatest)
            .input(button)
            .tap(|_, consumer| async move {
                let mut reader = consumer.subscribe();
                while let Ok(press) = reader.recv().await {
                    defmt::info!("🔘 Button pressed! Count: {}", press.value.count);
                }
            });
    });

    builder.run().await.unwrap()
}
```

**Note:** Embassy examples require target-specific configuration. See [embassy-knx-connector-demo](../examples/embassy-knx-connector-demo) or [embassy-mqtt-connector-demo](../examples/embassy-mqtt-connector-demo) for complete working examples.

---

---

## Using Connectors

AimDB connectors enable integration with external protocols and systems. KNX and MQTT run on both Tokio and Embassy; the Zenoh connector is `std` only for now.

### KNX Connector

The KNX connector provides KNX/IP tunneling support for building automation systems.

**Add to Cargo.toml:**
```toml
# Host. `critical-section-std-impl` selects the impl the connector's channels
# need to link — only a final binary may pick one.
aimdb-knx-connector = { version = "0.6", features = ["std", "critical-section-std-impl"] }
aimdb-tokio-adapter = { version = "0.8", features = ["tokio-runtime", "net"] }

# Embedded. `connector` is the same code on `no_std + alloc`; the HAL already
# provides a `critical-section` impl.
aimdb-knx-connector = { version = "0.6", features = ["connector"], default-features = false }

# REQUIRED PATCH (bug fixes not yet on crates.io)
[patch.crates-io]
knx-pico = { git = "https://github.com/aimdb-dev/knx-pico.git", branch = "master" }
```

**Example (Tokio):**
```rust
use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbResult};
use aimdb_knx_connector::KnxConnector;
use aimdb_tokio_adapter::net::{TokioDelay, TokioNet};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::net::Ipv4Addr;
use std::sync::Arc;

#[derive(Debug, Clone)]
struct LightState {
    is_on: bool,
}

#[tokio::main]
async fn main() -> DbResult<()> {
    let runtime = Arc::new(TokioAdapter::new()?);

    // The adapter owns the UDP socket and the clock; the connector owns
    // the tunnelling protocol.
    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(KnxConnector::new(
        TokioNet::udp(Ipv4Addr::UNSPECIFIED),
        TokioDelay,
        "knx://192.168.1.19:3671",
    ));

    builder.configure::<LightState>("light.state", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_from("knx://1/0/7")
            .with_deserializer(|_ctx, data: &[u8]| {
                let is_on = data.first().is_some_and(|&b| b != 0);
                Ok(LightState { is_on })
            })
            .finish()
            .tap(|_, consumer| async move {
                let mut reader = consumer.subscribe();
                while let Ok(state) = reader.recv().await {
                    println!("💡 Light: {}", if state.value.is_on { "ON" } else { "OFF" });
                }
            });
    });

    builder.run().await
}
```

This example uses the raw `link_from(..).with_deserializer(..).finish()`
builder: the escape hatch for a payload that is not a contract's wire format
(here a single KNX byte). Records of contract types link with `subscribe` /
`publish`, as in the MQTT example below.

**See also:**
- [tokio-knx-connector-demo](../examples/tokio-knx-connector-demo) - Full Tokio example
- [embassy-knx-connector-demo](../examples/embassy-knx-connector-demo) - Full Embassy example

### MQTT Connector

The MQTT connector enables pub/sub messaging with MQTT brokers.

**Add to Cargo.toml:**
```toml
# For Tokio
aimdb-mqtt-connector = { version = "0.8", features = ["tokio-runtime"] }

# For Embassy
aimdb-mqtt-connector = { version = "0.8", features = ["embassy-runtime"], default-features = false }

# REQUIRED PATCH for Embassy (version compatibility)
[patch.crates-io]
mountain-mqtt = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
mountain-mqtt-embassy = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
```

**Example (Tokio):** a record linked to MQTT through a connector handle. The
record type is a contract (`Linkable`), so `subscribe` / `publish` use its own
wire format and nothing restates it:

```toml
aimdb-data-contracts = { version = "0.3", features = ["linkable-json"] }
serde = { version = "1", features = ["derive"] }
```

```rust
use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbResult};
use aimdb_data_contracts::{publish, subscribe, Linkable, SchemaType};
use aimdb_mqtt_connector::{MqttConnector, MqttLinkExt};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The contract: JSON on the wire, from `#[derive(Linkable)]`.
#[derive(Debug, Clone, Serialize, Deserialize, Linkable)]
struct SensorReading {
    temperature: f32,
    humidity: f32,
}

impl SchemaType for SensorReading {
    const NAME: &'static str = "sensor_reading";
}

#[tokio::main]
async fn main() -> DbResult<()> {
    let runtime = Arc::new(TokioAdapter::new()?);
    let mut builder = AimDbBuilder::new().runtime(runtime);
    // `connect` registers the connector and returns a handle to link through.
    let mqtt = builder.connect(MqttConnector::new("mqtt://broker.example.com:1883"));

    builder.configure::<SensorReading>("sensor.reading", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .input(subscribe(mqtt, "sensor/room1").with_qos(1))
            .output(publish(mqtt, "sensor/room1/latest").with_qos(1))
            .tap(|_, consumer| async move {
                let mut reader = consumer.subscribe();
                while let Ok(reading) = reader.recv().await {
                    println!("📊 Temp: {:.1}°C, Humidity: {:.1}%",
                             reading.value.temperature, reading.value.humidity);
                }
            });
    });

    builder.run().await
}
```

The address needs no scheme; a full URL from a `#[link_address]` key works
too if its scheme is the connector's. For a type that is not a contract, or
a per-link serializer, use the raw builders (see the KNX example above and
[Links as values](#links-as-values)).

**See also:**
- [tokio-mqtt-connector-demo](../examples/tokio-mqtt-connector-demo) - Full Tokio example
- [embassy-mqtt-connector-demo](../examples/embassy-mqtt-connector-demo) - Full Embassy example

### Zenoh Connector and ROS 2

The Zenoh connector serves `zenoh://` links, and `ros2://` links that make
records ROS 2 topics through rmw_zenoh, with no ROS installation on the AimDB
side.

**Add to Cargo.toml:**
```toml
aimdb-zenoh-connector = { version = "0.1", features = ["std"] }
aimdb-data-contracts = { version = "0.3", features = ["ros2"] }   # #[derive(RosMessage)]
```

```rust
use aimdb_zenoh_connector::{Ros2Connector, Ros2Node, ZenohConnector};

// Plain Zenoh, and ROS 2 on the same session: register both
let zenoh = ZenohConnector::new("tcp/192.168.10.5:7447");
let ros2 = zenoh.ros2(Ros2Node::new("cell4_gateway")).register::<Temperature>();
let mut builder = AimDbBuilder::new()
    .runtime(runtime)
    .with_connector(zenoh)
    .with_connector(ros2);

builder.configure::<MachineState>("cell4.state", |reg| {
    reg.buffer(BufferCfg::SingleLatest)
        .linked_to_with("zenoh://aimdb/cell4/state", Postcard::<128>);
});
builder.configure::<Temperature>("cell4.temperature", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
        .linked_to("ros2://cell4/temperature");
});
```

See [AimDB and ROS 2](aimdb-and-ros2.md) for message types, domains, QoS and
the gateway pattern.

### Links as values

`link_to` / `link_from` build a link on the record being configured.
`OutboundLink` / `InboundLink` build the same link as a value, before any
record exists; `reg.output(..)` / `reg.input(..)` register it.

Register the connector with `connect` to get a handle, and start links from
it with `on`: the address needs no scheme, and a link cannot name a connector
that was never registered.

```rust
use aimdb_core::{InboundLink, OutboundLink};
use aimdb_data_contracts::{link_codecs::Json, LinkCodecBuilderExt};
use aimdb_mqtt_connector::{MqttLinkExt, MqttOutboundLinkExt};

let mut builder = AimDbBuilder::new().runtime(runtime);
let mqtt = builder.connect(MqttConnector::new("mqtt://broker.local:1883"));

let command = InboundLink::on(mqtt, "cell4/setpoint/set").with_link_codec(Json).with_qos(1);
let report = OutboundLink::on(mqtt, "cell4/setpoint")
    .with_link_codec(Json)
    .with_qos(1)
    .with_retain(true);

builder.configure::<Setpoint>("cell4.setpoint", |reg| {
    reg.buffer(BufferCfg::SingleLatest).input(command).output(report);
});
```

`on` also accepts a full URL whose scheme is the connector's, such as a
`#[link_address]` key's address. Without a handle, `OutboundLink::new(url)` /
`InboundLink::new(url)` take a full URL, as `link_to` / `link_from` do.

Every option and connector extension works on both forms, and both are
checked the same way when registered: mistakes (bad URL, missing codec,
unregistered scheme, a scheme that does not match the handle's connector)
are reported together by `build()`. Several inbound
links on one record are allowed; an inbound link together with a `.source()`
or `.transform()` is a `build()` error. A link that is neither registered nor
`.finish()`ed is a `must_use` warning.

`reg.input(..)` / `reg.output(..)` take anything implementing
`Input<T>` / `Output<T>`, so a device driver can provide its own input; a link
for the wrong record type or direction does not compile.

### Inputs and contracts: conventions

Wiring is meant to fail at compile time where it can. Four conventions get
there; the MQTT demos
([tokio](../examples/tokio-mqtt-connector-demo),
[embassy](../examples/embassy-mqtt-connector-demo), and their shared
[common crate](../examples/mqtt-connector-demo-common)) follow all of them.

**Inputs yield an event; records are built with `From<Event>`.** An input is
generic over its record and bounded by `T: From<Event>`:

```rust
impl<T> Input<T> for PushButton
where
    T: From<ButtonEvent> + Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T>) {
        reg.source(move |ctx, producer| async move {
            /* wait for an edge, debounce */
            producer.produce(T::from(ButtonEvent { /* … */ }));
        });
    }
}

impl From<ButtonEvent> for ButtonPress { /* … */ }
```

One input type then serves every contract that can be built from its event
(start trigger, door contact, counter pulse), with no producer function per
record. A device that sends its own payload over a connection is an input
too: `subscribe` for a contract's wire format, or `InboundLink` /
`link_from` with `with_deserializer`, mapping with `T::from`.

**Device channels are owned, non-`Clone` values.** A board hands out its
inputs once, as struct fields, and an input takes one by value:

```rust
let board = board::init(spawner).await;
let NucleoInputs { b1 } = board.inputs;

builder.configure::<ButtonPress>(ButtonKey::B1, |reg| {
    reg.buffer_sized::<8, 3>(EmbassyBufferType::SpmcRing)
        .input(PushButton::on(b1))
        .output(publish(mqtt, ButtonKey::B1.link_address().unwrap()));
});
```

Using `b1` for a second record is `use of moved value`. Pins never leave the
board module. This is a pattern, not a type in AimDB.

**Where the `From<Event>` impls live.** The orphan rule allows
`impl From<E> for C` only in the crate of `E` or of `C`. Convention: driver
crates own their event types and ship their inputs with them; contract
crates implement `From<Event>` for the events they accept. When neither
crate can (a contract and a driver from two other crates), use a newtype
around the contract, or a `.transform()` from a record of the driver's own
type.

**What is checked where.**

| Mistake | Caught |
|---|---|
| Record not buildable from the input's event (`From` missing) | compile time (E0277) |
| Contract without a wire format (not `Linkable`) at `publish` / `subscribe` | compile time (E0277) |
| Link or input for the wrong record type or direction | compile time (E0277) |
| One device channel wired into two records | compile time (E0382) |
| Link to a connector that was never registered | compile time with a handle; `build()` with a URL |
| Address scheme not the handle's connector | `build()` |
| Two writers of different kinds on one record (`.input(src)` + inbound link) | `build()` |

"One input per record" is not a compile error: a second source, or a source
next to an inbound link, fails `build()` as it always has, and several
inbound links on one record are allowed (fan-in).

---

## Write Timestamps

Every record write is stamped from the instance's monotonic clock
([design 058](design/058-write-timestamps.md)), and every read returns the
stamp with the value. Readers yield a `Stamped<T>`: the value is `.value`, the
write time `.stamp()`.

```rust
let mut reader = consumer.subscribe();
while let Ok(sample) = reader.recv().await {
    let age = db.now().since(sample.stamp()); // how long ago it was written
    println!("{:?} (written {:?} ago)", sample.value, age);
}
```

The same holds for `db.latest::<T>(key)`, `aimdb-sync`'s `get` family, and a
join's `JoinTrigger::stamp()`.

- **What it means:** when the value was written into *this* instance. A source
  value is stamped when produced, a transform or join output when computed, an
  inbound connector value when ingested. It is not wall-clock time, and not
  when a sensor measured; keep measurement time in the record's own data.
- **Scope:** nanoseconds since the instance started. Stamps compare within one
  instance lifetime only.
- **Remote access:** AimX 4.0 carries the stamp on every value as `ts`, and
  3.x peers are refused. See the
  [`aimdb-client` README](../aimdb-client/README.md#write-timestamps-aimx-40)
  for the frame shapes and the JavaScript precision note.

---

## Current Status & Limitations

### ✅ Ready for Use
- **aimdb-core**: Core database engine with all buffer types
- **aimdb-tokio-adapter**: Tokio runtime support (std)
- **aimdb-embassy-adapter**: Embassy runtime support (embedded)
- **aimdb-knx-connector**: KNX/IP tunneling (both runtimes)
- **aimdb-mqtt-connector**: MQTT (both runtimes)
- **aimdb-zenoh-connector**: Zenoh, and ROS 2 topics through rmw_zenoh (std)
- **aimdb-sync**: Synchronous API wrapper
- **aimdb-client**: Remote database access (AimX protocol)

### 🔧 Embassy Runtime Requires Patch
The Embassy adapter depends on a forked version of Embassy tracking the latest upstream main. You **must** include these patches for Embassy projects:

```toml
[patch.crates-io]
embassy-executor = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-time = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-sync = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
embassy-futures = { git = "https://github.com/aimdb-dev/embassy.git", branch = "main" }
# Add other embassy-* crates as needed (embassy-rp, embassy-stm32, etc.)
```

**Why this is needed:**
- Tracks latest Embassy upstream main branch
- Ensures compatibility with AimDB's Embassy adapter
- Includes latest bug fixes and improvements from Embassy project

**Note:** Tokio-only projects do not need Embassy patches.

### 🔧 KNX Connector Requires Patch
The KNX connector depends on a forked version of `knx-pico` with critical bug fixes. You **must** include this patch:

```toml
[patch.crates-io]
knx-pico = { git = "https://github.com/aimdb-dev/knx-pico.git", branch = "master" }
```

**Carried patches in the fork (on top of upstream 0.3.0):**
1. **NPDU length off-by-one** (not yet upstreamed, see [cc90202/knx-pico#4](https://github.com/cc90202/knx-pico/issues/4)):
   the length octet counts the APCI octet plus the data octets, so the data
   slice is `[9 .. 8 + npdu_length)`. Upstream uses `7 + npdu_length`, which
   panics on 6-bit telegrams (`npdu_length = 1`) and silently drops the value
   of single-octet ones (`npdu_length = 2`, e.g. DPT 5.001).
2. **`LDataFrame::six_bit_value`**: reads the 6-bit datapoint out of the APCI
   octet, so callers need no raw-byte indexing.
3. **Embassy compatibility**: Uses workspace-local embassy checkout

The DPT 9 (float) encoding/decoding fix landed upstream in knx-pico 0.3.0.

### 🔧 MQTT Connector Requires Patch (Embassy Only)
For Embassy runtime support, the MQTT connector requires patched `mountain-mqtt` dependencies:

```toml
[patch.crates-io]
mountain-mqtt = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
mountain-mqtt-embassy = { git = "https://github.com/aimdb-dev/mountain-mqtt.git", branch = "main" }
```

**Why?**
- Embassy dependency version compatibility with our workspace
- Optional for Tokio-only usage, but recommended for consistency

## Issue Reporting

We encourage you to test AimDB and report issues!

### Where to Report
- **AimDB issues**: https://github.com/aimdb-dev/aimdb/issues
- **KNX connector issues**: https://github.com/aimdb-dev/aimdb/issues (label: `knx-connector`)
- **knx-pico issues**: https://github.com/cc90202/knx-pico/issues (upstream)

### What to Include
1. **Minimal reproduction**: Small, self-contained example
2. **Environment**: OS, Rust version (`rustc --version`)
3. **Hardware**: If using embedded (board model, firmware version)
4. **KNX setup**: Gateway model, firmware version, network topology
5. **Logs**: Enable tracing with `RUST_LOG=debug`

### Common Issues

**"Failed to connect to KNX gateway"**
- Check gateway IP and port (default: 3671)
- Verify UDP port 3671 is not blocked by firewall
- Ensure gateway has available tunnel slots (typically 4-5 max)
- Try `ping <gateway-ip>` to verify network connectivity

**"ACK timeout" warnings**
- Network latency too high (>50ms)
- Gateway overloaded
- Network congestion
- Solution: Reduce telegram sending rate or upgrade network

**"Panic on malformed telegram"**
- Make sure you're using the patched knx-pico (see Cargo.toml above)
- Enable debug logs to see raw telegram bytes
- Report the specific telegram that causes panic

**Wrong temperature readings (DPT 9)**
- Make sure you're using knx-pico 0.3.0 or later (DPT 9 sign handling was fixed upstream)
- Verify you're using the correct DPT for your sensor
- Check telegram bytes with ETS Bus Monitor

## Feature Requests

We're actively developing AimDB. Current priorities:
1. ✅ KNX connector (done)
2. ✅ MQTT connector (done)
3. 🚧 Kafka connector (in progress)
4. 📋 DDS connector (planned)
5. 📋 Publish to crates.io (planned)

Want something else? Open an issue with the `enhancement` label!

## Version Pinning

For maximum stability, you can pin to specific versions on crates.io:

```toml
[dependencies]
aimdb-core = "=3.0.0"
aimdb-tokio-adapter = "=0.8.0"
aimdb-mqtt-connector = "=0.8.0"
aimdb-knx-connector = "=0.6.0"
```

Or use standard semver:
```toml
[dependencies]
aimdb-core = "3.0"  # Will use latest 3.0.x
aimdb-knx-connector = "0.6"  # Will use latest 0.6.x
```

## Migration Path

As bug fixes are upstreamed and published, the patches can be removed:

```toml
# Current (with patches)
[dependencies]
aimdb-knx-connector = "0.6"

[patch.crates-io]
knx-pico = { git = "https://github.com/aimdb-dev/knx-pico.git", branch = "master" }

# After upstream fixes are published
[dependencies]
aimdb-knx-connector = "0.6"  # or newer version

# [patch.crates-io]  <-- Just delete this section!
```

We'll announce when patches are no longer needed:
- GitHub releases
- README updates
- Discussions forum

Stay tuned! 🚀
