# AimDB Makefile
# Simple automation for common development tasks

.PHONY: help build test clean fmt fmt-check clippy doc all check test-embedded examples
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
	@printf "  $(YELLOW)Convenience:$(NC)\n"
	@printf "    all           Build everything\n"

## Core commands
build:
	@printf "$(GREEN)Building AimDB (all valid combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Building aimdb-core (no_std minimal)$(NC)\n"
	cargo build --package aimdb-core --no-default-features
	@printf "$(YELLOW)  → Building aimdb-core (std platform)$(NC)\n"
	cargo build --package aimdb-core --features "std,tracing,metrics"
	@printf "$(YELLOW)  → Building tokio adapter$(NC)\n"
	cargo build --package aimdb-tokio-adapter --features "tokio-runtime,tracing,metrics"
	@printf "$(YELLOW)  → Building sync wrapper$(NC)\n"
	cargo build --package aimdb-sync
	@printf "$(YELLOW)  → Building CLI tools$(NC)\n"
	cargo build --package aimdb-cli

test:
	@printf "$(GREEN)Running all tests (valid combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Testing aimdb-core (no_std minimal)$(NC)\n"
	cargo test --package aimdb-core --no-default-features
	@printf "$(YELLOW)  → Testing aimdb-core (std platform)$(NC)\n"
	cargo test --package aimdb-core --features "std,tracing"
	@printf "$(YELLOW)  → Testing aimdb-core remote module$(NC)\n"
	cargo test --package aimdb-core --lib --features "std" remote::
	@printf "$(YELLOW)  → Testing tokio adapter$(NC)\n"
	cargo test --package aimdb-tokio-adapter --features "tokio-runtime,tracing"
	@printf "$(YELLOW)  → Testing sync wrapper$(NC)\n"
	cargo test --package aimdb-sync
	@printf "$(YELLOW)  → Testing CLI tools$(NC)\n"
	cargo test --package aimdb-cli

fmt:
	@printf "$(GREEN)Formatting code (workspace members only)...$(NC)\n"
	@for pkg in aimdb-executor aimdb-core aimdb-embassy-adapter aimdb-tokio-adapter aimdb-sync aimdb-mqtt-connector aimdb-cli sync-api-demo tokio-mqtt-connector-demo embassy-mqtt-connector-demo; do \
		printf "$(YELLOW)  → Formatting $$pkg$(NC)\n"; \
		cargo fmt -p $$pkg 2>/dev/null || true; \
	done
	@printf "$(GREEN)✓ Formatting complete!$(NC)\n"

fmt-check:
	@printf "$(GREEN)Checking code formatting (workspace members only)...$(NC)\n"
	@FAILED=0; \
	for pkg in aimdb-executor aimdb-core aimdb-embassy-adapter aimdb-tokio-adapter aimdb-sync aimdb-mqtt-connector aimdb-cli sync-api-demo tokio-mqtt-connector-demo embassy-mqtt-connector-demo; do \
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
	@printf "$(YELLOW)  → Clippy on aimdb-core (no_std)$(NC)\n"
	cargo clippy --package aimdb-core --no-default-features --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (std)$(NC)\n"
	cargo clippy --package aimdb-core --features "std,tracing,metrics" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on tokio adapter$(NC)\n"
	cargo clippy --package aimdb-tokio-adapter --features "tokio-runtime,tracing,metrics" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on embassy adapter$(NC)\n"
	cargo clippy --package aimdb-embassy-adapter --features "embassy-runtime" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on sync wrapper$(NC)\n"
	cargo clippy --package aimdb-sync --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on CLI tools$(NC)\n"
	cargo clippy --package aimdb-cli --all-targets -- -D warnings

doc:
	@printf "$(GREEN)Generating dual-platform documentation...$(NC)\n"
	@# Create directory structure
	@mkdir -p target/doc-final/cloud
	@mkdir -p target/doc-final/embedded
	@printf "$(YELLOW)  → Building cloud/edge documentation$(NC)\n"
	cargo doc --package aimdb-core --features "std,tracing,metrics" --no-deps
	cargo doc --package aimdb-tokio-adapter --features "tokio-runtime,tracing,metrics" --no-deps
	cargo doc --package aimdb-sync --no-deps
	cargo doc --package aimdb-mqtt-connector --features "std,tokio-runtime" --no-deps
	cargo doc --package aimdb-cli --no-deps
	@cp -r target/doc/* target/doc-final/cloud/
	@printf "$(YELLOW)  → Building embedded documentation$(NC)\n"
	cargo doc --package aimdb-core --no-default-features --no-deps
	cargo doc --package aimdb-embassy-adapter --features "embassy-runtime" --no-deps
	cargo doc --package aimdb-mqtt-connector --no-default-features --features "embassy-runtime" --no-deps
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
	@printf "$(YELLOW)  → Checking aimdb-core (no_std minimal) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --no-default-features
	@printf "$(YELLOW)  → Checking aimdb-core (no_std/embassy) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --no-default-features
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --features "embassy-runtime"

## Example projects
examples:
	@printf "$(GREEN)Building all example projects...$(NC)\n"
	@printf "$(YELLOW)  → Building sync-api-demo (synchronous API wrapper)$(NC)\n"
	cargo build --package sync-api-demo
	@printf "$(YELLOW)  → Building tokio-mqtt-connector-demo (native, tokio runtime)$(NC)\n"
	cargo build --package tokio-mqtt-connector-demo
	@printf "$(YELLOW)  → Building embassy-mqtt-connector-demo (embedded, embassy runtime)$(NC)\n"
	cargo build --package embassy-mqtt-connector-demo --target thumbv7em-none-eabihf
	@printf "$(GREEN)All examples built successfully!$(NC)\n"

## Convenience commands
check: fmt clippy test test-embedded
	@printf "$(GREEN)Comprehensive development checks completed!$(NC)\n"
	@printf "$(BLUE)✓ Code formatting verified$(NC)\n"
	@printf "$(BLUE)✓ Linter passed$(NC)\n"
	@printf "$(BLUE)✓ All valid feature combinations tested$(NC)\n"
	@printf "$(BLUE)✓ Embedded target compatibility verified$(NC)\n"
	
all: build test examples
	@printf "$(GREEN)Build and test completed!$(NC)\n"
