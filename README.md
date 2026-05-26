<p align="center">
  <img src="assets/aimdb-logo.svg" alt="AimDB" width="450">
</p>
<p align="center">
    <strong>Distributed by design. Data-driven by default.</strong>
</p>
<p align="center">
<a href="https://github.com/aimdb-dev/aimdb/stargazers/" target="_blank">
    <img src="https://img.shields.io/github/stars/aimdb-dev/aimdb?style=social&label=Star&maxAge=2592000" alt="Stars">
</a>
<a href="https://github.com/aimdb-dev/aimdb/releases" target="_blank">
    <img src="https://img.shields.io/github/v/release/aimdb-dev/aimdb?color=white" alt="Release">
</a>
<a href="https://crates.io/crates/aimdb-core" target="_blank">
    <img src="https://img.shields.io/crates/v/aimdb-core.svg" alt="Crates.io">
</a>
<a href="https://github.com/aimdb-dev/aimdb/actions/workflows/ci.yml" target="_blank">
    <img src="https://img.shields.io/github/actions/workflow/status/aimdb-dev/aimdb/ci.yml?branch=main" alt="Build">
</a>
<a href="LICENSE" target="_blank">
    <img src="https://img.shields.io/badge/license-Apache%202.0-blue.svg" alt="License">
</a>
<a href="https://buttondown.com/aimdb" target="_blank">
    <img src="https://img.shields.io/badge/newsletter-subscribe-1adbc8.svg" alt="Newsletter">
</a>
</p>

AimDB turns data contracts into the architecture. Define your schemas once and deploy them unchanged across microcontrollers, edge gateways, Kubernetes and the browser, with explicit, typed migrations when the contract evolves.

AimDB is not a storage engine. It's a typed data plane where the Rust type *is* the wire format.

[![AimDB Live Demo](assets/demo.gif)](https://aimdb.dev)

> **[See it running](https://aimdb.dev)**  → Live weather stations streaming typed contracts across MCU, edge and cloud.

> **[Ask your AI about it](#ask-your-ai-about-your-running-system)**  → Query the live weather mesh in natural language. No install required.

---

## Why AimDB exists

Distributed systems spend most of their complexity budget translating between layers. IDLs, codegen, serialization, schema registries and glue services. AimDB removes that layer by making **the Rust type the contract**: defined once, compiled unchanged from a `no_std` microcontroller to the browser.

- **One type, every tier.** The same struct compiles for firmware and cloud. No conversion layer between them.
- **The buffer defines how data moves.** No manual queue wiring, no separate transport config.
- **No untyped boundaries.** Capabilities, like streaming, migration, observability and connectors, are unlocked by traits.

[The Next Era of Software Architecture Is Data-First](https://aimdb.dev/blog/data-driven-design)

---

## Quick start

### Run it locally in 5 min

```bash
cargo new my-aimdb-app && cd my-aimdb-app
cargo add aimdb-core aimdb-tokio-adapter
cargo add tokio --features full
```

Drop this into `src/main.rs`:

```rust
use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Temperature {
    pub celsius: f32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(TokioAdapter::new()?);
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<Temperature>("temp.indoor", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .source(|ctx, producer| async move {
                for celsius in [21.0, 22.5, 24.1] {
                    producer.produce(Temperature { celsius }).await.ok();
                    ctx.time().sleep(ctx.time().secs(1)).await;
                }
            })
            .tap(|ctx, consumer| async move {
                let mut reader = consumer.subscribe().unwrap();
                while let Ok(t) = reader.recv().await {
                    ctx.log().info(&format!("temp: {:.1}°C", t.celsius));
                }
            })
            .finish();
    });

    // `.run()` builds the database, collects every producer/consumer/transform
    // future, and drives them all on a single `FuturesUnordered`. It blocks
    // until shutdown. For programmatic access to the `AimDb` handle, call
    // `.build().await?` directly — it returns `(AimDb, AimDbRunner)`.
    builder.run().await?;
    Ok(())
}
```

`cargo run` — three temperature readings stream through a typed pipeline. The same code compiles for Embassy on a Cortex-M4 or WASM in the browser by swapping the runtime adapter.

### Run a real weather mesh in less than 30 min

A full MCU → edge → cloud mesh: three weather stations, MQTT broker and a central hub.

```bash
git clone https://github.com/aimdb-dev/aimdb
cd aimdb/examples/weather-mesh-demo
docker compose up
```

[Walkthrough in the docs](https://aimdb.dev/docs/getting-started)

---

## What you get

**Three buffer primitives** that cover most data-movement patterns:

| Buffer | Semantics | Use cases |
| --- | --- | --- |
| **SPMC Ring** | Bounded stream with independent consumers | Sensor telemetry, event logs |
| [**SingleLatest**](examples/hello-single-latest-async) | Only the current value matters | Feature flags, config, UI state |
| [**Mailbox**](examples/hello-mailbox) | Latest instruction wins | Device commands, actuation, RPC |

**Four capability traits** — opt-in, type-checked:

| Trait | What it unlocks |
| --- | --- |
| [`Streamable`](https://aimdb.dev/blog/streamable-crossing-boundaries) | Crossing WASM / WebSocket / CLI boundaries |
| [`Migratable`](https://aimdb.dev/blog/schema-migration-without-ceremony) | Typed schema evolution across deployed fleets |
| `Observable` | Automatic per-record metrics |
| [`Linkable`](https://aimdb.dev/blog/connectors-where-aimdb-meets-the-real-world) | Wire-format connectors |

**One async API across runtimes.** Tokio, Embassy, WASM — swap the runtime adapter, keep the code. → [How the runtime abstraction works](https://aimdb.dev/blog/building-aimdb-one-async-api)

**Connectors that ship today:** MQTT, KNX, WebSocket. Writing your own is one trait impl.

Deep dives: [data contracts](https://aimdb.dev/blog/data-contracts-deep-dive) · [source/tap/transform](https://aimdb.dev/blog/source-tap-transform) · [schema migration](https://aimdb.dev/blog/schema-migration-without-ceremony) · [reactive pipelines](https://aimdb.dev/blog/reactive-pipelines)

---

### How the dataflow fits together

A record is written by a `Source`, lands in a typed `Buffer` and fans out to in-process subscribers (`Tap`) and wire-format bridges (`Link` → connector):

```
   Producer                        Consumers
   ────────                        ─────────

                ┌──────────────┐ ───►  Tap   (in-process subscriber)
   Source  ───► │   Buffer     │
   (typed)      │ SPMC / SL /  │ ───►  Tap   (another subscriber)
                │   Mailbox    │
                └──────────────┘ ───►  Link ──► MQTT / KNX / WebSocket
```

The Rust type system enforces correctness at compile time, buffer semantics enforce flow guarantees at runtime and connectors wire to your infrastructure without an integration layer. The same code compiles for MCU, edge, cloud or browser — see [Platform Support](#platform-support) below.

---

## Ask your AI about your running system

AimDB ships an MCP server. Point any MCP-compatible client at a running instance and query it in natural language.

<img src="assets/copilot-communication.gif" alt="AimDB MCP demo" width="600">

Try it against the live demo — no install required. Add this to your workspace:

`.vscode/mcp.json`:

```json
{
  "servers": {
    "aimdb-weather": {
      "type": "http",
      "url": "http://aimdb.dev/mcp"
    }
  }
}
```

Then ask: *"What's the current temperature in Munich?"*

See the [MCP server docs](tools/aimdb-mcp/) for Claude Desktop and other editors or read the deep dive: [AI-Assisted System Introspection: AimDB Meets the Model Context Protocol](https://aimdb.dev/blog/ai-introspection-with-mcp).

---

## Learn more

- [Quick Start Guide](https://aimdb.dev/docs/getting-started) — dependencies, platform setup, your first contract
- [API reference (docs.rs)](https://docs.rs/aimdb-core) — full Rust API
- [Blog](https://aimdb.dev/blog) — design notes, deep dives, release write-ups
- [Live demo](https://aimdb.dev) — running sensor mesh

---

### Connectors

| Protocol | Status | Runtimes |
|----------|--------|----------|
| **MQTT** — `aimdb-mqtt-connector` | ✅ Ready | std, no_std |
| **KNX** — `aimdb-knx-connector` | ✅ Ready | std, no_std |
| **WebSocket** — `aimdb-websocket-connector` | ✅ Ready | std, wasm |
| **Kafka** | 📋 Planned | std |
| **Modbus** | 📋 Planned | std, no_std |

---

### Platform Support

| Target | Runtime | Adapter | Features | Footprint |
|--------|---------|---------|----------|-----------|
| **ARM Cortex-M** (STM32H5, STM32F4) | Embassy | `aimdb-embassy-adapter` | no_std, async | ~50KB+ |
| **Linux Edge** (RPi, gateways) | Tokio | `aimdb-tokio-adapter` | Full std | ~10MB+ |
| **Containers / K8s** | Tokio | `aimdb-tokio-adapter` | Full std | ~10MB+ |
| **Browser / SPA** | WASM | `aimdb-wasm-adapter` | wasm32, single-threaded | ~2MB+ |

---

## Help wanted

We're a small team building something ambitious. The fastest way to help is to take on a scoped piece of it. Each of these is sized for a few hours and includes file pointers, acceptance criteria and a place to ask questions:

- [#92 — `no_std` `Display` for `DbError` should include numeric fields](https://github.com/aimdb-dev/aimdb/issues/92) · 2–3h · core · embedded
- [#93 — Minimal example: `hello-single-latest`](https://github.com/aimdb-dev/aimdb/issues/93) · 2–3h · docs
- [#95 — CLI: add `aimdb instance ping` subcommand](https://github.com/aimdb-dev/aimdb/issues/95) · 3–4h · cli
- [#96 — CI: fail on broken rustdoc links](https://github.com/aimdb-dev/aimdb/issues/96) · 1–2h · docs
- [#97 — Doctests for `BufferCfg` variants](https://github.com/aimdb-dev/aimdb/issues/97) · 2–3h · core · docs
- [#99 — Async example: `hello-mailbox-async`](https://github.com/aimdb-dev/aimdb/issues/99) · 2–3h · docs
- [#100 — Async example: `hello-single-latest-async`](https://github.com/aimdb-dev/aimdb/issues/100) · 2–3h · docs
- [#101 — Async example: `hello-spmc-ring-async`](https://github.com/aimdb-dev/aimdb/issues/101) · 2–3h · docs

[See all good first issues →](https://github.com/aimdb-dev/aimdb/labels/good%20first%20issue)

Comment on an issue if you'd like to take it — we respond within a day. New ideas welcome on [Discussions](https://github.com/aimdb-dev/aimdb/discussions).

---

## Contributing

Found a bug or want a feature? Open a [GitHub issue](https://github.com/aimdb-dev/aimdb/issues).

Have questions or ideas? Join the discussion on [GitHub Discussions](https://github.com/aimdb-dev/aimdb/discussions).

See the [contributing guide](CONTRIBUTING.md) for build, test and style requirements.

---

## License

[Apache 2.0](LICENSE)

---

<p align="center">
  <strong>Distributed by design. Data-driven by default.</strong>
  <br><br>
  <a href="https://aimdb.dev/docs/getting-started">Get started</a> · <a href="https://aimdb.dev">Live demo</a> · <a href="https://github.com/aimdb-dev/aimdb/discussions">Join the discussion</a>
</p>
