# AimDB Makefile
# Simple automation for common development tasks

.PHONY: help build test clean fmt fmt-check clippy doc all check test-embedded examples deny audit security publish publish-check
.DEFAULT_GOAL := help

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
	@printf "  $(YELLOW)Convenience:$(NC)\n"
	@printf "    all           Build everything\n"

## Core commands
build:
	@printf "$(GREEN)Building AimDB (all valid combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Building aimdb-data-contracts (std)$(NC)\n"
	cargo build --package aimdb-data-contracts --features "std,simulatable,migratable,observable"
	@printf "$(YELLOW)  → Building aimdb-data-contracts (no_std)$(NC)\n"
	cargo build --package aimdb-data-contracts --no-default-features
	@printf "$(YELLOW)  → Building aimdb-core (no_std + alloc)$(NC)\n"
	cargo build --package aimdb-core --no-default-features --features alloc
	@printf "$(YELLOW)  → Building aimdb-core (std platform)$(NC)\n"
	cargo build --package aimdb-core --features "std,tracing,metrics"
	@printf "$(YELLOW)  → Building tokio adapter$(NC)\n"
	cargo build --package aimdb-tokio-adapter --features "tokio-runtime,tracing,metrics"
	@printf "$(YELLOW)  → Building sync wrapper$(NC)\n"
	cargo build --package aimdb-sync
	@printf "$(YELLOW)  → Building CLI tools$(NC)\n"
	cargo build --package aimdb-cli
	@printf "$(YELLOW)  → Building MCP server$(NC)\n"
	cargo build --package aimdb-mcp
	@printf "$(YELLOW)  → Building KNX connector$(NC)\n"
	cargo build --package aimdb-knx-connector --features "std,tokio-runtime"

test:
	@printf "$(GREEN)Running all tests (valid combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Testing aimdb-data-contracts (std)$(NC)\n"
	cargo test --package aimdb-data-contracts --features "std,simulatable,migratable,observable"
	@printf "$(YELLOW)  → Testing aimdb-core (no_std + alloc)$(NC)\n"
	cargo test --package aimdb-core --no-default-features --features alloc
	@printf "$(YELLOW)  → Testing aimdb-core (std platform)$(NC)\n"
	cargo test --package aimdb-core --features "std,tracing"
	@printf "$(YELLOW)  → Testing aimdb-core (std + metrics)$(NC)\n"
	cargo test --package aimdb-core --features "std,tracing,metrics"
	@printf "$(YELLOW)  → Testing aimdb-core remote module$(NC)\n"
	cargo test --package aimdb-core --lib --features "std" remote::
	@printf "$(YELLOW)  → Testing tokio adapter$(NC)\n"
	cargo test --package aimdb-tokio-adapter --features "tokio-runtime,tracing"
	@printf "$(YELLOW)  → Testing tokio adapter (with metrics)$(NC)\n"
	cargo test --package aimdb-tokio-adapter --features "tokio-runtime,tracing,metrics"
	@printf "$(YELLOW)  → Testing sync wrapper$(NC)\n"
	cargo test --package aimdb-sync
	@printf "$(YELLOW)  → Testing CLI tools$(NC)\n"
	cargo test --package aimdb-cli
	@printf "$(YELLOW)  → Testing MCP server$(NC)\n"
	cargo test --package aimdb-mcp
	@printf "$(YELLOW)  → Testing MQTT connector$(NC)\n"
	cargo test --package aimdb-mqtt-connector --features "std,tokio-runtime"
	@printf "$(YELLOW)  → Testing KNX connector$(NC)\n"
	cargo test --package aimdb-knx-connector --features "std,tokio-runtime"

fmt:
	@printf "$(GREEN)Formatting code (workspace members only)...$(NC)\n"
	@for pkg in aimdb-executor aimdb-derive aimdb-data-contracts aimdb-core aimdb-client aimdb-embassy-adapter aimdb-tokio-adapter aimdb-sync aimdb-mqtt-connector aimdb-knx-connector aimdb-cli aimdb-mcp sync-api-demo tokio-mqtt-connector-demo embassy-mqtt-connector-demo tokio-knx-connector-demo embassy-knx-connector-demo weather-mesh-common weather-hub weather-station-alpha weather-station-beta; do \
		printf "$(YELLOW)  → Formatting $$pkg$(NC)\n"; \
		cargo fmt -p $$pkg 2>/dev/null || true; \
	done
	@printf "$(GREEN)✓ Formatting complete!$(NC)\n"

fmt-check:
	@printf "$(GREEN)Checking code formatting (workspace members only)...$(NC)\n"
	@FAILED=0; \
	for pkg in aimdb-executor aimdb-derive aimdb-data-contracts aimdb-core aimdb-client aimdb-embassy-adapter aimdb-tokio-adapter aimdb-sync aimdb-mqtt-connector aimdb-knx-connector aimdb-cli aimdb-mcp sync-api-demo tokio-mqtt-connector-demo embassy-mqtt-connector-demo tokio-knx-connector-demo embassy-knx-connector-demo weather-mesh-common weather-hub weather-station-alpha weather-station-beta; do \
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
	@printf "$(YELLOW)  → Clippy on aimdb-data-contracts (no_std)$(NC)\n"
	cargo clippy --package aimdb-data-contracts --no-default-features -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (no_std + alloc)$(NC)\n"
	cargo clippy --package aimdb-core --no-default-features --features alloc --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (std)$(NC)\n"
	cargo clippy --package aimdb-core --features "std,tracing,metrics" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on tokio adapter$(NC)\n"
	cargo clippy --package aimdb-tokio-adapter --features "tokio-runtime,tracing,metrics" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on embassy adapter$(NC)\n"
	cargo clippy --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --features "embassy-runtime" -- -D warnings
	@printf "$(YELLOW)  → Clippy on embassy adapter with network support$(NC)\n"
	cargo clippy --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --features "embassy-runtime,embassy-net-support" -- -D warnings
	@printf "$(YELLOW)  → Clippy on sync wrapper$(NC)\n"
	cargo clippy --package aimdb-sync --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on client library$(NC)\n"
	cargo clippy --package aimdb-client --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on CLI tools$(NC)\n"
	cargo clippy --package aimdb-cli --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on MCP server$(NC)\n"
	cargo clippy --package aimdb-mcp --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on KNX connector (std)$(NC)\n"
	cargo clippy --package aimdb-knx-connector --features "std,tokio-runtime" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on KNX connector (embassy)$(NC)\n"
	cargo clippy --package aimdb-knx-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime" -- -D warnings
	@printf "$(YELLOW)  → Clippy on MQTT connector (embassy + defmt)$(NC)\n"
	cargo clippy --package aimdb-mqtt-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,defmt" -- -D warnings
	@printf "$(YELLOW)  → Clippy on KNX connector (embassy + defmt)$(NC)\n"
	cargo clippy --package aimdb-knx-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,defmt" -- -D warnings

doc:
	@printf "$(GREEN)Generating dual-platform documentation...$(NC)\n"
	@# Create directory structure
	@mkdir -p target/doc-final/cloud
	@mkdir -p target/doc-final/embedded
	@printf "$(YELLOW)  → Building cloud/edge documentation$(NC)\n"
	cargo doc --package aimdb-data-contracts --features "std,simulatable,migratable,observable" --no-deps
	cargo doc --package aimdb-core --features "std,tracing,metrics" --no-deps
	cargo doc --package aimdb-tokio-adapter --features "tokio-runtime,tracing,metrics" --no-deps
	cargo doc --package aimdb-sync --no-deps
	cargo doc --package aimdb-mqtt-connector --features "std,tokio-runtime" --no-deps
	cargo doc --package aimdb-knx-connector --features "std,tokio-runtime" --no-deps
	cargo doc --package aimdb-cli --no-deps
	cargo doc --package aimdb-mcp --no-deps
	@cp -r target/doc/* target/doc-final/cloud/
	@printf "$(YELLOW)  → Building embedded documentation$(NC)\n"
	cargo doc --package aimdb-core --no-default-features --features alloc --no-deps
	cargo doc --package aimdb-embassy-adapter --features "embassy-runtime" --no-deps
	cargo doc --package aimdb-mqtt-connector --no-default-features --features "embassy-runtime" --no-deps
	cargo doc --package aimdb-knx-connector --no-default-features --features "embassy-runtime" --no-deps
	@cp -r target/doc/* target/doc-final/embedded/
	@printf "$(YELLOW)  → Creating main index page$(NC)\n"
	@cp docs/index.html target/doc-final/index.html
	@printf "$(BLUE)Documentation generated at: file://$(PWD)/target/doc-final/index.html$(NC)\n"

clean:
	@printf "$(GREEN)Cleaning...$(NC)\n"
	cargo clean

## Testing commands
test-embedded:
	@printf "$(BLUE)Testing embedded/MCU cross-compilation compatibility...$(NC)\n"
	@printf "$(YELLOW)  → Checking aimdb-data-contracts (no_std) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-data-contracts --target thumbv7em-none-eabihf --no-default-features
	@printf "$(YELLOW)  → Checking aimdb-core (no_std minimal) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --no-default-features --features alloc
	@printf "$(YELLOW)  → Checking aimdb-core (no_std/embassy) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --no-default-features --features alloc
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime"
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter with network support on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,embassy-net-support"
	@printf "$(YELLOW)  → Checking aimdb-mqtt-connector (Embassy) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-mqtt-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime"
	@printf "$(YELLOW)  → Checking aimdb-mqtt-connector (Embassy + defmt) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-mqtt-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,defmt"
	@printf "$(YELLOW)  → Checking aimdb-knx-connector (Embassy) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-knx-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime"
	@printf "$(YELLOW)  → Checking aimdb-knx-connector (Embassy + defmt) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-knx-connector --target thumbv7em-none-eabihf --no-default-features --features "embassy-runtime,defmt"

## Example projects
examples:
	@printf "$(GREEN)Building all example projects...$(NC)\n"
	@printf "$(YELLOW)  → Building sync-api-demo (synchronous API wrapper)$(NC)\n"
	cargo build --package sync-api-demo
	@printf "$(YELLOW)  → Building tokio-mqtt-connector-demo (native, tokio runtime)$(NC)\n"
	cargo build --package tokio-mqtt-connector-demo
	@printf "$(YELLOW)  → Building embassy-mqtt-connector-demo (embedded, embassy runtime)$(NC)\n"
	cargo build --package embassy-mqtt-connector-demo --target thumbv7em-none-eabihf
	@printf "$(YELLOW)  → Building tokio-knx-connector-demo (native, tokio runtime)$(NC)\n"
	cargo build --package tokio-knx-connector-demo
	@printf "$(YELLOW)  → Building embassy-knx-connector-demo (embedded, embassy runtime)$(NC)\n"
	cargo build --package embassy-knx-connector-demo --target thumbv7em-none-eabihf
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-mesh-common$(NC)\n"
	cargo build --package weather-mesh-common
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-hub (cloud aggregator)$(NC)\n"
	cargo build --package weather-hub
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-station-alpha (edge, real API)$(NC)\n"
	cargo build --package weather-station-alpha
	@printf "$(YELLOW)  → Building weather-mesh-demo: weather-station-beta (edge, synthetic)$(NC)\n"
	cargo build --package weather-station-beta
	@printf "$(YELLOW)  → Building weather-station-gamma (embedded, embassy runtime)$(NC)\n"
	cargo build --package weather-station-gamma --target thumbv7em-none-eabihf
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
	@printf "$(YELLOW)      Only aimdb-executor (no deps) will fully validate before first publish.$(NC)\n"
	@printf "$(YELLOW)      This is expected behavior - actual publish will work in order.$(NC)\n"
	@printf "\n"
	@printf "$(YELLOW)  → Testing aimdb-executor (full validation)$(NC)\n"
	@cargo publish --dry-run -p aimdb-executor
	@printf "$(GREEN)✓ aimdb-executor is ready to publish!$(NC)\n"
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
	@printf "$(YELLOW)  → Publishing aimdb-executor (1/11)$(NC)\n"
	@cargo publish -p aimdb-executor
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-derive (2/11)$(NC)\n"
	@cargo publish -p aimdb-derive
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-core (3/11)$(NC)\n"
	@cargo publish -p aimdb-core
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-tokio-adapter (4/11)$(NC)\n"
	@cargo publish -p aimdb-tokio-adapter
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-embassy-adapter (5/11)$(NC)\n"
	@cargo publish -p aimdb-embassy-adapter --no-verify
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-client (6/11)$(NC)\n"
	@cargo publish -p aimdb-client
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-sync (7/11)$(NC)\n"
	@cargo publish -p aimdb-sync
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-mqtt-connector (8/11)$(NC)\n"
	@cargo publish -p aimdb-mqtt-connector
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-knx-connector (9/11)$(NC)\n"
	@cargo publish -p aimdb-knx-connector
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-cli (10/11)$(NC)\n"
	@cargo publish -p aimdb-cli
	@printf "$(YELLOW)  → Waiting 10s for crates.io propagation...$(NC)\n"
	@sleep 10
	@printf "$(YELLOW)  → Publishing aimdb-mcp (11/11)$(NC)\n"
	@cargo publish -p aimdb-mcp
	@printf "$(GREEN)✓ All crates published successfully!$(NC)\n"
	@printf "$(BLUE)🎉 AimDB v$(shell grep '^version' Cargo.toml | head -1 | cut -d '"' -f 2) is now live on crates.io!$(NC)\n"

## Convenience commands
check: fmt-check clippy test test-embedded deny
	@printf "$(GREEN)Comprehensive development checks completed!$(NC)\n"
	@printf "$(BLUE)✓ Code formatting verified$(NC)\n"
	@printf "$(BLUE)✓ Linter passed$(NC)\n"
	@printf "$(BLUE)✓ All valid feature combinations tested$(NC)\n"
	@printf "$(BLUE)✓ Embedded target compatibility verified$(NC)\n"
	@printf "$(BLUE)✓ Dependencies verified (deny)$(NC)\n"
	
all: build test examples
	@printf "$(GREEN)Build and test completed!$(NC)\n"
