# 059 — Probe

**Status:** 📝 Proposed

**Scope:** From the CLI or MCP, run a named probe on a running instance and
get back what it observed. Examples are MQTT topics on a broker, KNX group
addresses on a bus, or presses of a button. Core provides only the
mechanism: registering, calling, bounding and printing a probe. What a
probe does, and how deeply it hooks into the instance, is up to whoever
registers it. Ready-made probes cover the common cases.

---

## 1. What it is

A probe is an async function, registered on the instance under a name. A
client calls it by name with a filter and a duration. The probe observes
for that long and returns a list of entries.

```
$ aimdb probe --list
mqtt   topics on the broker (own connection)
knx    group addresses on the bus (running tunnel)
b1     presses of the user button

$ aimdb probe mqtt --filter 'sensors/#' --duration 10s
mqtt  sensors/temp/indoor     msgs=10  retained=yes  bytes=42
mqtt  sensors/temp/outdoor    msgs=10  retained=yes  bytes=42
mqtt  2 entries in 10 s

$ aimdb probe b1 --duration 30s        # press the button now
b1    rising   count=3  last_ms=2100
b1    1 entry in 30 s

$ aimdb probe knx --filter 1/2/ --duration 30s
knx   1/2/3   msgs=12  write=12        from=1.1.4
knx   1/2/4   msgs=2   write=1 resp=1  from=1.1.4,1.1.7
knx   2 entries in 30 s
```

## 2. Core: the mechanism only

```rust
pub struct ProbeRequest {
    pub filter: Option<String>,  // meaning is the probe's own
    pub duration: Duration,      // clamped by core: default 10 s, max 120 s
    pub limit: usize,            // default 200
}

pub struct ProbeEntry {
    pub name: String,                            // topic, group address, edge, …
    pub facts: Vec<(String, serde_json::Value)>, // counts, sizes, times
}

pub struct ProbeResult {
    pub entries: Vec<ProbeEntry>,
    pub truncated: bool,
    pub error: Option<String>,
}

pub trait Probe: Send + Sync {
    fn describe(&self) -> &str;                  // one line for `probe --list`
    fn run(&self, req: ProbeRequest) -> BoxFuture<'_, ProbeResult>;
}
```

There are three ways to register a probe. All of them end in the same
registry.

```rust
// 1. On the builder: anything, including a closure.
builder.probe("mqtt", MqttProbe::new("mqtts://broker:8883").with_credentials(u, p));

// 2. Inside an Input::wire, next to the source it observes.
reg.probe("b1", probe);

// 3. From a connector's build(), offered as an option by the connector.
builder.connect(KnxConnector::new(gw).with_probe("knx"));
```

Core is responsible for these, and nothing else:

- **Registry:** names must be unique, and a duplicate fails the build.
- **Calling:** the AimX method `probe` takes `{name, filter, duration_ms,
  limit}` and returns a `ProbeResult`. `probe.list` returns the names and
  their descriptions. These are new in AimX 4.1; an older server answers
  `NOT_FOUND`.
- **Bounds:** core cuts a probe off at `duration` + 5 s, enforces `limit`,
  and runs one probe at a time per instance. A second call gets `BUSY`.
- **Permission:** `probe` is allowed under the `ReadOnly` policy. A probe
  exists only because the application registered it, and that registration
  is the permission.
- **CLI:** `aimdb probe <name> [--filter] [--duration] [--limit]
  [--format text|json]` and `aimdb probe --list`. The exit code is `5` when
  the result has an `error`.
- **MCP:** the read-only tools `probe` and `probe_list`.

Core does not know about topics, buses or pins.

## 3. Integration depth is the application's choice

A probe can sit at any of three depths. Core treats all of them the same;
they differ in cost and in what they can see.

| Depth | How it observes | Can see | Costs | Touches the running app |
|---|---|---|---|---|
| **Standalone** | Opens its own connection for the window | Everything its credentials allow (`#`, the whole bus) | A socket, and a TLS session for `mqtts`; a free KNX tunnel slot | No |
| **Tap** | Watches traffic an existing session or input already receives | What already arrives: subscribed topics, every telegram the tunnel forwards, the pin | One atomic check per message when idle | Read-only |
| **Session** | Asks the running session to add something for the window (for example a temporary MQTT subscription) | Beyond the current subscriptions, without a new connection | A subscribe and unsubscribe on the live session | Yes, briefly |

The guideline for the ready-made probes:

- Default to **standalone** where a second connection is cheap (MQTT on
  Tokio).
- Default to **tap** where a second connection is scarce or impossible: a
  KNX tunnel slot, a pin, or a small MCU.
- Offer **session** only where it is the only way to see more without a
  second connection, and only as an explicit option.

## 4. Building blocks for implementers

These are small helpers, so a probe is a few lines:

- **`Collector`:** counts entries by name, keeps a few facts per name,
  respects `limit` and sorts the output. Every ready-made probe uses it.
- **`ProbeTap`:** an optional observer slot for a source to call with
  `tap.observe(name, |facts| …)`. It does nothing unless a probe is
  running. A connector or input that wants to be tappable holds one and
  calls it where it already handles a message or event. The probe installs
  a `Collector` into it for the window.
- **`probe_fn(desc, |req| async { … })`:** turns a closure into a `Probe`
  for application code.

## 5. Ready-made probes

| Probe | Depth | Notes |
|---|---|---|
| `MqttProbe::new(url)` | Standalone | Own client ID, clean session, subscribes to `filter` (default `#`) at QoS 0, never publishes. Tokio and Embassy. |
| `MqttConnector::with_probe(name)` | Tap | Topics the running session receives, including matches of wildcard links. |
| `MqttConnector::with_probe(name).discover()` | Session | Adds a temporary subscription for the window. It skips filters that overlap an application subscription, because some brokers would deliver those messages twice and a record could get a duplicate. |
| `KnxConnector::with_probe(name)` | Tap | Every group telegram the running tunnel forwards, including addresses with no route. Needs no tunnel slot. |
| `KnxProbe::new(gateway)` | Standalone | Own tunnel. Fails cleanly if the gateway has no free slot. |
| Inputs (`PushButton`, `Thermometer` in the demos) | Tap | The input holds a `ProbeTap` and calls it on each event. Shown as the pattern for application inputs. |

An application can use several at once, for example `mqtt` (standalone,
whole broker) next to `mqtt-live` (tap, the running session).

## 6. Rules for all probes

These are conventions that every ready-made probe follows. Core enforces
only the bounds.

- **Listen-only:** a probe sends no publish, no group write, read or
  response, and does not switch outputs. It sends only protocol frames
  (connect, keep-alive, subscribe and unsubscribe at session depth).
- **No payload values** by default: names, counts, sizes, times and source
  addresses only. An application probe may choose otherwise; it is the
  application's code.
- **Isolation:** what a standalone or session probe receives never reaches
  a record.

## 7. Later

- A KNX probe on routing multicast, as a standalone probe that needs no
  tunnel slot.
- Payload shapes, to help write a data contract for a discovered topic.
- Streaming entries as they appear, for long windows.
- Generating link declarations from a probe result.
