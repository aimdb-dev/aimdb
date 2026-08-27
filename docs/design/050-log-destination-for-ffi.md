# 050 — A log destination an FFI layer can install

**Status:** step 1 of §10 implemented (`aimdb-core` + the `aimdb-sync` mirror).
Steps 2 and 3 are the FFI doors, which live outside this repository.

**Scope:** additive to `aimdb-core` (1.2.0 → 1.3.0). No breaking change, and no
behaviour change at all for a build that does not enable the new feature. The
`tracing` path, the `defmt` path and every existing call site stay exactly as
they are.

**Vocabulary:** the **facade** is the crate-private `log_*` macro set in
[`aimdb-core/src/log.rs`](../../aimdb-core/src/log.rs). A **destination** is
whatever the process installed to receive what the facade emits. Before this
change there was exactly one kind of destination — a global
`tracing::Subscriber` — and that was the whole problem.

---

## 1. Where this sits

AimDB has two FFI doors now: `weather-station-py` (pyo3) and
`weather-station-cpp` (a C ABI). Both had to answer the same question — *how
does a non-Rust application receive aimdb's reporting?* — and both arrived at
the same unhappy answer: add `tracing` **and** `tracing-subscriber` to the
manifest, write a `Layer`, and install the process's global subscriber on the
application's behalf.

That is the trespass the doors otherwise refuse. `weather-station`'s own
`init_tracing` is a Rust convenience precisely because an *extension* deciding
where its host's diagnostics go is not the extension's call. The FFI layers then
do it anyway, because there is no other way in.

It has already produced a real defect. The C++ door has to hold the caller's
`void *user_data` somewhere, since a `tracing::Layer` has nowhere to put it — so
the C++ header keeps a static `SinkHolder`, writes it non-atomically while
aimdb's runtime thread reads it, and (because the C layer beneath is first-wins
while the header is last-wins) a second `init_logging` silently replaces the
first caller's sink while returning `false` to say it did not. Both halves of
that bug exist only because `user_data` cannot travel with the callback.

This design gives the facade a second, ordinary destination, so an FFI layer
hands aimdb a function and a context pointer instead of installing a subscriber.

**What it does not claim.** `log::set_logger` is a process-global, once-only
install, exactly as `tracing`'s is. The win is not that the destination stops
being global; it is that the destination is an ordinary value (so `user_data`
lives inside it), that the first-wins decision is made once in Rust, and that
`tracing-subscriber` leaves the FFI dependency graph. That a `cdylib` links its
own copy of `log`'s statics, and so does not fight a Rust host for them, is a
convenience of the layout rather than a property of the design.

## 2. Current state (verified)

**The facade is four macros over positional format arguments.** `log_debug!`,
`log_info!`, `log_warn!`, `log_error!` take `$s:literal $(, $x:expr)*` and expand
to the matching `tracing` event macro when the `tracing` feature is on, otherwise
to a borrow-and-drop that keeps call sites warning-free. There is no
`log_trace!`.

Two consequences worth stating, because they decide how cheap this change is:

- **No spans and no structured fields are in play.** Every call site is a
  literal plus positional arguments, so a destination that receives
  `(level, target, fmt::Arguments)` loses nothing that exists today.
- **The target is the expansion site's `module_path!()`.** That is where
  `aimdb_core::builder` and `aimdb_core::session::pump` come from in a
  consumer's output, and any new arm must preserve it.

**Volume:** 66 call sites in `aimdb-core`, 10 in `aimdb-sync` — and those two
crates are the only facade users in the workspace. Four other crates
(`aimdb-mqtt-connector`, `aimdb-knx-connector`, `aimdb-uds-connector`,
`aimdb-serial-connector`) report through `tracing::` directly, 24 call sites
between them; see §8.

**The module is private** — `#[macro_use] mod log;`
([`lib.rs:22`](../../aimdb-core/src/lib.rs#L22)) — while the macros are
`#[macro_export]`ed to the crate root. Anything a macro arm names has to be
reachable from the *consumer's* crate, which is why the current arm says
`::tracing::info!` and why every crate using the facade must declare `tracing`
itself and mirror the feature:

```toml
# aimdb-sync/Cargo.toml, aimdb-tokio-adapter, aimdb-mqtt-connector, and five more
tracing = ["dep:tracing", "aimdb-core/tracing"]
```

**The only way to receive an event, before this change:**

| Consumer | What it must do |
|---|---|
| A Rust binary | install a `tracing` subscriber — ordinary and correct |
| An embedded build | `defmt`, via the explicit gates in `router.rs` |
| A Python extension | add `tracing-subscriber`, write a `Layer`, `try_init()` |
| A C/C++ shared library | the same, plus a static of its own for `user_data` |

The last two rows are the finding. `weather-station-cpp` reaches
`tracing_subscriber::registry().with(env_filter).with(CLoggingLayer).try_init()`
from inside a `cdylib` that has been loaded into somebody else's process.

## 3. Decision: emit through the `log` facade as well

An optional `log` feature on `aimdb-core` whose only effect is a second arm in
each macro:

```rust
#[macro_export]
macro_rules! log_info {
    ($s:literal $(, $x:expr)* $(,)?) => {{
        #[cfg(feature = "tracing")]
        ::tracing::info!($s $(, $x)*);
        #[cfg(feature = "log")]
        $crate::__private::log::info!($s $(, $x)*);
        #[cfg(not(any(feature = "tracing", feature = "log")))]
        { let _ = ($( & $x ),*); }
    }};
}
```

with

```rust
#[doc(hidden)]
pub mod __private {
    // `::log`, not `log`: this crate has a private `log` module at its root.
    #[cfg(feature = "log")]
    pub use ::log;
}
```

That is the entire core change: one feature, one re-export, four macro arms.
No new public API, no new `unsafe`, nothing to keep working for the next decade.

**Why `log` rather than a bespoke hook.** `log` already is the thing CR-12 asked
for, built and stabilised:

| What the FFI layer needs | `log` gives it |
|---|---|
| a destination that is a value, not a subscriber written against a framework | `trait Log { fn log(&self, record: &Record) }` |
| somewhere to keep `user_data` | the `Log` impl *is* the context — one leaked `Box` at install |
| first-wins, reported honestly | `set_logger` / `set_boxed_logger` → `Result<(), SetLoggerError>` |
| a cheap global level gate | `set_max_level` / `max_level()` |
| the emitting module | `Record::target()`, defaulted to `module_path!()` |
| no `tracing-subscriber` in the graph | `log` pulls in nothing |
| `no_std` | `log` is `no_std` by default; `std` only adds `set_boxed_logger` |

The alternative — a hand-written `set_log_sink(&'static dyn LogSink)` in
`aimdb-core` — is in §7. It is a worse trade for a crate that already carries
enough public surface.

### 3.1 What the C++ door becomes

```rust
struct CSink { callback: ws_log_callback, user_data: *mut c_void }

// SAFETY: the pointer is opaque to this layer; the C contract requires it to
// outlive the process, and the callback to be callable from any thread.
unsafe impl Send for CSink {}
unsafe impl Sync for CSink {}

impl log::Log for CSink {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool { true }
    fn flush(&self) {}
    fn log(&self, record: &log::Record<'_>) { /* CStrings, then callback(...) */ }
}

// ws_init_logging:
let sink = Box::leak(Box::new(CSink { callback, user_data }));
match log::set_logger(sink) {
    Ok(()) => { log::set_max_level(level); true }
    Err(_)  => false,
}
```

`user_data` now travels with the callback through Rust, which deletes the
defect that motivated this design:

- `detail::SinkHolder`, `detail::sink_holder()` and `detail::sink_trampoline`
  disappear from `weather_station.hpp`. There is no static to race on.
- The first-wins decision lives in one place, in Rust, for every binding —
  so neither the Python door nor the next one can reintroduce it.
- `tracing` and `tracing-subscriber` leave both FFI manifests.

`Record` also carries `file()` and `line()`. The C callback signature is being
designed now and widening it later is a breaking ABI change, so decide then
rather than after.

### 3.2 Feature layout

```toml
# aimdb-core/Cargo.toml
log = ["dep:log"]          # not in `default`

# aimdb-sync/Cargo.toml
log = ["aimdb-core/log"]   # no `dep:log` — see below
```

- **Not default.** 66 call sites on an MCU should not each grow a level check
  because a host FFI layer wanted one.
- **The feature must be mirrored by every facade user; the dependency need not
  be.** The macros are `#[macro_export]`ed, so `#[cfg(feature = "log")]` inside
  them is resolved against the *expanding* crate. `aimdb-core/log` alone leaves
  `aimdb-sync`'s ten call sites unrouted, which is why `aimdb-sync` carries a
  `log` feature of its own. What the `$crate::__private::log` re-export removes
  is the `dep:log` half of the tax that `::tracing` still charges — routing
  `::tracing` through `__private` the same way is worth doing and strictly
  separable from this change.
- **`tracing` and `log` may both be on**, and then both arms run. See §5.1:
  "both on" is not the same as "the process wanted both".
- **`defmt` is untouched.** The explicit `#[cfg(feature = "defmt")]` gates in
  `router.rs` stay exactly where they are.

## 4. The contract a destination must meet

This lives in the crate docs, because it is the part no signature can carry and
the part an FFI layer gets wrong:

1. **It is called from any thread**, aimdb's runtime thread included — the same
   thread every shutdown waits for.
2. **It must not unwind.** For a `cdylib` reached from C or C++ this is
   undefined behaviour, not a crash. The FFI layer catches on both sides.
3. **It must not call back into aimdb on a path that itself logs.** A logger
   that publishes a reading recurses without bound. Calling the *getters* is
   fine and is exercised by the C++ spike, which reenters `ws_station_is_closed`
   and `ws_station_slot` from inside the logging path on the runtime thread,
   including during that station's own shutdown.
4. **It must not block on anything a thread might hold while calling into
   aimdb.** That is the whole lock ordering, and it is what makes rule 3 in the
   C header ("must not call `ws_station_free`") a rule rather than a suggestion.
5. **It cannot be uninstalled.** `set_logger` is once per process by
   construction; the leaked `Box` and whatever it points at must outlive the
   process.

Rule 3 is stricter here than it was under `tracing`: `tracing`'s dispatcher
carries a reentrancy guard, and `log` has none. Under `tracing` a violation cost
a dropped event; under `log` it is an unbounded recursion on the runtime thread.

## 5. Filtering, and the things that get worse

Before this change the C++ door took a filter string in `tracing`'s `EnvFilter`
syntax and got per-target directives (`aimdb_core::builder=debug`) for free,
because `tracing-subscriber` was in the graph. Dropping it means dropping that.

What replaces it, in order of who does the work:

- **`log::set_max_level`** — one relaxed load and a compare, checked before the
  arguments are formatted. Covers the common case ("info, and debug when I am
  chasing something").
- **Per-target directives are the destination's business.** A `Log` impl sees
  `record.target()` and can match a prefix list; the C header already tells C
  callers they filter with `strncmp`, since C has no logger hierarchy. Roughly
  thirty lines in the FFI crate, and it keeps the filtering policy where the
  policy-holder is.

This is a real regression in convenience for the two FFI doors and should be
named as one in their READMEs rather than discovered. It is not a regression for
any Rust consumer, who keeps `tracing` and `EnvFilter` exactly as today.

### 5.1 Duplicate delivery

A process that installed two destinations sees each event once at each — that is
the point. A process that installed *one* can still see an event twice, and
because feature unification can turn `log` on for a workspace without anyone
asking, this is not always a choice the process made. Three bridges cause it:

- `tracing-subscriber`'s `tracing-log` feature, **on by default**:
  `SubscriberInitExt::init`/`try_init` installs a `LogTracer` that converts
  `log` records into `tracing` events. A Rust app that installed only a
  subscriber then sees every aimdb event twice.
- `tracing`'s own optional `log` feature, which emits a `log` record per
  `tracing` event.
- Any `log`→`tracing` bridge the host installs itself.

Documented rather than prevented: making the arms mutually exclusive by
precedence would mean the `log` arm silently vanishing whenever unification
turned `tracing` on, which is the worse failure. An FFI door that wants no
duplicates builds with `--no-default-features --features log` and leaves
`tracing` off entirely.

## 6. What it costs

| Configuration | Per call site |
|---|---|
| feature off (default, including every MCU build) | nothing — the same expansion as before |
| feature on, no logger installed | `log`'s `STATIC_MAX_LEVEL` check and a relaxed `max_level()` load; arguments are borrowed, never formatted |
| feature on, logger installed, below the level | the same two loads |
| feature on, event delivered | one virtual call plus whatever the destination does |

No allocation is added on any path in `aimdb-core`. The FFI layer allocates two
`CString`s per delivered event, as it does today.

Binary size: one `log` crate (no proc macros, no `syn`, no `regex`) against
`tracing-subscriber` leaving both FFI builds — a net reduction where it matters.

One unification hazard worth knowing: `log`'s `release_max_level_*` features are
additive and compile the gate out globally, so any crate in the graph that
enables one silences the facade for everybody.

## 7. Alternative considered: a bespoke `set_log_sink`

```rust
pub trait LogSink: Sync {
    fn log(&self, level: LogLevel, target: &str, args: fmt::Arguments<'_>);
}
pub fn set_log_sink(sink: &'static dyn LogSink) -> Result<(), SinkAlreadySet>;
```

Same shape, and it is what `log::set_logger` *is*. Storing a `&'static dyn` with
no `alloc` and no unstable `ptr::from_raw_parts` means the `log` crate's own
state machine — an `AtomicU8` guarding a cell, with the write published by a
release store — about sixty lines with `unsafe` in them, plus a `LogLevel` enum,
a `SinkAlreadySet` error, a `set_max_level`, and a permanent public API in a
crate at 1.2.0.

`portable-atomic` is already an unconditional dependency
([`Cargo.toml:105`](../../aimdb-core/Cargo.toml#L105)), so the CAS-less-target
problem has a house answer if this route is ever taken.

Take it only if a second facade in `log.rs` is judged worse than sixty lines of
`unsafe` and a new public API. It buys one thing `log` does not: freedom from a
dependency the embedded profile might not want — which the feature gate already
provides.

**Also considered and rejected: a `__private::emit()` function** in place of the
cfg'd macro arm. It would move the feature decision into `aimdb-core` entirely
and delete the mirroring requirement of §3.2. It also means constructing
`fmt::Arguments` at all 76 call sites unconditionally and trusting the inliner to
remove it when both destinations are off — which is exactly the guarantee the
embedded profile is owed. Worth revisiting if the facade ever grows a third
destination.

## 8. Non-goals

- **The four crates that call `tracing::` directly.** `aimdb-mqtt-connector`,
  `aimdb-knx-connector`, `aimdb-uds-connector` and `aimdb-serial-connector`
  report outside the facade, so a `log` destination will not see those 24 call
  sites. Migrating them is ordinary follow-up work and is not this change.
- **Per-`AimDbHandle` routing.** Two stations in one C++ process cannot separate
  their events, which is a genuine limitation. Fixing it means threading a
  context through 76 context-free macro call sites; it is a different design.
- **Replacing `tracing`.** It stays the default and the recommended destination
  for Rust consumers, with spans available to them if the facade ever grows
  them.
- **Structured fields.** The facade has none today; adding them is not this
  change.
- **Changing `defmt`.**

## 9. Acceptance criteria

`log::set_logger` is once per process, so each criterion that installs a logger
owns its own integration-test binary.

1. `aimdb-core` builds and tests unchanged with the feature off, and `log` does
   not appear in `cargo tree` for such a build. *(`make test`, unchanged
   combinations.)*
2. With `--features log` and a logger installed above the emitted level, an
   event costs the two level loads and never reaches `Display::fmt`; above the
   level the same call site formats exactly once.
   *(`tests/log_facade_gate.rs`, via an argument that counts its own
   formatting.)*
3. With a logger installed, an event arrives once, with
   `target() == "aimdb_core::builder"` for a builder event — the same string a
   `tracing` subscriber sees today. *(`tests/log_facade_delivery.rs`.)*
4. `--features "log,tracing"` delivers to both, once each.
   *(the same test, with a counting `tracing::Subscriber` written by hand —
   deliberately not `tracing-subscriber`.)*
5. `--no-default-features --features "alloc,log"` compiles for
   `thumbv7em-none-eabihf`. *(`make test-embedded`.)*
6. A second `set_logger` returns `Err` and the first destination keeps
   receiving. *(`tests/log_facade_first_wins.rs`.)*
7. `aimdb-sync`'s call sites reach the destination, so the mirrored feature of
   §3.2 cannot be dropped unnoticed.
   *(`aimdb-sync/tests/log_facade.rs`, named by `make test`.)*
8. `weather-station-cpp` builds with no `tracing-subscriber` in `cargo tree`,
   `make spike-cpp` stays green, and its sink round still reports `aimdb_core::*`
   targets. *(Out of this repository; step 2 below.)*

## 10. Sequencing

1. **Done.** `aimdb-core`: feature, re-export, four macro arms, crate docs for
   §4/§5.1; the `aimdb-sync` mirror; criteria 1–7. Releasable as 1.3.0.
2. `weather-station-cpp`: `ws_init_logging` forwards to `set_logger`; the header
   loses `SinkHolder` and the trampoline; the prefix filter replaces `EnvFilter`.
   The two defects in the header are deleted rather than fixed.
3. `weather-station-py`: the same, for the `logging` bridge.
4. Optional, separable: route `::tracing` through `$crate::__private` too, and
   drop the mirrored `tracing` feature from the eight crates that carry it.
5. Optional, separable: move the four direct-`tracing::` connectors onto the
   facade, so a `log` destination sees them too.
