#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "=== Starting test Postgres ==="
docker compose -f "$PROJECT_DIR/docker-compose.test.yml" up -d --wait 2>&1

cleanup() {
    echo "=== Stopping test Postgres ==="
    docker compose -f "$PROJECT_DIR/docker-compose.test.yml" down -v 2>&1
}
trap cleanup EXIT

echo "=== Running unit tests ==="
cargo test -p inkstone-core 2>&1

echo "=== Running integration tests ==="
# --test-threads=1 required: all tests share the same test database
TEST_DATABASE_URL="postgres://inkstone:inkstone@localhost:5433/inkstone_test" \
    cargo test -p inkstone-server --test integration -- --test-threads=1 2>&1

echo "=== Running websocket tests ==="
TEST_DATABASE_URL="postgres://inkstone:inkstone@localhost:5433/inkstone_test" \
    cargo test -p inkstone-server --test websocket -- --test-threads=1 2>&1
