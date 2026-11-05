# 059 — Probe

**Status:** 📝 Proposed

**Scope:** Call a read-only probe function on a running instance from the
CLI or MCP, and print the lines it returns.

---

## 1. What it is

A connector or input registers a function. A client calls it over AimX. The
function returns lines. That's it.

```
$ aimdb --connect serial:///dev/ttyACM0 probe
mqtt   ok    connected to 192.168.1.10:1883 as "embassy-demo-001"  up_s=2460
mqtt   warn  sensors/temp/indoor: 1 send rejected  sent=1230 rejected=1
knx    fail  not connected, retrying in 3 s
b1     ok    7 rising edges since start, last 41 s ago  edges=7
probe  fail  1 fail, 1 warn, 2 ok
```

## 2. Core API

```rust
pub enum Verdict { Ok, Info, Warn, Fail }

#[derive(Serialize, Deserialize)]
pub struct ProbeLine {
    pub verdict: Verdict,
    pub subject: String,                 // filled in by core
    pub text: String,
    pub facts: Vec<(String, serde_json::Value)>,
}

pub trait Probe: Send + Sync {
    fn probe(&self) -> BoxFuture<'_, Vec<ProbeLine>>;
}
```

Small helpers make the lines easy to write:
`ProbeLine::ok("connected").fact("up_s", 2460)`.

**Registering a probe:**

- A connector calls `db.register_probe(scheme, probe)` inside
  `ConnectorBuilder::build()`. The subject is the scheme.
- An input calls `reg.probe(probe)` inside `Input::wire`. The subject is the
  record key.

Core keeps the registered probes in a list on the database.

**What a probe reads:** a probe never talks to hardware or to the broker
itself. A connector's session loop already knows its state. It writes that
state into a small shared struct (`Arc<Mutex<Status>>`: connected or not,
since when, last error), and the probe reads the struct. An input keeps a
few counters next to its source task in the same way.

## 3. Running a probe

`aimdb_core::probe::run(db, filter, timeout)`:

1. Pick the probes that match the filter (scheme or record key, or all).
2. Call them all, each with the timeout (default 10 s, max 60 s). A probe
   that runs past it gets the line `fail  no answer within 10 s`.
3. Collect the lines in registration order and add a summary line
   (`probe  fail  1 fail, 1 warn, 2 ok`). The summary's verdict is the worst
   one in the list.

Connectors in the repo that don't register a probe yet get the line
`info  <scheme>: no probe`.

## 4. Remote call

- **AimX method `probe`**, a normal one-shot call:
  `{"connector": "mqtt", "record": "b1", "timeout_ms": 10000}`, where every
  field is optional. The reply is an array of `ProbeLine`. It is allowed
  under the `ReadOnly` security policy. Adding the method raises the
  protocol version to 4.1. An older server answers `NOT_FOUND`.
- **CLI:** `aimdb probe [--connector <scheme>] [--record <key>]
  [--timeout <secs>] [--format text|json]`. The text format prints one line
  per probe line, as in §1. The JSON format prints one object per line. The
  exit code is `0` with no `fail` line and `5` with one.
- **MCP:** a `probe` tool with the same parameters. It returns the text
  form. It is read-only, so it needs no `--allow-writes`.

## 5. Rules

- **Read-only:** a probe reports state. It never writes, publishes or
  reconnects.
- **No payload values:** lines carry state, counts, times, addresses and
  topic names only.
- **Bounded:** core enforces the timeout. It also caps the lines per probe
  (32) and the text per line (160 bytes).

## 6. First implementations

| Who | Reports |
|---|---|
| MQTT (Tokio, Embassy) | Connected or not, broker and client ID, uptime, last error; per outbound route: sent and rejected, from `OutboundRoutes::stats()` |
| KNX | Gateway, tunnel state, last error, per-route counts |
| AimX servers | Listening address, connections in use |
| `PushButton`, `Thermometer` demos | Events since start, the last one |

## 7. Later, if needed

These are deliberately left out. Each one can be added without changing the
API above.

- **Active checks:** an MQTT ping or a KNX connection-state request at probe
  time, instead of the last known state.
- **Recent events:** the last few warnings and errors per connector, with
  their age.
- **Streaming lines** for long probes.
- **Discovery:** topics and group addresses seen under prefixes that the
  application allows.
