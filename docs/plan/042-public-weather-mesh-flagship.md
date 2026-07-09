# Implementation plan — 042 Public weather mesh flagship

**Design:** [042 — Public weather mesh flagship](../design/042-public-weather-mesh-flagship.md)

**Status:** 📝 Planned

This plan translates the design's sequencing (§10) into concrete work packages
(WPs). Each WP is marked by where the work happens:

- **OSS** — this repository; file-level detail below.
- **External** — the private ops repo, the EMQX Cloud console, or the
  aimdb.dev website. Tracked here as checklists so the launch dependency
  graph is complete, but implemented elsewhere.

Codebase facts this plan relies on (verified against the current tree):

- `StringKey::intern` ([`aimdb-core/src/record_id.rs`](../../aimdb-core/src/record_id.rs))
  and `reg.link_from(&topic)` with a runtime string already work — the hub
  already passes `key.link_address()` through a `String`. The slot pool
  needs **no `aimdb-core` changes**, as the design claims (D2).
- The Tokio MQTT client supports `mqtts://` and URL-embedded credentials
  ([`tokio_client.rs`](../../aimdb-mqtt-connector/src/tokio_client.rs),
  `use-native-tls`; credentials are parsed from the connector URL), so hub
  and cloud stations can authenticate to EMQX Cloud today. The Embassy
  client has no TLS — design §8.1, WP7.
- The CLI ([`tools/aimdb-cli`](../../tools/aimdb-cli), binary `aimdb`) is
  clap-based, one file per command in `src/commands/`; it has no HTTP
  client dependency yet.
- `weather-station-gamma` hardcodes `MQTT_BROKER_IP`/`MQT_BROKER_PORT`
  consts in `main.rs`; its `build.rs` is currently just linker args — the
  §8.2 config generation slots in there.

---

## WP1 — Hub slot pool (OSS) — design §4

**Crate:** `examples/weather-mesh-demo/weather-hub`

The hub gains an env-gated *mesh mode* alongside the existing three-enum
demo mode. The local docker-compose demo must remain byte-for-byte
unchanged in behaviour (design §4: "the existing `sensors/{alpha,beta,gamma}/…`
topics remain in the local docker-compose demo").

Changes in [`weather-hub/src/main.rs`](../../examples/weather-mesh-demo/weather-hub/src/main.rs):

1. **Mode switch:** if `MESH_SLOTS` is set, configure the slot pool; if
   unset, run the current `TempKey`/`HumidityKey` enum configuration
   untouched.
2. **Slot pool loop:** for `slot in 0..n` (`n` = `MESH_SLOTS`, default 64,
   D10):
   - `StringKey::intern(format!("station.{slot}.temperature"))`, buffer
     `SpmcRing { capacity: 100 }`, `.observe()`, and
     `.link_from("mqtt://station/{slot}/temperature")` with the existing
     `Temperature::from_bytes` deserializer — same for `Humidity`.
   - `DewPoint` derived per slot via `transform_join` on that slot's
     Temperature + Humidity, reusing the derivation already written in
     [`weather-station-alpha/src/main.rs`](../../examples/weather-mesh-demo/weather-station-alpha/src/main.rs)
     (the `t.celsius - (100.0 - h.percent) / 5.0` join). Deriving at the
     hub keeps the dashboard supplied even if a station publishes only
     temperature/humidity.
   - Drop the per-value `.log()` console lines in mesh mode — 64 slots of
     per-message logging is noise; `.observe()` metrics remain.
3. **Broker URL:** accept a full connector URL via `MQTT_URL`
   (e.g. `mqtts://hub-sub:…@xxxx.eu-central-1.emqx.cloud:8883`), falling
   back to the current `MQTT_BROKER` host-only var for the local demo. The
   hub uses the subscribe-only EMQX credential (design §5). Enable the
   `use-native-tls` feature of `aimdb-mqtt-connector` in
   `weather-hub/Cargo.toml`.

**Verification:** local mosquitto + `MESH_SLOTS=4 cargo run -p weather-hub`;
`mosquitto_pub -t station/2/temperature -m <payload>`; confirm
`station.2.temperature` updates via `aimdb record list` and that a malformed
payload is rejected with a contract error (design §9). Unset `MESH_SLOTS`
and confirm the docker-compose demo behaves exactly as before.

## WP2 — EMQX Cloud setup (external) — design §5

Checklist, in the ops repo / EMQX console:

- [ ] Decide the tier — **blocked on the §8.1 decision** (serverless is
      TLS-only; see WP7). Per design, option 1 (Embassy TLS) is
      recommended, which permits the serverless tier.
- [ ] Deployment in EU region.
- [ ] Hub credential: subscribe-only on `station/#`.
- [ ] Station ACL template: publish-only on `station/{slot}/#`.
- [ ] Per-client rate and connection limits.
- [ ] API key for the provisioning service (never leaves the service, D6).

## WP3 — Provisioning service (external) + open endpoint doc (OSS) — design §6

The service itself is a thin closed-source HTTPS shim in the private ops
repo: GitHub identity verification, admission rules (one slot per account,
minimum account age, cap N, kill switch), slot allocation, EMQX credential
create/delete, 30-day silent-slot recycling, one
`github_account ↔ slot ↔ credential` table. Includes registering the GitHub
OAuth app (device flow; client ID is public).

**OSS artifact in this WP:** the open endpoint format doc —
`docs/design/043-join-endpoint-v1.md` (next free number) — specifying
`POST /v1/join` v1 exactly as design §6.3 defines it: the
`auth.kind = github | claim-token` envelope, the opaque `app` map, the
`profile_version`-ed response, and the `403/409/503` error bodies with
human-readable `message`. This doc is the contract `aimdb join` (WP4) is
built against, so it lands **before or with** WP4.

## WP4 — `aimdb join` (OSS) — design §7

**Crate:** `tools/aimdb-cli`

- New `src/commands/join.rs`, wired as `Command::Join(JoinCommand)` in
  `src/main.rs` following the existing per-command pattern:

  ```
  aimdb join <provisioning-url> [--token <t>] [--out station.toml]
  ```

- Default auth path: GitHub device flow — POST to
  `github.com/login/device/code` with the compiled-in public client ID,
  print the user code + verification URL, poll
  `github.com/login/oauth/access_token` respecting `interval`/`slow_down`.
  `--token` switches the envelope to `claim-token` and skips the flow.
- Prompt on stdin for the deployment's `app` fields (v1: station name,
  city) and pass them through untouched — no weather-mesh types in the CLI.
- `POST /v1/join` per the 043 doc; on `403/409/503` print the body's
  `message` as-is and exit non-zero; on success write the profile as TOML
  to `--out` (default `station.toml`, mode 0600) and print the assigned
  slot + station name.
- Dependencies, gated behind a `join` cargo feature (default **on**):
  `reqwest` (rustls, no default features) and `toml`. Never a client of
  the EMQX management API (D6/D7).

**Verification:** unit tests for profile serialization and error-body
handling against a mock server (`tools/aimdb-cli` already has
`tokio-test`/`tempfile` dev-deps); manual device-flow run against the real
GitHub endpoint with the flagship OAuth app.

## WP5 — Generic cloud station (OSS) — design §7/§8

**New crate:** `examples/weather-mesh-demo/weather-station` (workspace
member), the binary in the design's demo loop:
`cargo run -p weather-station -- --config station.toml`.

- Parses `station.toml` (`serde` + `toml`): broker URL/credentials,
  `station_id` (slot), `app.name`, coarsened `app.lat`/`app.lon`.
- Configures `Temperature`/`Humidity` with `StringKey::intern`
  (`station.{slot}.temperature`, …) and `.link_to("mqtt://station/{slot}/temperature")`,
  plus the derived `DewPoint` — a config-driven port of alpha's setup,
  reusing alpha's Open-Meteo client at the profile's lat/lon.
- First-successful-publish banner: direct URL to the station's live chart
  (`aimdb.dev/mesh/<name>`) and a ready-made MCP prompt — the shareable
  moment (design §8 tier 2).
- Revoked-credential failure UX: on MQTT auth failure, say the slot was
  likely revoked and point at the join page instead of retrying forever
  (design §9).
- Alpha, beta, and the docker-compose demo remain untouched.

**Verification:** hand-written `station.toml` against local mosquitto +
WP1 hub in mesh mode; then the full three-command loop of design §7 once
WP2/WP3 are live.

## WP6 — Gamma build-time config (OSS) — design §8.2

**Crate:** `examples/weather-mesh-demo/weather-station-gamma`

- [`build.rs`](../../examples/weather-mesh-demo/weather-station-gamma/build.rs):
  read the profile at `$MESH_CONFIG` (same `station.toml`), generate
  `station_config.rs` into `OUT_DIR` (broker host/port/credentials, slot,
  name); with `MESH_CONFIG` unset, emit the current local-demo defaults
  (`192.168.1.3:1883`, `sensors/gamma/…` topics) so tier 1 stays
  zero-setup. Declare `cargo:rerun-if-env-changed=MESH_CONFIG` and
  `cargo:rerun-if-changed=<path>`.
- `main.rs`: replace the `MQTT_BROKER_IP`/`MQTT_BROKER_PORT` consts with
  `include!(concat!(env!("OUT_DIR"), "/station_config.rs"))`; success
  banner over defmt.
- Small and independent — can land with WP1. **Contingency:** until WP7,
  `MESH_CONFIG`-built firmware can only target the local demo broker
  (no TLS on Embassy).

**Verification:** build with and without `MESH_CONFIG` and diff the
generated file; flash via the existing `cargo run` probe-rs runner and via
`flash.sh` (split build/flash must keep working).

## WP7 — Embassy TLS (OSS, post-launch acceptable) — design §8.1 option 1

**Crate:** `aimdb-mqtt-connector` (`embassy_client.rs`)

Real connector work, to be scoped in its own design doc before
implementation: `embedded-tls` session over the Embassy TCP socket, entropy
from the STM32H5 TRNG (already bound via the `rng` interrupt in gamma), and
an SNTP task as the time source for certificate validation (no RTC
battery). Keeps the "MCU speaks directly to a managed cloud broker" claim
true. Blocks only the *hardware* on-ramp to the public mesh; the cloud
path (WP1–WP5) launches without it.

## WP8 — Landing, station pages, privacy note (external) — design §8/§9

Website repo checklist:

- [ ] `aimdb.dev/mesh` landing: live map + charts, MCP snippet, join
      instructions (tier 0/2 funnel).
- [ ] Station identity pages `aimdb.dev/mesh/<name>`: live chart, uptime,
      last reading.
- [ ] Dashboard filters to slots with recent data (empty slots invisible,
      design §4).
- [ ] Privacy note: what metadata is published, 2-decimal coordinate
      rounding (D8), 30-day retention and slot recycling (D9).

---

## Sequencing and dependencies

```
WP1 hub slot pool ──────────────┐
WP3(OSS) 043 endpoint doc ─► WP4 aimdb join ─┐
WP5 weather-station ────────────┤             ├─► launch (cloud path)
WP6 gamma build config ─────────┘             │
WP2 EMQX setup ◄─ tier decision (§8.1) ───────┤
WP3(ext) provisioning service ────────────────┘
WP7 Embassy TLS ─► hardware on-ramp (post-launch OK)
WP8 website ──────► launch
```

- **Unblocked today (pure OSS):** WP1, WP3's 043 doc, WP4, WP5, WP6.
- **WP2** waits only on the §8.1 tier decision (recommended: serverless +
  WP7 later).
- **WP3 (service)** is the only new operational component and gates the
  live end-to-end flow.
- **Launch =** WP1 + WP2 + WP3 + WP4 + WP5 + WP8. **WP6/WP7** complete the
  hardware story and may follow.

## End-to-end verification

1. **Local drill (no cloud dependencies):** mosquitto + WP1 hub with
   `MESH_SLOTS=8` + WP5 station with a hand-written `station.toml` →
   `aimdb record list --url tcp://localhost:7433` shows the slot updating;
   a deliberately malformed publish surfaces a contract error at the hub.
2. **Hosted smoke test:** the design §7 three-command loop against the real
   stack — `aimdb join https://mesh.aimdb.dev`, run the station, verify via
   `aimdb record list --url tcp://aimdb.dev:7433` — measured against the
   design's target of **under 10 minutes** from join to dot on the map.
3. **Regression:** `docker compose up` in `examples/weather-mesh-demo`
   behaves exactly as before (tier 1 unchanged), and the gamma local-demo
   build with `MESH_CONFIG` unset is byte-identical in behaviour.
