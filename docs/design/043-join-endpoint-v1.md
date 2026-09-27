# 043 — Join endpoint format v1

**Status:** 📝 Proposed (rev 2)

- **rev 1** — first draft, written for the public weather mesh (042): the
  client collected station metadata by prompts and the response carried a
  slot-based `station_id`.
- **rev 2** — made deployment-neutral. The client no longer collects
  metadata; `app` becomes optional in both directions. Adds a discovery
  document so the CLI needs no compiled-in OAuth client ID. Re-joining is
  allowed to rotate credentials. No server implemented rev 1, so v1 is
  revised in place.

**Scope:** the open wire format a *provisioning endpoint* speaks, and the
profile file `aimdb join` writes. Implementing the server side is out of
scope for this repository; the format is public so any AimDB deployment can
implement it. The client is designed in
[056](./056-aimdb-join-cli.md).

---

## 1. Overview

A provisioning endpoint admits a new node to a deployment: it verifies an
identity, applies the deployment's admission rules, creates whatever the node
needs (typically a broker credential scoped to the node's own topics), and
returns a **profile** the node runs with.

```
aimdb join <base-url>
  GET  <base-url>/v1/join   → discovery document (§2)
  POST <base-url>/v1/join   → profile (§3, §4)
```

The path is fixed; the base URL is the deployment. TLS is required: the
request carries an identity token and the response carries a credential.

## 2. Discovery — `GET /v1/join`

```json
{
  "auth": [
    { "kind": "github", "client_id": "Iv1.0123456789abcdef" },
    { "kind": "claim-token" }
  ],
  "app_fields": [
    { "name": "label", "required": false, "help": "Shown next to your node" }
  ]
}
```

| Field | Contract |
|---|---|
| `auth` | The identity kinds this deployment accepts, in order of preference. `github` carries the public OAuth client ID for the device flow. |
| `app_fields` | Optional. Names of `app` values the deployment accepts in the join request, with a help text. Empty or absent: the deployment wants none. |

Unknown fields are ignored. A deployment that does not serve discovery (404)
is treated as accepting `claim-token` only.

## 3. Request — `POST /v1/join`

```json
{
  "auth": { "kind": "github", "token": "gho_…" },
  "app":  { "label": "balcony" }
}
```

### 3.1 `auth`

| `kind` | `token` |
|---|---|
| `github` | A GitHub **user access token** obtained through the OAuth device flow with the client ID from discovery, requesting no scopes. The service verifies that the token belongs to that OAuth app, applies identity-based rules, and should revoke the token after use. |
| `claim-token` | A pre-issued, single-use opaque token distributed by the operator. |

Deployments reject kinds they do not accept with `403`.

### 3.2 `app`

Optional string-keyed map of strings, passed through by the client without
interpretation. Only names announced in `app_fields` are meaningful; the
service validates and may ignore or reject others (`422`).

## 4. Response

### 4.1 Success — `200 OK`

```json
{
  "profile_version": 1,
  "node_id": "alice",
  "broker": {
    "url": "mqtts://mqtt.example.com:8883",
    "username": "alice",
    "password": "…"
  },
  "app": {
    "publish_prefix": "sensors/alice/"
  }
}
```

| Field | Contract |
|---|---|
| `profile_version` | Format version, `1` for this document (§6). |
| `node_id` | The node's identity in the deployment. Opaque to the client. |
| `broker.url` | Connector URL of the data-plane broker, without credentials. |
| `broker.username`, `broker.password` | The node's credential. The service scopes it; the client never sees how. |
| `app` | Optional deployment-specific values for the node, e.g. the topic prefix its credential may publish under. Opaque to the client. |

### 4.2 Re-joining

A join from an identity that already holds a membership is the
deployment's decision:

- **rotate** (recommended): issue a new password for the existing
  membership and return `200` with the full profile. A lost profile is then
  recovered by joining again.
- **refuse**: return `409` with a `message` naming the existing membership.

### 4.3 Errors

Error bodies are JSON with a human-readable `message`, which the client
prints as-is:

```json
{ "message": "Admissions are paused — try again later." }
```

| Status | Meaning |
|---|---|
| `403` | Identity rejected: unsupported kind, token invalid, rules not met, admissions paused. |
| `409` | Identity already holds a membership and the deployment does not rotate (§4.2). |
| `422` | Invalid `app` values. |
| `429` | Too many requests. |
| `503` | Deployment full or temporarily not admitting. |

Any other non-2xx status is unexpected; the client reports it with the body
if present.

## 5. Profile file

The client writes the §4.1 response as TOML, readable by the owner only
(mode `0600` on Unix):

```toml
profile_version = 1
node_id = "alice"

[broker]
url = "mqtts://mqtt.example.com:8883"
username = "alice"
password = "…"

[app]
publish_prefix = "sensors/alice/"
```

The mapping is mechanical (JSON object → TOML table). Consumers read this
file, never the HTTP response.

## 6. Versioning

The CLI ships on the workspace release cadence and must not need a release in
lockstep with deployments:

- Unknown fields are ignored everywhere (discovery, response, profile).
  Services may add fields without a version bump.
- `profile_version` changes only when the meaning of an existing field
  changes. A client receiving a newer version writes the profile and warns; a
  consumer that cannot interpret a profile names the version it expected.
- The request and discovery formats are versioned by the path (`/v1/join`).
  New auth kinds may be added to v1; services reject unknown kinds with `403`.

## 7. What this format is not

- **Not a client of the broker's management API.** Credentials are minted
  server-side; no admin secret can end up in a client binary.
- **Not a schema registry.** Join carries no topic lists or payload schemas;
  payload validity is enforced by the consuming records' deserializers.
- **Not a session.** One request, one response; no client state beyond the
  profile file.

## 8. References

- [056 — `aimdb join` CLI](./056-aimdb-join-cli.md)
- [GitHub OAuth device flow](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow)
- [GitHub: check a token](https://docs.github.com/en/rest/apps/oauth-applications#check-a-token)
  (how a service verifies a token belongs to its OAuth app)
