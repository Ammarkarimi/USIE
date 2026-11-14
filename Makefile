# Developer entry points. Requirements: Go 1.24+, Rust 1.90+, Node 20+ with
# pnpm, git, and PostgreSQL 14+ server binaries for the test database.

export GOTOOLCHAIN := local
ENGINE := engines/target/release/usie-engine

.PHONY: all build engine services web test test-rust test-go test-web lint fmt dev db-start db-stop

all: build

build: engine services web

engine:
	cd engines && cargo build --release --locked

services:
	go build -o bin/ ./services/api-gateway/cmd/usie-api ./services/execution-orchestrator/cmd/usie-worker

web:
	pnpm install --frozen-lockfile
	pnpm --filter @usie/web build

test: test-rust test-go test-web

test-rust:
	cd engines && cargo test --locked

# Integration tests need the engine binary and a PostgreSQL server; each test
# creates and drops its own database. Set USIE_TEST_DATABASE_URL to use an
# existing server instead of the disposable local one.
test-go: engine
	USIE_TEST_DATABASE_URL="$${USIE_TEST_DATABASE_URL:-$$(scripts/test-db.sh start)}" \
	USIE_ENGINE_BIN="$(CURDIR)/$(ENGINE)" go test -count=1 ./...

test-web:
	pnpm install --frozen-lockfile
	pnpm -r run typecheck
	pnpm -r run test

lint:
	cd engines && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings
	test -z "$$(gofmt -l database internal services)"
	go vet ./...
	golangci-lint run ./...

fmt:
	cd engines && cargo fmt
	gofmt -w database internal services

# Runs the API (serving the built web app) and one worker against the local
# PostgreSQL. Open http://127.0.0.1:8080 and create the first account.
dev: build
	scripts/dev.sh

db-start:
	@scripts/test-db.sh start

db-stop:
	@scripts/test-db.sh stop
