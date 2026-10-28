# Current record values over HTTP

A generic polling example: three simulated sensors store their current values in
AimDB `SingleLatest` records. Custom Axum routes on the WebSocket connector read
those records through `Extension<AimDb>` and `AimDb::latest`, without taps or
mirrored state. Record types need no serde derives or remote-access registration.

```sh
cargo run -p http-current-values
```

In another terminal:

```sh
curl -s http://127.0.0.1:8080/temperature
curl -s http://127.0.0.1:8080/humidity
curl -s http://127.0.0.1:8080/window
```

Each response is plain text (`text/plain; charset=utf-8`), without a trailing newline:

| Endpoint | Value | Initial response |
| --- | --- | --- |
| `/temperature` | Degrees Celsius, one decimal place | `21.5` |
| `/humidity` | Relative humidity percent, one decimal place | `45.0` |
| `/window` | Closed = `0`, open = `1` | `0` |

Values are initialized before serving and updated every two seconds. Polling
clients can issue a GET at their preferred interval; intermediate updates may be
skipped. Reads do not consume values. A missing value returns HTTP 503; a record
lookup or type error returns HTTP 500. Stream buffers (`SpmcRing`) have no canonical
latest value, so use `SingleLatest` for this pattern.

The server binds to `127.0.0.1:8080`. To poll from another machine, change `addr`
in `src/main.rs` to `0.0.0.0:8080` and use the server's LAN address in client URLs.
These custom routes have **no authentication** and bypass the connector's
`AuthHandler`. Restrict network access or add authentication middleware to the
router before exposing it. The connector also serves `/ws`, `/health`, and
`/version` on the same port. Stop the example with Ctrl-C.
