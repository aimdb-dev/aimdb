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
