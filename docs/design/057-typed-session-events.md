# Typed session operational events

Issue: [#357](https://github.com/aimdb-dev/aimdb/issues/357).

The session pilot represents existing operational reporting as internal
`SessionEvent<'a>` values. It covers the generic server/client engines, framing,
and AimX dispatch. Call sites describe what happened; `session::events::report`
contains each message's severity and text and forwards through the existing
`log_*!` facade to every enabled destination (`tracing`, `log`, `defmt`).

Payloads borrow strings and errors during synchronous reporting, or carry
small numeric/error values. Reporting adds no allocation in production. Events
are crate-private and have `Debug` and optional `defmt::Format` implementations.
AimX-specific variants require `remote`. No new event is emitted for a branch
that was previously silent. Accept failure summaries retain their existing
first-failure, powers-of-ten, and recovery policy.

## Logging target migration

All migrated messages now originate from `aimdb_core::session::events`.
Filters targeting `aimdb_core::session::server`, `::client`, `::io`, or
`aimdb_core::session::aimx::dispatch` must include the new target. A filter for
`aimdb_core::session` continues to cover all of them. Message text, severity,
and emission conditions are unchanged. Source module/file metadata now points
to the reporting function.

## Test capture

With `cfg(test)` and `std`, an internal synchronous callback can inspect typed
events and collect owned projections. Capture runs regardless of enabled logging
destinations, before backend forwarding. It is absent from production builds.

The async capture wrapper installs its callback only while polling its wrapped
future. A thread-local slot isolates threads; a guard restores the previous
callback on return or unwind, supporting nested capture. No installed callback
survives an await or task migration. Spawned tasks must be wrapped individually;
capture does not propagate to unrelated tasks. Callbacks must avoid recursively
reporting events. The callback is invoked outside the slot's `RefCell` borrow.

## Follow-up boundaries

Other core subsystems remain unchanged. This pilot adds neither public observer
configuration nor metrics counters, `StageMetrics` integration, or AimX metadata.
Those need a separate decision about event frequency, ownership, and observation
lifetime before extending this internal reporting seam.

## Workspace rollout

The same internal pattern now covers the remaining operational reporting in the
workspace. Each subsystem owns its enum and reporter; there is no shared event
registry, public observer, production event queue, or additional metrics layer.
The session API and its existing event capture remain unchanged.

| Owner | Operational event family | Target |
| --- | --- | --- |
| Core router | Ingest failed | `aimdb_core::router::events` |
| Core outbound | Lag, topic overflow, serialization failure, closure | `aimdb_core::outbound::events` |
| Core transforms | Input resolution failure, lag, input/task termination | `aimdb_core::transform::events` |
| Core record streams | Lag and terminal errors | `aimdb_core::remote::stream::events` |
| Core remote writes | Producer/transform ownership refusal | `aimdb_core::typed_record::events` |
| MQTT | Native failures/closure, embedded QoS downgrade, build/background/session/TLS failures, liveness, rejected packets and clock outcomes | `aimdb_mqtt_connector::events` |
| KNX | Send/ACK failures, rejected outbound values and bind retries | `aimdb_knx_connector::events` |
| Zenoh / ROS 2 | Declaration/open/publish failures, invalid keys, dropped samples, ingest panic, session/route lifecycle, configuration and shared-session warnings | `aimdb_zenoh_connector::events` |
| WebSocket | Bind/server failures, listening, upgrade refusals and query failures | `aimdb_websocket_connector::events` |
| Persistence | Missing backend, encode/store/cleanup failures and skipped invalid rows | `aimdb_persistence::events` |
| SQLite | Corrupt JSON replaced with null | `aimdb_persistence_sqlite::events` |
| Sync | Startup/handoff failures and implicit shutdown | `aimdb_sync::events` |
| Embassy buffers | Subscriber slots exhausted | `aimdb_embassy_adapter::events` |
| MCP executable | Ready/EOF/stop, malformed input, method failures and response failures | `aimdb_mcp::events` |

### Payloads and observation

Strings and diagnostics are borrowed only during synchronous reporting. Concrete
core error types retain their types. Foreign diagnostics use local borrowed
`Detail` wrappers that preserve the original Display/Debug formatting policy;
there is no eager conversion to an owned message. Events retain domain fields
such as sequence numbers, topics, counts, limits and ROS domains. Feature gates
on backend-specific variants follow the backend, not the enabled log destination.

The new event modules have local `cfg(test)` capture modules, following the pilot.
Host tests of `no_std` crates import `std` only inside those modules. Tests can
assert payloads or collect owned projections. Each separately spawned future or
OS thread must explicitly install its own observer; ambient capture does not
propagate across those boundaries. This keeps test concurrency independent of
runtime choice. Capture is absent from production and does not require logging.

### Destination compatibility

Library reporters use the core `log_*!` facade. Persistence, SQLite and WebSocket
now forward their `tracing` feature to core and also offer `log`. A destination
is selected by the reporting crate's feature, not just a feature on a transitive
dependency. Standard-library-only crates recognize the facade's `defmt` cfg but
do not advertise a defmt backend. Existing initialization/debug messages are
outside this migration and retain their previous destinations.

Embassy adds `log` and `defmt` features. Its defaults are now `alloc,defmt`;
`defmt` forwards to core, so the default enables core defmt reporting too.
Previously the adapter's subscriber-exhaustion diagnostic was unconditional.
With `--no-default-features`, enable `defmt`, `log` or `tracing` explicitly to
receive that operational event as a log. The generic `RuntimeOps::log` bridge
still uses its existing defmt implementation and dependency.

Persistence cleanup retains its legacy stderr fallback when no facade
destination is enabled. The reporter owns that fallback; enabling `log` avoids
a duplicate stderr fallback. MCP retains its existing tracing subscriber and
stderr writer. Its transport and JSON-RPC stdout remain unchanged.

All migrated sites use the targets in the table instead of their original
source-module targets. Module-prefix filters for the whole crate continue to
work. Per-module filters must be updated. Severity, message text, emission
frequency, retry timing, counters and returned errors remain unchanged.

### Audit boundaries

Every existing production warning/error in the owners above is an operational
event, except the generic runtime log forwarding entrypoints. Existing silent
branches stay silent. Data-loss, failure and terminal lifecycle messages are
modeled without converting every informational message into an event.

The following remain ordinary logging: builder/connector initialization,
subscription setup counts, transform construction/startup, successful remote
writes, WebSocket route collection, MCP configuration/startup banners, and all
debug/trace diagnostics. ROS codec and domain warnings are separate typed events.

No local event enum is needed in TCP, UDS, serial, the client, Tokio/Wasm,
serialization/contracts/derive/codegen, CLI, or examples: their remaining logs
are initialization, generic forwarding, user-facing output, or generated text.
Transport/session operations already reported by core are not reported again.
