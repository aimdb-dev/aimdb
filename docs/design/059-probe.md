# 059 — Probe

**Status:** 📝 Proposed

**Scope:** From the CLI or MCP, run a discovery probe on a running instance.
For MQTT the probe lists the topics the broker offers. For KNX it lists the
group addresses that occur on the bus. The probe uses the instance's
network and credentials. It opens its own short-lived connection and is
independent of the instance's connectors and records.

---

## 1. What it is

A probe is a function registered on the instance. A client calls it over
AimX. It opens its **own** connection, listens for a while, closes the
connection and returns what it saw.

```
$ aimdb --connect serial:///dev/ttyACM0 probe mqtt --filter 'sensors/#' --duration 10s
mqtt  sensors/temp/indoor     msgs=10  retained=yes  bytes=42
mqtt  sensors/temp/outdoor    msgs=10  retained=yes  bytes=42
mqtt  sensors/humidity/attic  msgs=3   retained=no   bytes=17
mqtt  3 topics in 10 s

$ aimdb --connect tcp://hub:7777 probe knx --duration 30s
knx   1/2/3   msgs=12  write=12          from=1.1.4        bytes=2
knx   1/2/4   msgs=2   write=1 resp=1    from=1.1.4,1.1.7  bytes=1
knx   3/0/1   msgs=1   read=1            from=1.1.12       bytes=0
knx   3 group addresses in 30 s
```

Why it runs on the instance rather than on the laptop: the instance can
already reach the broker or the KNX gateway, and it has the credentials.
The laptop often has neither.

Why it uses its own connection:

- The running session never changes, so a probe cannot disturb the
  application.
- What the probe receives never reaches a record.
- The probe can listen on `#`, or on the whole bus, without stepping on the
  application's subscriptions.
- A probe also works when no connector for that broker exists. It only
  needs an address.

## 2. Core API

```rust
pub struct ProbeRequest {
    pub filter: Option<String>,   // MQTT topic filter, KNX group-address prefix
    pub duration: Duration,       // listen window; default 10 s, max 120 s
    pub limit: usize,             // max entries returned; default 200
}

pub struct ProbeEntry {
    pub name: String,                            // topic or group address
    pub facts: Vec<(String, serde_json::Value)>, // msgs, retained, bytes, …
}

pub struct ProbeResult {
    pub entries: Vec<ProbeEntry>,
    pub truncated: bool,          // more were seen than `limit`
    pub error: Option<String>,    // e.g. "connect refused: not authorized"
}

pub trait Probe: Send + Sync {
    fn run(&self, req: ProbeRequest) -> BoxFuture<'_, ProbeResult>;
}
```

### Registering a probe

The probe is a builder-level item, independent of connectors:

```rust
builder.probe("mqtt", MqttProbe::new("mqtts://broker:8883").with_credentials(u, p));
builder.probe("knx",  KnxProbe::new("192.168.1.4:3671"));
```

A connector can also register a probe that reuses its own configuration
(URL, credentials, TLS settings, dialer):

```rust
builder.connect(MqttConnector::new(url).with_probe());
```

The application decides which probes exist. If none is registered, a
caller gets `NOT_FOUND`.

## 3. MQTT probe

1. Connect with a new client ID (`<client_id>-probe-<n>`). Use a clean
   session and no will message, with the same broker, credentials and TLS
   settings as configured.
2. Subscribe to the `filter` (default `#`, or the probe's configured
   default) at QoS 0.
3. Listen for `duration`. Retained messages arrive at once; live ones arrive
   as they are published. For each topic, keep the message count, whether
   it was retained, the payload size of the last message and when it was
   last seen.
4. Disconnect and return the topics, sorted.

The probe never publishes. Payloads are counted and measured, never
returned (§6).

If the broker refuses the subscription (ACL), the probe returns `error:
"subscribe to # refused"`. The caller can retry with a narrower filter.

Backends: on Tokio the probe runs a separate `rumqttc` client. On Embassy
it uses the connector's dialer and `mountain-mqtt` client state, with a
second socket (§5).

## 4. KNX probe

1. Open its own tunnel to the gateway.
2. Listen for `duration`. For each group address, record the telegram count
   per service (write, response, read), the source individual addresses
   (up to 4) and the data length.
3. Disconnect and return the group addresses, sorted.

The probe never sends a telegram. The only frames it sends are the tunnel's
own `CONNECT`, `CONNECTIONSTATE` and `DISCONNECT` requests.

**Tunnel slots:** KNX IP interfaces offer few tunnels, often 1 to 4. When
none is free, the gateway refuses the connection and the probe returns
`error: "gateway has no free tunnel (E_NO_MORE_CONNECTIONS)"`. Option, not
in v1: with a KNX IP router, the probe can listen to routing multicast
(`224.0.23.12`) instead, which needs no tunnel slot at all.

## 5. Running it

- **AimX method `probe`**, a one-shot call:
  `{"name": "mqtt", "filter": "sensors/#", "duration_ms": 10000, "limit": 200}`.
  The call returns a `ProbeResult` once the window ends. AimX 4.1 adds the
  method. An older server answers `NOT_FOUND`.
  - The probe future runs inside the call. No extra task is spawned, and
    `duration` bounds it, plus about 5 s for connecting and disconnecting.
  - One probe at a time per instance. A second call gets `BUSY`.
  - `probe.list` returns the registered probe names, so the CLI and MCP can
    show what is available.
- **CLI:** `aimdb probe <name> [--filter <f>] [--duration <secs>]
  [--limit <n>] [--format text|json]`, plus `aimdb probe --list`. The exit
  code is `0` on success and `5` when the result has an `error`.
- **MCP:** a `probe` tool with the same parameters, and `probe_list`.

**Embedded:** a probe needs a second socket and, for `mqtts`, a second TLS
session (tens of KB of RAM while it runs). The Embassy stack must have a
spare socket. The probe's memory is allocated only while it runs. A board
too small for it simply registers no probe.

## 6. Rules

- **Opt-in:** a probe exists only if the application registered it.
- **Listen-only:** no publish and no group write, read or response. Only
  the protocol's own connect, keep-alive and disconnect frames are sent.
- **Isolated:** its own connection. Nothing it receives reaches a record
  or a running connector.
- **No payload values:** names, counts, sizes, times and source addresses
  only.
- **Bounded:** duration, entry limit and one probe at a time.
- **Permission:** a probe is a read-only call, but it can map a broker or a
  bus with the device's credentials. For that reason it is allowed under
  the `ReadOnly` policy only when the application registered it, and the
  application can restrict it with a fixed filter:
  `MqttProbe::new(url).only("sensors/#")`. A request filter must then lie
  under that filter.

## 7. First implementations

| Probe | Crate | Backends |
|---|---|---|
| `MqttProbe` | `aimdb-mqtt-connector` | Tokio (`rumqttc`), Embassy (`mountain-mqtt`) |
| `KnxProbe` | `aimdb-knx-connector` | Tokio and Embassy, through the shared tunnel engine |

## 8. Later

- **KNX routing multicast** as an alternative to a tunnel.
- **Payload shapes,** for example "JSON object with keys a, b, c", to help
  write a data contract for a discovered topic.
- **Generating link declarations** (`link_from("mqtt://…")`) from a probe
  result.
- **Streaming entries** as they are first seen, for long windows.
