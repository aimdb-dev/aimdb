# AimX Remote-Access: Spawn-Free via Nested `FuturesUnordered`

**Version:** 0.2 (implemented; minor deviations from draft documented below)
**Status:** ✅ Implemented
**Issue:** [#114](https://github.com/aimdb-dev/aimdb/issues/114)
**Predecessor:** [Design 028 — Remove Spawn Trait](028-M13-remove-spawn-trait.md)
**Last Updated:** May 26, 2026
**Milestone:** M13 — Architectural clean-up (follow-up)

## Implementation deviations from draft

- **Select macro.** The draft specified `futures_util::select_biased!`.
  Implementation uses `tokio::select! { biased; }` — same priority
  semantics, no `pin_mut!` required for `!Unpin` futures (notably
  `read_line`), and avoids adding the `async-await` feature flag on
  `futures-util`. Inside `#[cfg(feature = "std")]`-gated code the
  runtime-agnostic argument did not apply.
- **`AimxConfig` bounds shape.** The draft proposed
  `max_connections: Option<usize>`. Implementation kept the existing
  `max_connections: usize` (already in the public builder API; default
  16) and added a new `max_subs_per_connection: usize` (default 32).
  Non-breaking; `max_connections` is now actually enforced by the
  supervisor — previously the field existed but went unread.
- **`async_stream` vs `stream::unfold`.** Picked `futures_util::stream::unfold`
  to avoid the new proc-macro dependency. Behaviour is identical.
- **WS client `WsClientConnectorImpl::connect` return shape.** Implemented
  as `Result<(Self, BoxFuture), String>`, matching the draft.

---

## Table of Contents

- [Summary](#summary)
- [Motivation](#motivation)
- [Current State — Site Inventory](#current-state--site-inventory)
  - [Site 1 — Supervisor](#site-1--supervisor)
  - [Site 2 — Per-subscription event streamer](#site-2--per-subscription-event-streamer)
  - [Site 3 — `subscribe_record_updates` consumer task](#site-3--subscribe_record_updates-consumer-task)
  - [Site 4 — WS client connector](#site-4--ws-client-connector)
- [Proposed Design](#proposed-design)
  - [Pattern](#pattern)
  - [New helper: `stream_record_updates`](#new-helper-stream_record_updates)
  - [Site 1 rewrite — supervisor](#site-1-rewrite--supervisor)
  - [Sites 2 + 3 rewrite — handler](#sites-2--3-rewrite--handler)
  - [Site 4 rewrite — WS client](#site-4-rewrite--ws-client)
- [Cancellation Semantics](#cancellation-semantics)
- [Bounds and Backpressure](#bounds-and-backpressure)
- [Type-System Impact](#type-system-impact)
- [Implementation Plan](#implementation-plan)
- [Testing](#testing)
- [Risks](#risks)
- [Decisions](#decisions)
- [Out of Scope](#out-of-scope)

---

## Summary

Replace every remaining `tokio::spawn` in the AimX remote-access path with a
nested `FuturesUnordered<BoxFuture<'static, ()>>` driven by
`futures_util::select_biased!`. Each dynamic fan-out point — the supervisor's
accept loop, the connection handler's subscription set, the WS client's
read/write/keepalive/reconnect loops — becomes a future that owns its own
unordered set. Cancellation collapses to a single mechanism: dropping the
future.

This does **not** lift `#[cfg(feature = "std")]` on AimX. It removes the
first blocker so that the eventual gate-removal work does not have to
rewrite the concurrency model under time pressure.

---

## Motivation

Design 028 deliberately deferred this work
([028 §Out of Scope](028-M13-remove-spawn-trait.md#out-of-scope)). The
deferral is now redeemed by this design.

### Problem 1 — Three exit paths per subscription

A single AimX subscription today runs across **two** chained
`tokio::spawn` tasks (Sites 2 + 3) joined by an mpsc channel. The subscription
can terminate via three independent mechanisms:

1. `oneshot::Sender<()>::send(())` from `ConnectionState::cancel_all_subscriptions`
   or the `record.unsubscribe` handler — fires the inner select-arm in
   the buffer-reader task at [builder.rs:1425](../../aimdb-core/src/builder.rs#L1425).
2. The buffer-reader task's `value_tx.send(json_val).await.is_err()` exit
   path ([builder.rs:1435](../../aimdb-core/src/builder.rs#L1435)) — fires
   when `value_rx` has been dropped.
3. The forwarder task's `event_tx.send(event).is_err()` exit path
   ([handler.rs:1114](../../aimdb-core/src/remote/handler.rs#L1114)) — fires
   when the connection's event funnel has been dropped.

Race windows between these three paths produce orphaned task pairs (e.g.
the `oneshot` fires and the buffer-reader exits, but the forwarder keeps
draining `value_rx` until it drains empty and `value_tx` drops). The
resulting "task ghost" is brief but real, and is the kind of thing that
becomes a leak once we add features (e.g. metrics that hold per-sub
counters on a task-local).

With nested `FuturesUnordered` there is a single per-subscription future,
and dropping it is the only exit path.

### Problem 2 — Unbounded spawn is silent

Every accepted connection allocates a Tokio task (`supervisor.rs:123`); every
`record.subscribe` allocates another (`handler.rs:1042`); every WS client
reconnect allocates two more (`connector.rs:495,500`). A misbehaving client
that opens N connections × M subscriptions consumes N×M independent Tokio
tasks. The first symptom is OOM.

With nested sets, the cost is the same heap-wise but is **observable** in
the read loop's poll behaviour, so future backpressure work has a place
to attach (`AimxConfig::max_connections`, `max_subs_per_connection`).

### Problem 3 — `subscribe_record_updates` is the wrong shape

[`AimDb::subscribe_record_updates`](../../aimdb-core/src/builder.rs#L1376)
exists only to spawn an internal forwarder task that converts a
`BufferReader` into an mpsc of JSON values. With `Spawn` gone, this is the
last `runtime.spawn`-shaped method on the public-ish `AimDb<R>` surface
(it is `pub fn` but used only by the handler). It hides the cancellation
mechanism inside the implementation and forces every caller to handle
two channels. A `Stream`-returning helper is the right shape.

---

## Current State — Site Inventory

### Site 1 — Supervisor

[`supervisor.rs:110-149`](../../aimdb-core/src/remote/supervisor.rs#L110-L149):
the accept loop wraps `handle_connection(...)` in `tokio::spawn`. The
supervisor future is itself returned from `build_supervisor_future` and
driven by `AimDbRunner`; only the per-connection dispatch is still a spawn.

### Site 2 — Per-subscription event streamer

[`handler.rs:1042`](../../aimdb-core/src/remote/handler.rs#L1042): inside
`handle_record_subscribe`, a `tokio::spawn` runs
`stream_subscription_events(sub_id, value_rx, event_tx)`. That task is
the **second** task in the subscription chain — it reads JSON values from
the mpsc that the buffer-reader task fills (Site 3), wraps each in an
`Event`, and pushes onto the connection's event funnel.

The `JoinHandle` is `std::mem::drop`-ped immediately ([handler.rs:1055](../../aimdb-core/src/remote/handler.rs#L1055));
cancellation flows in via the receiver-dropped exit on `event_tx`.

### Site 3 — `subscribe_record_updates` consumer task

[`builder.rs:1414`](../../aimdb-core/src/builder.rs#L1414): `tokio::spawn`
runs a `tokio::select!` between `cancel_rx` (oneshot) and
`json_reader.recv_json()`. Exits via:

- `cancel_rx` fires (Unsubscribe / connection cleanup).
- `value_tx.send(...).await.is_err()` (downstream dropped).
- `BufferClosed` / generic error from the buffer.

The method returns `(mpsc::Receiver<Value>, oneshot::Sender<()>)`. The
handler stores the `Sender<()>` in `SubscriptionHandle::cancel_tx` and the
`Receiver<Value>` flows into Site 2.

### Site 4 — WS client connector

[`client/connector.rs`](../../aimdb-websocket-connector/src/client/connector.rs)
has **six** `tokio::spawn` call sites, not the two named in #114:

| Line | Task | Where |
|---|---|---|
| 134 | initial write loop | `connect()` |
| 147 | initial read loop | `connect()` |
| 161 | initial keepalive | `connect()` |
| 168 | initial reconnect watcher | `connect()` |
| 495 | reconnect write loop | `run_reconnect_watcher()` |
| 500 | reconnect read loop | `run_reconnect_watcher()` |

The reconnect watcher is the dynamic-fan-out site: on each reconnect it
spawns two fresh tasks (lines 495, 500) and lets the previous ones die
when the underlying `WebSocketStream` halves close. The other four
(lines 134–168) are static one-per-connector spawns that could equally
be returned futures, but were left as `tokio::spawn` because the
`ConnectorBuilder::build()` API at the time of #88 collected only
*outbound publisher* futures, not infrastructure futures.

---

## Proposed Design

### Pattern

Every dynamic fan-out point owns its own
`FuturesUnordered<BoxFuture<'static, ()>>` and drives it with
`futures_util::select_biased!`:

```rust
let mut children: FuturesUnordered<BoxFuture<'static, ()>> = FuturesUnordered::new();
loop {
    futures_util::select_biased! {
        // Outer signal — accept, request-read, reconnect-tick, etc.
        outer = outer_source.fuse() => match outer {
            NewChild(fut) => children.push(fut),
            ...
        },
        // Drain completed children so the set doesn't grow forever
        _ = children.select_next_some() => {}
    }
}
```

`select_next_some()` from `futures-util` returns `Pending` when the set is
empty, avoiding the busy-loop that `next()` would produce. The macro
requires `default-features = false, features = ["async-await"]` on
`futures-util`, which is already enabled by design 028.

`select_biased!` is preferred over plain `select!` so that the outer
signal (accept / read-request / reconnect-need) is always polled first —
without it, a flood of child completions could starve the outer source.

### New helper: `stream_record_updates`

Replace `AimDb::subscribe_record_updates` with a `Stream`-returning helper
that does not own a task:

```rust
// aimdb-core/src/remote/stream.rs (new file)
#[cfg(feature = "std")]
pub(crate) fn stream_record_updates<R>(
    db: &AimDb<R>,
    record_key: &str,
) -> DbResult<impl Stream<Item = serde_json::Value> + Send + 'static>
where
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    let id = db.inner.resolve_str(record_key)
        .ok_or_else(|| DbError::RecordKeyNotFound { key: record_key.into() })?;
    let record = db.inner.storage(id)
        .ok_or_else(|| DbError::InvalidRecordId { id: id.raw() })?;
    let mut json_reader = record.subscribe_json()?;

    Ok(async_stream::stream! {
        loop {
            match json_reader.recv_json().await {
                Ok(v) => yield v,
                Err(DbError::BufferLagged { .. }) => continue, // log + skip
                Err(_) => break, // BufferClosed or fatal
            }
        }
    })
}
```

**Dependency note.** `async_stream` is the cleanest expression; if we
prefer to avoid the extra dep, use `futures_util::stream::unfold` with a
`(reader, ())` state — same shape, no macro.

The helper owns no task and no channel. Cancellation = drop the stream.
Lag handling stays where it is today (skip-and-continue). The
`BufferClosed` and generic-error branches collapse to `break`, since the
caller now sees a `None` from the stream and exits its own loop
naturally.

Visibility: `pub(crate)`. The old `pub fn subscribe_record_updates`
disappears.

### Site 1 rewrite — supervisor

`supervisor.rs` accept loop becomes:

```rust
let supervisor_future: BoxFuture = Box::pin(async move {
    let mut connections: FuturesUnordered<BoxFuture<'static, ()>> =
        FuturesUnordered::new();

    loop {
        futures_util::select_biased! {
            accept_res = listener.accept().fuse() => match accept_res {
                Ok((stream, _addr)) => {
                    if let Some(max) = config.max_connections {
                        if connections.len() >= max {
                            // refuse: close stream with a one-line error
                            drop(stream);
                            continue;
                        }
                    }
                    let db_clone = db.clone();
                    let config_clone = config.clone();
                    connections.push(Box::pin(async move {
                        let _ = crate::remote::handler::handle_connection(
                            db_clone, config_clone, stream,
                        ).await;
                    }));
                }
                Err(_e) => {
                    // log; continue accepting
                }
            },
            _ = connections.select_next_some() => {
                // a connection ended — implicit cleanup, nothing to do
            }
        }
    }
});
```

`select_biased!` polls `accept` first; busy connections never starve the
accept loop. The optional `max_connections` cap is the new bound surface
described in [Bounds and Backpressure](#bounds-and-backpressure).

### Sites 2 + 3 rewrite — handler

The two-task chain (Sites 2 + 3) collapses to a single per-subscription
future. The handler's outer loop today already uses `tokio::select!` to
interleave `stream.read_line` and `event_rx.recv`
([handler.rs:185-248](../../aimdb-core/src/remote/handler.rs#L185-L248)).
We add a third arm to drain a subscription `FuturesUnordered`:

```rust
let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
let mut conn_state = ConnectionState::new(event_tx.clone());
let mut subs: FuturesUnordered<BoxFuture<'static, ()>> = FuturesUnordered::new();

loop {
    let mut line = String::new();
    futures_util::select_biased! {
        // Incoming request line
        read_result = stream.read_line(&mut line).fuse() => match read_result {
            Ok(0) => break, // client closed
            Ok(_) => {
                let response = handle_request(
                    &db, &config, &mut conn_state, &mut subs, request, &event_tx
                ).await;
                if send_response(&mut stream, &response).await.is_err() { break; }
            }
            Err(_) => break,
        },
        // Outgoing event to client
        Some(event) = event_rx.recv().fuse() => {
            if send_event(&mut stream, &event).await.is_err() { break; }
        },
        // Drain finished subscriptions
        _ = subs.select_next_some() => {}
    }
}
// Drop subs: every subscription future is cancelled by being dropped.
drop(subs);
```

`handle_request` is threaded the `&mut subs` set and the `event_tx`
clone, so `handle_record_subscribe` can push a fresh subscription future
instead of spawning. The subscription future is:

```rust
async fn run_subscription(
    stream: impl Stream<Item = serde_json::Value> + Send + 'static,
    subscription_id: String,
    event_tx: mpsc::UnboundedSender<Event>,
    cancel: Arc<AtomicBool>,
) {
    futures_util::pin_mut!(stream);
    let mut sequence: u64 = 1;
    while let Some(json_value) = stream.next().await {
        if cancel.load(Ordering::Relaxed) { break; }

        let duration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let event = Event {
            subscription_id: subscription_id.clone(),
            sequence,
            data: json_value,
            timestamp: format!("{}.{:09}", duration.as_secs(), duration.subsec_nanos()),
            dropped: None,
        };
        if event_tx.send(event).is_err() { break; }
        sequence += 1;
    }
}
```

`ConnectionState::subscriptions` changes from
`HashMap<String, SubscriptionHandle>` to
`HashMap<String, Arc<AtomicBool>>`. `SubscriptionHandle` and the
`oneshot` cancel channel are deleted. `cancel_all_subscriptions` becomes
trivial: on connection exit the whole `FuturesUnordered` is dropped, and
the handler exits naturally. No explicit drain needed.

### Site 4 rewrite — WS client

The WS client connector currently spawns six tasks. The natural model is
one outer future that owns a `FuturesUnordered` and on reconnect drops
the old read/write children and pushes new ones.

`WsClientConnectorImpl::connect()` becomes
`WsClientConnectorImpl::connector_future()` (or returns a tuple
`(impl Connector, BoxFuture)`). The future:

```rust
async fn connector_future(...) {
    let mut tasks: FuturesUnordered<BoxFuture<'static, ()>> = FuturesUnordered::new();

    // Initial read / write / keepalive — pushed once
    tasks.push(Box::pin(Self::run_write_loop(ws_write, write_rx)));
    tasks.push(Box::pin(Self::run_read_loop(ws_read, router, ctx)));
    if let Some(interval) = keepalive_interval {
        tasks.push(Box::pin(Self::run_keepalive(state.clone(), interval)));
    }

    // Reconnect coordinator (an mpsc<NewLoops>) — or a select! arm that
    // periodically checks status and emits new read/write futures.
    let (reconnect_tx, mut reconnect_rx) = mpsc::unbounded_channel::<NewLoops>();
    if auto_reconnect {
        tasks.push(Box::pin(Self::run_reconnect_watcher(
            state.clone(), url, subscribe_topics, router.clone(),
            max_reconnect_attempts, Some(runtime_ctx), reconnect_tx,
        )));
    }

    loop {
        futures_util::select_biased! {
            Some(new_loops) = reconnect_rx.recv().fuse() => {
                tasks.push(Box::pin(Self::run_write_loop(new_loops.write_sink, new_loops.write_rx)));
                tasks.push(Box::pin(Self::run_read_loop(new_loops.read_stream, &router_for_reconnect, Some(&ctx_for_reconnect))));
            },
            _ = tasks.select_next_some() => {
                // a child ended — read/write halves close together on disconnect;
                // reconnect watcher handles the rest
            }
        }
    }
}
```

`run_reconnect_watcher` no longer calls `tokio::spawn`. Instead, when it
completes a fresh `connect_async`, it sends a `NewLoops { write_sink,
read_stream, write_tx }` over `reconnect_tx`. The outer loop pushes the
new futures into `tasks` and the old halves are already dead.

`ConnectorBuilder::build()` returns this future plus the outbound
publisher futures already collected today, all appended to
`AimDbRunner`'s vec.

> **Alternative shape.** Instead of an mpsc, the reconnect watcher can
> itself return a `Stream<Item = NewLoops>`; then the outer loop's
> `select_biased!` has one arm per stream and the loop is purely
> futures-based. Pick during implementation; the mpsc shape is shown
> because it matches today's code most directly.

---

## Cancellation Semantics

| Event | Today | After |
|---|---|---|
| Connection closed by client | RX drops → buffer-reader exits → forwarder drains and exits | Outer handler loop breaks; per-conn `FuturesUnordered` dropped; all subs cancelled in one step |
| `record.unsubscribe` | `oneshot::Sender<()>::send(())` fires inner select-arm | `Arc<AtomicBool>::store(true)`; sub future exits at next `stream.next()` poll |
| Supervisor drop (runner shutdown) | `tokio::spawn`-ed connections orphan; runtime waits for them | Outer `FuturesUnordered` dropped → all connection futures dropped → all sub futures dropped |
| Buffer producer closed | `BufferClosed` error → reader exits → forwarder mpsc-EOFs → exits | Stream yields `None` → sub future exits |

**Unsubscribe delay.** The Arc<AtomicBool> approach yields a delay of up
to one poll cycle: an idle subscription does not see the flag flip until
the next buffered value arrives. For AimX semantics this is acceptable —
`record.unsubscribe` has never been a synchronous contract. Document it
in the AimX protocol docs.

**Why not `tokio::sync::Notify` or oneshot?** Both are tokio-specific
and would re-introduce the dual-path cancellation that this design is
trying to delete. The whole point is one mechanism: drop the future.
`AtomicBool` is the minimum primitive that lets Unsubscribe target a
specific subscription inside the set; if and when AimX un-gates from
`std`, the path is already runtime-agnostic.

---

## Bounds and Backpressure

[`AimxConfig`](../../aimdb-core/src/remote/config.rs) gains two optional
caps:

```rust
pub struct AimxConfig {
    // ... existing fields ...

    /// Maximum concurrent client connections. `None` = unbounded (current behaviour).
    pub max_connections: Option<usize>,

    /// Maximum subscriptions per connection. Today: implicit via
    /// `subscription_queue_size`; this becomes an explicit cap.
    pub max_subs_per_connection: Option<usize>,
}
```

`subscription_queue_size` stays (it controls per-sub channel depth, not
sub count). When `max_connections` is `Some(n)` and reached, the
supervisor refuses new connects by dropping the accepted `UnixStream`
without handshake — that maps to "connection refused" on the client
side. When `max_subs_per_connection` is `Some(n)` and reached, the
handler returns the existing `too_many_subscriptions` response code.

Defaults: both `None` to preserve current behaviour for in-flight
deployments. The acceptance criterion in #114 is "pick one and document";
this design picks **soft caps as opt-in**, documented in CHANGELOG and
the AimX guide.

---

## Type-System Impact

Surface area is small — design 028 already dropped `R: Spawn` everywhere.
The signatures that move:

| Item | Before | After |
|---|---|---|
| `AimDb::subscribe_record_updates` | `pub fn` returning `(mpsc::Receiver, oneshot::Sender)` | **deleted** |
| `stream_record_updates` (new) | — | `pub(crate) fn stream_record_updates<R>(db, key) -> DbResult<impl Stream<…>>` |
| `SubscriptionHandle` (handler.rs) | `{ subscription_id, record_name, cancel_tx }` | **deleted** |
| `ConnectionState::subscriptions` | `HashMap<String, SubscriptionHandle>` | `HashMap<String, Arc<AtomicBool>>` |
| `ConnectionState::cancel_all_subscriptions` | iterates handles, sends to each oneshot | **deleted** — outer drop handles it |
| `stream_subscription_events` | `async fn`, spawned via `tokio::spawn` | **deleted** — folded into `run_subscription` |
| `handle_record_subscribe` | `(&db, &config, &mut conn_state, request_id, params)` | adds `&mut subs: &mut FuturesUnordered<BoxFuture>` and `&event_tx` |
| `handle_request` | dispatches all method calls | threads `&mut subs` + `&event_tx` into the `record.subscribe` arm |

No public `aimdb-core` API changes beyond `subscribe_record_updates`
removal. That method has no in-tree callers besides `handler.rs`
([git grep verified](../../aimdb-core/CHANGELOG.md)), so the breakage
is internal-only.

WS client: `WsClientConnectorImpl::connect` signature changes from
`Result<Self, String>` to `Result<(Self, BoxFuture<'static, ()>), String>`
(or equivalent). This is internal to the connector crate; the
`ConnectorBuilder::build()` surface already returns a Vec of
`BoxFuture`s, which absorbs the new infrastructure future.

---

## Implementation Plan

Each step should pass `make check` before the next begins.

### Step 1 — `stream_record_updates` helper

- Add `aimdb-core/src/remote/stream.rs` with `stream_record_updates`.
- Choose `async_stream` vs hand-rolled `stream::unfold` (see helper section).
- Wire it as `pub(crate)`; do **not** yet delete `subscribe_record_updates`.
- Add a unit test that drives the stream against a fake record and
  asserts values + lag-skip behaviour.

### Step 2 — Handler: collapse subscriptions into one future

- In `handle_connection`, introduce `subs: FuturesUnordered<BoxFuture>`.
- Replace `SubscriptionHandle` with `Arc<AtomicBool>`; update
  `ConnectionState`.
- Convert `tokio::select!` in the connection loop to
  `futures_util::select_biased!` with the third (`subs.select_next_some()`)
  arm.
- Rewrite `handle_record_subscribe` to push `run_subscription(...)` into
  `subs` instead of spawning.
- Delete `stream_subscription_events`.
- Update `handle_record_unsubscribe` to `cancel.store(true, Relaxed)`.
- Delete `cancel_all_subscriptions` (outer drop handles cleanup).

### Step 3 — Supervisor: nested `FuturesUnordered`

- Rewrite `build_supervisor_future` body: replace `tokio::spawn(handle_connection(...))`
  with `connections.push(Box::pin(handle_connection(...)))`.
- Wrap accept loop in `futures_util::select_biased!` with `connections.select_next_some()`
  arm.
- Add optional `max_connections` enforcement.

### Step 4 — Delete `subscribe_record_updates`

- Remove the method body and doc comment from `builder.rs`.
- `git grep subscribe_record_updates` must be empty.

### Step 5 — `AimxConfig` bounds

- Add `max_connections` and `max_subs_per_connection` (both `Option<usize>`,
  default `None`).
- Update config docs and CHANGELOG.

### Step 6 — WS client connector

- Refactor `WsClientConnectorImpl::connect` to return
  `(Self, BoxFuture<'static, ()>)`.
- Move write/read/keepalive/reconnect-watcher spawns into a single
  `connector_future` that owns a `FuturesUnordered`.
- Reconnect watcher emits `NewLoops` over an mpsc; outer loop pushes
  fresh read/write futures on reception.
- Update `WsClientConnectorBuilder` to return the infrastructure future
  in its `Vec<BoxFuture>`.

### Step 7 — Tests

- Existing AimX integration tests must pass unchanged. They live in the
  remote-access demo (`examples/remote-access-demo/`) and any in-tree
  tests under `aimdb-core`. As of this design, the in-tree test
  directory does not exist; coverage relies on the demo binary plus
  unit tests inside each module.
- Add one cancellation-on-drop test: open a subscription, drop the
  client connection, assert (via a fresh subscriber on the same record)
  that the buffer's consumer-side reader is dropped — measurable by
  asserting that consumer count goes back to its pre-subscribe value
  within N polls.

### Step 8 — Docs and CHANGELOG

- Update [028 §Remote supervisor](028-M13-remove-spawn-trait.md#remote-supervisor)
  status: "Bridge state" → "Target state achieved."
- Update [028 §Out of Scope](028-M13-remove-spawn-trait.md#out-of-scope):
  remove the AimX follow-up section.
- Add CHANGELOG entry under "Internal refactors" for both
  `aimdb-core` and `aimdb-websocket-connector`. No user-facing
  semver bump.
- Update AimX protocol docs (if any) noting the one-poll-cycle
  Unsubscribe semantic.

---

## Testing

Beyond the cancellation-on-drop test in Step 7:

- **Fairness.** With M concurrent connections each holding K subscriptions
  pushing values continuously, every connection should observe events
  within a bounded delta. `select_biased!` polls accept-first on the
  supervisor and read-first on the handler, which is the right default;
  `FuturesUnordered` itself is fair within a level. Add a multi-connection
  stress test that asserts the max-to-min event latency ratio stays
  within (e.g.) 10×.
- **Bounds.** Test that `max_connections = Some(n)` refuses connection
  n+1 and accepts again after one drops.
- **Reconnect.** Test that the WS client's reconnect watcher can survive
  N reconnect cycles without leaking futures (assert `tasks.len()` stays
  bounded after settling).

---

## Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| One busy connection starves siblings inside the supervisor's FuturesUnordered | Low | Set is fair within a level; `select_biased!` only biases accept-vs-drain. Fairness test in Step 7. |
| Unsubscribe delay (one-poll-cycle) surfaces as a perceived bug for idle subscriptions | Low | Documented in AimX protocol docs; no client today relies on synchronous Unsubscribe. |
| `select_biased!` adds compile-time / binary-size overhead vs hand-rolled `poll_fn` | Negligible | Already on dep tree via design 028 (`AimDbRunner::run`). |
| Embassy stack overflow when `std` gate eventually drops and supervisor runs in the shared task | Medium (later) | Out of scope here. When the gate-removal PR lands, place the supervisor in its own Embassy task with an explicit `stack_size`. Already noted in [028 §Embassy](028-M13-remove-spawn-trait.md#embassy-nostd--alloc). |
| `async_stream` dep adds proc-macro overhead | Low | Alternative: hand-rolled `stream::unfold` (no new dep, slightly more verbose). Decide during Step 1. |
| Race: `Unsubscribe` arrives between `stream.next()` poll and event delivery | Low | The flag is checked **after** `stream.next().await` returns and **before** `event_tx.send`. The dropped event matches the protocol's "events between Unsubscribe and ack may or may not be delivered" semantic. |

---

## Decisions

1. **Unsubscribe cancellation primitive** → **`Arc<AtomicBool>`.**
   Honours #114's recommendation. Keeps the path runtime-agnostic for
   the eventual `std`-gate lift. One-poll-cycle delay on idle
   subscriptions is acceptable AimX semantics.

2. **`stream_record_updates` visibility** → **`pub(crate)`.**
   The only caller is `handler.rs`. Promoting to `pub` would re-create
   a not-quite-public API; if external callers ever need it we can
   widen later.

3. **WS client (Site 4) in the same PR** → **Yes.**
   Same pattern, same review surface. Splitting would force the
   reviewer to context-switch on the second PR with no new insight.

4. **Connection / subscription bounds** → **Opt-in caps in `AimxConfig`.**
   `max_connections` and `max_subs_per_connection` both default
   `None` (current unbounded behaviour). Operators who care
   set them; #114's acceptance criterion is satisfied either way.

5. **`async_stream` dependency** → **Decide in Step 1.**
   The `unfold` shape is fine; the dep saves ~6 lines. Not load-bearing.

6. **`SubscriptionHandle` retention** → **Delete.**
   `record_name` was only used in tracing; `cancel_tx` is gone.
   Replacing with `Arc<AtomicBool>` directly in the HashMap value
   is one fewer indirection.

7. **WS client reconnect coordination** → **mpsc<NewLoops> from watcher to outer loop.**
   Concrete and matches today's structure. A pure-Stream alternative
   exists but is more invasive; either is acceptable during
   implementation.

---

## Out of Scope

- **Lifting `#[cfg(feature = "std")]` on AimX.** Requires porting the
  Unix-domain-socket transport, serialising security policy without
  `std::path::PathBuf`, and replacing `tokio::io::BufReader`. Tracked
  separately; this design only removes the concurrency-model blocker.
- **Changing the AimX wire protocol.** Unchanged.
- **WS *server* per-connection handlers.** Live in Axum, which spawns
  internally. Same category as Tokio's listener internals — not an
  AimDB-owned spawn site.
- **Removing `R` from `Producer<T, R>` / `Consumer<T, R>`.** Deferred in
  028 ([Alternative C](028-M13-remove-spawn-trait.md#alternatives-considered)),
  still deferred here.
- **MCP / persistence / sync crates.** No spawn calls in their hot paths;
  no work needed.
