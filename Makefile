# AimDB Makefile
# Simple automation for common development tasks

.PHONY: help build test clean clean-embedded fmt fmt-check clippy doc all check test-embedded test-wasm wasm wasm-test wasm-test-deps examples deny audit security publish publish-check readme-check codegen-drift check-no-sim check-no-globals check-toolchain-pin
.DEFAULT_GOAL := help

# Separate target dir for embedded checks so an interrupted example build
# (cargo build --target thumbv7em-none-eabihf) cannot leave corrupted .rmeta
# files that break the next cargo check run (E0786).  Clean it with
# `make clean-embedded`.
EMBEDDED_CHECK_TARGET_DIR := target/embedded-check

# Disable incremental compilation to avoid "Stale file handle" linker errors
# on Docker overlay filesystems when many cargo invocations run in sequence
# with different feature sets (as the test/check targets do).
export CARGO_INCREMENTAL := 0

# Colors for output
GREEN := \033[0;32m
YELLOW := \033[0;33m
BLUE := \033[0;34m
RED := \033[0;31m

# Crates that must never appear in aimdb-sync's --no-default-features graph.
# `tokio` is the obvious one; `libc` is here because it defaults to a `std`
# feature, so a target-specific dependency added for a std-only path (the
# pthread_atfork fork detector) silently un-no_std's the crate if it is not
# marked optional and gated behind `std`.
SYNC_NO_STD_FORBIDDEN := tokio|libc
NC := \033[0m # No Color

## Show available commands
help:
	@printf "$(GREEN)AimDB Development Commands$(NC)\n"
	@printf "\n"
	@printf "  $(YELLOW)Core Commands:$(NC)\n"
	@printf "    build         Build all components (std + embedded)\n"
	@printf "    test          Run all tests (std + embedded)\n"
	@printf "    examples      Build all example projects\n"
	@printf "    fmt           Format code\n"
	@printf "    fmt-check     Check code formatting (CI mode)\n"
	@printf "    clippy        Run linter\n"
	@printf "    doc           Generate docs\n"
	@printf "    clean         Clean build artifacts\n"
	@printf "\n"
	@printf "  $(YELLOW)Testing Commands:$(NC)\n"
	@printf "    check                Comprehensive development check (fmt + clippy + all tests)\n"
	@printf "    test-embedded        Test embedded/MCU cross-compilation compatibility\n"
	@printf "    test-wasm            Test WASM cross-compilation compatibility\n"
	@printf "    readme-check         Verify the README quickstart matches its compiled example\n"
	@printf "    codegen-drift        Compile codegen output against the workspace API\n"
	@printf "\n"
	@printf "  $(YELLOW)Security & Quality:$(NC)\n"
	@printf "    deny                 Check dependencies (licenses, advisories, bans)\n"
	@printf "    audit                Audit dependencies for known vulnerabilities\n"
	@printf "    security             Run all security checks (deny + audit)\n"
	@printf "\n"
	@printf "  $(YELLOW)Release Management:$(NC)\n"
	@printf "    publish-check        Test crates.io publish (dry-run, no git commit required)\n"
	@printf "    publish              Publish all crates to crates.io (requires clean git state)\n"
	@printf "\n"
	@printf "  $(YELLOW)WASM Commands:$(NC)\n"
	@printf "    wasm                 Build WASM adapter with wasm-pack\n"
	@printf "    wasm-test            Run WASM tests in headless browser\n"
	@printf "    wasm-test-deps       Install Chrome + matching chromedriver for wasm-test\n"
	@printf "\n"
	@printf "  $(YELLOW)Convenience:$(NC)\n"
	@printf "    all           Build everything\n"

## Core commands
build:
	@printf "$(GREEN)Building AimDB (all valid combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Building aimdb-data-contracts (std)$(NC)\n"
	cargo build --package aimdb-data-contracts --features "std,simulatable,migratable,observable,linkable-json,linkable-postcard"
	@printf "$(YELLOW)  → Building aimdb-data-contracts (no_std)$(NC)\n"
	cargo build --package aimdb-data-contracts --no-default-features --features alloc
	@printf "$(YELLOW)  → Building aimdb-data-contracts (no_std + format-neutral linkable)$(NC)\n"
	cargo build --package aimdb-data-contracts --no-default-features --features alloc,linkable
	@printf "$(YELLOW)  → Building aimdb-data-contracts (no_std + linkable-postcard)$(NC)\n"
	cargo build --package aimdb-data-contracts --no-default-features --features alloc,linkable-postcard
	@printf "$(YELLOW)  → Building aimdb-data-contracts (no_std + linkable-json + migratable)$(NC)\n"
	cargo build --package aimdb-data-contracts --no-default-features --features alloc,linkable-json,migratable
	@printf "$(YELLOW)  → Building aimdb-core (no_std + alloc)$(NC)\n"
	cargo build --package aimdb-core --no-default-features --features alloc
	@printf "$(YELLOW)  → Building aimdb-core (std platform)$(NC)\n"
	cargo build --package aimdb-core --features "std,tracing,observability"
	@printf "$(YELLOW)  → Building aimdb-core (no_std + alloc + observability)$(NC)\n"
	cargo build --package aimdb-core --no-default-features --features "alloc,observability"
	@printf "$(YELLOW)  → Building aimdb-core (no_std + alloc + connector-session contracts)$(NC)\n"
	cargo build --package aimdb-core --no-default-features --features "alloc,connector-session"
	@printf "$(YELLOW)  → Building aimdb-core (std + connector-session engines)$(NC)\n"
	cargo build --package aimdb-core --features "std,connector-session"
	@printf "$(YELLOW)  → Building tokio adapter$(NC)\n"
	cargo build --package aimdb-tokio-adapter --features "tokio-runtime,tracing,observability"
	@printf "$(YELLOW)  → Building tokio adapter (runtime-neutral transports)$(NC)\n"
	cargo build --package aimdb-tokio-adapter --features "net"
	@printf "$(YELLOW)  → Building sync wrapper$(NC)\n"
	cargo build --package aimdb-sync
	@printf "$(YELLOW)  → Building sync wrapper (no_std)$(NC)\n"
	cargo build --package aimdb-sync --no-default-features
	@printf "$(YELLOW)  → Asserting no std-only crates in sync wrapper (no_std)$(NC)\n"
	@out=$$(cargo tree -p aimdb-sync --no-default-features -e features,no-dev 2>&1) || { \
		printf "$(RED)✗ cargo tree failed — refusing to pass vacuously:$(NC)\n"; \
		printf '%s\n' "$$out"; exit 1; \
	}; \
	if printf '%s\n' "$$out" | grep -qiE '$(SYNC_NO_STD_FORBIDDEN)'; then \
		printf "$(RED)✗ a std-only crate leaked into the no_std build$(NC)\n"; \
		printf '%s\n' "$$out" | grep -iE '$(SYNC_NO_STD_FORBIDDEN)'; exit 1; \
	fi
	@printf "$(BLUE)✓ no_std graph is free of $(SYNC_NO_STD_FORBIDDEN)$(NC)\n"
	@printf "$(YELLOW)  → Building codegen library$(NC)\n"
	cargo build --package aimdb-codegen
	@printf "$(YELLOW)  → Building CLI tools$(NC)\n"
	cargo build --package aimdb-cli
	@printf "$(YELLOW)  → Building MCP server$(NC)\n"
	cargo build --package aimdb-mcp
	@printf "$(YELLOW)  → Building persistence backend$(NC)\n"
	cargo build --package aimdb-persistence
	@printf "$(YELLOW)  → Building persistence SQLite backend$(NC)\n"
	cargo build --package aimdb-persistence-sqlite
	@printf "$(YELLOW)  → Building KNX connector$(NC)\n"
	cargo build --package aimdb-knx-connector --features "std,tokio-runtime"
	@printf "$(YELLOW)  → Building WebSocket connector (server + client)$(NC)\n"
	cargo build --package aimdb-websocket-connector --features "server,client"
	@printf "$(YELLOW)  → Building UDS connector$(NC)\n"
	cargo build --package aimdb-uds-connector
	@printf "$(YELLOW)  → Building serial connector (tokio)$(NC)\n"
	cargo build --package aimdb-serial-connector --no-default-features --features "tokio-runtime"
	@printf "$(YELLOW)  → Building TCP connector (tokio)$(NC)\n"
	cargo build --package aimdb-tcp-connector --no-default-features --features "tokio-runtime"
	@printf "$(YELLOW)  → Building WASM adapter$(NC)\n"
	cargo build --package aimdb-wasm-adapter --target wasm32-unknown-unknown --features "wasm-runtime"
	@printf "$(YELLOW)  → Building benchmarking infrastructure (host-only, incl. benches)$(NC)\n"
	cargo build --package aimdb-bench --benches

test:
	@printf "$(GREEN)Running all tests (valid combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Testing aimdb-data-contracts (std)$(NC)\n"
	cargo test --package aimdb-data-contracts --features "std,simulatable,migratable,observable,linkable-json,linkable-postcard"
	@printf "$(YELLOW)  → Testing aimdb-data-contracts (no_std + alloc + format-neutral linkable)$(NC)\n"
	cargo test --package aimdb-data-contracts --no-default-features --features alloc,linkable
	@printf "$(YELLOW)  → Testing aimdb-data-contracts (no_std + alloc + linkable-postcard)$(NC)\n"
	cargo test --package aimdb-data-contracts --no-default-features --features alloc,linkable-postcard
	@printf "$(YELLOW)  → Testing aimdb-data-contracts (no_std + alloc + linkable-json + migratable)$(NC)\n"
	cargo test --package aimdb-data-contracts --no-default-features --features alloc,linkable-json,migratable
	@printf "$(YELLOW)  → Testing aimdb-core (no_std + alloc)$(NC)\n"
	cargo test --package aimdb-core --no-default-features --features alloc
	@printf "$(YELLOW)  → Testing aimdb-core (std platform)$(NC)\n"
	cargo test --package aimdb-core --features "std,tracing"
	@printf "$(YELLOW)  → Testing aimdb-core (std + observability)$(NC)\n"
	cargo test --package aimdb-core --features "std,tracing,observability"
	@printf "$(YELLOW)  → Testing aimdb-core (no_std + alloc + observability)$(NC)\n"
	cargo test --package aimdb-core --no-default-features --features "alloc,observability"
	@printf "$(YELLOW)  → Testing aimdb-core (log destination: gate, first-wins, target)$(NC)\n"
	cargo test --package aimdb-core --features "std,log"
	@printf "$(YELLOW)  → Testing aimdb-core (both destinations: once each)$(NC)\n"
	cargo test --package aimdb-core --features "std,log,tracing" --test log_facade_delivery
	@printf "$(YELLOW)  → Testing aimdb-core (no_std + alloc + remote)$(NC)\n"
	cargo test --package aimdb-core --no-default-features --features "alloc,remote"
	@printf "$(YELLOW)  → Testing aimdb-core remote module$(NC)\n"
	cargo test --package aimdb-core --lib --features "std" remote::
	@printf "$(YELLOW)  → Testing aimdb-core connector-session (contracts object-safety)$(NC)\n"
	cargo test --package aimdb-core --lib --features "std,connector-session" session::
	@printf "$(YELLOW)  → Testing aimdb-core connector-session engines (session_engine)$(NC)\n"
	cargo test --package aimdb-core --features "std,connector-session" --test session_engine
	@printf "$(YELLOW)  → Testing aimdb-client (engine-based AimX client + UDS round-trip)$(NC)\n"
	cargo test --package aimdb-client
	@printf "$(YELLOW)  → Testing aimdb-client (endpoint resolver, serial transport arm)$(NC)\n"
	cargo test --package aimdb-client --no-default-features --features "transport-serial"
	@printf "$(YELLOW)  → Testing aimdb-client (endpoint resolver, TCP transport arm)$(NC)\n"
	cargo test --package aimdb-client --no-default-features --features "transport-tcp"
	@printf "$(YELLOW)  → Testing tokio adapter$(NC)\n"
	cargo test --package aimdb-tokio-adapter --features "tokio-runtime,tracing"
	@printf "$(YELLOW)  → Testing tokio adapter (with observability)$(NC)\n"
	cargo test --package aimdb-tokio-adapter --features "tokio-runtime,tracing,observability"
	@printf "$(YELLOW)  → Testing tokio adapter (runtime-neutral transports)$(NC)\n"
	cargo test --package aimdb-tokio-adapter --features "net"
	@printf "$(YELLOW)  → Testing embassy adapter (host, no executor: buffers, join-queue, connector spine, doctests)$(NC)\n"
	cargo test --package aimdb-embassy-adapter --no-default-features --features "alloc,embassy-sync,embassy-time,connectors"
	@printf "$(YELLOW)  → Testing embassy adapter (host: runtime-neutral transports, UART + UDP over two embassy-net stacks)$(NC)\n"
	cargo test --package aimdb-embassy-adapter --no-default-features --features "alloc,net"
	@printf "$(YELLOW)  → Testing embassy adapter (host: the neutral clock; --lib only, embassy-time's uptime timestamp collides with the test binaries')$(NC)\n"
	cargo test --package aimdb-embassy-adapter --no-default-features --features "alloc,net,embassy-sync,embassy-time" --lib
	@printf "$(YELLOW)  → Testing WASM adapter (host lib: buffer semantics + shared contract suite; browser layer runs via wasm-test)$(NC)\n"
	cargo test --package aimdb-wasm-adapter --no-default-features --lib
	@printf "$(YELLOW)  → Testing WASM adapter (host lib with observability)$(NC)\n"
	cargo test --package aimdb-wasm-adapter --no-default-features --features observability --lib
	@printf "$(YELLOW)  → Testing sync wrapper$(NC)\n"
	cargo test --package aimdb-sync
	@printf "$(YELLOW)  → Testing sync wrapper (no_std)$(NC)\n"
	cargo test --package aimdb-sync --no-default-features
	@printf "$(YELLOW)  → Testing sync wrapper (log destination; guards the mirrored feature)$(NC)\n"
	cargo test --package aimdb-sync --features log --test log_facade
	@printf "$(YELLOW)  → Testing sync wrapper (data-contracts: set_value family)$(NC)\n"
	cargo test --package aimdb-sync --features data-contracts
	@printf "$(YELLOW)  → Testing codegen library$(NC)\n"
	cargo test --package aimdb-codegen
	@printf "$(YELLOW)  → Testing CLI tools$(NC)\n"
	cargo test --package aimdb-cli
	@printf "$(YELLOW)  → Testing MCP server$(NC)\n"
	cargo test --package aimdb-mcp
	@printf "$(YELLOW)  → Testing persistence backend$(NC)\n"
	cargo test --package aimdb-persistence
	@printf "$(YELLOW)  → Testing persistence SQLite backend$(NC)\n"
	cargo test --package aimdb-persistence-sqlite
	@printf "$(YELLOW)  → Testing MQTT connector (tokio, no TLS backend)$(NC)\n"
	cargo test --package aimdb-mqtt-connector --features "std,tokio-runtime"
	@printf "$(YELLOW)  → Testing MQTT connector (tokio + native-tls)$(NC)\n"
	cargo test --package aimdb-mqtt-connector --features "std,tokio-runtime,tokio-native-tls"
	@printf "$(YELLOW)  → Testing MQTT connector (tokio + rustls)$(NC)\n"
	cargo test --package aimdb-mqtt-connector --features "std,tokio-runtime,tokio-rustls"
	@printf "$(YELLOW)  → Testing KNX connector$(NC)\n"
	cargo test --package aimdb-knx-connector --features "std,tokio-runtime"
	@printf "$(YELLOW)  → Testing WebSocket connector (server + client: unit, real-socket e2e, AimDB round-trip)$(NC)\n"
	cargo test --package aimdb-websocket-connector --features "server,client"
	@printf "$(YELLOW)  → Testing WebSocket connector client-only build$(NC)\n"
	cargo test --package aimdb-websocket-connector --no-default-features --features "client" --lib
	@printf "$(YELLOW)  → Testing UDS connector$(NC)\n"
	cargo test --package aimdb-uds-connector
	@printf "$(YELLOW)  → Testing serial connector (tokio: COBS framing + AimX round-trip over a duplex)$(NC)\n"
	cargo test --package aimdb-serial-connector --no-default-features --features "_test-tokio"
	@printf "$(YELLOW)  → Testing serial connector (embassy: COBS framing + client-engine smoke on the EmbassyAdapter clock)$(NC)\n"
	cargo test --package aimdb-serial-connector --no-default-features --features "embassy-runtime"
	@printf "$(YELLOW)  → Testing TCP connector (tokio: length-prefix framing + AimX loopback)$(NC)\n"
	cargo test --package aimdb-tcp-connector --no-default-features --features "_test-tokio"
	@printf "$(YELLOW)  → Testing TCP connector (embassy: socket recycle + concurrent slots + redial over an embassy-net loopback)$(NC)\n"
	cargo test --package aimdb-tcp-connector --no-default-features --features "_test-embassy-loopback" --test embassy_loopback
	@printf "$(YELLOW)  → Testing TCP connector (accept pool over two embassy-net stacks)$(NC)\n"
	cargo test --package aimdb-tcp-connector --no-default-features --features "_test-embassy-loopback" --test accept_pool

fmt:
	@printf "$(GREEN)Formatting code (workspace members only)...$(NC)\n"
	@for pkg in aimdb-derive aimdb-data-contracts aimdb-core aimdb-client aimdb-embassy-adapter aimdb-tokio-adapter aimdb-wasm-adapter aimdb-sync aimdb-persistence aimdb-persistence-sqlite aimdb-mqtt-connector aimdb-knx-connector aimdb-websocket-connector aimdb-uds-connector aimdb-serial-connector aimdb-tcp-connector aimdb-codegen aimdb-cli aimdb-mcp sync-api-demo tokio-mqtt-connector-demo embassy-mqtt-connector-demo tokio-knx-connector-demo embassy-knx-connector-demo embassy-serial-connector-demo embassy-bench-stm32h5 weather-mesh-common weather-hub weather-station-alpha weather-station-beta hello-mailbox hello-mailbox-async hello-single-latest hello-single-latest-async hello-spmc-ring hello-spmc-ring-async aimdb-bench; do \
		printf "$(YELLOW)  → Formatting $$pkg$(NC)\n"; \
		cargo fmt -p $$pkg 2>/dev/null || true; \
	done
	@printf "$(GREEN)✓ Formatting complete!$(NC)\n"

fmt-check:
	@printf "$(GREEN)Checking code formatting (workspace members only)...$(NC)\n"
	@FAILED=0; \
	for pkg in aimdb-derive aimdb-data-contracts aimdb-core aimdb-client aimdb-embassy-adapter aimdb-tokio-adapter aimdb-wasm-adapter aimdb-sync aimdb-persistence aimdb-persistence-sqlite aimdb-mqtt-connector aimdb-knx-connector aimdb-websocket-connector aimdb-uds-connector aimdb-serial-connector aimdb-tcp-connector aimdb-codegen aimdb-cli aimdb-mcp sync-api-demo tokio-mqtt-connector-demo embassy-mqtt-connector-demo tokio-knx-connector-demo embassy-knx-connector-demo embassy-serial-connector-demo embassy-bench-stm32h5 weather-mesh-common weather-hub weather-station-alpha weather-station-beta hello-mailbox hello-mailbox-async hello-single-latest hello-single-latest-async hello-spmc-ring hello-spmc-ring-async aimdb-bench; do \
		printf "$(YELLOW)  → Checking $$pkg$(NC)\n"; \
		if ! cargo fmt -p $$pkg -- --check 2>&1; then \
			printf "$(RED)❌ Formatting check failed for $$pkg$(NC)\n"; \
			FAILED=1; \
		fi; \
	done; \
	if [ $$FAILED -eq 1 ]; then \
		printf "$(RED)✗ Formatting check failed! Run 'make fmt' to fix.$(NC)\n"; \
		exit 1; \
	fi
	@printf "$(GREEN)✓ All packages properly formatted!$(NC)\n"

clippy:
	@printf "$(GREEN)Running clippy (all valid combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Clippy on aimdb-derive$(NC)\n"
	cargo clippy --package aimdb-derive --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-data-contracts (std)$(NC)\n"
	cargo clippy --package aimdb-data-contracts --features "std,simulatable,migratable,observable,linkable-json,linkable-postcard" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-data-contracts (no_std + alloc)$(NC)\n"
	cargo clippy --package aimdb-data-contracts --no-default-features --features alloc -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-data-contracts (no_std + alloc + format-neutral linkable)$(NC)\n"
	cargo clippy --package aimdb-data-contracts --no-default-features --features alloc,linkable --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-data-contracts (no_std + alloc + linkable-postcard)$(NC)\n"
	cargo clippy --package aimdb-data-contracts --no-default-features --features alloc,linkable-postcard --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-data-contracts (no_std + alloc + linkable-json + migratable)$(NC)\n"
	cargo clippy --package aimdb-data-contracts --no-default-features --features alloc,linkable-json,migratable --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (no_std + alloc)$(NC)\n"
	cargo clippy --package aimdb-core --no-default-features --features alloc --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (no_std + alloc + remote)$(NC)\n"
	cargo clippy --package aimdb-core --no-default-features --features "alloc,remote" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (std)$(NC)\n"
	cargo clippy --package aimdb-core --features "std,tracing,observability" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (connector-session contracts, no_std + alloc and std)$(NC)\n"
	cargo clippy --package aimdb-core --no-default-features --features "alloc,connector-session" --all-targets -- -D warnings
	cargo clippy --package aimdb-core --features "std,connector-session" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (log destination, alone and beside tracing)$(NC)\n"
	cargo clippy --package aimdb-core --features "std,log" --all-targets -- -D warnings
	cargo clippy --package aimdb-core --features "std,log,tracing" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on tokio adapter$(NC)\n"
	cargo clippy --package aimdb-tokio-adapter --features "tokio-runtime,tracing,observability" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on tokio adapter (runtime-neutral transports)$(NC)\n"
	cargo clippy --package aimdb-tokio-adapter --features "net" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on embassy adapter$(NC)\n"
	cargo clippy --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --features "embassy-runtime" -- -D warnings
	@printf "$(YELLOW)  → Clippy on embassy adapter with network support$(NC)\n"
	cargo clippy --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --features "embassy-runtime,embassy-net-support" -- -D warnings
	@printf "$(YELLOW)  → Clippy on embassy adapter (runtime-neutral transports, target and host tests)$(NC)\n"
	cargo clippy --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --no-default-features --features "alloc,net,embassy-runtime" -- -D warnings
	cargo clippy --package aimdb-embassy-adapter --no-default-features --features "alloc,net" --all-targets -- -D warnings
	cargo clippy --package aimdb-embassy-adapter --no-default-features --features "alloc,net,embassy-sync,embassy-time" --lib -- -D warnings
	@printf "$(YELLOW)  → Clippy on sync wrapper$(NC)\n"
	cargo clippy --package aimdb-sync --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on sync wrapper (no_std)$(NC)\n"
	cargo clippy --package aimdb-sync --no-default-features --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on sync wrapper (data-contracts)$(NC)\n"
	cargo clippy --package aimdb-sync --features data-contracts --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on sync wrapper (log destination)$(NC)\n"
	cargo clippy --package aimdb-sync --features log --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on client library$(NC)\n"
	cargo clippy --package aimdb-client --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on client library (serial transport arm)$(NC)\n"
	cargo clippy --package aimdb-client --no-default-features --features "transport-serial" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on client library (TCP transport arm)$(NC)\n"
	cargo clippy --package aimdb-client --no-default-features --features "transport-tcp" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on codegen library$(NC)\n"
	cargo clippy --package aimdb-codegen --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on CLI tools$(NC)\n"
	cargo clippy --package aimdb-cli --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on CLI tools (serial transport)$(NC)\n"
	cargo clippy --package aimdb-cli --features "transport-serial" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on CLI tools (TCP transport)$(NC)\n"
	cargo clippy --package aimdb-cli --features "transport-tcp" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on MCP server$(NC)\n"
	cargo clippy --package aimdb-mcp --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on MCP server (serial transport)$(NC)\n"
	cargo clippy --package aimdb-mcp --features "transport-serial" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on persistence backend$(NC)\n"
	cargo clippy --package aimdb-persistence --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on persistence SQLite backend$(NC)\n"
	cargo clippy --package aimdb-persistence-sqlite --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on KNX connector (std)$(NC)\n"
	cargo clippy --package aimdb-knx-connector --features "std,tokio-runtime" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on KNX connector (embassy)$(NC)\n"
	cargo clippy --package aimdb-knx-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime" -- -D warnings
	@printf "$(YELLOW)  → Clippy on MQTT connector (tokio, no TLS backend)$(NC)\n"
	cargo clippy --package aimdb-mqtt-connector --features "std,tokio-runtime" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on MQTT connector (tokio + native-tls)$(NC)\n"
	cargo clippy --package aimdb-mqtt-connector --features "std,tokio-runtime,tokio-native-tls" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on MQTT connector (tokio + rustls)$(NC)\n"
	cargo clippy --package aimdb-mqtt-connector --features "std,tokio-runtime,tokio-rustls" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on MQTT connector (embassy + defmt)$(NC)\n"
	cargo clippy --package aimdb-mqtt-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,defmt" -- -D warnings
	@printf "$(YELLOW)  → Clippy on MQTT connector (embassy + TLS + defmt)$(NC)\n"
	cargo clippy --package aimdb-mqtt-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,embassy-tls,defmt" -- -D warnings
	@printf "$(YELLOW)  → Clippy on KNX connector (embassy + defmt)$(NC)\n"
	cargo clippy --package aimdb-knx-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,defmt" -- -D warnings
	@printf "$(YELLOW)  → Clippy on WebSocket connector$(NC)\n"
	cargo clippy --package aimdb-websocket-connector --features "tokio-runtime,client" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on UDS connector$(NC)\n"
	cargo clippy --package aimdb-uds-connector --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on serial connector (tokio)$(NC)\n"
	cargo clippy --package aimdb-serial-connector --no-default-features --features "_test-tokio" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on serial connector (embassy)$(NC)\n"
	cargo clippy --package aimdb-serial-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime" -- -D warnings
	@printf "$(YELLOW)  → Clippy on serial connector (embassy + defmt)$(NC)\n"
	cargo clippy --package aimdb-serial-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,defmt" -- -D warnings
	@printf "$(YELLOW)  → Clippy on TCP connector (tokio)$(NC)\n"
	cargo clippy --package aimdb-tcp-connector --no-default-features --features "_test-tokio" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on TCP connector (embassy)$(NC)\n"
	cargo clippy --package aimdb-tcp-connector --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime" -- -D warnings
	@printf "$(YELLOW)  → Clippy on TCP connector (embassy + defmt)$(NC)\n"
	cargo clippy --package aimdb-tcp-connector --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,defmt" -- -D warnings
	@printf "$(YELLOW)  → Clippy on TCP connector (embassy-net loopback smoke, host)$(NC)\n"
	cargo clippy --package aimdb-tcp-connector --no-default-features --features "_test-embassy-loopback" --test embassy_loopback -- -D warnings
	@printf "$(YELLOW)  → Clippy on TCP connector (accept pool, host)$(NC)\n"
	cargo clippy --package aimdb-tcp-connector --no-default-features --features "_test-embassy-loopback" --test accept_pool -- -D warnings
	@printf "$(YELLOW)  → Clippy on WASM adapter$(NC)\n"
	cargo clippy --package aimdb-wasm-adapter --target wasm32-unknown-unknown --features "wasm-runtime" -- -D warnings
	@printf "$(YELLOW)  → Clippy on benchmarking infrastructure (host-only, incl. benches)$(NC)\n"
	cargo clippy --package aimdb-bench --all-targets -- -D warnings

# Doc links are public API: one pointing at a private or feature-gated item
# breaks the published page, and nothing else in `check` looks at rustdoc.
doc: export RUSTDOCFLAGS := -D warnings
doc:
	@printf "$(GREEN)Generating dual-platform documentation...$(NC)\n"
	@# Create directory structure
	@mkdir -p target/doc-final/cloud
	@mkdir -p target/doc-final/embedded
	@printf "$(YELLOW)  → Building cloud/edge documentation$(NC)\n"
	cargo doc --package aimdb-data-contracts --features "std,simulatable,migratable,observable,linkable-json,linkable-postcard" --no-deps
	cargo doc --package aimdb-core --features "std,tracing,observability" --no-deps
	cargo doc --package aimdb-tokio-adapter --features "tokio-runtime,tracing,observability,net" --no-deps
	cargo doc --package aimdb-sync --no-deps
	cargo doc --package aimdb-mqtt-connector --features "std,tokio-runtime" --no-deps
	cargo doc --package aimdb-knx-connector --features "std,tokio-runtime" --no-deps
	cargo doc --package aimdb-codegen --no-deps
	cargo doc --package aimdb-cli --no-deps
	cargo doc --package aimdb-mcp --no-deps
	cargo doc --package aimdb-persistence --no-deps
	cargo doc --package aimdb-persistence-sqlite --no-deps
	cargo doc --package aimdb-websocket-connector --features "tokio-runtime" --no-deps
	@cp -r target/doc/* target/doc-final/cloud/
	@printf "$(YELLOW)  → Building embedded documentation$(NC)\n"
	cargo doc --package aimdb-core --no-default-features --features alloc --no-deps
	cargo doc --package aimdb-embassy-adapter --features "embassy-runtime,net" --no-deps
	cargo doc --package aimdb-mqtt-connector --no-default-features --features "embassy-runtime" --no-deps
	cargo doc --package aimdb-knx-connector --no-default-features --features "embassy-runtime" --no-deps
	@cp -r target/doc/* target/doc-final/embedded/
	@printf "$(YELLOW)  → Building WASM/browser documentation$(NC)\n"
	cargo doc --package aimdb-wasm-adapter --target wasm32-unknown-unknown --features "wasm-runtime" --no-deps
	@printf "$(YELLOW)  → Creating main index page$(NC)\n"
	@cp docs/index.html target/doc-final/index.html
	@printf "$(BLUE)Documentation generated at: file://$(PWD)/target/doc-final/index.html$(NC)\n"

clean:
	@printf "$(GREEN)Cleaning...$(NC)\n"
	cargo clean
	@rm -rf $(EMBEDDED_CHECK_TARGET_DIR)

clean-embedded:
	@printf "$(GREEN)Cleaning embedded check artifacts...$(NC)\n"
	@rm -rf $(EMBEDDED_CHECK_TARGET_DIR)
	cargo clean --target thumbv7em-none-eabihf

## Testing commands
test-wasm:
	@printf "$(BLUE)Testing WASM cross-compilation compatibility...$(NC)\n"
	@printf "$(YELLOW)  → Checking aimdb-wasm-adapter on wasm32-unknown-unknown target$(NC)\n"
	cargo check --package aimdb-wasm-adapter --target wasm32-unknown-unknown --features "wasm-runtime"
	@printf "$(GREEN)✓ WASM target compatibility verified!$(NC)\n"

test-embedded:
	@printf "$(BLUE)Testing embedded/MCU cross-compilation compatibility...$(NC)\n"
	@printf "$(YELLOW)  → Checking aimdb-data-contracts (no_std + alloc) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-data-contracts --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features alloc
	@printf "$(YELLOW)  → Checking aimdb-data-contracts (no_std + alloc + format-neutral linkable) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-data-contracts --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features alloc,linkable
	@printf "$(YELLOW)  → Checking aimdb-data-contracts (no_std + alloc + linkable-postcard) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-data-contracts --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features alloc,linkable-postcard
	@printf "$(YELLOW)  → Checking aimdb-data-contracts (no_std + alloc + linkable-json + migratable) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-data-contracts --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features alloc,linkable-json,migratable
	@printf "$(YELLOW)  → Checking weather-mesh-common (no_std migratable, real TemperatureV1ToV2 chain, no direct serde_json dep) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package weather-mesh-common --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features migratable
	@printf "$(YELLOW)  → Checking aimdb-core (no_std minimal) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features alloc
	@printf "$(YELLOW)  → Checking aimdb-core (no_std + alloc + remote) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "alloc,remote"
	@printf "$(YELLOW)  → Checking aimdb-core session engines (no_std + connector-session) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "alloc,connector-session"
	@printf "$(YELLOW)  → Checking aimdb-core AimX codec + dispatch (full no_std AimX server: connector-session + remote) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "alloc,connector-session,remote"
	@printf "$(YELLOW)  → Checking aimdb-core (no_std/embassy) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features alloc
	@printf "$(YELLOW)  → Checking aimdb-core (no_std + log destination) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "alloc,log"
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime"
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter with network support on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,embassy-net-support"
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter with observability on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,observability"
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter connector spine (connector-io) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,connector-io"
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter runtime-neutral transports, with and without the clock, on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "alloc,net,embassy-runtime"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "alloc,net"
	@printf "$(YELLOW)  → Checking aimdb-mqtt-connector (Embassy) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-mqtt-connector --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime"
	@printf "$(YELLOW)  → Checking aimdb-mqtt-connector (Embassy + defmt) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-mqtt-connector --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,defmt"
	@printf "$(YELLOW)  → Checking aimdb-knx-connector (Embassy) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-knx-connector --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime"
	@printf "$(YELLOW)  → Checking aimdb-knx-connector (Embassy + defmt) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-knx-connector --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,defmt"
	@printf "$(YELLOW)  → Checking aimdb-serial-connector (Embassy: full no_std AimX serial client+server) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-serial-connector --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime"
	@printf "$(YELLOW)  → Checking aimdb-serial-connector (Embassy + defmt) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-serial-connector --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,defmt"
	@printf "$(YELLOW)  → Checking aimdb-tcp-connector (Embassy TCP client) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-tcp-connector --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime"
	@printf "$(YELLOW)  → Checking aimdb-tcp-connector (Embassy TCP client + defmt) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-tcp-connector --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,defmt"
	@printf "$(YELLOW)  → Checking aimdb-sync (no_std) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-sync --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features
	@printf "$(YELLOW)  → Checking aimdb-mqtt-connector (Embassy + TLS) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-mqtt-connector --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,embassy-tls"

## Example projects
examples:
	@printf "$(GREEN)Building all example projects...$(NC)\n"
	@printf "$(YELLOW)  → Building sync-api-demo (synchronous API wrapper)$(NC)\n"
	cargo build --package sync-api-demo
	@printf "$(YELLOW)  → Building mqtt-connector-demo-common (shared MQTT demo code, runtime-agnostic)$(NC)\n"
	cargo build --package mqtt-connector-demo-common
	@printf "$(YELLOW)  → Building tokio-mqtt-connector-demo (native, tokio runtime)$(NC)\n"
	cargo build --package tokio-mqtt-connector-demo
	@printf "$(YELLOW)  → Building embassy-mqtt-connector-demo (embedded, embassy runtime)$(NC)\n"
	cargo build --package embassy-mqtt-connector-demo --target thumbv8m.main-none-eabihf
	@printf "$(YELLOW)  → Building knx-connector-demo-common (shared KNX demo code, runtime-agnostic)$(NC)\n"
	cargo build --package knx-connector-demo-common
	@printf "$(YELLOW)  → Building tokio-knx-connector-demo (native, tokio runtime)$(NC)\n"
	cargo build --package tokio-knx-connector-demo
	@printf "$(YELLOW)  → Building embassy-knx-connector-demo (embedded, embassy runtime)$(NC)\n"
	cargo build --package embassy-knx-connector-demo --target thumbv8m.main-none-eabihf
	@printf "$(YELLOW)  → Building embassy-serial-connector-demo (embedded, embassy runtime)$(NC)\n"
	cargo build --package embassy-serial-connector-demo --target thumbv8m.main-none-eabihf
	@printf "$(YELLOW)  → Building embassy-bench-stm32h5 (B3 on-target profiling, embassy runtime)$(NC)\n"
	cargo build --package embassy-bench-stm32h5 --target thumbv8m.main-none-eabihf
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-mesh-common$(NC)\n"
	cargo build --package weather-mesh-common
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-hub (cloud aggregator)$(NC)\n"
	cargo build --package weather-hub
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-station-alpha (edge, real API)$(NC)\n"
	cargo build --package weather-station-alpha
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-station-beta (edge, synthetic)$(NC)\n"
	cargo build --package weather-station-beta
	@printf "$(YELLOW)  → Building weather-station-gamma (embedded, embassy runtime)$(NC)\n"
	cargo build --package weather-station-gamma --target thumbv8m.main-none-eabihf
	@printf "$(YELLOW)  → Building remote-access-demo (AimX server + client)$(NC)\n"
	cargo build --package remote-access-demo
	@printf "$(YELLOW)  → Building hello-mailbox (sync)$(NC)\n"
	cargo build --package hello-mailbox
	@printf "$(YELLOW)  → Building hello-mailbox-async $(NC)\n"
	cargo build --package hello-mailbox-async
	@printf "$(YELLOW)  → Building hello-single-latest$(NC)\n"
	cargo build --package hello-single-latest
	@printf "$(YELLOW)  → Building hello-single-latest-async$(NC)\n"
	cargo build --package hello-single-latest-async
	@printf "$(YELLOW)  → Building hello-spmc-ring$(NC)\n"
	cargo build --package hello-spmc-ring
	@printf "$(YELLOW)  → Building hello-spmc-ring-async$(NC)\n"
	cargo build --package hello-spmc-ring-async
	@printf "$(YELLOW)  → Building readme-quickstart (compiled README example)$(NC)\n"
	cargo build --package readme-quickstart
	@printf "$(GREEN)All examples built successfully!$(NC)\n"

## Security & Quality commands
deny:
	@printf "$(GREEN)Checking dependencies with cargo-deny...$(NC)\n"
	@if ! command -v cargo-deny >/dev/null 2>&1; then \
		printf "$(YELLOW)  ⚠ cargo-deny not found, installing...$(NC)\n"; \
		cargo install cargo-deny --locked; \
	fi
	@printf "$(YELLOW)  → Checking licenses$(NC)\n"
	@printf "$(YELLOW)  → Checking security advisories$(NC)\n"
	@printf "$(YELLOW)  → Checking banned dependencies$(NC)\n"
	@printf "$(YELLOW)  → Checking dependency sources$(NC)\n"
	cargo deny check

audit:
	@printf "$(GREEN)Auditing dependencies for vulnerabilities...$(NC)\n"
	@if ! command -v cargo-audit >/dev/null 2>&1; then \
		printf "$(YELLOW)  ⚠ cargo-audit not found, installing...$(NC)\n"; \
		cargo install cargo-audit --locked; \
	fi
	cargo audit

security: deny audit
	@printf "$(GREEN)All security checks completed!$(NC)\n"
	@printf "$(BLUE)✓ Dependencies verified (licenses, advisories, bans)$(NC)\n"
	@printf "$(BLUE)✓ Known vulnerabilities checked$(NC)\n"

## Release Management commands
publish-check:
	@printf "$(GREEN)Testing crates.io publish readiness...$(NC)\n"
	@printf "$(YELLOW)Note: cargo package requires dependencies to exist on crates.io.$(NC)\n"
	@printf "$(YELLOW)      Only aimdb-derive (no deps) will fully validate before first publish.$(NC)\n"
	@printf "$(YELLOW)      This is expected behavior - actual publish will work in order.$(NC)\n"
	@printf "\n"
	@printf "$(YELLOW)  → Testing aimdb-derive (full validation)$(NC)\n"
	@cargo publish --dry-run -p aimdb-derive
	@printf "$(GREEN)✓ aimdb-derive is ready to publish!$(NC)\n"
	@printf "\n"
	@printf "$(BLUE)ℹ  Other crates cannot be fully validated until dependencies are published.$(NC)\n"
	@printf "$(BLUE)   Run 'make publish' to publish all crates in dependency order.$(NC)\n"

publish:
	@printf "$(GREEN)Publishing AimDB crates to crates.io...$(NC)\n"
	@printf "$(YELLOW)⚠  This will publish crates in dependency order$(NC)\n"
	@printf "$(YELLOW)⚠  Ensure git state is clean and version tags are correct$(NC)\n"
	@printf "\n"
	@if [ -z "$$CI" ]; then \
		read -p "Continue with publish? [y/N] " -n 1 -r; \
		echo; \
		if [[ ! $$REPLY =~ ^[Yy]$$ ]]; then \
			printf "$(RED)Publish cancelled$(NC)\n"; \
			exit 1; \
		fi; \
	else \
		printf "$(BLUE)Running in CI mode - skipping confirmation$(NC)\n"; \
	fi
	@printf "$(YELLOW)  → Publishing aimdb-derive (1/16)$(NC)\n"
	@cargo publish -p aimdb-derive
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-codegen (2/16)$(NC)\n"
	@cargo publish -p aimdb-codegen
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-core (3/16)$(NC)\n"
	@cargo publish -p aimdb-core
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-data-contracts (4/16)$(NC)\n"
	@cargo publish -p aimdb-data-contracts
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-tokio-adapter (5/16)$(NC)\n"
	@cargo publish -p aimdb-tokio-adapter
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-embassy-adapter (6/16)$(NC)\n"
	@cargo publish -p aimdb-embassy-adapter --no-verify
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-client (7/16)$(NC)\n"
	@cargo publish -p aimdb-client
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-sync (8/16)$(NC)\n"
	@cargo publish -p aimdb-sync
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-persistence (9/16)$(NC)\n"
	@cargo publish -p aimdb-persistence
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-persistence-sqlite (10/16)$(NC)\n"
	@cargo publish -p aimdb-persistence-sqlite
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-mqtt-connector (11/16)$(NC)\n"
	@cargo publish -p aimdb-mqtt-connector
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-knx-connector (12/16)$(NC)\n"
	@cargo publish -p aimdb-knx-connector
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-websocket-connector (13/16)$(NC)\n"
	@cargo publish -p aimdb-websocket-connector
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-wasm-adapter (14/16)$(NC)\n"
	@cargo publish -p aimdb-wasm-adapter --no-verify
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-cli (15/16)$(NC)\n"
	@cargo publish -p aimdb-cli
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-mcp (16/16)$(NC)\n"
	@cargo publish -p aimdb-mcp
	@printf "$(GREEN)✓ All 16 crates published successfully!$(NC)\n"
	@printf "$(BLUE)🎉 AimDB v$(shell grep '^version' Cargo.toml | head -1 | cut -d '"' -f 2) is now live on crates.io!$(NC)\n"

## Drift guards
# The README quickstart is compiled as examples/readme-quickstart; this target
# fails when the README code block and the example diverge, or when the
# example no longer compiles (design 038 §2.6/§3.13).
readme-check:
	@printf "$(GREEN)Checking README quickstart against examples/readme-quickstart...$(NC)\n"
	@awk '/^```rust$$/{f=1;next} f&&/^```$$/{exit} f' README.md \
		| diff -u - examples/readme-quickstart/src/main.rs \
		|| { printf "$(RED)README quickstart drifted from examples/readme-quickstart/src/main.rs$(NC)\n"; exit 1; }
	cargo check --package readme-quickstart
	@printf "$(GREEN)✓ README quickstart is in sync and compiles$(NC)\n"

# Compiles aimdb-codegen's generated output (common crate, hub crate, flat
# schema) against the local workspace so template drift against the real API
# breaks loudly (design 038 §3.10 decision).
codegen-drift:
	@printf "$(GREEN)Checking codegen templates against the workspace API...$(NC)\n"
	./tools/scripts/codegen-drift-check.sh

# Prove that simulation code (the dev-tier `simulatable` contract) never
# reaches a production binary. `rand` is the tracer: it is reachable iff
# `simulatable` is enabled (aimdb-data-contracts/src/simulatable.rs).
# The guard must not fail open:
# "rand is absent" is accepted only when `cargo tree -i rand` fails with its
# specific "did not match any packages" error — any other failure (missing
# submodule, typo'd package name, registry trouble) aborts the check instead of
# passing it vacuously. A positive control per example asserts the sim build
# DOES find `rand`, proving the tracer still traces. Also asserts `simulatable`
# is not a default feature of the contracts crate (which would pull `rand` into
# its own default graph).
SIM_EXAMPLES := weather-station-beta weather-station-gamma
check-no-sim:
	@printf "$(GREEN)Proving production graphs are simulation-free...$(NC)\n"
	@tree_rand() { cargo tree -p "$$1" $$2 -e normal -i rand 2>&1; }; \
	assert_rand_free() { \
		if out=$$(tree_rand "$$1" "$$2"); then \
			printf "$(RED)✗ $$3$(NC)\n"; \
			exit 1; \
		elif ! printf '%s\n' "$$out" | grep -q 'did not match any packages'; then \
			printf "$(RED)✗ cargo tree for '$$1' failed for a reason other than 'rand is absent' — refusing to pass vacuously:$(NC)\n"; \
			printf '%s\n' "$$out"; \
			exit 1; \
		fi; \
	}; \
	for bin in $(SIM_EXAMPLES); do \
		assert_rand_free "$$bin" "" "'$$bin' (default/production, no sim) pulls in 'rand' — simulation code leaked into production"; \
		printf "$(BLUE)✓ $$bin production graph is rand-free$(NC)\n"; \
		if ! tree_rand "$$bin" "--features sim" >/dev/null; then \
			printf "$(RED)✗ positive control failed: '$$bin --features sim' does not pull 'rand' — the tracer no longer traces, so the rand-free results above prove nothing$(NC)\n"; \
			exit 1; \
		fi; \
		printf "$(BLUE)✓ $$bin sim graph finds rand (tracer positive control)$(NC)\n"; \
	done; \
	assert_rand_free "aimdb-data-contracts" "" "aimdb-data-contracts pulls 'rand' with default features — 'simulatable' must never be a default feature"; \
	printf "$(BLUE)✓ 'simulatable' is not a default feature of aimdb-data-contracts$(NC)\n"; \
	printf "$(GREEN)✓ Production is simulation-free$(NC)\n"

# No library crate may install a process-global on its host's behalf.
# Applications may, so `tools/`, `examples/` and codegen's generated `main` are
# not scanned. The one exception: the fork detector owns the runtime thread it
# protects. A positive control keeps the guard from failing open.
GLOBALS_SCANNED := aimdb-core aimdb-data-contracts aimdb-derive aimdb-client \
	aimdb-tokio-adapter aimdb-embassy-adapter aimdb-wasm-adapter aimdb-sync \
	aimdb-persistence aimdb-persistence-sqlite aimdb-mqtt-connector \
	aimdb-knx-connector aimdb-websocket-connector aimdb-uds-connector \
	aimdb-serial-connector aimdb-tcp-connector
GLOBALS_PATTERN := tracing_subscriber::|set_global_default|set_boxed_logger|set_logger\(|set_max_level|panic::set_hook|panic::take_hook|pthread_atfork|sigaction|signal_hook|set_var\(
GLOBALS_ALLOWED := aimdb-sync/src/fork\.rs
check-no-globals:
	@printf "$(GREEN)Checking that no library crate installs a process-global...$(NC)\n"
	@scan() { \
		grep -rnE '$(GLOBALS_PATTERN)' --include='*.rs' "$$@" 2>/dev/null \
			| grep -vE ':[0-9]+:[[:space:]]*(//|\*)' || true; \
	}; \
	control=$$(mktemp -d) || exit 1; \
	printf 'fn main() { tracing_subscriber::fmt().init(); }\n' > "$$control/probe.rs"; \
	control_hit=$$(scan "$$control"); \
	rm -rf "$$control"; \
	if [ -z "$$control_hit" ]; then \
		printf "$(RED)✗ positive control failed: the scanner no longer flags a subscriber install, so a clean result proves nothing$(NC)\n"; \
		exit 1; \
	fi; \
	found=$$(scan $(addsuffix /src,$(GLOBALS_SCANNED)) | grep -vE '^($(GLOBALS_ALLOWED)):' || true); \
	if [ -n "$$found" ]; then \
		printf "$(RED)✗ a library crate installs a process-global:$(NC)\n"; \
		printf '%s\n' "$$found"; \
		printf "$(YELLOW)  A library does not get to make this decision for its host. Hand the$(NC)\n"; \
		printf "$(YELLOW)  application a way to make it instead — see design 050 for how the log$(NC)\n"; \
		printf "$(YELLOW)  destination does it. If the global genuinely belongs to the crate that$(NC)\n"; \
		printf "$(YELLOW)  owns the runtime thread, add it to GLOBALS_ALLOWED with the reason.$(NC)\n"; \
		exit 1; \
	fi; \
	printf "$(BLUE)✓ scanner verified against a positive control$(NC)\n"; \
	printf "$(GREEN)✓ No library crate installs a process-global$(NC)\n"

# The devcontainer image builds from the .devcontainer/ context, so it cannot
# read rust-toolchain.toml and has to repeat the channel in ARG RUST_VERSION.
# Nothing else keeps the two honest. Every lookup is checked for emptiness so a
# parse that quietly returns nothing cannot make this pass vacuously.
check-toolchain-pin:
	@printf "$(GREEN)Checking the devcontainer agrees with rust-toolchain.toml...$(NC)\n"
	@pin=$$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' rust-toolchain.toml); \
	arg=$$(sed -n 's/^ARG RUST_VERSION=\(.*\)$$/\1/p' .devcontainer/Dockerfile); \
	if [ -z "$$pin" ]; then \
		printf "$(RED)✗ no [toolchain] channel found in rust-toolchain.toml$(NC)\n"; exit 1; \
	fi; \
	if [ -z "$$arg" ]; then \
		printf "$(RED)✗ no ARG RUST_VERSION found in .devcontainer/Dockerfile$(NC)\n"; exit 1; \
	fi; \
	if [ "$$pin" != "$$arg" ]; then \
		printf "$(RED)✗ compiler drift: rust-toolchain.toml pins $$pin, .devcontainer/Dockerfile builds $$arg$(NC)\n"; \
		printf "$(YELLOW)  Update ARG RUST_VERSION in .devcontainer/Dockerfile to $$pin.$(NC)\n"; \
		exit 1; \
	fi; \
	printf "$(BLUE)✓ compiler pinned to $$pin on both sides$(NC)\n"; \
	targets=$$(sed -n 's/^targets[[:space:]]*=[[:space:]]*\[\(.*\)\]/\1/p' rust-toolchain.toml | tr -d '"' | tr ',' ' '); \
	if [ -z "$$targets" ]; then \
		printf "$(RED)✗ no targets found in rust-toolchain.toml$(NC)\n"; exit 1; \
	fi; \
	for t in $$targets; do \
		if ! grep -q "rustup target add $$t" .devcontainer/Dockerfile; then \
			printf "$(RED)✗ '$$t' is pinned in rust-toolchain.toml but the devcontainer never installs it$(NC)\n"; \
			exit 1; \
		fi; \
		printf "$(BLUE)✓ $$t present in both$(NC)\n"; \
	done; \
	components=$$(sed -n 's/^components[[:space:]]*=[[:space:]]*\[\(.*\)\]/\1/p' rust-toolchain.toml | tr -d '"' | tr ',' ' '); \
	if [ -z "$$components" ]; then \
		printf "$(RED)✗ no components found in rust-toolchain.toml$(NC)\n"; exit 1; \
	fi; \
	for c in $$components; do \
		if ! grep -q -- "-c $$c" .devcontainer/Dockerfile; then \
			printf "$(RED)✗ '$$c' is pinned in rust-toolchain.toml but the devcontainer never installs it$(NC)\n"; \
			exit 1; \
		fi; \
		printf "$(BLUE)✓ $$c present in both$(NC)\n"; \
	done; \
	printf "$(GREEN)✓ Devcontainer and rust-toolchain.toml agree$(NC)\n"

## Convenience commands
check: check-toolchain-pin fmt-check clippy test test-embedded test-wasm deny readme-check codegen-drift check-no-sim check-no-globals
	@printf "$(GREEN)Comprehensive development checks completed!$(NC)\n"
	@printf "$(BLUE)✓ Code formatting verified$(NC)\n"
	@printf "$(BLUE)✓ Linter passed$(NC)\n"
	@printf "$(BLUE)✓ All valid feature combinations tested$(NC)\n"
	@printf "$(BLUE)✓ Embedded target compatibility verified$(NC)\n"
	@printf "$(BLUE)✓ WASM target compatibility verified$(NC)\n"
	@printf "$(BLUE)✓ Dependencies verified (deny)$(NC)\n"
	@printf "$(BLUE)✓ README quickstart in sync and compiling$(NC)\n"
	@printf "$(BLUE)✓ Codegen output compiles against the workspace$(NC)\n"
	@printf "$(BLUE)✓ No library crate installs a process-global$(NC)\n"
	@printf "$(BLUE)✓ Devcontainer and CI pinned to the same compiler$(NC)\n"

## WASM commands
wasm:
	@printf "$(GREEN)Building WASM adapter with wasm-pack...$(NC)\n"
	@if ! command -v wasm-pack >/dev/null 2>&1; then \
		printf "$(YELLOW)  ⚠ wasm-pack not found, installing...$(NC)\n"; \
		cargo install wasm-pack --locked; \
	fi
	cd aimdb-wasm-adapter && wasm-pack build --target web --out-dir pkg
	@printf "$(GREEN)✓ WASM build complete! Output in aimdb-wasm-adapter/pkg/$(NC)\n"

wasm-test:
	@printf "$(GREEN)Running WASM tests in headless browser...$(NC)\n"
	@if ! command -v wasm-pack >/dev/null 2>&1; then \
		printf "$(YELLOW)  ⚠ wasm-pack not found, installing...$(NC)\n"; \
		cargo install wasm-pack --locked; \
	fi
	@if ! command -v google-chrome >/dev/null 2>&1 && ! command -v chromium >/dev/null 2>&1; then \
		printf "$(RED)  ✗ No Chrome/Chromium on PATH — headless WASM tests need a browser.$(NC)\n"; \
		printf "$(YELLOW)    Run 'make wasm-test-deps' (or rebuild the devcontainer).$(NC)\n"; \
		exit 1; \
	fi
	@if ! command -v chromedriver >/dev/null 2>&1; then \
		printf "$(RED)  ✗ chromedriver not on PATH.$(NC)\n"; \
		printf "$(YELLOW)    Run 'make wasm-test-deps' — the copy wasm-pack downloads itself$(NC)\n"; \
		printf "$(YELLOW)    tracks Chrome stable and drifts out of sync with the installed browser.$(NC)\n"; \
		exit 1; \
	fi
	cd aimdb-wasm-adapter && CHROMEDRIVER="$$(command -v chromedriver)" wasm-pack test --headless --chrome
	@printf "$(GREEN)✓ WASM tests passed!$(NC)\n"

# Installs Chrome plus the chromedriver build that matches its major version.
# wasm-pack downloads a chromedriver on its own, but always the newest one, which
# refuses to drive an older Chrome ("only supports Chrome version N").  Pinning the
# driver to the installed browser is what keeps wasm-test reproducible.
wasm-test-deps:
	@printf "$(GREEN)Installing headless WASM test dependencies...$(NC)\n"
	@if ! command -v google-chrome >/dev/null 2>&1; then \
		printf "$(YELLOW)  → Installing google-chrome-stable$(NC)\n"; \
		curl -fsSL https://dl.google.com/linux/linux_signing_key.pub \
			| sudo gpg --dearmor -o /usr/share/keyrings/google-chrome.gpg; \
		echo "deb [arch=$$(dpkg --print-architecture) signed-by=/usr/share/keyrings/google-chrome.gpg] https://dl.google.com/linux/chrome/deb/ stable main" \
			| sudo tee /etc/apt/sources.list.d/google-chrome.list >/dev/null; \
		sudo apt-get update -qq; \
		sudo apt-get install -y -qq google-chrome-stable unzip; \
	fi
	@printf "$(YELLOW)  → Installing matching chromedriver$(NC)\n"
	@set -e; \
	major="$$(google-chrome --version | sed -E 's/[^0-9]*([0-9]+).*/\1/')"; \
	url="$$(curl -fsSL https://googlechromelabs.github.io/chrome-for-testing/known-good-versions-with-downloads.json \
		| jq -r --arg m "$$major." '[.versions[] | select(.version | startswith($$m))] | last \
			| .downloads.chromedriver[] | select(.platform == "linux64") | .url')"; \
	if [ -z "$$url" ] || [ "$$url" = "null" ]; then \
		printf "$(RED)  ✗ No chromedriver published for Chrome $$major$(NC)\n"; exit 1; \
	fi; \
	tmp="$$(mktemp -d)"; \
	curl -fsSL -o "$$tmp/chromedriver.zip" "$$url"; \
	unzip -oq "$$tmp/chromedriver.zip" -d "$$tmp"; \
	sudo install -m 0755 "$$tmp/chromedriver-linux64/chromedriver" /usr/local/bin/chromedriver; \
	rm -rf "$$tmp"
	@printf "$(GREEN)✓ $$(google-chrome --version) / $$(chromedriver --version | cut -d' ' -f1-2)$(NC)\n"

all: build test examples
	@printf "$(GREEN)Build and test completed!$(NC)\n"
