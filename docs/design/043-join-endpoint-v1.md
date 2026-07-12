# 043 — Join endpoint format v1

**Status:** 📝 Proposed

**Scope:** the open wire format of the provisioning endpoint `aimdb join`
speaks (`POST /v1/join`), and the station profile file the CLI writes. No
code in this repo implements the *server* side — the flagship's provisioning
service lives in the private ops repo ([042](./042-public-weather-mesh-flagship.md)
D6) — but the format is public so any AimDB deployment can implement it and
the CLI stays deployment-agnostic.

**Consumers:** `aimdb join` ([`tools/aimdb-cli`](../../tools/aimdb-cli),
042 §7) is built against this document; `weather-station`
([`examples/weather-mesh-demo`](../../examples/weather-mesh-demo)) consumes
the profile file defined in §4.

---

## 1. Overview

A *provisioning endpoint* admits a new edge node to a deployment: it verifies
an identity, applies the deployment's admission rules, allocates whatever
resources a member needs (for the weather mesh flagship: a slot number and a
scoped MQTT credential), and returns a **station profile** the node runs
with. The CLI is generic over the endpoint URL:

```
aimdb join <provisioning-url> [--token <t>] [--out station.toml]
```

`aimdb join https://mesh.aimdb.dev` sends `POST https://mesh.aimdb.dev/v1/join`.
The path is fixed; the base URL is the deployment. TLS is required — the
request carries a bearer-equivalent token and the response carries a broker
credential.

## 2. Request

```
POST /v1/join
Content-Type: application/json

{
  "auth": { "kind": "github", "token": "gho_…" },
  "app":  { "name": "graz-balcony", "location": "Graz" }
}
```

### 2.1 `auth` — identity envelope

`auth.kind` keeps the format deployment-agnostic. v1 defines two kinds:

| `kind` | `token` | Used by |
|---|---|---|
| `github` | A GitHub **user access token**, obtained by the CLI via the OAuth device flow (no scopes — public identity only). The service verifies it against the GitHub API and applies identity-based admission rules (one active slot per account, minimum account age, …). | The public flagship (self-serve). |
| `claim-token` | A pre-issued, one-time opaque token, distributed out of band by the deployment operator. | Private deployments without public self-serve. `aimdb join --token <t>` selects this kind. |

Deployments accept the kinds they support and reject others with `403`.

### 2.2 `app` — deployment-specific metadata

`app` is an **opaque string-keyed map**: the CLI collects its fields via
interactive prompts (driven by the deployment's docs) and passes them through
untouched — no deployment-specific types exist in the CLI. The service
validates, normalizes, and echoes the final values back in the profile (§3).

For the weather mesh flagship, v1 `app` fields are:

| Field | Meaning |
|---|---|
| `name` | Station name — becomes the public identity page (`aimdb.dev/mesh/<name>`). |
| `location` | City-level location. The service geocodes and **coarsens it to 2 decimal places (~1 km)** before it appears anywhere (042 D8); precise location is never collected. |

## 3. Response

### 3.1 Success — `200 OK`

```json
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

| Field | Contract |
|---|---|
| `profile_version` | Format version of this response, `1` for this document. See §5. |
| `station_id` | The node's identity within the deployment — opaque to the CLI. The flagship uses `slot-<n>`, where `<n>` is the assigned slot in the hub's pool (042 §4); the station binary derives its topic/record namespace (`station/<n>/…`, `station.<n>.…`) from it. |
| `broker.url` | Connector URL of the data-plane broker, without credentials. `mqtts://` for the flagship (broker is TLS-only). |
| `broker.username`, `broker.password` | The per-station credential, scoped server-side to the station's own topics (flagship: publish-only on `station/<n>/#`). Individually revocable (042 D9). |
| `app` | The **final** deployment-specific values (post validation/coarsening) — clients must use these, not what they sent. The flagship echoes `name` and the coarsened `lat`/`lon` the station feeds to its weather source. |

### 3.2 Errors

Error responses are JSON with a human-readable `message` the CLI prints
**as-is** and exits non-zero — the service owns the failure UX text:

```json
{ "message": "The mesh is full (64/64 slots) — try again later." }
```

| Status | Meaning | Flagship examples |
|---|---|---|
| `403` | Identity rejected. | Account too new; admissions paused (kill switch); unsupported `auth.kind`. |
| `409` | The identity already holds a membership. The `message` points at the existing profile (station name/id) rather than erroring opaquely. | "Your account already runs `graz-balcony` (slot-17). Revoke it first or reuse its profile." |
| `503` | Deployment full or temporarily not admitting. | Mesh at cap N. |

Any other non-2xx status is unexpected; the CLI reports it with the body if
one is present.

## 4. The station profile file (`station.toml`)

`aimdb join` writes the §3.1 response as TOML (mode `0600` — it contains a
credential), default path `station.toml`, `--out` to override:

```toml
profile_version = 1
station_id = "slot-17"

[broker]
url = "mqtts://xxxx.eu-central-1.emqx.cloud:8883"
username = "station-17"
password = "…"

[app]
name = "graz-balcony"
lat = 47.07
lon = 15.44
```

The mapping is mechanical (JSON object → TOML table, `app` passed through);
consumers (`weather-station --config station.toml`, gamma's `MESH_CONFIG`
build embedding — 042 §8.2) read this file, never the HTTP response.

## 5. Versioning and compatibility

The CLI ships on the workspace release cadence and must not need a release
in lockstep with mesh-side changes (042 §6.3). Therefore:

- **Unknown fields are ignored**, both in the response (CLI) and in the
  profile (station binaries). Services may add fields without a version bump.
- `profile_version` is bumped only for **breaking** changes to the meaning
  of existing fields. A client that receives a `profile_version` newer than
  it knows writes the profile anyway and warns; a station binary that cannot
  interpret a profile says which version it expected.
- The request envelope is versioned by the URL path (`/v1/join`). New auth
  kinds may be added to v1 (a service rejects unknown kinds with `403`, which
  the CLI reports verbatim).

## 6. What this format deliberately is not

- **Not a client of the broker's management API.** The response carries a
  ready-made credential; how it was minted (EMQX Cloud API for the flagship)
  is invisible to the client, so no admin key can end up in a public binary
  (042 D6/D7).
- **Not a schema registry.** Payload validity is enforced by contract
  deserialization at the hub (042 §9); join-time carries no topic lists or
  schemas.
- **Not a session.** One request, one response, no state on the client
  beyond the profile file. Re-joining with an identity that already holds a
  membership is a `409`, not an idempotent re-issue — credential re-issue is
  a service-side policy decision.

## 7. References

- [042 — Public weather mesh flagship](./042-public-weather-mesh-flagship.md)
  §6 (admission flow), §7 (`aimdb join`), D5–D8
- [Implementation plan — 042](../plan/042-public-weather-mesh-flagship.md)
  WP3/WP4
- [GitHub OAuth device flow](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow)
