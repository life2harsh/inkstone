#!/usr/bin/env bash
set -euo pipefail

echo "Starting Inkstone dev environment..."

# Start Postgres
docker compose up -d postgres

# Wait for Postgres to be ready
echo "Waiting for Postgres..."
until docker compose exec -T postgres pg_isready -U inkstone 2>/dev/null; do
  sleep 1
done
echo "Postgres is ready."

# Run the server
cargo run -p inkstone-server
