# AimDB Makefile
# Simple automation for common development tasks

.PHONY: help build test clean fmt clippy doc all
.DEFAULT_GOAL := help

# Colors for output
GREEN := \033[0;32m
YELLOW := \033[0;33m
NC := \033[0m # No Color

## Show available commands
help:
	@echo "$(GREEN)AimDB Development Commands$(NC)"
	@echo ""
	@echo "  build       Build the project"
	@echo "  test        Run all tests"
	@echo "  fmt         Format code"
	@echo "  clippy      Run linter"
	@echo "  doc         Generate docs"
	@echo "  clean       Clean build artifacts"
	@echo "  check       Quick development check (fmt + clippy + test)"
	@echo "  all         Build everything"

## Core commands
build:
	@echo "$(GREEN)Building AimDB...$(NC)"
	cargo build --all-features

test:
	@echo "$(GREEN)Running tests...$(NC)"
	cargo test --all-features

fmt:
	@echo "$(GREEN)Formatting code...$(NC)"
	cargo fmt --all

clippy:
	@echo "$(GREEN)Running clippy...$(NC)"
	cargo clippy --all-targets --all-features -- -D warnings

doc:
	@echo "$(GREEN)Generating documentation...$(NC)"
	cargo doc --all-features --no-deps --open

clean:
	@echo "$(GREEN)Cleaning...$(NC)"
	cargo clean

## Convenience commands
check: fmt clippy test
	@echo "$(GREEN)Development checks completed!$(NC)"

all: build test
	@echo "$(GREEN)Build and test completed!$(NC)"
