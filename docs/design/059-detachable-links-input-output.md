# 059 — Detachable link builders and `Input` / `Output`

**Status:** 📝 Proposed

**Scope:** [#389](https://github.com/aimdb-dev/aimdb/issues/389), step 1 of
[#388](https://github.com/aimdb-dev/aimdb/issues/388). One link builder per
direction in `aimdb-core`, generic over what it is attached to; a detached
form that can be built before any registrar exists; link validation moved
into `RecordRegistrar`; the `Input<T>` / `Output<T>` traits with
`RecordRegistrar::input` / `output`; and the MQTT, Zenoh and
`LinkCodecBuilderExt` extension traits written once for both forms.
Not breaking: existing `link_to(..)…finish()` code and existing extension
impls keep compiling.

**Out of scope** (later sub-issues of #388): `AimDbBuilder::connect` and
the `Connection` handle, `publish` / `subscribe` in `aimdb-data-contracts`,
migrating the demos, and the device-input conventions (`From<Event>`, owned
channels) in the usage guide.

**Reference:** spike branch `throwaway/detached-links` (fad6437, on
`aff220a`). Reference only; the implementation is a clean rewrite.

---

## 1. Problem

`link_to()` and `link_from()` return builders that hold the registrar:

```rust
pub struct OutboundConnectorBuilder<'r, 'a, T> {
    registrar: &'r mut RecordRegistrar<'a, T>,
    url: String,
    config: Vec<(String, String)>,
    /* serializer, serializer_into, topic_writer */
}
```

A link therefore cannot exist before its record's registrar does. #388 wants
to write a link as a value first and attach it later:

```rust
let out = publish(mqtt, "sensors/temp/indoor").qos(1).retain(false);
// … later, inside configure():
reg.output(out);
```

With today's types, `publish(..)` cannot reuse `OutboundConnectorBuilder`,
so it cannot reuse the option methods on it (`with_config`,
`with_serializer`, `with_topic_fn`, …) or the connector extension traits
implemented for it (`MqttLinkExt`, `MqttOutboundLinkExt`, `Ros2LinkExt`,
`LinkCodecBuilderExt`). The #388 prototype duplicated every option as a
result. Every new option would have to be written twice from then on, and
the two copies could drift.

A second, smaller problem: the checks a link goes through (URL parses, codec
present, scheme registered, key capture exists, …) live in the two
`finish()` methods, which only the attached builders have. A second way to
register links would need a second copy of them.

## 2. Goals and non-goals

| # | Goal |
|---|---|
| G1 | A link can be built without a registrar and attached later. |
| G2 | Every option method and every connector extension trait is written once and works on both the attached and the detached form. |
| G3 | One validation path. An attached and a detached link with the same options register the same `ConnectorLink` / `InboundConnectorLink`, or record the same `ConfigError`. |
| G4 | `reg.input(x)` / `reg.output(x)` accept anything implementing `Input<T>` / `Output<T>`; a wrong pairing is E0277 at that call, with a readable message. |
| G5 | No source change for existing users: `link_to(..)…finish()`, `link_from(..)…finish()`, `OutboundConnectorBuilder` / `InboundConnectorBuilder` in signatures and in downstream `impl` blocks. |
| G6 | `no_std + alloc` unchanged; no new dependencies; no new allocation on any path. |

**Non-goals:**

- compile-time single-writer enforcement (see §5; #388 gets that from owned
  device channels, not from `.input()`)
- connection handles and scheme checking at the type level (#388 step 2)
- blanket `Input` / `Output` impls for closures or for `source` / `tap`
- changing what any link option means, or the order options are applied in

## 3. Design

### 3.1 One builder per direction

```rust
/// Marks a link builder that is not attached to a record yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct Detached;

#[must_use = "a link does nothing until it is passed to `.output()` or finished with `.finish()`"]
pub struct OutboundLinkBuilder<T: Send + Sync + 'static + Debug + Clone, R> {
    at: R,
    url: String,
    config: Vec<(String, String)>,
    context_serializer: Option<TypedContextSerializerFn<T>>,
    context_serializer_into: Option<(usize, TypedContextSerializerIntoFn<T>)>,
    topic_writer: Option<TopicWriterCfg<T>>,
}

#[must_use = "a link does nothing until it is passed to `.input()` or finished with `.finish()`"]
pub struct InboundLinkBuilder<T: Send + Sync + 'static + Debug + Clone, R> {
    at: R,
    url: String,
    config: Vec<(String, String)>,
    context_deserializer: Option<TypedContextDeserializerFn<T>>,
    match_deserializer: Option<TypedMatchDeserializerFn<T>>,
    key: Option<(String, u16)>,
    topic_resolver: Option<crate::connector::TopicResolverFn>,
}

/// Attached form: what `link_to()` / `link_from()` return.
pub type OutboundConnectorBuilder<'r, 'a, T> =
    OutboundLinkBuilder<T, &'r mut RecordRegistrar<'a, T>>;
pub type InboundConnectorBuilder<'r, 'a, T> =
    InboundLinkBuilder<T, &'r mut RecordRegistrar<'a, T>>;

/// Detached form: built with `new(url)`, attached by `.output()` / `.input()`.
pub type OutboundLink<T> = OutboundLinkBuilder<T, Detached>;
pub type InboundLink<T> = InboundLinkBuilder<T, Detached>;
```

Notes:

- `R` is unbounded. Fields are private, so the only values of the type are
  the attached form (from `link_to` / `link_from`) and the detached form
  (from `new`). A sealed bound on `R` would add a public-looking trait and
  buy nothing.
- The `T` bounds stay on the struct, as today. Type aliases do not enforce
  bounds (`type_alias_bounds`), so the aliases carry none.
- `Detached` is a zero-sized marker. It is the only new public type besides
  the two builders and the two traits.

### 3.2 Where each method lives

| Method | `impl` block | Notes |
|---|---|---|
| `with_config`, `with_serializer`, `with_serializer_into`, `clear_serializer_into`, `with_timeout_ms`, `with_topic_writer`, `with_topic_fn` | `impl<T, R> OutboundLinkBuilder<T, R>` | unchanged bodies; `Self` is preserved |
| `with_config`, `with_deserializer`, `with_match_deserializer`, `key`, `with_timeout_ms`, `with_topic_resolver` | `impl<T, R> InboundLinkBuilder<T, R>` | unchanged bodies |
| `clear_wire_format` (private) | same generic blocks | |
| `new(url)` | `impl<T> OutboundLink<T>` / `impl<T> InboundLink<T>` | public |
| `attach(self, at: R) -> …Builder<T, R>` (private) | detached form only | moves every field, sets `at` |
| `detach(self) -> (R, …Link<T>)` (private) | any `R` | moves every field, returns `at` separately |
| `finish(self) -> &'r mut RecordRegistrar<'a, T>` | attached form only | `let (reg, link) = self.detach(); reg.push_…(link); reg` |

`link_to(url)` becomes `OutboundLink::new(url).attach(self)`, and
`link_from(url)` becomes `InboundLink::new(url).attach(self)`. There is one
constructor per direction, so a new field is added in one place.

`finish()` stays on the attached form only. A detached link has nothing to
return to; its counterpart is `reg.output(link)` / `reg.input(link)`.

### 3.3 Validation in one place

The bodies of today's two `finish()` methods move into the registrar:

```rust
impl<'a, T> RecordRegistrar<'a, T> {
    /// Validates and registers an outbound link, or records why not.
    fn push_outbound(&mut self, link: OutboundLink<T>) { /* old finish() body */ }

    /// Validates and registers an inbound link, or records why not.
    fn push_inbound(&mut self, mut link: InboundLink<T>) {
        match self.inbound_link(&mut link) {
            Ok(inbound) => self.rec.add_inbound_connector(inbound),
            Err(message) => self.rec.push_config_error(
                ConfigError::new(self.record_key.clone(), Some(link.url), message),
            ),
        }
    }

    /// Old `InboundConnectorBuilder::link()`.
    fn inbound_link(&self, link: &mut InboundLink<T>)
        -> Result<InboundConnectorLink, String> { /* … */ }
}
```

Both stay private (`pub(crate)` at most). Everything a link is checked
against already lives on the registrar (`connector_builders`, `record_key`,
`rec.inbound_connectors()` for the key-capacity check), so nothing else
moves. The checks and their messages are unchanged:

| Direction | Check | Recorded as `ConfigError` |
|---|---|---|
| both | URL parses as a `LinkAddress` | `Invalid connector URL` |
| both | a connector is registered for the scheme | `No connector registered for scheme '…'` |
| outbound | resource has no `{` / `}` | `Outbound topics cannot contain '{' or '}'` |
| outbound | serializer present | `Outbound connector requires a serializer…` |
| inbound | exactly one of `with_deserializer` / `with_match_deserializer` | `…requires a deserializer…` / `…not both` |
| inbound | topic pattern parses | pattern error text |
| inbound | `key(name, cap)`: `cap ≥ 1`, `name` is a capture, capacity matches other keyed links of this record | as today |

As today, errors are recorded at wiring time and reported together by
`build()` as `DbError::InvalidConfiguration`; nothing panics. A detached link
is checked when it is attached, not when it is built: `OutboundLink::new("")`
is a valid value, and `reg.output(it)` records `Invalid connector URL`.
Checking the scheme needs the registrar's connector list, so it cannot move
earlier anyway; keeping every check at one point keeps the rule simple.

Side effects of a successful outbound registration move with the body:
the profiling stage (`push_link()` under `observability`) and
`last_stage = Some((StageKind::Link, idx))`. So `.with_name("…")` after
`.output(link)` names that link, exactly as after `.link_to(..).finish()`.
A rejected link sets no stage, as today.

### 3.4 `Input<T>` and `Output<T>`

```rust
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be the input of a `{T}` record",
    label = "not an input for `{T}`",
    note = "an input implements `Input<{T}>`; device inputs usually do so for every `T: From<Event>`"
)]
pub trait Input<T: Send + Sync + 'static + Debug + Clone> {
    /// Registers this input's writer on the record.
    fn wire(self, reg: &mut RecordRegistrar<'_, T>);
}

#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be an output of a `{T}` record",
    label = "not an output for `{T}`"
)]
pub trait Output<T: Send + Sync + 'static + Debug + Clone> {
    /// Registers this output's reader on the record.
    fn wire(self, reg: &mut RecordRegistrar<'_, T>);
}

impl<'a, T> RecordRegistrar<'a, T> {
    pub fn input(&mut self, input: impl Input<T>) -> &mut Self {
        input.wire(self);
        self
    }
    pub fn output(&mut self, output: impl Output<T>) -> &mut Self {
        output.wire(self);
        self
    }
}

impl<T> Input<T> for InboundLink<T> {
    fn wire(self, reg: &mut RecordRegistrar<'_, T>) { reg.push_inbound(self) }
}
impl<T> Output<T> for OutboundLink<T> {
    fn wire(self, reg: &mut RecordRegistrar<'_, T>) { reg.push_outbound(self) }
}
```

Decisions:

- **`wire` takes `&mut RecordRegistrar`, not a narrower handle.** An input
  may call `.source()`, `.link_from()`, `.buffer()` or anything else a
  `configure` closure can. A narrower trait would need its own copy of those
  methods. The trade-off is that `Input` can do more than produce; that is
  documented, not enforced.
- **`on_unimplemented` sits on the trait definitions.** It has no effect on
  impls (the #388 open question 4 assumed otherwise). `{Self}` prints the
  full type (`OutboundLinkBuilder<Temp, Detached>`), not the alias; a
  `Publish` wrapper in #388 step 2 will read better.
- **The `T` bounds are on the traits** because `RecordRegistrar<'_, T>` in
  the method signature requires them.
- **Only the detached form implements the traits.** Passing an attached
  builder to `.output()` cannot compile anyway (it borrows the registrar
  mutably), and an impl for it would make that error harder to read.
- **`OutboundLink` does not implement `Input`, and vice versa.** Swapping
  directions is E0277 with the messages above.

Target shape, compiling in the spike:

```rust
reg.input(InboundLink::new("mqtt://in").with_link_codec(Json).with_qos(1))
   .output(OutboundLink::new("mqtt://out").with_link_codec(Json).with_qos(1).with_retain(false));
```

A wrong record type:

```
error[E0277]: `OutboundLinkBuilder<Temp, Detached>` cannot be an output of a `Other` record
help: the trait `Output<Other>` is not implemented for `OutboundLinkBuilder<Temp, Detached>`
      but trait `Output<Temp>` is implemented for it
```

### 3.5 Extension traits over any `R`

Each connector extension changes from the attached alias to the generic
type; bodies are unchanged:

```rust
// before
impl<'r, 'a, T> MqttLinkExt for OutboundConnectorBuilder<'r, 'a, T> where T: … { … }
// after
impl<T, R> MqttLinkExt for OutboundLinkBuilder<T, R> where T: … { … }
```

| Crate | File | Traits |
|---|---|---|
| `aimdb-mqtt-connector` | `src/link_ext.rs` | `MqttLinkExt` (out + in), `MqttOutboundLinkExt` (out) |
| `aimdb-zenoh-connector` | `src/link_ext.rs` | `Ros2LinkExt` (out + in) |
| `aimdb-data-contracts` | `src/link_codec.rs` | `LinkCodecBuilderExt<T>` (out + in) |

The `use` lines change to `aimdb_core::{OutboundLinkBuilder, InboundLinkBuilder}`.
The doc comments that name `OutboundConnectorBuilder` (MQTT `link_ext.rs`
module docs, `connector.rs` intra-doc links) are updated to the generic
names so rustdoc links resolve to the type that carries the methods.

A downstream crate with its own `impl X for OutboundConnectorBuilder<'r, 'a, T>`
keeps compiling: that impl is now an impl for the attached instantiation
only. It works on detached links once rewritten for any `R`.

### 3.6 Exports

`aimdb-core/src/lib.rs` re-exports, next to the existing names:

```rust
pub use typed_api::{
    Consumer, Detached, InboundConnectorBuilder, InboundLink, InboundLinkBuilder,
    Input, OutboundConnectorBuilder, OutboundLink, OutboundLinkBuilder, Output,
    Producer, RecordRegistrar, StageKind,
};
```

`aimdb_core::typed_api::*` paths keep working too (`aimdb-data-contracts`
uses them).

## 4. What changes for whom

| Code | Before | After |
|---|---|---|
| `reg.link_to(u)….finish()` | compiles | compiles, same behaviour |
| `fn f(b: OutboundConnectorBuilder<'_, '_, T>)` | compiles | compiles (alias) |
| `impl X for OutboundConnectorBuilder<'r, 'a, T>` downstream | compiles | compiles, attached form only |
| `OutboundConnectorBuilder::with_serializer` in rustdoc | struct page | alias page; methods on `OutboundLinkBuilder` |
| `reg.link_to(u).with_qos(1);` without `.finish()` | silently drops the link | `unused_must_use` warning (§8.1) |
| `reg.output(OutboundLink::new(u)…)` | — | new |

## 5. `.input()` and the single-writer rule

`.input()` adds no new rule; it reuses the existing ones.

- A second `.source()` and a second `.transform()` are recorded at the
  setter (`TypedRecord::set_producer` / `set_transform`).
- Mixing origin kinds (`.source()`, `.transform()`, inbound links) is
  rejected once in `build()` (`builder.rs`, "conflicting writers").

Correction to #389's wording ("a second `.input()` is a `build()` error"):
that holds when the inputs are sources or a source and a link, but **not
for two inbound links**. Several inbound links on one record are fan-in and
are allowed today (`add_inbound_connector`); they count as one origin kind.
`.input()` keeps that:

| Wiring | Result |
|---|---|
| `.input(src_a).input(src_b)` (both call `.source()`) | `build()` error at the second `set_producer` |
| `.input(src).input(InboundLink::new(..))` | `build()` error, conflicting writers |
| `.input(InboundLink::new(a)).input(InboundLink::new(b))` | allowed: fan-in, as two `link_from` |
| `.input(x)` twice with the same owned channel | E0382, from ownership (#388), not from core |

Recommendation: keep fan-in. Rejecting it only through `.input()` would make
`.input(link)` and `.link_from(..).finish()` behave differently, which is
what G3 rules out. If #388 wants "one input per record" for `subscribe`, it
can be a separate decision for both paths.

The "conflicting writers" message names `.link_from()` even when the user
wrote `.input(..)`. It should name the kind instead: `source + inbound link`
(small wording change in `builder.rs`, same check).

## 6. Alternatives considered

- **Two separate builders, attached wrapping detached**
  (`struct OutboundConnectorBuilder { reg, link: OutboundLink<T> }`). Option
  methods would need forwarding on the wrapper, and every extension trait two
  impls again. That is the duplication this change removes.
- **An options trait** (`trait OutboundOptions<T> { fn config_mut(..) }`)
  with blanket extension impls over it. Works, but makes core's field
  layout a public trait contract, and extension crates would program against
  a trait instead of a type. The generic `R` keeps everything private.
- **Detached only; drop `link_to` / `link_from`.** Breaks every user, for
  no gain: the attached form costs one alias and one `finish()`.
- **Validate in the detached constructor.** The scheme check needs the
  registrar's connector list, so validation would be split in two. One point
  of validation, at attachment, is easier to reason about (§3.3).
- **`on_unimplemented` on impls.** Not supported by rustc; the attribute is
  only read on trait definitions.

## 7. Implementation plan

All in one PR, in this order so each step compiles:

1. In `typed_api.rs`, rename the structs to `OutboundLinkBuilder<T, R>` /
   `InboundLinkBuilder<T, R>` with `at: R`, add `Detached`, the four aliases,
   `new`, `attach`, `detach`. Move option methods to `impl<T, R>`.
2. Move `finish()` bodies into `RecordRegistrar::push_outbound` /
   `push_inbound` / `inbound_link`; `finish()` becomes detach-and-push.
   Rewrite `link_to` / `link_from` via `new(..).attach(self)`.
3. Add `Input` / `Output`, `RecordRegistrar::input` / `output`, and the two
   impls.
4. Exports in `lib.rs`.
5. Extension traits in MQTT, Zenoh and data-contracts; doc-comment links.
6. Tests (§9), docs (§10), CHANGELOG.

The spike was about +420 / −95 lines, mostly `typed_api.rs` and its tests.

### 7.1 Pitfalls

- **Shadowed `link` in `push_outbound`.** The old body ends with
  `let mut link = ConnectorLink::new(url, route_factory); link.config = self.config;`.
  With the parameter also named `link`, this becomes
  `link.config = link.config` on the *new* value and the user's config is
  dropped; only a dead-code warning hints at it. Name the local `out`.
  Inbound has the same shape (`InboundConnectorLink::new(..)` … `Ok(link)`);
  name it `inbound`. The test in §9 that checks config on the detached path
  guards this.
- **Partial moves.** The error paths use `Some(link.url)`, moving out of
  `link`. That is fine because each is followed by `return`; keep it that
  way rather than cloning.
- **`observability` cfg.** `push_link()` and `last_stage` move together into
  `push_outbound`; build both with and without the feature.
- **`self.registrar` → `self`.** Inside the moved bodies every
  `self.registrar.x` becomes `self.x`, and every `self.x` (link field)
  becomes `link.x`. Doing this by hand rather than by script avoids the
  shadowing above.

## 8. Decisions to confirm in review

### 8.1 `#[must_use]` on the builders

Recommended. A detached link that is never attached, and an attached builder
whose `.finish()` was forgotten, both register nothing today with no
diagnostic. With `#[must_use]` both warn. The only new warnings are for code
that already silently lost a link, but under `-D warnings` they fail the
build; this is called out in the CHANGELOG.

### 8.2 `Clone` for the detached form

`OutboundLink<T>` / `InboundLink<T>` hold only `String`, `Vec` and `Arc`s,
so `Clone` is cheap and would let one configured link be reused for several
records. Not needed for #388 step 1; proposed to leave it out until a caller
needs it, since adding it later is not breaking.

### 8.3 Naming

`OutboundLink` / `InboundLink` for the detached form,
`…LinkBuilder<T, R>` for the generic type, `…ConnectorBuilder` kept as
aliases. Deprecating the `…ConnectorBuilder` aliases is not proposed; they
are the natural name of what `link_to` returns.

## 9. Tests

In `typed_api::tests`, reusing `make_registrar`, `MockBuffer` and
`MockConnectorBuilder`:

| Test | Checks |
|---|---|
| `detached_inbound_link_registers_like_link_from` | same `InboundConnectorLink` (url, config incl. `qos` and wire format, key, resolver present) as the attached path |
| `detached_outbound_links_register_like_link_to` | same `ConnectorLink` (url, **config**), one profiling stage each, `with_name` after `.output(..)` names it |
| `detached_links_record_the_same_errors_as_attached_ones` | invalid URL, missing (de)serializer, both deserializers, unregistered scheme, bad key: same messages via both paths, nothing registered |
| `custom_input_wires_a_source` | a generic `struct Counter; impl<T: From<i32> + …> Input<T> for Counter` calling `reg.source(..)` sets a producer |
| `two_inbound_inputs_fan_in` | two `.input(InboundLink)` register two links and `build()` accepts them (§5) |
| `input_source_and_link_conflict` | `.input(src)` + `.input(InboundLink)` → "conflicting writers" from `build()` |

Compile-fail (trybuild, in `aimdb-data-contracts/tests/compile-fail/`, which
already has the harness and depends on core):

- an `OutboundLink<Temp>` passed to `reg.output` of a `Other` record, and an
  `OutboundLink` passed to `reg.input`; the `.stderr` pins the
  `on_unimplemented` text.

Commands (as `make` runs them):

```sh
cargo test  -p aimdb-core --features "std,tracing,observability"
cargo test  -p aimdb-core --no-default-features --features alloc
cargo build -p aimdb-core --no-default-features --features alloc --target thumbv8m.main-none-eabihf
cargo test  -p aimdb-mqtt-connector --features std
cargo test  -p aimdb-data-contracts --features "std,simulatable,migratable,observable,linkable-json,linkable-postcard,linkable-cdr,ros2"
cargo test  -p aimdb-zenoh-connector --features std
cargo check -p tokio-mqtt-connector-demo
make clippy   # -D warnings
```

Not `--all-features` on core: it pulls in defmt and the host test binary
fails to link (`_defmt_acquire`). `cargo check --workspace` fails on the host
because of cortex-m asm in embedded crates; that is expected.

## 10. Documentation

- Rustdoc on `OutboundLinkBuilder` / `InboundLinkBuilder` explaining the two
  forms, with the target-shape example; on `Input` / `Output` with a minimal
  custom input.
- `docs/aimdb-usage-guide.md`: a short "Links as values" subsection next to
  `link_to` / `link_from`. The `From<Event>` and owned-channel conventions
  wait for #388's later steps.
- `aimdb-core/CHANGELOG.md` (Added: detached links, `Input` / `Output`;
  Changed: builders generic over `R`, `#[must_use]`), and the matching
  entries for `aimdb-mqtt-connector`, `aimdb-zenoh-connector`,
  `aimdb-data-contracts` ("extension traits now also apply to detached
  links"). Global `CHANGELOG.md` gets one line under Unreleased.

## 11. Acceptance criteria (from #389)

- [ ] `OutboundLinkBuilder<T, R>` / `InboundLinkBuilder<T, R>`, `Detached`,
      `OutboundLink<T>` / `InboundLink<T>`, exported from `aimdb-core`.
- [ ] `OutboundConnectorBuilder` / `InboundConnectorBuilder` valid as
      aliases; `link_to` / `link_from` behave as before.
- [ ] `Input<T>` / `Output<T>` with `on_unimplemented`, and
      `RecordRegistrar::input` / `output`.
- [ ] MQTT, Zenoh and `LinkCodecBuilderExt` extension traits for any `R`.
- [ ] Tests from §9.
- [ ] Docs and CHANGELOG entries from §10.
