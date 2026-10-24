# 056 — `aimdb join` CLI

**Status:** 📝 Proposed

**Scope:** the `join` subcommand of `tools/aimdb-cli`, speaking the
provisioning format of [043](./043-join-endpoint-v1.md) (rev 2). Client only;
provisioning services are out of scope. Implemented from scratch on `main`.

---

## 1. Requirements

| # | Requirement |
|---|---|
| R1 | No deployment value is compiled in. The GitHub OAuth client ID comes from discovery (043 §2). |
| R2 | The command never reads stdin. All input comes from arguments, so it runs unattended (`</dev/null`, CI, provisioning scripts). |
| R3 | Deployment-neutral: `app` values are passed through from `--app` without interpretation; the CLI knows no field names. |
| R4 | The profile is readable by the owner only from its first byte, is never half-written, and never replaces an existing file unless asked. |
| R5 | The identity token travels over TLS only, and is never printed, logged, written to disk, or shown in `Debug` output. |
| R6 | The profile keeps every field of the response, including fields this CLI does not know (043 §6). |
| R7 | Failure classes have distinct exit codes, so scripts can retry network failures and not rejections. |

## 2. Command

```
aimdb join <base-url> [--token <claim-token>] [--app <name>=<value>]...
                      [--out <path>] [--force]
```

| Option | Behaviour |
|---|---|
| `<base-url>` | Deployment base URL. Must be `https://`, except loopback hosts (`localhost`, `127.0.0.0/8`, `::1`), which may use `http://` for local testing (R5). |
| `--token` | Use the `claim-token` kind; skips the device flow. |
| `--app name=value` | Repeatable. Values for the deployment's `app_fields` (043 §2). Split at the first `=`, so values may contain `=`. A repeated name is a usage error. |
| `--out` | Profile path. Default `aimdb-profile.toml`. |
| `--force` | Replace an existing profile. Without it, an existing file is an error (R4). |

The global `--connect` option does not apply to `join`.

## 3. Flow

1. **Discover.** `GET <base>/v1/join`.
   - `200` with a valid document: read `auth` and `app_fields`.
   - `404`: the deployment accepts `claim-token` only and announces no
     `app_fields`.
   - Any other status, a malformed body, or a network/TLS error: exit 3.
2. **Check inputs** before any further request.
   - `--app` names not listed in `app_fields`: warning, sent anyway (the
     service decides, 043 §3.2). After a `404` there is no list, so no
     warnings are issued.
   - Required `app_fields` missing from `--app`: exit 2.
3. **Authenticate.**
   - `--token` given: `claim-token`, whether or not discovery lists it (the
     service rejects kinds it does not accept with `403`).
   - Otherwise, `github` listed in discovery: run the OAuth device flow with
     the discovered `client_id`, requesting no scopes. Print the user code
     and verification URL, poll respecting `interval` and `slow_down` (use
     the returned `interval`, else add 5 s), stop at `expires_in`. Then
     fetch `GET /user` and print the GitHub login for confirmation; failure
     of that call is cosmetic and ignored.
   - Neither: exit 2, naming the kinds the deployment accepts.
4. **Join.** `POST <base>/v1/join` with `auth` and `app` (`app` omitted when
   empty).
5. **Write.** On `200`, write the profile (§4) and print `node_id` and the
   path. On a `043 §4.3` error status, print the body's `message` as-is
   (or the status if the body has none) and exit 1. Any other status: exit 3
   with the status and body.

### 3.1 Transport rules

- One HTTP client for all requests: rustls with the platform trust store,
  user agent `aimdb-cli/<version>`, 30 s timeout per request.
- **No redirects are followed** on requests to the deployment. A `3xx` is an
  unexpected response (exit 3) that names the `Location`, so the user can
  pass the right base URL. Otherwise an `https://` base could redirect a
  `307`/`308` POST, token included, to `http://`.
- The GitHub endpoints (`github.com/login/device/code`,
  `github.com/login/oauth/access_token`, `api.github.com/user`) are
  constants. Tests reach a mock through an internal parameter of the flow
  function, never through an environment variable or flag: an override
  would let anyone redirect the token.
- Types that hold the token implement `Debug` with the token redacted, or
  not at all. Error messages never include request bodies.

## 4. Writing the profile

- **Content.** The success body is parsed as a JSON object. `profile_version`
  (integer), `node_id` (string), and `broker.url`, `broker.username`,
  `broker.password` (strings) must be present; otherwise exit 3. The
  **whole object** is then converted to TOML (R6). `null` values are
  dropped, since TOML has no null; every other JSON value has a TOML
  equivalent. Keys keep the order of the response.
- **Version.** Warn when `profile_version` is newer than the CLI knows;
  write anyway (043 §6).
- **Creation.** Write to a temporary file in the target directory (the
  `tempfile` crate: `0600` on Unix from creation), `fsync` it, then
  - without `--force`: persist with no-clobber (`persist_noclobber`), so a
    file that appeared since the check is still not replaced (exit 2);
  - with `--force`: rename over the target.

  Then `fsync` the directory on Unix. The mode is correct whether or not a
  file existed, and a crash never leaves a half-written profile.
- An existing target without `--force` is detected before any network
  request (exit 2), and checked again atomically at the end.
- On Windows the file gets the default ACL of its directory; the user is
  responsible for where they put it.

## 5. Exit codes

| Code | Meaning |
|---|---|
| 0 | Profile written |
| 1 | Rejected by the deployment (403, 409, 422, 429, 503); its `message` is printed |
| 2 | Usage error: bad URL, `http://` to a non-loopback host, bad or repeated `--app`, missing required `--app`, no usable auth kind, existing profile without `--force` |
| 3 | Network, TLS, redirect, or unexpected response (discovery or join), malformed profile |
| 4 | Device flow failed: denied, expired, or an unknown error from GitHub |

These codes follow the taxonomy every `aimdb` command shares (#334): 1
rejected, 2 usage, 3 environment, 4 and up command-specific. `CliError::exit_code()`
maps the existing variants onto it (connection, discovery, protocol, I/O and
unparsable replies → 3; unsupported endpoint and invalid command-line JSON →
2; record not found, write-protected, server and other errors → 1), and
`main` exits with it. `join` adds its own variants for the table above,
reusing the `EXIT_*` constants in `tools/aimdb-cli/src/error.rs`.

## 6. Build and dependencies

- Behind a `join` cargo feature, on by default.
- `reqwest` `0.12` (already in the lockfile) with `default-features = false`
  and `json`, `rustls-tls-native-roots`; no OpenSSL. Not the bundled
  `webpki-roots`, whose license is not on the `deny.toml` allowlist.
  Pinned to `0.12`: `0.13` renames the TLS features.
- `toml` for the profile, `tempfile` moved from dev to normal dependency.
- `cargo deny` must pass with the feature on.

## 7. Tests

All network tests run against local mock servers; none reach GitHub or the
internet.

1. URL rules: `http://example.com` rejected (2); `http://127.0.0.1:…`,
   `http://[::1]:…` and `http://localhost:…` accepted; `https://` accepted.
2. `--app` parsing: `=` inside values, missing `=`, repeated names.
3. Discovery: `github` + `client_id` drives the device flow with that ID;
   `404` → `claim-token` only and no `--app` warnings; `500` and malformed
   JSON → 3; neither kind usable → 2 with the accepted kinds named;
   unknown fields ignored.
4. Inputs: unknown `--app` names warned and sent; missing required names
   rejected with no request after discovery.
5. Device flow against a GitHub mock: `authorization_pending`, `slow_down`
   (interval increases), `expired_token` (4), `access_denied` (4), success;
   a failing `GET /user` does not fail the join.
6. Join responses: `200` round trip; each 043 §4.3 status → 1 with
   `message` printed as-is; error body without JSON → 1 naming the status;
   `500` → 3; `307` to `http://` → 3 and the mock sees no second request.
7. Profile content: unknown fields at top level, in `broker` and in `app`
   survive into the TOML; `null` dropped; missing required fields → 3; a
   newer `profile_version` warns and writes.
8. Profile file: mode `0600` on a fresh file; an existing file refused
   without `--force` and left unchanged; with `--force`, a pre-existing
   `0644` file ends up `0600`.
9. Closed stdin (`</dev/null`) never blocks.
10. The token never appears in stdout, stderr, the profile, or `Debug`
    output of the request types.

## 8. Non-goals

- A provisioning service. The format is public; deployments implement it.
- Talking to any broker's management API.
- Running a node, or a library that turns a profile into a connector.
  Consumers read the profile file (043 §5); MQTT maps directly to
  `MqttConnector::new(broker.url).with_credentials(username, password)`.
- Storing tokens or refreshing credentials.

## 9. Sequencing

1. `CliError::exit_code()` and its use in `main` (done in #334).
2. `join` feature, dependencies, `cargo deny`.
3. Discovery, input checks, claim-token join, profile writing (§3, §4),
   with tests 1–4 and 6–10.
4. GitHub device flow with test 5.
5. Document the command in the CLI README and changelog.

## 10. References

- [043 — Join endpoint format v1](./043-join-endpoint-v1.md)
- [GitHub OAuth device flow](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow)
