SHELL := /bin/sh

CARGO ?= cargo
NPM ?= npm
PYTHON ?= python3

.DEFAULT_GOAL := help

.PHONY: help setup build build-release fmt fmt-check lint check test test-rust \
	test-console test-console-e2e test-python test-collectors test-install test-live console-install \
	console-build console-dev docs docs-serve hygiene ci clean

help: ## Show available maintenance targets
	@awk 'BEGIN {FS = ":.*## "; printf "Usage: make <target>\n\nTargets:\n"} /^[a-zA-Z0-9_-]+:.*## / {printf "  %-18s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

setup: console-install ## Install development dependencies

build: console-build ## Build the console and Rust workspace
	$(CARGO) build --workspace

build-release: console-build ## Build the release CLI with locked dependencies
	$(CARGO) build --release --locked -p af-cli

fmt: ## Format Rust sources
	$(CARGO) fmt --all

fmt-check: ## Check Rust formatting
	$(CARGO) fmt --all -- --check

lint: fmt-check console-install ## Run Rust and console static checks
	$(CARGO) clippy --workspace --all-targets -- -D warnings
	$(NPM) --prefix console run gen:types:check
	$(NPM) --prefix console run lint:hex
	$(NPM) --prefix console run lint:arith
	$(NPM) --prefix console run check

check: lint test hygiene ## Run the standard local validation suite

test: test-rust test-console test-python test-collectors ## Run deterministic tests

test-rust: console-build ## Run Rust workspace and feature tests
	$(CARGO) test --workspace
	$(CARGO) test -p af-cli --features experimental-opencode

test-console: console-install ## Run console unit tests
	$(NPM) --prefix console test

test-console-e2e: console-install ## Run console Playwright tests
	$(NPM) --prefix console run e2e

test-python: ## Run Python sidecar tests
	$(PYTHON) -m pytest python/tests -q

test-collectors: ## Run collector and statusline shell tests
	collectors/claude-code/test_hooks.sh
	collectors/opencode/test_collector.sh
	statusline/test_statusline.sh

test-install: ## Test the isolated installer flow
	scripts/test-install.sh

test-live: ## Run manual live agent tests; pass ARGS="codex ..."
	scripts/test-live.sh $(ARGS)

console-install: ## Install locked console dependencies
	$(NPM) --prefix console ci

console-build: console-install ## Build console assets
	$(NPM) --prefix console run build

console-dev: console-install ## Start the console development server
	$(NPM) --prefix console run dev

docs: ## Build the documentation site
	scripts/docs.sh build

docs-serve: ## Serve docs; override with PORT=8001 or HOST=0.0.0.0
	DOCS_HOST=$(or $(HOST),localhost) DOCS_PORT=$(or $(PORT),8000) scripts/docs.sh serve

hygiene: ## Check repository hygiene and patch whitespace
	scripts/check-repository-hygiene.sh
	git diff --check

ci: lint test test-console-e2e docs hygiene test-install ## Run the full repository validation suite

clean: ## Remove generated build and test artifacts
	$(CARGO) clean
	rm -rf console/dist console/test-results site .pytest_cache
	find python -type d -name __pycache__ -prune -exec rm -rf {} +
