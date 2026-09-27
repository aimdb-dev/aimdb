# 056 — `aimdb join` CLI

**Status:** 📝 Proposed

**Scope:** the `join` subcommand of `tools/aimdb-cli`, speaking the
provisioning format of [043](./043-join-endpoint-v1.md) (rev 2). Client only;
provisioning services are out of scope.

---

## 1. Starting point

An implementation exists on the `planning` branch (`e0dcf9b`,
`tools/aimdb-cli/src/commands/join.rs`, ~480 lines with 9 tests), written
against 043 rev 1. Ported onto `main` it compiles unchanged and its tests
pass. A review found five defects to fix before release:

| # | Defect | Effect |
|---|---|---|
| D1 | The GitHub OAuth client ID is read with `option_env!` at build time | Binaries built without the variable (including `cargo install`) cannot run the device flow |
| D2 | `prompt()` treats end of input as an empty answer and asks again | `aimdb join … </dev/null` loops forever; unusable from scripts |
| D3 | The `app` prompts are hard-coded (station name, city) | Not deployment-neutral |
| D4 | The profile is opened with mode `0600` + truncate | `0600` applies only on creation; an existing world-readable file keeps its mode. Any existing profile is silently overwritten |
| D5 | Any base URL is accepted | `http://` sends the identity token in clear text |

043 rev 2 removes the need for prompts (D2, D3) and adds discovery (D1).

## 2. Command

```
aimdb join <base-url> [--token <claim-token>] [--app <name>=<value>]...
                      [--out <path>] [--force]
```

| Option | Behaviour |
|---|---|
| `<base-url>` | Deployment base URL. Must be `https://`, except loopback hosts (`localhost`, `127.0.0.0/8`, `::1`) for local testing (D5). |
| `--token` | Use the `claim-token` kind; skips the device flow. |
| `--app name=value` | Repeatable. Values for the deployment's `app_fields` (043 §2). Never prompted (D2, D3). |
| `--out` | Profile path. Default `aimdb-profile.toml`. |
| `--force` | Replace an existing profile. Without it, an existing file is an error (D4). |

## 3. Flow

1. **Discover.** `GET <base>/v1/join`. Read the accepted auth kinds and
   `app_fields`. A 404 means `claim-token` only.
2. **Check inputs.** `--app` names not listed in `app_fields` produce a
   warning; required fields that are missing are an error before any
   network call to GitHub.
3. **Authenticate.**
   - `--token` given: `claim-token`.
   - Otherwise, `github` from discovery: run the OAuth device flow with the
     **discovered** `client_id` (D1), no scopes. Print the user code and
     verification URL, poll respecting `interval` and `slow_down`, stop at
     `expires_in`. Print the GitHub login for confirmation.
   - Neither available: error naming the kinds the deployment accepts.
4. **Join.** `POST <base>/v1/join` with `auth` and `app`.
5. **Write.** On `200`, write the profile (§4) and print `node_id` and the
   path. On an error status, print the body's `message` as-is and exit with
   code 1 (§5).

The token is held in memory only: never printed, logged or written.

## 4. Writing the profile

- Serialize the response as TOML (043 §5).
- Write to a temporary file in the target directory, created with mode
  `0600` on Unix (`OpenOptions::mode` + `create_new`), then `fsync` and
  rename over the target. The mode is correct from the first byte whether or
  not a file existed (D4), and a crash never leaves a half-written profile.
- Refuse an existing target without `--force`.
- Warn when `profile_version` is newer than the CLI knows; write anyway
  (043 §6).

## 5. Exit codes

| Code | Meaning |
|---|---|
| 0 | Profile written |
| 1 | Rejected by the deployment (403, 409, 422, 429, 503) — its `message` is printed |
| 2 | Usage error: bad URL, `http://` to a non-loopback host, existing profile without `--force`, missing required `--app` |
| 3 | Network, TLS or unexpected response |
| 4 | Device flow failed: denied, expired |

Distinct codes let scripts retry network failures and not rejections.

## 6. Build and dependencies

- Behind a `join` cargo feature, on by default.
- `reqwest` with `rustls` and the platform trust store
  (`rustls-tls-native-roots`), `json` feature; no OpenSSL. The bundled
  `webpki-roots` are avoided because their license is not on the
  `deny.toml` allowlist (as on `planning`, `5e728a8`).
- `toml` for the profile.
- No OAuth client ID or other deployment value is compiled in.

## 7. Tests

Kept from `planning` (adapted to rev 2): response parsing, unknown fields
ignored, error `message` surfaced as-is, malformed success body, mock-server
round trip, profile mode `0600`.

Added:

1. Discovery: `github` + `client_id` used for the device flow; 404 →
   `claim-token` only; neither available → exit code 2 with the accepted
   kinds.
2. `--app` parsing (`=` inside values, repeated names rejected), unknown
   names warned, missing required names rejected before any request.
3. `http://example.com` rejected; `http://127.0.0.1:…` and
   `http://localhost:…` accepted.
4. Existing profile: refused without `--force`; with `--force`, a
   pre-existing `0644` file ends up `0600`.
5. Closed stdin (`</dev/null`) never blocks: the command needs no input.
6. Device flow against a mock of GitHub's endpoints: `authorization_pending`,
   `slow_down` (interval increases), `expired_token`, `access_denied`.
7. Exit codes for each class in §5.

## 8. Non-goals

- A provisioning service. The format is public; deployments implement it.
- Talking to any broker's management API.
- Running a node. Consumers read the profile file (043 §5).
- Storing tokens or refreshing credentials.

## 9. Sequencing

1. Port `join.rs` from `planning` onto `main` with its tests.
2. Apply rev 2 of 043: discovery, optional `app` via `--app`, no prompts,
   `node_id`.
3. Fix D4 and D5; add exit codes.
4. Add the §7 tests; document the command in the CLI README.

## 10. References

- [043 — Join endpoint format v1](./043-join-endpoint-v1.md)
- `planning` branch: `e0dcf9b` (implementation), `5e728a8` (system trust roots)
- [GitHub OAuth device flow](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow)
