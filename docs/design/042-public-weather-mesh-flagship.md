# 042 — Public weather mesh flagship

**Status:** 📝 Proposed

**Scope:** `examples/weather-mesh-demo` (hub slot pool, station config), `tools/aimdb-cli` (new `join` subcommand), a small closed-source provisioning service (private ops repo), EMQX Cloud broker setup, and the public landing/dashboard pages. Possible follow-up in `aimdb-mqtt-connector` (Embassy TLS, §8.1).

**Goal:** turn the three-station weather-mesh demo into a single public, hosted mesh that anyone can join with one command — a live demonstration of typed data contracts across MCU → edge → cloud, and a source of real propose/admit traffic for dogfooding.

---

## 1. Context

`examples/weather-mesh-demo` is a working three-tier demo: typed, versioned contracts (`Temperature`, `Humidity`, `DewPoint`, `GpsLocation`, including a `TemperatureV1 → V2` migration), an Open-Meteo-fed station (`alpha`), an Embassy/STM32H563 hardware station (`gamma`), and a Tokio hub. A hosted instance already streams live to a browser dashboard, and the mesh is queryable by any LLM agent via the public MCP endpoint (`aimdb.dev/mcp`, over the `transport-tcp` client transport — [`aimdb-client/src/endpoint.rs`](../../aimdb-client/src/endpoint.rs)).

What separates this from "a public mesh anyone can join" is membership: the hub configures exactly three stations via closed `TempKey`/`HumidityKey`/`DewPointKey` enums with compile-time `#[link_address]` topics ([`weather-mesh-common/src/lib.rs`](../../examples/weather-mesh-demo/weather-mesh-common/src/lib.rs)). Admitting a fourth station today means a new enum variant and a recompile. The demo broker config is also demo-grade: [`docker/mosquitto.conf`](../../examples/weather-mesh-demo/docker/mosquitto.conf) allows anonymous, unlimited connections.

This spec closes both gaps and defines the join UX end to end.

## 2. Decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **One flagship instance**, operated by the project — not a multi-tenant, self-serve platform. | Scope control; the value is a live artifact, not a product. |
| D2 | Hub membership becomes a **bounded slot pool of N string-keyed records** (`StringKey::intern`), configured at startup. | No core changes, no enum surgery; N doubles as the abuse cap. See §4. |
| D3 | Genuinely dynamic (post-`run()`) registration is **out of scope** — deferred to the knowledge-graph layer work, which faces the same static/dynamic-plane problem. Solve it once there. | Avoid solving the hard problem twice. |
| D4 | Broker is **EMQX Cloud** (managed, EU region), replacing self-hosted Mosquitto for the public instance. | Real auth/ACLs/rate limits with zero broker ops; keeps public write ingress off the web host. See §5. |
| D5 | Admission is **self-serve, gated by GitHub identity** (OAuth device flow in the CLI), not a human queue. | Identity substitutes for approval: one active slot per GitHub account, minimum account age, global cap N, post-hoc revocation, and a kill switch to pause admissions. Joining is one command; the maintainer moderates after the fact instead of gating before. See §6. |
| D6 | Credentials are issued by a **small closed-source provisioning service** (server-side, private ops repo). Everything that runs on a contributor's machine stays **open source**. | Client-side secrets are extractable — closed-source client code is obfuscation, not security, and closed-source Rust is impractical for `no_std` targets anyway. Server-side is the only place the EMQX API key and abuse control can actually live. See §6.2. |
| D7 | The join client is a new **`aimdb join` subcommand** in the existing CLI, generic over a provisioning endpoint URL — not a mesh-specific tool, and never a direct client of the EMQX management API. | One tool joins the mesh *and* verifies data arrival over AimX; "how do new edge nodes get admitted" is a product question, not a demo hack. See §7. |
| D8 | Station GPS is **coarsened to 2 decimal places (~1 km)** before publication; precise location is never collected. | The dashboard publishes contributor home locations otherwise. See §9. |
| D9 | Retention: **30 days of live data**; slots silent for 30 days are recycled; per-station credentials are individually revocable. | Policy stated up front on the join page. Storage cost is negligible at weather cadence — these are policy choices, not cost choices. |
| D10 | Slot cap **N = 64** initially (env-configurable, raised by restart). | Large enough that "mesh full" is unlikely at launch; small enough to bound broker connections and dashboard fan-out. |

## 3. Non-goals

- Multi-tenant / bring-your-own-cloud hosting.
- Runtime (post-startup) record registration in `aimdb-core` (D3).
- Any closed-source code in the client path — connector, station, or CLI (D6).
- Proprietary application logic in this deployment. The private ops repo holds deployment configuration and the provisioning shim only; the hub and contracts are the OSS crates.
- Topic validation or schema negotiation at admission time — a station that publishes malformed payloads is rejected by the existing contract deserialization at the hub, visibly (§9).

## 4. Hub: the slot pool

The closed enums are a property of the *example*, not the engine. `aimdb-core` already provides everything needed:

- [`StringKey::intern`](../../aimdb-core/src/record_id.rs) exists precisely for runtime-constructed keys.
- `reg.link_from(&topic)` already takes a runtime string (the current hub passes `key.link_address()` through a `String`).
- Inbound runtime topic resolution is covered by design [018](./018-M7-dynamic-mqtt-topics.md).

The hub change is therefore a loop:

```rust
let n: u16 = std::env::var("MESH_SLOTS")
    .ok().and_then(|v| v.parse().ok()).unwrap_or(64);

for slot in 0..n {
    let key = StringKey::intern(format!("station.{slot}.temperature"));
    let topic = format!("mqtt://station/{slot}/temperature");
    builder.configure::<Temperature>(key, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
        reg.observe();
        reg.link_from(&topic)
            .with_deserializer(|_ctx, data: &[u8]| Temperature::from_bytes(data))
            .finish();
    });
}
// … same loop for Humidity; DewPoint derived per slot as today.
```

Properties:

- **No recompile per station.** Admission assigns an unused slot; the record and subscription already exist.
- **Slot count is fixed per process start.** Raising N is a hub restart, which is acceptable for a single flagship instance (D1).
- **N is the abuse cap.** The broker ACL (§5) is the enforcement point; N bounds worst-case hub state and dashboard fan-out.
- The dashboard filters to slots with recent data, so empty slots are invisible rather than a wall of grey tiles.

Topic scheme: `station/{slot}/{temperature|humidity|dew_point}` — flat, slot-scoped, and matching the ACL pattern below. The existing `sensors/{alpha,beta,gamma}/…` topics remain in the local docker-compose demo, which is unchanged by this spec.

The typed-contract story is unaffected: schema types stay typed end to end; only the *key namespace* moves from a closed enum to bounded strings. The three-enum version remains in the repo as the local demo, where compile-time keys are the point.

## 5. Broker: EMQX Cloud

The public instance uses a managed EMQX Cloud deployment (EU region) instead of self-hosted Mosquitto/EMQX:

- **Isolation:** the public write ingress no longer shares a host with the project website; broker abuse cannot starve the web server.
- **Auth & ACL:** one credential per station, created at admission via the EMQX Cloud API, restricted to publish on `station/{slot}/#` only. The hub uses a separate subscribe-only credential.
- **Limits:** per-client rate and connection limits configured at the broker — the only place abuse control is enforceable (D6).
- **Revocation:** deleting a station's credential evicts it without touching any other station (D9).

The hub connects **outbound** to the broker over `mqtts://…:8883` — supported today by the Tokio client ([`tokio_client.rs`](../../aimdb-mqtt-connector/src/tokio_client.rs), `use-native-tls`). The serverless tier is TLS-only, which is the right default for internet-facing stations but creates one open problem for the MCU path — see §8.1 before committing to a tier.

## 6. Admission and provisioning

### 6.1 Flow

```
contributor CLI                    GitHub                  provisioning svc         EMQX Cloud
    │  aimdb join <url>               │                          │                      │
    │── device-code request ─────────▶│                          │                      │
    │◀─ user code + verify URL ───────│                          │                      │
    │   (user confirms code at        │                          │                      │
    │    github.com/login/device)     │                          │                      │
    │◀─ access token (poll) ──────────│                          │                      │
    │   prompts: station name, city   │                          │                      │
    │── POST /v1/join {token, app} ──────────────────────────── ▶│                      │
    │                                 │◀── verify identity ──────│                      │
    │                                 │    check admission rules │── create user + ACL ▶│
    │                                 │                          │   allocate slot      │
    │◀────────────── station profile (station.toml) ──────────── │                      │
```

- **GitHub device flow** replaces the issue/Action/email plumbing: the CLI prints a short code, the user confirms it at `github.com/login/device`, and the CLI polls for the access token. The OAuth app's client ID is public by design — nothing secret ships in the CLI. No scopes beyond identity are requested.
- **Station metadata** (name, city-level location) is collected by interactive CLI prompts and sent in the join request — no form, no issue template.
- **Admission rules are enforced server-side**, where they cannot be bypassed: one active slot per GitHub account, minimum account age (e.g. 90 days), global cap N (D10), and an admissions kill switch. Mass abuse thus requires mass *aged* GitHub accounts, and N bounds the blast radius regardless.
- **Moderation is post-hoc**, proportionate to the exposure: payloads are schema-validated at the hub, each station can only write its own `station/{slot}/#` topics, and revoking one credential evicts one station (D9). The worst case is garbage numbers in the offender's own slot until revoked.
- The public roster of who's on the mesh is the station pages (§8) — no admission queue needed for transparency.

### 6.2 Provisioning service

A thin HTTPS shim, closed source, in the private ops repo. It is the *only* holder of the EMQX Cloud API key. Responsibilities: verify GitHub identities, enforce the admission rules (§6.1), allocate slots, create/delete broker credentials and ACLs, return station profiles, recycle silent slots (D9). State is one small table: `github_account ↔ slot ↔ broker credential`.

What it is **not**: it is not in the data path (stations speak MQTT to the broker directly), and it is not a client-side component. The boundary is the same one the deployment already draws — ops/config private, contracts/connectors/examples open.

### 6.3 Provisioning endpoint format (open)

The request/response format is documented openly so `aimdb join` (§7) is generic, not flagship-specific. Version 1:

```
POST /v1/join
Content-Type: application/json

{
  "auth": { "kind": "github", "token": "gho_…" },
  "app":  { "name": "graz-balcony", "location": "Graz" }
}
```

`auth.kind` keeps the format deployment-agnostic: the flagship accepts `github`; a private deployment can accept `claim-token` (pre-issued one-time tokens, no public self-serve) with the same envelope.

```
200 OK
{
  "profile_version": 1,
  "station_id": "slot-17",
  "broker": {
    "url": "mqtts://xxxx.eu-central-1.emqx.cloud:8883",
    "username": "station-17",
    "password": "…"
  },
  "app": {
    "name": "graz-balcony",
    "lat": 47.07,
    "lon": 15.44
  }
}
```

- `app` is an opaque, deployment-specific map: the CLI collects it via prompts (driven by the deployment's docs) and passes it through; the service validates, coarsens the location (D8), and echoes the final values back in the profile. Other AimDB deployments define their own `app` contents.
- Errors: `403` (identity rejected: account too new, admissions paused), `409` (account already holds a slot — body points at the existing profile), `503` (mesh full). Bodies carry a human-readable `message` the CLI prints as-is.
- `profile_version` is versioned and backward-compatible from day one: the CLI ships on the workspace release cadence and must not need a release in lockstep with mesh-side changes.

## 7. `aimdb join`

New subcommand in [`tools/aimdb-cli`](../../tools/aimdb-cli) (binary is already named `aimdb`, clap-based):

```
aimdb join <provisioning-url> [--token <t>] [--out station.toml]
```

- Default path: runs the GitHub device flow, prompts for the `app` fields, calls `/v1/join` (§6.3), and writes the profile to `station.toml` (or `--out`). `--token` selects the `claim-token` auth kind instead, for deployments without public self-serve.
- **Generic by construction:** the endpoint URL is an argument, the `app` map is passed through untouched, and no weather-mesh types appear in the CLI. `join` is a product capability — "how a new edge node gets admitted to a deployment" — that debuts on the flagship.
- **Never talks to the EMQX management API.** Doing so would require the admin key in a public binary — the same client-side-secret problem D6 exists to prevent, except leaking the key that can mint credentials for the whole broker.
- One new dependency (`reqwest` with rustls), gated behind a `join` cargo feature, on by default in released binaries.

The three-command loop this enables is the demo:

```
$ aimdb join https://mesh.aimdb.dev
  → To authorize, enter code WDJB-MJHT at https://github.com/login/device
  ✓ authorized as @alexschnoerch
  Station name: graz-balcony
  Location (city): Graz
  ✓ slot-17 "graz-balcony" — credentials written to station.toml

$ cargo run -p weather-station -- --config station.toml &

$ aimdb record list --url tcp://aimdb.dev:7433
  ✓ station.17.temperature   21.4 °C   2s ago
```

Join, run, and verify through the hub's own remote-access protocol — one tool, with the last command quietly demonstrating AimX introspection.

## 8. Station UX

Three tiers, ordered by commitment:

- **Tier 0 — look (30 s):** `aimdb.dev/mesh` — live map + charts, and a copy-pasteable MCP snippet ("point your agent at `aimdb.dev/mcp` and ask which station is coldest"). Already running; landing-page work only.
- **Tier 1 — run locally (5 min, no admission):** the existing docker-compose demo, unchanged. Gives impatient visitors something immediate and shows the contracts are ordinary, readable Rust.
- **Tier 2 — join the public mesh:** §6 flow, then §7 commands. Target: **under 10 minutes from `aimdb join` to dot on the map** — with self-serve admission there is no waiting step left in the funnel. On first successful publish, the station prints the direct URL to its live chart and a ready-made MCP prompt for its own data — the shareable moment.

**Hardware path:** same `station.toml`, consumed at firmware build time and flashed via probe-rs as `weather-station-gamma` does today — contingent on §8.1.

**Station identity pages** (`aimdb.dev/mesh/<name>`): live chart, uptime, last reading. Gives contributors a link to share and a reason to keep stations running — which sustains the propose/admit traffic this exists to generate.

### 8.1 Open problem: Embassy TLS

EMQX Cloud serverless is TLS-only, and [`embassy_client.rs`](../../aimdb-mqtt-connector/src/embassy_client.rs) has no TLS support — so the MCU station cannot currently connect to the public broker directly. Options:

1. **Add TLS to the Embassy MQTT client** (e.g. `embedded-tls`). Real work, but a valuable connector feature in its own right, and it keeps the "MCU speaks directly to a managed cloud broker" claim true. **Recommended.**
2. EMQX Cloud dedicated tier with a plain-TCP listener. Solves it with money and reopens the unencrypted-public-broker question this migration closes.
3. A local bridge/gateway in front of the MCU. Cheapest; quietly breaks the direct-to-cloud story.

This is the one place the broker choice touches the product. Decide before committing to a broker tier; option 1 can land after launch if the hardware station joins later than the cloud-fed path.

## 9. Privacy, lifecycle, and failure UX

- **Location:** only city-level location is collected at admission; published coordinates are rounded to 2 decimals (~1 km) (D8). A short privacy note on the join page states exactly what station metadata is published and for how long.
- **Retention:** 30 days of live data; optionally, downsampled aggregates kept indefinitely (D9).
- **Slot lifecycle:** silent for 30 days → credential revoked, slot recycled, stated up front (D9).
- **Schema errors are a feature:** a malformed payload is rejected by contract deserialization at the hub; the station's own logs say *what contract was violated and what was expected*. The differentiator, demonstrated at the exact moment a user is paying attention.
- **Failure paths:** rejected identity or full mesh → the CLI prints the service's `message` as-is (§6.3); revoked slot → the station says so instead of retrying forever; re-running `aimdb join` with an account that already holds a slot points at the existing profile rather than erroring opaquely.

## 10. Sequencing

1. **Hub slot pool** (§4) — OSS, `examples/weather-mesh-demo`, independent of everything else.
2. **EMQX Cloud setup** (§5) — EU region; resolve §8.1 before choosing the tier.
3. **Provisioning service** (§6.2) — private ops repo; includes the GitHub OAuth app registration, identity/admission checks, and slot-recycling job.
4. **`aimdb join`** (§7) + the open endpoint format doc (§6.3) — OSS.
5. **Landing page, station pages, privacy note** (§8, §9).
6. Launch; Embassy TLS (§8.1 option 1) may follow as the hardware-station on-ramp.

Steps 1 and 4 are pure OSS and unblocked today. Step 3 is the only new operational component, and it is a thin shim in front of the EMQX Cloud API.

## 11. References

- [016 — RecordKey trait](./016-M6-record-key-trait.md) (`StringKey`, `intern`)
- [018 — Dynamic MQTT topics](./018-M7-dynamic-mqtt-topics.md) (runtime topic resolution)
- [041 — Data contracts as first-class capabilities](./041-data-contracts-integration.md) (contract tiers; `Simulatable` stays dev-only)
- [`examples/weather-mesh-demo`](../../examples/weather-mesh-demo) — the demo this spec extends
- [`tools/aimdb-cli`](../../tools/aimdb-cli), [`aimdb-client`](../../aimdb-client) — AimX client surface, `transport-tcp`
