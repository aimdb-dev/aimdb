# AimDB External Usage Guide

This guide shows how to use AimDB in your own projects, whether you're building for standard platforms (Linux, macOS) or embedded systems.

## Declare records before configuring the builder

Enable `aimdb-data-contracts`' `aimdb-core` feature for `Key<T>`;
`linkable`, `migratable` and `observable` already enable it.

A record declaration binds its name to its payload type. Declare it once,
then reuse it for registration, reads, writes, transforms and links:

```rust,ignore
use aimdb_data_contracts::Key;
use aimdb_core::Name;

// Temperature implements SchemaType. The key supplies its schema metadata,
// even when no connector or observation is configured.
const INDOOR: Key<Temperature> = Key::new("temp.indoor");
const COUNT: Name<u64> = Name::new("internal.count");

builder.configure(INDOOR, |r| { r.buffer(BufferCfg::SingleLatest); });
builder.configure(COUNT, |r| { r.buffer(BufferCfg::SingleLatest); });
let producer = db.producer(INDOOR)?;
let sample = db.latest(INDOOR)?;
```

`RecordKey` enums require `#[record_key(contract = Temperature, prefix = "temp.")]`
or `#[record_key(payload = u64)]`; `key_prefix` is removed. Public record APIs
infer the payload from the declaration, so omit the payload turbofish.
`configure`, transforms and joins require a static typed key. For runtime
registration declare a `Key::<Temperature>::interned(&name)` or
`Name::<u64>::interned(&name)` before calling the builder. For repeated
lookups declare a borrowed `Name::<Temperature>::new(&name)`; lookups do not
intern names. A `Name<T>` declaration reports no contract schema, including
when its payload happens to implement `SchemaType`.

The key is the only source of a record's schema: link helpers and
`.observe()` no longer assign one. Instead, `configure` hands the closure a
registrar typed by whether the key declares a contract
(`RecordRegistrar<'_, T, Declared>` for `Key<T>` / `contract = T`,
`RecordRegistrar<'_, T, Undeclared>` for `Name<T>` / `payload = T`). The
contract verbs — `publish`, `subscribe`, `linked_to`, `linked_from`, their
`_with` forms and `.observe()` — exist only on `Declared` registrars, so using
one on a `Name<T>` record is a compile error ("this record's key declares no
data contract") rather than a record silently reported without a schema. The
raw `.link_to()` / `.link_from()` builders work in both states. Helper
functions that take a registrar name the state they need
(`&mut RecordRegistrar<'_, T, Declared>`) or are generic over
`S: SchemaState`; custom `Input` / `Output` implementations are generic over
`S` too. `contract_schema()` and public schema setters are removed. Existing envelope and version options
are unchanged: select inline explicitly before `accepts_from`, and use a
static `with_contract` binding for a bare legacy sender. A historical wire
version does not change the record's current schema.

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
    let record_1 = aimdb_core::Name::<SensorData>::new("sensor.data");
    builder.configure(record_1, |reg| {
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

impl<T, S: aimdb_core::SchemaState> Input<T, S> for PushButton
where
    T: From<ButtonEvent> + Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T, S>) {
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
    let record_2 = aimdb_core::Name::<ButtonPress>::new("button.press");
    builder.configure(record_2, |reg| {
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

    let record_3 = aimdb_core::Name::<LightState>::new("light.state");
    builder.configure(record_3, |reg| {
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

    let record_4 = aimdb_data_contracts::Key::<SensorReading>::new("sensor.reading");
    builder.configure(record_4, |reg| {
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

let record_5 = aimdb_data_contracts::Key::<MachineState>::new("cell4.state");
builder.configure(record_5, |reg| {
    reg.buffer(BufferCfg::SingleLatest)
        .linked_to_with("zenoh://aimdb/cell4/state", Postcard::<128>);
});
let record_6 = aimdb_data_contracts::Key::<Temperature>::new("cell4.temperature");
builder.configure(record_6, |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
        .linked_to("ros2://cell4/temperature");
});
```

See [AimDB and ROS 2](aimdb-and-ros2.md) for message types, domains, QoS and
the gateway pattern.

### Generic payload codecs

The built-in `Json`, `Postcard<N>`, and `Cdr<N>` markers also implement
`PayloadDecoder` and `PayloadEncoder`. These traits operate on any serde
type, so a migration chain can select a historical payload type before
calling the codec. `decode_payload::<T>(bytes)` returns `(value, consumed)`;
`encode_payload(&value, out)` writes into caller storage. The built-in implementations require the
corresponding `linkable-json`, `linkable-postcard`, or `linkable-cdr` feature.

The consumed count excludes trailing data and includes CDR encapsulation
and alignment bytes. JSON whitespace after the value counts as consumed.
These operations expose a remainder for admission to check; they do not
change `LinkCodec::decode` behavior. Postcard and CDR encoding into caller
storage remains allocation-free on success; JSON uses its owned encoding
path. A custom codec implements the generic traits explicitly. Implementing
`LinkCodec<T>` for one type, or using the `Default` marker that delegates to
that type's `Linkable`, does not provide generic historical decoding.

### Inbound errors and counters

`with_deserializer` and `with_match_deserializer` return `Result<T, IngestError>`.
Use `IngestError::new(Rejection::DecodeError, message)` for a decode failure,
or convert an existing string error with `.map_err(IngestError::from)`.
The reason identifies the counter; the diagnostic explains the failed value.
Each matching route counts its own result and continues after a rejection.

`InboundDispatch::stats(route.id)` returns a snapshot with `accepted`, the
rejection counters, and upgrades grouped by source version. Counters belong
to the registered link and survive dispatcher clones and reconnects. With
`observability` enabled they also appear in each record's `inbound_routes`
metrics in `record.list` and `record.get`.

Contract admission uses `with_admitting_deserializer(versions, callback)`:
the callback returns `IngestValue::current(value)` or
`IngestValue::upgraded(value, from)`. Core produces the value before
reporting the successful outcome. Source-version counters are preallocated
at registration; custom callbacks reporting undeclared versions increment
`untracked_upgrades`. Recording outcomes does not allocate; snapshotting
an upgrade list can allocate. Custom fused `IngestFn` callbacks likewise
return `IngestOutcome` on success and `IngestError` on failure.

### Explicit-version migration

Enable `migratable` plus the selected codec feature. `migration_chain!` takes
`type Current = T; steps { Step: Old => New, ... }`; it no longer takes
`version_field`. Call `T::migrate_from(version, payload, &codec)` with the
version from the envelope or static binding. It decodes the selected type
once and applies `up` steps. `value.migrate_into(version, &codec, out)`
downgrades and encodes into caller storage, returning its length.
`MigrationError::BufferTooSmall` requests a larger output buffer.

Each version must implement `ContractShape`, normally through `catalog!`.
`T::shape_of(version)` returns that version's locked shape. The chain
also generates `Reaches<V>` and `VersionOf<T>` proofs for link declarations.
An empty `steps {}` defines a current-only chain whose minimum version is
`T::VERSION`; it does not require `Clone`. Historical sends clone the
current value before running by-value downgrade steps. A payload field
named `schema_version` is ordinary data and is never inspected for dispatch.

New contract payloads need no `schema_version` field: `SchemaType::VERSION`
and the envelope or static binding identify the version. Removing an existing
field changes the wire shape, including on binary codecs. For a locked contract,
add a new version and migration step; keep historical layouts unchanged when
supporting deployed senders. `AIMDB_UPDATE_LOCK=1` accepts new versions but
cannot overwrite a changed shape under an existing `NAME@VERSION`.
The embedded fixture keeps its frozen v1/v2 payload layouts and migrates to
field-free v3/v4 layouts as an example. Bare legacy JSON with an extra version
field can also be read by a field-free type through an explicit static binding:
serde ignores that extra field; it never selects a migration.

`migratable` does not enable JSON. Custom codecs implement `PayloadDecoder`
and `PayloadEncoder`; the built-in codec features provide them too.

The [rolling-upgrade fixture](../tools/rolling-upgrade-fixture/README.md) runs
separately locked firmware and hub contracts through both MQTT backends and
builds their shared link declarations for Cortex-M.

### Envelope admission

With `migratable` enabled, `LinkContract<T, D>` holds a registration-time
admission policy and its concrete `PayloadDecoder`. `inline(codec, decoder)`
accepts only the current version; `inline_from::<V>(codec, decoder)` accepts
`V..=T::VERSION` and requires the chain's `Reaches<V>` proof.
`bound_current(codec, decoder)` is a bare current-version policy;
`bound::<U>(codec, decoder)` binds a bare payload to exactly `U::VERSION`
and requires `U: VersionOf<T>`. The codec ID must describe the decoder.

`admit_frame(bytes, &policy)` parses the configured inline carrier or builds
a static envelope, then calls `admit(&envelope, &policy)`. It checks name,
version, the locked shape of the received version, and codec before decoding
once. Missing locked shapes skip the shape comparison. An unspecified
header codec uses the link's decoder. Bound payloads must be consumed in
full, including JSON whitespace; trailing values or bytes are rejected.

`Admit<T>` is `Result<IngestValue<T>, IngestError>`, directly usable with
`with_admitting_deserializer`. Success carries the upgraded source version;
core stamps the value locally when producing it and then counts the outcome.
Received wire stamps are ignored. Rejections include the received identity
and expected version range; core's router adds the record key and topic to
its diagnostic. Decoder and migration conversion failures are `decode_error`.
Neither admission nor its successful result allocates beyond allocations
required by the chosen decoder or the contract's own conversion steps.

### Envelope and version declarations on links

Bare payloads remain the default. For mixed firmware versions on an AimDB
MQTT topic, enable the `migratable` and selected codec features, then opt
both peers into inline envelopes:

```rust,ignore
use aimdb_data_contracts::{link_codecs::Postcard, EnvelopeCarrier,
    EnvelopeLinkExt, LinkCodecBuilderExt, VersionedInboundExt, VersionedOutboundExt};

r.link_from("mqtt://sensors/temperature")
    .with_link_codec(Postcard::<128>)
    .with_envelope(EnvelopeCarrier::Inline)
    .accepts_from::<1>()
    .finish();
r.link_to("mqtt://legacy/temperature")
    .with_envelope(EnvelopeCarrier::Inline)
    .sends_as::<1>()
    .with_link_codec(Postcard::<128>)
    .finish();
```

An inline link defaults to the current version only. Outbound headers carry
the shape of the version sent, when locked; `without_shape()` opts out.
Scratch storage reserves the codec capacity plus 19 header bytes. Oversized
values use an owned fallback that includes the same header and target version.

A deployed bare v1 device needs a static binding on the hub:

```rust,ignore
r.link_from("mqtt://old/temperature")
    .with_contract::<TemperatureV1>()
    .with_link_codec(Postcard::<128>)
    .finish();
```

Bindings require a version in the current record's chain. `accepts_from`
applies only to inline links and cannot combine with a static binding.
Outbound `with_contract::<U>()` and `sends_as::<V>()` may combine when their
versions agree. Conflicting declarations fail registration through `build()`.
The methods work on detached links too, in either order around codec
selection, and preserve connector methods such as MQTT `with_qos`.

Builtin `Json`, `Postcard<N>`, and `Cdr<N>` codecs support these declarations.
Use `with_versioned_codec(custom)` for a custom codec implementing both
`PayloadDecoder` and `PayloadEncoder` as well as `LinkCodec<T>`. Its instance
and state are retained. A codec for just one payload type, including the
`Default` marker's delegation to `Linkable`, cannot migrate historical types;
select an explicit generic codec for versioned links. Custom bare codecs
that accept a prefix must report consumption through
`LinkCodec::decode_with_consumed` (or `Linkable::from_bytes_with_consumed`).
Ordinary builtin `LinkCodec::decode` semantics stay unchanged outside link
admission; bare link admission rejects trailing bytes.

Inline headers are tested on both MQTT backends and require matching carrier
configuration on the peers. Foreign ROS 2, KNX, and dashboard links retain
their bare payloads unless explicitly configured otherwise. AimX transport
metadata carriers remain deferred.

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

let record_7 = aimdb_core::Name::<Setpoint>::new("cell4.setpoint");
builder.configure(record_7, |reg| {
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
impl<T, S: aimdb_core::SchemaState> Input<T, S> for PushButton
where
    T: From<ButtonEvent> + Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T, S>) {
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

builder.configure(ButtonKey::B1, |reg| {
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

### A contracts crate as the source of truth

Applications built by different people exchange contracts by their
identity, `SchemaType::NAME@VERSION`, and `VERSION` is bumped by hand. A
field renamed, retyped or reordered without a bump keeps the identity, and
the reader then fails to decode, or decodes a field with another meaning.
A contracts crate's own tests check one rule:

> **The same `NAME@VERSION` always has the same wire shape.**

The check detects structural changes in contracts of a supported profile
(below), and fails for contracts it can tell are outside it.

`catalog!` lists the crate's contracts in one place, and the enums nested in
them as `helpers`:

```rust
aimdb_data_contracts::catalog! {
    contracts: [
        TemperatureV1,       // mqtt_demo.temperature@1, kept for the migration chain
        Temperature,         // mqtt_demo.temperature@2
        TemperatureCommand,
        ButtonPress,
    ],
    helpers: [SensorMode],   // an enum used inside a contract
}
```

Helpers have no `NAME@VERSION` of their own; their shape is part of every
contract that uses them. Without helpers, `catalog! { Temperature,
ButtonPress }` is enough.

```toml
[dev-dependencies]
aimdb-data-contracts = { version = "0.3", features = ["lock"] }
```

It generates `pub const CATALOG: &[ContractId]` (contracts only, `no_std`)
and a test that
checks the catalog against `aimdb.lock` at the crate root. The `lock`
feature belongs in `[dev-dependencies]` only, so firmware builds never see
it. `catalog!` includes the lock at compile time, so the file must exist.
For a new contracts crate, first create `aimdb.lock` containing only
`version = 1`, then run `AIMDB_UPDATE_LOCK=1 cargo test` to populate it.
Commit the generated lock:


```toml
# aimdb.lock: generated, do not edit. AIMDB_UPDATE_LOCK=1 cargo test to update.
version = 1

[[contract]]
name = "mqtt_demo.temperature"
version = 1
shape = "sha256:51d0a7c3…"
```

`catalog!` also implements `ContractShape` for each listed type. Its
`SHAPE` is the first eight bytes of the locked SHA-256, interpreted as a
big-endian integer. A version not yet in the lock has `SHAPE = None` until
the lock is updated. `CATALOG` entries expose `name_id()` (FNV-1a 32 of
`NAME`) and `shape()`. Distinct names with the same ID fail compilation;
several versions of the same name remain valid.

Every firmware and hub CI build must run its contracts crate's lock test.
The runtime shape comes from the committed lock, so skipping the test can
let a changed struct advertise a stale hash. The lock parser and shape
lookup need neither allocation nor the dev-only TOML and SHA-256 crates.
For types outside a catalog, `impl ContractShape for MyType {}` provides
`SHAPE = None` without claiming a locked shape.

| Change | Result |
|---|---|
| New contract or new `VERSION` | fails until accepted with `AIMDB_UPDATE_LOCK=1 cargo test` |
| Contract removed from the catalog | fails until accepted with `AIMDB_UPDATE_LOCK=1 cargo test` |
| Shape changed under the same `NAME@VERSION` | **always fails**; updating the lock does not accept it |
| Two catalog entries with the same `NAME@VERSION` | always fails |
| A contract outside the supported profile | always fails, naming the reason |

A locked entry never changes, so the lock holds a hash of each shape. A
failure shows the current one:

```
mqtt_demo.temperature@1: the shape changed without a VERSION bump
  locked:  sha256:276b9776…
  current: sha256:f7fa28f3…  struct { "sensor_id": str, "temp_c": f32 }
  fix: bump VERSION (and add a migration step), or revert the change
```

The shape is traced with `serde-reflection`: each contract is
deserialized, after its helpers, until every variant was seen, and the
values it yields are serialized back into the same trace, for a binary
format (postcard, CDR) and for a human-readable one (JSON). The shape
covers wire names after `rename` / `rename_all`, field order and exact
types (which postcard and CDR depend on), variant names and indices,
nested types, `Option`, sequences, maps, tuples and arrays.
`#[serde(default)]` is not part of the shape: it makes a change
compatible, not different.

Supported contracts:

- derived structs and externally tagged enums, made of fixed-width
  primitives, strings, bytes, arrays, tuples, `Option`, sequences, maps
  and other such types
- symmetric `rename` / `rename_all`
- every enum nested in a contract listed in `helpers`, including enums
  behind `Option`, `Vec`, a map or another enum; a missing one fails
- recursive enums with a first variant that ends the recursion

Rejected by the check: aliases and asymmetric renames, `#[serde(skip)]`
variants, types sent differently to JSON than to binary formats (such as
`Ipv4Addr`), `#[serde(flatten)]`, untagged and internally tagged enums,
hand-written `Deserialize` impls that reject the traced values, and two
instances of one generic type with different shapes.

Limits the check cannot detect, so keep to them:

- **Every container needs its own serde name.** The tracer knows types only
  by name: two types named `Status` (or a helper standing in for another
  type of its name) are merged into one shape, and a change to one of them
  can go unnoticed. Rename one with `#[serde(rename = "…")]`.
- `skip_serializing_if` (omitting a field for some values) is not visible
  in the shape. JSON readers cope with a missing field; postcard and CDR
  do not.
- Hand-written `Serialize` / `Deserialize` impls are checked only for what
  the traced values show; test their wire format separately.
- Contracts whose wire format is a hand-written serializer (the KNX DPT
  records) are out of scope: their format lives in code, not in the type.
- The lock checks one crate's history. Two crates defining the same `NAME`
  are not detected; prefix contract names per crate (`mqtt_demo.…`).

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
