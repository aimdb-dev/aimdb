# AimDB Makefile
# Simple automation for common development tasks

.PHONY: help build test clean fmt clippy doc all check test-embedded test-feature-validation
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
	@printf "    fmt           Format code\n"
	@printf "    clippy        Run linter\n"
	@printf "    doc           Generate docs\n"
	@printf "    clean         Clean build artifacts\n"
	@printf "\n"
	@printf "  $(YELLOW)Testing Commands:$(NC)\n"
	@printf "    check                Comprehensive development check (fmt + clippy + all tests)\n"
	@printf "    test-embedded        Test embedded/MCU cross-compilation compatibility\n"
	@printf "    test-feature-validation Test that invalid feature combinations fail\n"
	@printf "\n"
	@printf "  $(YELLOW)Convenience:$(NC)\n"
	@printf "    all           Build everything\n"

## Core commands
build:
	@printf "$(GREEN)Building AimDB (all valid combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Building aimdb-core (minimal embedded)$(NC)\n"
	cargo build --package aimdb-core --features "embedded"
	@printf "$(YELLOW)  → Building aimdb-core (std platform)$(NC)\n"
	cargo build --package aimdb-core --features "std,tokio-runtime,tracing,metrics"
	@printf "$(YELLOW)  → Building tokio adapter$(NC)\n"
	cargo build --package aimdb-tokio-adapter --features "tokio-runtime,tracing,metrics"
	@printf "$(YELLOW)  → Building embassy adapter$(NC)\n"
	cargo build --package aimdb-embassy-adapter --features "embassy-runtime"
	@printf "$(YELLOW)  → Building CLI tools$(NC)\n"
	cargo build --package aimdb-cli

test:
	@printf "$(GREEN)Running all tests (valid combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Testing aimdb-core (embedded platform)$(NC)\n"
	cargo test --package aimdb-core --features "embedded"
	@printf "$(YELLOW)  → Testing aimdb-core (std platform)$(NC)\n"
	cargo test --package aimdb-core --features "std,tokio-runtime,tracing"
	@printf "$(YELLOW)  → Testing tokio adapter$(NC)\n"
	cargo test --package aimdb-tokio-adapter --features "tokio-runtime,tracing"
	@printf "$(YELLOW)  → Testing CLI tools$(NC)\n"
	cargo test --package aimdb-cli

fmt:
	@printf "$(GREEN)Formatting code...$(NC)\n"
	cargo fmt --all

clippy:
	@printf "$(GREEN)Running clippy (all valid combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Clippy on aimdb-core (embedded)$(NC)\n"
	cargo clippy --package aimdb-core --features "embedded" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on aimdb-core (std)$(NC)\n"
	cargo clippy --package aimdb-core --features "std,tokio-runtime,tracing,metrics" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on tokio adapter$(NC)\n"
	cargo clippy --package aimdb-tokio-adapter --features "tokio-runtime,tracing,metrics" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on embassy adapter$(NC)\n"
	cargo clippy --package aimdb-embassy-adapter --features "embassy-runtime" --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on CLI tools$(NC)\n"
	cargo clippy --package aimdb-cli --all-targets -- -D warnings

doc:
	@printf "$(GREEN)Generating comprehensive documentation...$(NC)\n"
	@printf "$(YELLOW)  → Documenting std components with valid features$(NC)\n"
	cargo doc --package aimdb-core --features "std,tokio-runtime,tracing,metrics" --no-deps
	cargo doc --package aimdb-tokio-adapter --features "tokio-runtime,tracing,metrics" --no-deps
	cargo doc --package aimdb-cli --no-deps
	@printf "$(YELLOW)  → Documenting embassy adapter (embedded-only)$(NC)\n"
	cargo doc --package aimdb-embassy-adapter --features "embassy-runtime" --no-deps
	@printf "$(YELLOW)  → Opening documentation$(NC)\n"
	@# Open the main core documentation
	cargo doc --package aimdb-core --features "std,tokio-runtime,tracing,metrics" --no-deps --open

clean:
	@printf "$(GREEN)Cleaning...$(NC)\n"
	cargo clean

## Testing commands
test-embedded:
	@printf "$(BLUE)Testing embedded/MCU cross-compilation compatibility...$(NC)\n"
	@printf "$(YELLOW)  → Checking aimdb-core (embedded) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --features "embedded"
	@printf "$(YELLOW)  → Checking aimdb-core (embassy-runtime) on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-core --target thumbv7em-none-eabihf --features "embedded,embassy-runtime"
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter on thumbv7em-none-eabihf target$(NC)\n"
	cargo check --package aimdb-embassy-adapter --target thumbv7em-none-eabihf --features "embassy-runtime"

test-feature-validation:
	@printf "$(BLUE)Testing feature flag validation (invalid combinations should fail)...$(NC)\n"
	@printf "$(YELLOW)  → Testing invalid combination: std + embedded$(NC)\n"
	@! cargo build --package aimdb-core --features "std,embedded" 2>/dev/null || (echo "❌ Should have failed" && exit 1)
	@printf "$(GREEN)    ✓ Correctly failed$(NC)\n"
	@printf "$(YELLOW)  → Testing invalid combination: tokio-runtime + embassy-runtime$(NC)\n"
	@! cargo build --package aimdb-core --features "tokio-runtime,embassy-runtime" 2>/dev/null || (echo "❌ Should have failed" && exit 1)
	@printf "$(GREEN)    ✓ Correctly failed$(NC)\n"
	@printf "$(YELLOW)  → Testing invalid combination: embedded + metrics$(NC)\n"
	@! cargo build --package aimdb-core --features "embedded,metrics" 2>/dev/null || (echo "❌ Should have failed" && exit 1)
	@printf "$(GREEN)    ✓ Correctly failed$(NC)\n"
	@printf "$(GREEN)All invalid combinations correctly rejected!$(NC)\n"

## Convenience commands
check: fmt clippy test test-embedded test-feature-validation
	@printf "$(GREEN)Comprehensive development checks completed!$(NC)\n"
	@printf "$(BLUE)✓ Code formatted$(NC)\n"
	@printf "$(BLUE)✓ Linter passed$(NC)\n"
	@printf "$(BLUE)✓ All valid feature combinations tested$(NC)\n"
	@printf "$(BLUE)✓ Embedded target compatibility verified$(NC)\n"
	@printf "$(BLUE)✓ Invalid feature combinations correctly rejected$(NC)\n"

all: build test
	@printf "$(GREEN)Build and test completed!$(NC)\n"
