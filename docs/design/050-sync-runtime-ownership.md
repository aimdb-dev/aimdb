# 050 — `aimdb-sync`: one runtime, one invariant

**Status:** **implemented.** §6's open question is decided — `Weak`, not `Arc`
— and the reasoning below is amended where building it proved the note wrong.
Written against the tip of the #230 → #232 → #233 stack, which has merged.

**Scope:** internal restructuring of `aimdb-sync`. No change to `aimdb-core`, no
change to the blocking API's shape (`attach` / `producer` / `consumer` / `set` /
`get` / `detach` all keep their signatures). One deliberate semantic decision is
open — §6 — and it is the reason this is a design note rather than a patch.

**Vocabulary:** the **runtime thread** is the OS thread `aimdb-sync` spawns to
own a Tokio runtime on the caller's behalf. A **stamp** is the fork generation
recorded when an object is created, compared later to decide whether the object
predates a `fork()`.

---

## 1. Where this sits

`aimdb-sync` exists so an FFI or legacy caller gets a blocking API from a plain
`fn main()` with no `#[tokio::main]`. To do that it spawns and owns a runtime
thread the caller never sees. That decision is sound and is not in question
here — owning the thread is precisely why the crate, and not its callers, is the
layer that can notice the thread has gone.

What follows is about how that owned thread is *represented*.

## 2. What prompted this

Four defects were found in `aimdb-sync` in a single review pass. They look
unrelated. They are the same defect.

| # | Defect | Found by |
|---|---|---|
| 1 | `SyncConsumer`'s fork guard shipped with no test covering it | reading, not a failing test |
| 2 | `SyncError::ForkedChild` absent from `kind()`'s test and from the `lib.rs` variant list | reading; survived a PR that rewrote that exact test |
| 3 | `thread_alive` added to `AimDbHandle`, and both fork guards silently forgot to release it | reading |
| 4 | `set_value` / `try_set_value` / `set_value_at` are guarded only because they happen to delegate to `set` / `try_set` | reading; nothing enforces it |

None was caught by a test, because in each case the code compiles, runs and
passes. Each is an instance of *someone had to remember something, and the
compiler could not help.*

## 3. The two structural causes

### 3.1 The invariant is enforced by convention, not construction

"Is this runtime still usable after a `fork()`?" is **one fact**. Today it is
stored in three places and checked in nine:

| | count | where |
|---|---|---|
| types carrying a copied `made_in` stamp | 3 | [`AimDbHandle`](../../aimdb-sync/src/handle.rs#L112), [`SyncProducer`](../../aimdb-sync/src/producer.rs#L39), [`SyncConsumer`](../../aimdb-sync/src/consumer.rs#L55) |
| hand-written `check_fork` implementations | 3 | `handle.rs`, `producer.rs`, `consumer.rs` |
| call sites that must *remember* to call it | 9 | 2 + 2 + 5 |
| public methods on `SyncProducer` / `SyncConsumer` | 5 + 5 | every one a chance to forget |

The stamp is *copied per object* rather than being a property of the shared
runtime. Adding a tenth public method reintroduces the bug for free, and nothing
in the type system objects. Defects 1, 2 and 4 above are all this.

This is the shape the repo already rejected once. #231's commit message reads
*"make panic-freedom a checked property, not a convention."* The fork guard is
the same class of problem and received the opposite treatment.

### 3.2 The runtime thread is not a value

`AimDbHandle` currently holds five fields, four of which are facets of one
concept:

```rust
pub struct AimDbHandle {
    thread_handle:  Option<JoinHandle<()>>,                 // the thread
    shutdown_tx:    Option<mpsc::Sender<ShutdownSignal>>,   // how to stop it
    runtime_handle: tokio::runtime::Handle,                 // how to enter it
    db:             Arc<AimDb>,
    thread_alive:   Option<Mutex<Receiver<()>>>,            // whether it still runs
    made_in:        crate::fork::Generation,                // whether it is ours
}
```

"The runtime thread" is spelled out four times. Releasing it therefore means
remembering four `take()` calls in two separate guards — which is exactly how
defect 3 happened: #232 added `thread_alive` and the guards were not updated.

The current mitigation, [`release_inherited`](../../aimdb-sync/src/handle.rs#L295),
centralises the *release* but leaves the four fields loose. It is a smaller
version of the same problem, not its removal.

## 4. Proposed shape

Group the thread into one owned value, and make the check unavoidable by making
it the only route to the runtime.

```rust
/// One value, one OS thread. Constructed together, released together.
struct Runtime {
    thread:   JoinHandle<()>,
    shutdown: mpsc::Sender<ShutdownSignal>,
    alive:    mpsc::Receiver<()>,
    handle:   tokio::runtime::Handle,
    made_in:  fork::Generation,
}

impl Runtime {
    /// The only way to reach the Tokio handle. The fork check lives here
    /// because this is the chokepoint every operation must pass through.
    fn enter(&self) -> SyncResult<&tokio::runtime::Handle> {
        if fork::forked_since(self.made_in) {
            return Err(SyncError::ForkedChild);
        }
        Ok(&self.handle)
    }
}

pub struct AimDbHandle {
    rt: Option<Runtime>,   // None once detached
    db: Arc<AimDb>,
}
```

What this buys, concretely against §2:

- **Defects 1 and 4 become unrepresentable.** A producer or consumer method
  cannot publish or read without a `tokio::runtime::Handle`, and the only way to
  get one is `enter()`. A tenth method inherits the guard by construction.
- **Defect 3 becomes unrepresentable.** Releasing the thread is `self.rt.take()`
  — one field, impossible to half-do.
- **Defect 2 is unaffected** and stays a matter of discipline; it is a
  documentation and test-coverage gap, not a structural one.

Note the fork check must remain *before* the `Weak<AimDb>` upgrade, for the
reason #230 documented: in a forked child the upgrade **succeeds**, because the
`Arc` came across with the address space. That is exactly why a child's `set()`
would otherwise return `Ok` into a buffer nobody drains. `enter()` preserves
this ordering naturally — you reach the runtime before you reach the data.

## 5. What it does not change

- The public API's shape. `attach`, `producer`, `consumer`, `set`, `get`,
  `detach`, `detach_timeout` keep their signatures.
- `aimdb-core`. Nothing crosses the crate boundary.
- The `pthread_atfork` mechanism, the generation-counter-not-poison-flag
  reasoning, and the relaxed-atomic hot path — all of which #230 established and
  measured, and none of which this touches. Detection stays as it is; only its
  *plumbing* changes.
- `fork::generation` / `fork::forked_since` remain crate-private (#230 narrowed
  them precisely so this refactor is free to choose a different shape).

## 6. Decided: producers and consumers hold `Weak<Runtime>`

This was the note's open question, and it recommended `Weak` while calling
`Arc` "arguably the better contract" for FFI callers. `Arc` was then tried, and
implementing it settled the question in the opposite direction from that aside.

`Arc` buys exactly one thing: a producer outliving its handle keeps working
rather than failing with `RuntimeShutdown`. It costs two things that `Weak`
gets for free, because both fall out of ownership:

- **Liveness.** With `Weak`, the failed upgrade *is* the check. With `Arc` it
  has to be rebuilt — a flag the runtime thread flips on every exit path.
- **Waking a blocked reader.** Dropping the handle drops the database, which
  closes its buffers, which is what wakes a consumer parked in `get()`.
  `aimdb-core` has no explicit close, so with `Arc` nothing else does: a
  consumer kept the database alive, `detach` closed nothing, and the reader
  parked forever. That took a level-triggered stop channel and a `select`
  around every blocking read to fix.

So the ledger is one behaviour gained against three mechanisms added. And the
behaviour gained is itself questionable: under `Arc` a forgotten producer keeps
an OS thread and a Tokio runtime alive with nobody owning them — the stranded
thread #232 had just removed, reintroduced as a feature.

| | `Weak` (chosen) | `Arc` (tried, reverted) |
|---|---|---|
| producer outliving its handle | `RuntimeShutdown` | keeps working |
| liveness check | free — the failed upgrade | explicit flag |
| waking a blocked reader | free — buffers close | stop channel + select per read |
| forgotten producer | harmless | strands a thread and a runtime |
| behaviour vs. today | identical | changed |

`Weak` also keeps the whole change reviewable as "no behaviour changed", which
is worth more than the aside was.

**One consequence, and how it is contained.** `SyncConsumer` needs a Tokio
handle that outlives the runtime: a `Reader` can still drain what is already
buffered after a `detach`, and delivering that data is behaviour the
characterization tests pin — gating reads on a live `Runtime` broke it.

The first attempt stored a bare `handle` field beside a hand-written `guard()`
that returned `Ok` when the upgrade failed. That is the convention shape this
design exists to remove, rebuilt on one type: five opt-in call sites and a
field any of them could use without checking anything.

It is now a `RuntimeRef` in `runtime.rs`, holding the `Weak` and the handle with
**both fields private to that module**. `consumer.rs` cannot obtain a handle
except through `RuntimeRef::enter`, which checks first. So the blocking reads
are checked by construction, exactly as the publish path is.

## 7. Secondary: the `Drop` / `detach` duality

`AimDbHandle` has two shutdown paths for one resource:
[`detach_internal`](../../aimdb-sync/src/handle.rs#L450), reached from `detach`
and `detach_timeout`, and [`Drop`](../../aimdb-sync/src/handle.rs#L568). One can
report failure; the other cannot.

#232 resolved the acute problem by making `Drop` non-blocking — the right
answer, arrived at as a bug fix rather than as a starting principle. With
`Runtime` as a value the residual duplication collapses naturally: `Drop`
becomes "signal and release", `detach` becomes "signal, release, and wait", and
both are expressed against one field instead of four.

This is worth folding into the same change. It is not worth a separate one.

## 8. Why this is also a testing problem

The fork test suite in `aimdb-sync/tests/` had to fork real processes from a
parent holding a live, freshly started Tokio runtime — the least safe possible
moment, because the child inherits an allocator lock held by threads that did
not survive. Measured on the #230 branch, the suite failed **11 of 60 runs**
before mitigation, and required both a `mem::forget` of inherited state and a
settle period before the fork to reach zero. The settle is a duration, not a
handshake; it is a mitigation, not a guarantee.

That difficulty is downstream of §3.2. Because `AimDbHandle` inseparably *means*
"a spawned OS thread", there is no way to construct the fork condition without
one. With `Runtime` as a value, `made_in` is ordinary data: the refusal paths —
every `SyncProducer` and `SyncConsumer` method, `producer()`, `consumer()`,
`detach()`, `Drop` — become unit-testable against a `Runtime` stamped with a
stale generation. No thread, no fork, no sleep.

The end-to-end fork tests should not all be deleted; **one** genuine
`fork()`-and-assert case is worth keeping, because a unit test cannot prove the
`pthread_atfork` handler is really installed. But it should be one cheap case
instead of five expensive ones, and the watchdog in
`aimdb-sync/tests/fork_child/mod.rs` should stay regardless.

## 9. How it was done

Done after #230, #232 and #233 merged, as a single self-contained change.

The steps, as executed:

1. Introduce `Runtime`, move the four fields and the stamp into it. Internal
   only; no signature changes.
2. Route every runtime access through `enter()`. Delete the three `check_fork`
   implementations and the nine call sites.
3. Point `SyncProducer` / `SyncConsumer` at `Weak<Runtime>` (§6).
4. Collapse `Drop` / `detach_internal` onto the single field (§7).
5. Convert the refusal-path tests from fork tests to unit tests, keeping one
   end-to-end fork case (§8).

Outcome, measured against the merged stack:

| | before | after |
|---|---|---|
| types carrying a copied fork stamp | 3 | 0 |
| hand-written `check_fork` implementations | 3 | 0 |
| call sites that must remember to guard | 9 | 0 |
| fields on `AimDbHandle` | 6 | 2 |
| forking tests | 5, in 2 binaries | 2, in 1 |
| modules | +`runtime.rs`, −`waiter.rs` | |

Two forking tests remain rather than one. `dropping_an_inherited_handle_does_not_panic`
covers a destructor joining a thread this process never had, which panics
inside `std` — that needs a genuinely dead thread and no unit test can supply
one. The other proves the `pthread_atfork` handler is really installed.

## 10. Risks and what this does not fix

- **It is a refactor of working code.** Every defect in §2 is fixed on the
  stack today; the guard is complete as it stands. The argument for this work is
  that the *next* one is free to reappear, not that the crate is broken.
- **`enter()` is only a chokepoint if nothing else exposes the handle.** If a
  future method returns `&tokio::runtime::Handle` or clones it out, the
  guarantee is gone. That constraint needs stating in the type's docs, and it is
  the one thing here still enforced by convention.
- **It does not remove the fork hazard from tests entirely** (§8) — one real
  fork case remains, and it is the expensive one.
- **It does not address `fork` on non-Unix**, where `generation()` is
  permanently `0`. That is correct today and stays correct; it is noted only so
  the next reader does not mistake it for an oversight.

## 11. Settled, and what is left

1. **`Weak` or `Arc`?** Settled as `Weak` — see §6. The note's own aside
   favouring `Arc` did not survive contact with the implementation.
2. **Should `Runtime` be exposed?** It is `pub(crate)`. Keep it private until an
   FFI layer exists and can say what it needs — the reasoning that made
   `fork::generation` crate-private in #230.
3. **Is the end-to-end fork coverage enough?** Two tests, for the two things a
   unit test cannot reach (§9). The watchdog in `tests/fork_child/mod.rs` stays
   regardless: it converts a deadlock from a six-hour CI hang into a bounded
   failure, and that should not depend on how likely the deadlock is.

Still open, unchanged by this work:

- `enter()` is a chokepoint only while nothing else hands out the Tokio handle.
  Nothing does: `Runtime` and `RuntimeRef` keep theirs private to
  `runtime.rs`, so the constraint is enforced by module privacy rather than by
  memory. Adding a `pub(crate)` accessor that returns one would silently undo
  that, which is worth stating because it is the only way back to the old
  shape.
- Two explicit `check()` calls remain, both in `handle.rs`, and both guard a
  real resource — the `JoinHandle`. `detach_internal` and `Drop` use one to
  decide whether to release the thread rather than join one this process never
  had. Joining *is* the action there, so the check is a branch on state, not a
  gate someone could forget.

  `producer()` briefly had a third. It was removed once the question "what does
  it guard?" got a straight answer: nothing. Creating a producer touches neither
  the database nor the runtime — `test_error_propagation` pins that an
  unregistered key still yields a producer, with `set()` reporting the problem.
  A forked child is one more thing `set()` reports, through the `db()` it must
  pass. Keeping the check would have made `fork` the sole exception to this
  crate's own lazy-producer contract, and would have left a category —
  "checks that guard nothing" — for the next one to join.

  `consumer()` does refuse in a child, because subscribing goes through `db()`.
  That asymmetry predates all of this: an unregistered key already fails at
  `consumer()` and not at `producer()`.

- `SyncConsumer::try_get` was briefly the last opt-in check, on the
  argument that it touches no runtime resource so there was nothing to gate.
  That was wrong: it touches the `Reader`, which is precisely the thing to gate.
  The reader now lives in a `Guarded<Reader<T>>` whose value is private to
  `runtime.rs`, so every read reaches it through a checked accessor. The
  consumer has one field and no way to skip the check.
