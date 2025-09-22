<div align="center">
  <img src="assets/logo.png" alt="AimDB Logo" width="300">
</div>

[![Build Status](https://img.shields.io/github/actions/workflow/status/aimdb-dev/aimdb/ci.yml?branch=main)](https://github.com/aimdb-dev/aimdb/actions)
[![Security Audit](https://img.shields.io/github/actions/workflow/status/aimdb-dev/aimdb/security.yml?branch=main&label=security)](https://github.com/aimdb-dev/aimdb/actions)
[![Crates.io](https://img.shields.io/crates/v/aimdb.svg)](https://crates.io/crates/aimdb)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)
[![Docs](https://docs.rs/aimdb/badge.svg)](https://docs.rs/aimdb)
[![Website](https://img.shields.io/badge/website-aimdb.dev-blue.svg)](https://aimdb.dev)

> **One codebase. Any hardware. Always in sync.**

AimDB is an **async, in-memory database** that keeps state and streams **consistent across MCU → edge → cloud** — without internal brokers, glue code or vendor lock-in. If you’ve ever juggled MQTT bridges, SQLite caches and custom sync scripts just to move live data, AimDB is here to simplify your world.

---

## 🚀 Why AimDB Matters  
Modern devices generate massive real-time data, but today’s stacks are fragmented and slow:  
- Multiple brokers/databases/sync layers to keep MCU, edge and cloud in step.  
- Device-specific integrations that make hardware swaps risky.  
- Batch-oriented pipelines that miss millisecond-level insights.  

AimDB collapses these layers into **one lightweight engine**:  
- Lock-free ring buffers + async transforms = **ms-level reactivity (<50 ms)**.  
- Protocol-agnostic bridges (MQTT, DDS, Kafka) = **no vendor lock**.  
- Portable across platforms with **swappable async runtimes**.  

---

## 🧩 High-Level Architecture & Tech Stack  
- **Language**: Rust 🦀 (async/await, `no_std` capable for MCUs)  
- **Runtime**: Supports **Embassy** for embedded async execution or standard Rust runtimes like Tokio/async-std.  
- **Data Core**: In-memory state + lock-free ring buffers + notifiers + async transforms.  
- **Protocols**: MQTT, Kafka, DDS (plug-in bridges).  
- **Platforms**: MCUs, Linux-class edge devices, cloud VMs/containers.  
- **Extras**: Built-in identity & access hooks, metering for future live-data assets.  

---

## 🏃 Quick Start  
Clone, build and run your first live stream in **≤15 minutes**:  

```bash
# 1. Clone the repo
git clone https://github.com/your-org/aimdb.git
cd aimdb

# 2. Build (requires Rust 1.80+ and cargo)
cargo build --release

# 3. Run a demo stream (simulated edge node)
cargo run --example quickstart
```

You should see events syncing between simulated devices and a local edge gateway!  

---

## 🤝 Contributing  
We love contributions! Here’s how to jump in:  
1. Clone the repository.
   ```bash
   git clone https://github.com/aimdb-dev/aimdb.git
   ```
2. Create a feature branch. 
   ```bash
   git checkout -b feature/my-awesome-idea
   ```
3. Follow our Coding Standards (Rustfmt + Clippy; clear commit messages).  
4. Open a Pull Request with a concise description and link any related issues.  
5. Discuss ideas or questions in GitHub Discussions or our chat (see below).  

Bug reports, docs fixes experimental connectors are all welcome — don’t be shy!  

---

## 🛣 Roadmap & Help Wanted  
Early priorities where you can make a huge impact:  
- ✅ **MCU Runtime** – tighten Embassy executor integration and notification system.  
- 🧪 **Connectors** – expand MQTT/Kafka/DDS bridges, add gRPC/WebSocket bridges.  
- 📊 **Observability** – lightweight metrics and health probes.  
- 📚 **Docs & Examples** – more templates, edge-to-cloud demos.  
- 🔐 **Access Control Hooks** – refine built-in identity & metering.  

Check the Issues board for “help wanted” labels or propose your own ideas.  

---

## 🌐 Community  
- � **Discussions**: [GitHub Discussions](https://github.com/aimdb-dev/aimdb/discussions)
- � **Issues**: [Bug Reports & Feature Requests](https://github.com/aimdb-dev/aimdb/issues)
- 📖 **Docs**: [Project Wiki](https://github.com/aimdb-dev/aimdb/wiki)

Your voice shapes AimDB — ask questions, share feedback and showcase what you build.  

---

## ✨ Tagline  
**Let’s build the future of edge intelligence — together!**
