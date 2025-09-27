# AimDB Makefile
# Simple automation for common development tasks

.PHONY: help build test clean fmt clippy doc all check test-embedded
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
	@printf "    check         Comprehensive development check (fmt + clippy + all tests)\n"
	@printf "    test-embedded Test embedded/MCU cross-compilation compatibility\n"
	@printf "\n"
	@printf "  $(YELLOW)Convenience:$(NC)\n"
	@printf "    all           Build everything\n"

## Core commands
build:
	@printf "$(GREEN)Building AimDB (all combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Building default (no_std)$(NC)\n"
	cargo build
	@printf "$(YELLOW)  → Building std components with all features$(NC)\n"
	cargo build --workspace --exclude aimdb-embassy-adapter --all-features
	@printf "$(YELLOW)  → Building embassy adapter (no_std only)$(NC)\n"
	cd aimdb-embassy-adapter && cargo build

test:
	@printf "$(GREEN)Running all tests (all combinations)...$(NC)\n"
	@printf "$(YELLOW)  → Testing default (no_std)$(NC)\n"
	cargo test
	@printf "$(YELLOW)  → Testing std components with all features$(NC)\n"
	cargo test --workspace --exclude aimdb-embassy-adapter --all-features
	@printf "$(YELLOW)  → Testing embassy adapter (no_std only)$(NC)\n"
	cd aimdb-embassy-adapter && cargo test

fmt:
	@printf "$(GREEN)Formatting code...$(NC)\n"
	cargo fmt --all

clippy:
	@printf "$(GREEN)Running clippy...$(NC)\n"
	@printf "$(YELLOW)  → Clippy on default (no_std)$(NC)\n"
	cargo clippy --all-targets -- -D warnings
	@printf "$(YELLOW)  → Clippy on std components with all features$(NC)\n"
	cargo clippy --workspace --exclude aimdb-embassy-adapter --all-targets --all-features -- -D warnings
	@printf "$(YELLOW)  → Clippy on embassy adapter$(NC)\n"
	cd aimdb-embassy-adapter && cargo clippy --all-targets -- -D warnings

doc:
	@printf "$(GREEN)Generating documentation...$(NC)\n"
	cargo doc --workspace --exclude aimdb-embassy-adapter --all-features --no-deps --open

clean:
	@printf "$(GREEN)Cleaning...$(NC)\n"
	cargo clean

## Testing commands
test-embedded:
	@printf "$(BLUE)Testing embedded/MCU cross-compilation compatibility...$(NC)\n"
	@printf "$(YELLOW)  → Checking aimdb-core on thumbv7em-none-eabihf target$(NC)\n"
	cd aimdb-core && cargo check --target thumbv7em-none-eabihf --no-default-features
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter on thumbv7em-none-eabihf target$(NC)\n"
	cd aimdb-embassy-adapter && cargo check --target thumbv7em-none-eabihf

## Convenience commands
check: fmt clippy test test-embedded
	@printf "$(GREEN)Comprehensive development checks completed!$(NC)\n"
	@printf "$(BLUE)✓ Code formatted$(NC)\n"
	@printf "$(BLUE)✓ Linter passed$(NC)\n"
	@printf "$(BLUE)✓ All feature combinations tested$(NC)\n"
	@printf "$(BLUE)✓ Embedded target compatibility verified$(NC)\n"



all: build test
	@printf "$(GREEN)Build and test completed!$(NC)\n"
