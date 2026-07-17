#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

echo "=== Running cargo fmt check ==="
cargo fmt --all --check

echo "=== Running core unit tests ==="
cargo test -p inkstone-core

echo "=== Running client unit tests ==="
cargo test -p inkstone-client

echo "=== Running web unit tests ==="
cargo test -p inkstone-web

echo "=== Running workspace cargo check ==="
cargo check --workspace

echo "=== Building web WASM artifact with wasm-pack ==="
wasm-pack build \
  crates/inkstone-web \
  --target web \
  --out-dir ../../apps/web/pkg

echo "=== Build completed successfully ==="
