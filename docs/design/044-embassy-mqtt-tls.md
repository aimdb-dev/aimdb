# 044 — Embassy MQTT client: TLS

**Status:** 🚧 Implemented on `feat/embassy-mqtt-tls` (hardware verification pending)

**Scope:** the `embassy-tls` feature of
[`aimdb-mqtt-connector`](../../aimdb-mqtt-connector) — TLS, broker
authentication, DNS resolution, and an SNTP time source for the Embassy MQTT
client — plus the `weather-station-gamma` wiring that consumes it. Resolves
the open problem in [042 §8.1](./042-public-weather-mesh-flagship.md)
(option 1, recommended there) and issue `000-wp7-embassy-mqtt-tls`.

**Consumers:** `weather-station-gamma`
([`examples/weather-mesh-demo`](../../examples/weather-mesh-demo)) today;
the MCU station template (issue 006, `aimdb-weather-mesh` repo) once it
tracks this. The hub/cloud path already has TLS via the Tokio client
(`rumqttc`, `use-native-tls`) and is untouched.

---

## 1. Context

EMQX Cloud serverless — the flagship mesh broker (042 D4) — is TLS-only.
The Embassy MQTT client ([`embassy_client.rs`](../../aimdb-mqtt-connector/src/embassy_client.rs))
speaks plain TCP to an IPv4 literal, unauthenticated. Until it can do
`mqtts://` + username/password, an MCU cannot reach the public mesh at all:
gamma's `build.rs` deliberately rejects `mqtts://` profiles, and the
`MQTT_USERNAME`/`MQTT_PASSWORD` consts its `MESH_CONFIG` embedding generates
are dead code.

TLS on a Cortex-M without an OS means three problems the desktop client gets
for free:

- **The TLS session itself** — record layer, handshake, certificate
  verification, all `no_std`.
- **Entropy** — key exchange needs a CSPRNG; the STM32H5 has an on-chip
  TRNG (gamma already binds its `rng` interrupt for the network-stack seed).
- **Wall-clock time** — certificate validity checking needs the current
  Unix time; the reference board has no RTC battery, so time must come from
  the network (SNTP) after DHCP and *before* the first handshake.

Additionally the broker is now a **hostname**, not an IP (serverless
deployments get a DNS name, and SNI is mandatory on shared ingress), so the
client also needs DNS resolution.

## 2. Decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | TLS via **`embedded-tls` 0.19** (TLS 1.3 only), behind a new `embassy-tls` cargo feature. | The only maintained pure-Rust `no_std` TLS 1.3 client; its `embedded-io-async` 0.7 traits match our embassy-net pin exactly, so the session wraps the existing `TcpSocket` and slots under mountain-mqtt's `ConnectionEmbedded` unchanged. TLS 1.3-only is acceptable: EMQX supports it, and this client exists for the flagship path. |
| D2 | Certificate verification via embedded-tls's **`rustpki`** verifier (pure Rust: `der` + `p256`, plus `rsa` and `p384` enabled), against a **root CA the firmware embeds** (DER, supplied by the application). | The `webpki` alternative drags `ring` (C, painful on thumbv8m). `rsa`+`p384` cost flash but make public CA chains (e.g. Let's Encrypt, both its RSA and ECDSA/P-384 intermediates) verify out of the box — 2 MB parts don't care. The station profile (043 §4) carries no CA today, so distribution is the firmware's job; see §9. |
| D3 | Entropy is **injected**: `TlsOptions.rng: &'static mut dyn CryptoRngCore` (rand_core 0.6). | The connector cannot depend on `embassy-stm32` (it is chip-agnostic); the application owns the TRNG. `embassy_stm32::rng::Rng` implements the trait directly, and `&mut dyn CryptoRngCore` satisfies embedded-tls's `CryptoRngCore` bound via rand_core's blanket impls. |
| D4 | Time via a **connector-internal SNTP task** (embassy-net UDP), spawned automatically with the TLS manager; a global monotonic-anchored Unix offset backs the `TlsClock` impl. | The board has no RTC; SNTP is 48 bytes of UDP. Making the connector spawn it keeps "TLS just works" true for consumers — no extra app wiring, no way to forget it. The offset is an `AtomicU32` anchored to `embassy_time::Instant`, so one sync serves all future handshakes; the task re-syncs hourly to bound drift. |
| D5 | The TLS handshake **waits for time sync** before connecting. | `rustpki` skips validity checking when the clock returns `None` — silently accepting expired/not-yet-valid certs on cold boot is worse than a delayed connect. Startup order becomes DHCP → SNTP → TLS, each logged over defmt. |
| D6 | The TLS path runs a **connector-local port of mountain-mqtt-embassy's manager loop** (`run_tls`); the plain-TCP path keeps calling upstream `run()` unchanged. | Upstream `run()` constructs a bare `TcpSocket` internally — there is no seam to insert a TLS session. Porting the ~100-line loop into `aimdb-mqtt-connector` (reusing the fork's public `Settings`, `MqttEvent`, `ClientNoQueue`, `ConnectionEmbedded`) keeps the fork delta minimal and honours the WP7 acceptance criterion that the local plain-TCP demo path is untouched. |
| D7 | Broker **hostname resolution via embassy-net DNS**, per connection attempt, on the TLS path only. SNI and hostname verification use the URL host; `mqtts://` with an **IP literal is rejected at `build()`**. | Serverless brokers are DNS names and records can change between reconnects. An IP-literal `mqtts://` URL can never pass `rustpki`'s hostname matching (it compares DNS names; with no name, only a cert with *no* names at all would match) — failing loudly at build beats a forever-retrying handshake on a headless board. The plain path stays IPv4-literal-only: unchanged is the contract. |
| D8 | Authentication is **orthogonal to TLS**: `MqttConnectorBuilder::with_credentials(username, password)` feeds MQTT CONNECT on both paths. The fork gains upstream 0.4's `ConnectionSettings::with_auth`/`authenticated` constructors — nothing else. | The struct always had the fields; only constructors were missing at our 0.2 fork point. Mirroring upstream's exact API keeps an eventual fork retirement mechanical. Credentials over plain TCP transit cleartext — acceptable on the local demo LAN, and the flagship profile is `mqtts://`. |
| D9 | **URL scheme selects the transport**: `mqtt://` (default port 1883, plain) vs `mqtts://` (default 8883, TLS). `mqtts://` without `with_tls(...)` is a build error, as is `with_tls` on `mqtt://`. | Same convention as the Tokio client and every MQTT tool; the profile's `broker.url` flows through unmodified. Failing loudly at `build()` beats a connect-time surprise on a headless board. |
| D10 | TLS record buffers are **application-provided** (`&'static mut [u8]` in `TlsOptions`; 16 640 read / 4 096 write recommended). | A 16 KB read buffer is the price of correctness (a TLS 1.3 peer may send full-size records regardless of our `max_fragment_length` offer). Hiding it inside the connector would bloat every embassy-mqtt user and take sizing away from the one party that knows the board. |

## 3. Non-goals

- **Mutual TLS / client certificates** and **PSK** — the flagship
  authenticates with per-slot username/password (042 §6); embedded-tls
  supports both if a deployment ever needs them.
- **Session resumption / early data** — reconnects redo the full handshake;
  at weather cadence that is fine.
- **TLS or DNS for the plain-TCP path** — unchanged by design (D6, D7).
- **Tokio client changes** — `mqtts://` already works there.
- **Removing gamma's/the template's `mqtt://`-only guard sight unseen** —
  gamma's guard is lifted here because the same commit wires the TLS it
  guards against; the *template's* guard (WP7 scope item 3) falls only after
  issue 006 lands and an end-to-end `mqtts://` profile run passes on
  hardware.

## 4. Connector API

```rust
use aimdb_mqtt_connector::embassy_client::{MqttConnectorBuilder, TlsOptions};

static TLS_READ_BUF: StaticCell<[u8; 16_640]> = StaticCell::new();
static TLS_WRITE_BUF: StaticCell<[u8; 4_096]> = StaticCell::new();
static TLS_RNG: StaticCell<Rng<'static, peripherals::RNG>> = StaticCell::new();

let mqtt = MqttConnectorBuilder::new("mqtts://xxxx.eu-central-1.emqx.cloud:8883", stack)
    .with_client_id("station-17")
    .with_credentials(MQTT_USERNAME, MQTT_PASSWORD)
    .with_tls(TlsOptions::new(
        TLS_RNG.init(rng),                  // &'static mut dyn CryptoRngCore (D3)
        CA_DER,                             // &'static [u8], root CA, DER (D2)
        TLS_READ_BUF.init([0; 16_640]),
        TLS_WRITE_BUF.init([0; 4_096]),
    ));
```

`TlsOptions::new` defaults the SNTP server to `pool.ntp.org`;
`.with_sntp_server(host)` overrides it (D4). The builder keeps its existing
shape — `ConnectorBuilder::build()` decides plain vs TLS from the URL scheme
(D9) and returns the same `Vec` of runner futures, now including the SNTP
task when TLS is on.

## 5. The TLS manager loop (`run_tls`)

Per connection attempt, in order, every step logged and every failure
falling through to the reconnect delay (same policy as upstream `run()`):

1. **Wait for time** — until `sntp::unix_now()` is `Some` (D5; first boot
   only, the offset persists across reconnects).
2. **Resolve** the broker host (`dns_query`, A record) (D7).
3. **TCP connect** on a socket borrowed from the same 4 KB rx/tx buffers the
   plain path sizes.
4. **TLS open** — `TlsConnection::new(socket, read_buf, write_buf)` +
   `open()` with SNI = URL host, cipher suite `TLS_AES_128_GCM_SHA256`, and
   a `CryptoProvider` combining the injected RNG with
   `rustpki::CertVerifier<Aes128GcmSha256, SntpClock, 4096>` over the
   embedded CA (D2, D3).
5. **MQTT session** — `ConnectionEmbedded::new(tls_connection)` into
   `ClientNoQueue`, then the ported loop: CONNECT with credentials (D8),
   re-subscribe the inbound routes (per session, so inbound routing survives
   reconnects — the plain path queues subscriptions only once at startup),
   then ping/poll/action-drain with the same `Settings` timeouts,
   `MqttEvent`s, and pending-action retry semantics as upstream.

The buffers and RNG are `&'static mut` reborrowed each iteration — attempts
are strictly sequential, so this satisfies the borrow checker without
copies. The whole task is boxed through the adapter's force-`Send`
`into_box_future`, under the same single-core-executor invariant as the rest
of the connector.

## 6. SNTP (`sntp` module)

Minimal SNTPv4 client (RFC 4330 subset), `embassy-tls`-gated:

- one UDP socket, one 48-byte request (`LI=0, VN=4, Mode=client`), transmit
  timestamp taken from the reply, sanity-checked (`Mode=server`,
  `stratum 1..=15`, non-zero timestamp);
- global state is a single `AtomicU32` — Unix seconds minus
  `embassy_time::Instant::now().as_secs()` at sync — read by
  `unix_now() -> Option<u64>` and the `SntpClock: TlsClock` impl;
- the task retries every 10 s until the first sync, then re-syncs hourly;
  the server hostname resolves through the same DNS as D7.

The `u32` offset and second resolution are deliberate: certificate validity
has day granularity, and the representation is unambiguous until 2106.
(The NTP-era rollover in 2036 is a documented limitation of the on-wire
format shared by every SNTP consumer; a future era-aware fix is contained in
one parse function.)

## 7. Gamma wiring

- **`build.rs`**: the `mqtt://`-only panic is replaced by real `mqtts://`
  handling — default port 8883, `cargo:rustc-cfg=mesh_tls`, and CA embedding:
  `MESH_CA` (env var, path to the root CA in PEM or DER) is decoded to DER in
  `OUT_DIR/ca.der` and surfaced as `MQTT_CA_DER`. A `mqtts://` profile
  without `MESH_CA` is a build error with instructions. Plain profiles and
  the no-profile local demo generate byte-identical config to today.
- **`main.rs`**: TLS statics (RNG cell, record buffers) and the
  `.with_credentials(...)`/`.with_tls(...)` calls sit behind
  `#[cfg(mesh_tls)]`, so a plain-demo build carries zero TLS code or RAM —
  the same compile-time sim-to-real philosophy as design 041. The TRNG
  moves into a `StaticCell` so the one instance seeds the net stack *and*
  feeds TLS. `StackResources` grows 3 → 5 (DNS + SNTP sockets).

## 8. Cost

On the reference STM32H563ZI (640 KB RAM / 2 MB flash), the TLS build adds
~21 KB of statically-allocated RAM (16 640 + 4 096 record buffers, D10) plus
the handshake's stack transient, and roughly 150–250 KB of flash
(p256 + rsa + p384 + SHA-2 + embedded-tls). Comfortable here; a smaller
part could drop `rsa`/`p384` (features are additive pass-throughs) and pin
its broker CA chain to P-256.

## 9. Open questions

- **CA distribution.** The firmware embeds the root CA (D2), but the station
  profile doesn't carry one — joining the flagship from an MCU needs the CA
  file fetched out of band (`MESH_CA=isrg-root-x1.pem`). Proposal for a 043
  revision: optional `broker.ca` (PEM) in the profile, emitted by the
  provisioning service; gamma's `build.rs` would prefer it over `MESH_CA`.
  Belongs to WP2/WP7 follow-up once the EMQX tier (issue 007) fixes the
  actual chain.
- **Record-size negotiation.** embedded-tls offers `max_fragment_length`,
  but RFC 8449 `record_size_limit` is what modern stacks respect; if EMQX
  honours either, the 16 KB read buffer could shrink substantially. Measure
  against the real broker before optimising.

## 10. Verification

- Host: existing `make check`/`make test` legs, plus unit tests for the URL
  scheme/port/TLS-option validation and the SNTP packet codec.
- Cross: the existing thumbv7em clippy leg gains an
  `embassy-runtime,embassy-tls,defmt` variant; gamma builds for
  thumbv8m.main with a synthetic `mqtts://` profile + `MESH_CA` (compile
  proof of the full wiring) and without one (plain demo unchanged).
- Hardware (the WP7 acceptance): gamma + real mesh profile connects,
  authenticates, publishes through the TLS broker — runs on the bench once
  a broker credential exists (issue 007), tracked in the WP7 issue.
