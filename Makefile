# AimDB Makefile
# Simple automation for common development tasks

.PHONY: help build test clean clean-embedded fmt fmt-check clippy doc all check test-embedded test-wasm wasm wasm-test examples deny audit security publish publish-check readme-check codegen-drift check-no-sim
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
	@printf "\n"
	@printf "  $(YELLOW)Convenience:$(NC)\n"
	@printf "    all           Build everything\n"

## Core commands
build:
	@printf "$(GREEN)Building AimDB (all valid combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Building aimdb-data-contracts (std)$(NC)\n"
	cargo build --package aimdb-data-contracts --features "std,simulatable,migratable,observable"
	@printf "$(YELLOW)  → Building aimdb-data-contracts (no_std)$(NC)\n"
	cargo build --package aimdb-data-contracts --no-default-features --features alloc
	@printf "$(YELLOW)  → Building aimdb-data-contracts (no_std + linkable + migratable)$(NC)\n"
	cargo build --package aimdb-data-contracts --no-default-features --features alloc,linkable,migratable
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
	@printf "$(YELLOW)  → Building sync wrapper$(NC)\n"
	cargo build --package aimdb-sync
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
	@printf "$(YELLOW)  → Building WS protocol$(NC)\n"
	cargo build --package aimdb-ws-protocol
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
	cargo test --package aimdb-data-contracts --features "std,simulatable,migratable,observable"
	@printf "$(YELLOW)  → Testing aimdb-data-contracts (no_std + alloc + migratable)$(NC)\n"
	cargo test --package aimdb-data-contracts --no-default-features --features alloc,migratable
	@printf "$(YELLOW)  → Testing aimdb-core (no_std + alloc)$(NC)\n"
	cargo test --package aimdb-core --no-default-features --features alloc
	@printf "$(YELLOW)  → Testing aimdb-core (std platform)$(NC)\n"
	cargo test --package aimdb-core --features "std,tracing"
	@printf "$(YELLOW)  → Testing aimdb-core (std + observability)$(NC)\n"
	cargo test --package aimdb-core --features "std,tracing,observability"
	@printf "$(YELLOW)  → Testing aimdb-core (no_std + alloc + observability)$(NC)\n"
	cargo test --package aimdb-core --no-default-features --features "alloc,observability"
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
	@printf "$(YELLOW)  → Testing embassy adapter (host, no executor: buffers, join-queue, connector spine, doctests)$(NC)\n"
	cargo test --package aimdb-embassy-adapter --no-default-features --features "alloc,embassy-sync,embassy-time,connectors"
	@printf "$(YELLOW)  → Testing WASM adapter (host lib: buffer semantics + shared contract suite; browser layer runs via wasm-test)$(NC)\n"
	cargo test --package aimdb-wasm-adapter --no-default-features --lib
	@printf "$(YELLOW)  → Testing sync wrapper$(NC)\n"
	cargo test --package aimdb-sync
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
	@printf "$(YELLOW)  → Testing MQTT connector$(NC)\n"
	cargo test --package aimdb-mqtt-connector --features "std,tokio-runtime"
	@printf "$(YELLOW)  → Testing KNX connector$(NC)\n"
	cargo test --package aimdb-knx-connector --features "std,tokio-runtime"
	@printf "$(YELLOW)  → Testing WS protocol$(NC)\n"
	cargo test --package aimdb-ws-protocol
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

fmt:
	@printf "$(GREEN)Formatting code (workspace members only)...$(NC)\n"
	@for pkg in aimdb-derive aimdb-data-contracts aimdb-core aimdb-client aimdb-embassy-adapter aimdb-tokio-adapter aimdb-wasm-adapter aimdb-sync aimdb-persistence aimdb-persistence-sqlite aimdb-mqtt-connector aimdb-knx-connector aimdb-ws-protocol aimdb-websocket-connector aimdb-uds-connector aimdb-serial-connector aimdb-tcp-connector aimdb-codegen aimdb-cli aimdb-mcp sync-api-demo tokio-mqtt-connector-demo embassy-mqtt-connector-demo tokio-knx-connector-demo embassy-knx-connector-demo embassy-serial-connector-demo embassy-bench-stm32h5 weather-mesh-common weather-hub weather-station weather-station-alpha weather-station-beta hello-mailbox hello-mailbox-async hello-single-latest-async aimdb-bench; do \
		printf "$(YELLOW)  → Formatting $$pkg$(NC)\n"; \
		cargo fmt -p $$pkg 2>/dev/null || true; \
	done
	@printf "$(GREEN)✓ Formatting complete!$(NC)\n"

fmt-check:
	@printf "$(GREEN)Checking code formatting (workspace members only)...$(NC)\n"
	@FAILED=0; \
	for pkg in aimdb-derive aimdb-data-contracts aimdb-core aimdb-client aimdb-embassy-adapter aimdb-tokio-adapter aimdb-wasm-adapter aimdb-sync aimdb-persistence aimdb-persistence-sqlite aimdb-mqtt-connector aimdb-knx-connector aimdb-ws-protocol aimdb-websocket-connector aimdb-uds-connector aimdb-serial-connector aimdb-tcp-connector aimdb-codegen aimdb-cli aimdb-mcp sync-api-demo tokio-mqtt-connector-demo embassy-mqtt-connector-demo tokio-knx-connector-demo embassy-knx-connector-demo embassy-serial-connector-demo embassy-bench-stm32h5 weather-mesh-common weather-hub weather-station weather-station-alpha weather-station-beta hello-mailbox hello-mailbox-async hello-single-latest-async aimdb-bench; do \
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
	cargo clippy --package aimdb-data-contracts --features "std,simulatable,migratable,observable" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-data-contracts (no_std + alloc)$(NC)\n"
	cargo clippy --package aimdb-data-contracts --no-default-features --features alloc -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-data-contracts (no_std + alloc + linkable + migratable)$(NC)\n"
	cargo clippy --package aimdb-data-contracts --no-default-features --features alloc,linkable,migratable -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (no_std + alloc)$(NC)\n"
	cargo clippy --package aimdb-core --no-default-features --features alloc --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (no_std + alloc + remote)$(NC)\n"
	cargo clippy --package aimdb-core --no-default-features --features "alloc,remote" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (std)$(NC)\n"
	cargo clippy --package aimdb-core --features "std,tracing,observability" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on tokio adapter$(NC)\n"
	cargo clippy --package aimdb-tokio-adapter --features "tokio-runtime,tracing,observability" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on embassy adapter$(NC)\n"
	cargo clippy --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --features "embassy-runtime" -- -D warnings
	@printf "$(YELLOW)  → Clippy on embassy adapter with network support$(NC)\n"
	cargo clippy --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --features "embassy-runtime,embassy-net-support" -- -D warnings
	@printf "$(YELLOW)  → Clippy on sync wrapper$(NC)\n"
	cargo clippy --package aimdb-sync --all-targets -- -D warnings
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
	@printf "$(YELLOW)  → Clippy on MQTT connector (embassy + defmt)$(NC)\n"
	cargo clippy --package aimdb-mqtt-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,defmt" -- -D warnings
	@printf "$(YELLOW)  → Clippy on KNX connector (embassy + defmt)$(NC)\n"
	cargo clippy --package aimdb-knx-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,defmt" -- -D warnings
	@printf "$(YELLOW)  → Clippy on WS protocol$(NC)\n"
	cargo clippy --package aimdb-ws-protocol --all-targets -- -D warnings
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
	@printf "$(YELLOW)  → Clippy on WASM adapter$(NC)\n"
	cargo clippy --package aimdb-wasm-adapter --target wasm32-unknown-unknown --features "wasm-runtime" -- -D warnings
	@printf "$(YELLOW)  → Clippy on benchmarking infrastructure (host-only, incl. benches)$(NC)\n"
	cargo clippy --package aimdb-bench --all-targets -- -D warnings

doc:
	@printf "$(GREEN)Generating dual-platform documentation...$(NC)\n"
	@# Create directory structure
	@mkdir -p target/doc-final/cloud
	@mkdir -p target/doc-final/embedded
	@printf "$(YELLOW)  → Building cloud/edge documentation$(NC)\n"
	cargo doc --package aimdb-data-contracts --features "std,simulatable,migratable,observable" --no-deps
	cargo doc --package aimdb-core --features "std,tracing,observability" --no-deps
	cargo doc --package aimdb-tokio-adapter --features "tokio-runtime,tracing,observability" --no-deps
	cargo doc --package aimdb-sync --no-deps
	cargo doc --package aimdb-mqtt-connector --features "std,tokio-runtime" --no-deps
	cargo doc --package aimdb-knx-connector --features "std,tokio-runtime" --no-deps
	cargo doc --package aimdb-codegen --no-deps
	cargo doc --package aimdb-cli --no-deps
	cargo doc --package aimdb-mcp --no-deps
	cargo doc --package aimdb-persistence --no-deps
	cargo doc --package aimdb-persistence-sqlite --no-deps
	cargo doc --package aimdb-ws-protocol --no-deps
	cargo doc --package aimdb-websocket-connector --features "tokio-runtime" --no-deps
	@cp -r target/doc/* target/doc-final/cloud/
	@printf "$(YELLOW)  → Building embedded documentation$(NC)\n"
	cargo doc --package aimdb-core --no-default-features --features alloc --no-deps
	cargo doc --package aimdb-embassy-adapter --features "embassy-runtime" --no-deps
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
	@printf "$(YELLOW)  → Checking aimdb-data-contracts (no_std + alloc + linkable + migratable) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-data-contracts --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features alloc,linkable,migratable
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
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime"
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter with network support on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,embassy-net-support"
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter with observability on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,observability"
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter connector spine (connector-io) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --target-dir $(EMBEDDED_CHECK_TARGET_DIR) --no-default-features --features "embassy-runtime,connector-io"
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
	cargo build --package embassy-mqtt-connector-demo --target thumbv7em-none-eabihf
	@printf "$(YELLOW)  → Building knx-connector-demo-common (shared KNX demo code, runtime-agnostic)$(NC)\n"
	cargo build --package knx-connector-demo-common
	@printf "$(YELLOW)  → Building tokio-knx-connector-demo (native, tokio runtime)$(NC)\n"
	cargo build --package tokio-knx-connector-demo
	@printf "$(YELLOW)  → Building embassy-knx-connector-demo (embedded, embassy runtime)$(NC)\n"
	cargo build --package embassy-knx-connector-demo --target thumbv7em-none-eabihf
	@printf "$(YELLOW)  → Building embassy-serial-connector-demo (embedded, embassy runtime)$(NC)\n"
	cargo build --package embassy-serial-connector-demo --target thumbv7em-none-eabihf
	@printf "$(YELLOW)  → Building embassy-bench-stm32h5 (B3 on-target profiling, embassy runtime)$(NC)\n"
	cargo build --package embassy-bench-stm32h5 --target thumbv7em-none-eabihf
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-mesh-common$(NC)\n"
	cargo build --package weather-mesh-common
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-hub (cloud aggregator)$(NC)\n"
	cargo build --package weather-hub
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-station-alpha (edge, real API)$(NC)\n"
	cargo build --package weather-station-alpha
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-station-beta (edge, synthetic)$(NC)\n"
	cargo build --package weather-station-beta
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-station (public mesh, profile-driven)$(NC)\n"
	cargo build --package weather-station
	@printf "$(YELLOW)  → Building weather-station-gamma (embedded, embassy runtime)$(NC)\n"
	cargo build --package weather-station-gamma --target thumbv7em-none-eabihf
	@printf "$(YELLOW)  → Building remote-access-demo (AimX server + client)$(NC)\n"
	cargo build --package remote-access-demo
	@printf "$(YELLOW)  → Building hello-mailbox (sync)$(NC)\n"
	cargo build --package hello-mailbox
	@printf "$(YELLOW)  → Building hello-mailbox-async $(NC)\n"
	cargo build --package hello-mailbox-async
	@printf "$(YELLOW)  → Building hello-single-latest-async$(NC)\n"
	cargo build --package hello-single-latest-async
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
	@printf "$(YELLOW)  → Publishing aimdb-derive (1/17)$(NC)\n"
	@cargo publish -p aimdb-derive
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-codegen (2/17)$(NC)\n"
	@cargo publish -p aimdb-codegen
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-core (3/17)$(NC)\n"
	@cargo publish -p aimdb-core
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-data-contracts (4/17)$(NC)\n"
	@cargo publish -p aimdb-data-contracts
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-tokio-adapter (5/17)$(NC)\n"
	@cargo publish -p aimdb-tokio-adapter
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-embassy-adapter (6/17)$(NC)\n"
	@cargo publish -p aimdb-embassy-adapter --no-verify
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-client (7/17)$(NC)\n"
	@cargo publish -p aimdb-client
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-sync (8/17)$(NC)\n"
	@cargo publish -p aimdb-sync
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-persistence (9/17)$(NC)\n"
	@cargo publish -p aimdb-persistence
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-persistence-sqlite (10/17)$(NC)\n"
	@cargo publish -p aimdb-persistence-sqlite
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-mqtt-connector (11/17)$(NC)\n"
	@cargo publish -p aimdb-mqtt-connector
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-knx-connector (12/17)$(NC)\n"
	@cargo publish -p aimdb-knx-connector
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-ws-protocol (13/17)$(NC)\n"
	@cargo publish -p aimdb-ws-protocol
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-websocket-connector (14/17)$(NC)\n"
	@cargo publish -p aimdb-websocket-connector
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-wasm-adapter (15/17)$(NC)\n"
	@cargo publish -p aimdb-wasm-adapter --no-verify
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-cli (16/17)$(NC)\n"
	@cargo publish -p aimdb-cli
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-mcp (17/17)$(NC)\n"
	@cargo publish -p aimdb-mcp
	@printf "$(GREEN)✓ All 17 crates published successfully!$(NC)\n"
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

## Convenience commands
check: fmt-check clippy test test-embedded test-wasm deny readme-check codegen-drift check-no-sim
	@printf "$(GREEN)Comprehensive development checks completed!$(NC)\n"
	@printf "$(BLUE)✓ Code formatting verified$(NC)\n"
	@printf "$(BLUE)✓ Linter passed$(NC)\n"
	@printf "$(BLUE)✓ All valid feature combinations tested$(NC)\n"
	@printf "$(BLUE)✓ Embedded target compatibility verified$(NC)\n"
	@printf "$(BLUE)✓ WASM target compatibility verified$(NC)\n"
	@printf "$(BLUE)✓ Dependencies verified (deny)$(NC)\n"
	@printf "$(BLUE)✓ README quickstart in sync and compiling$(NC)\n"
	@printf "$(BLUE)✓ Codegen output compiles against the workspace$(NC)\n"

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
	cd aimdb-wasm-adapter && wasm-pack test --headless --chrome
	@printf "$(GREEN)✓ WASM tests passed!$(NC)\n"

all: build test examples
	@printf "$(GREEN)Build and test completed!$(NC)\n"
