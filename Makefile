# AimDB Makefile
# Simple automation for common development tasks

.PHONY: help build test clean fmt clippy doc all check test-features test-embedded test-std test-no-std
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
	@printf "    build         Build the project\n"
	@printf "    test          Run all tests (std + all features)\n"
	@printf "    fmt           Format code\n"
	@printf "    clippy        Run linter\n"
	@printf "    doc           Generate docs\n"
	@printf "    clean         Clean build artifacts\n"
	@printf "\n"
	@printf "  $(YELLOW)Testing Commands:$(NC)\n"
	@printf "    check         Comprehensive development check (fmt + clippy + all tests)\n"
	@printf "    test-features Test different feature combinations\n"
	@printf "    test-embedded Test embedded/MCU compatibility\n"
	@printf "    test-std      Test std-only features\n"
	@printf "    test-no-std   Test no_std compatibility\n"
	@printf "\n"
	@printf "  $(YELLOW)Convenience:$(NC)\n"
	@printf "    all           Build everything\n"

## Core commands
build:
	@printf "$(GREEN)Building AimDB...$(NC)\n"
	cargo build --all-features

test:
	@printf "$(GREEN)Running standard tests...$(NC)\n"
	cargo test --all-features

fmt:
	@printf "$(GREEN)Formatting code...$(NC)\n"
	cargo fmt --all

clippy:
	@printf "$(GREEN)Running clippy...$(NC)\n"
	cargo clippy --all-targets --all-features -- -D warnings

doc:
	@printf "$(GREEN)Generating documentation...$(NC)\n"
	cargo doc --all-features --no-deps --open

clean:
	@printf "$(GREEN)Cleaning...$(NC)\n"
	cargo clean

## Testing commands
test-features:
	@printf "$(BLUE)Testing different feature combinations...$(NC)\n"
	@printf "$(YELLOW)  → Testing core no features (minimal)$(NC)\n"
	cd aimdb-core && cargo test --no-default-features
	@printf "$(YELLOW)  → Testing core std features$(NC)\n"
	cd aimdb-core && cargo test --features std
	@printf "$(YELLOW)  → Testing embassy adapter$(NC)\n"
	cd aimdb-embassy-adapter && cargo test
	@printf "$(YELLOW)  → Testing all workspace crates$(NC)\n"
	cargo test --all-features

test-embedded:
	@printf "$(BLUE)Testing embedded/MCU compatibility...$(NC)\n"
	@printf "$(YELLOW)  → Checking aimdb-core on thumbv7em-none-eabihf target$(NC)\n"
	cd aimdb-core && cargo check --target thumbv7em-none-eabihf --no-default-features
	@printf "$(YELLOW)  → Checking aimdb-embassy-adapter on thumbv7em-none-eabihf target$(NC)\n"
	cd aimdb-embassy-adapter && cargo check --target thumbv7em-none-eabihf
	@printf "$(YELLOW)  → Testing Embassy adapter in no_std$(NC)\n"
	cd aimdb-embassy-adapter && cargo test

test-std:
	@printf "$(BLUE)Testing std-only features...$(NC)\n"
	cd aimdb-core && cargo test --features std

test-no-std:
	@printf "$(BLUE)Testing no_std compatibility...$(NC)\n"
	@printf "$(YELLOW)  → Testing core minimal no_std$(NC)\n"
	cd aimdb-core && cargo test --no-default-features
	@printf "$(YELLOW)  → Testing Embassy adapter no_std$(NC)\n"
	cd aimdb-embassy-adapter && cargo test

## Convenience commands
check: fmt clippy test-features test-embedded
	@printf "$(GREEN)Comprehensive development checks completed!$(NC)\n"
	@printf "$(BLUE)✓ Code formatted$(NC)\n"
	@printf "$(BLUE)✓ Linter passed$(NC)\n"
	@printf "$(BLUE)✓ All feature combinations tested$(NC)\n"
	@printf "$(BLUE)✓ Embedded target compatibility verified$(NC)\n"

all: build test
	@printf "$(GREEN)Build and test completed!$(NC)\n"
