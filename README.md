# Inkstone

Self-hostable, local-first, E2EE collaborative Markdown knowledge workspace.

**Status:** v0.1 — Sync spine.

## Prerequisites

- Docker + Docker Compose
- Rust 1.88+ (`rustup`, `cargo`)
- Node 20+ (for frontend)

## Quick Start

```bash
# 1. Generate Cargo.lock (first-time setup)
cargo generate-lockfile

# 2. Start Postgres
docker compose up -d postgres

# 3. Run the server (runs migrations on startup)
cargo run -p inkstone-server
```

Server listens on `http://0.0.0.0:8080`.

## Frontend

```bash
cd apps/web
npm install
npm run dev
```

## All Commands

```bash
cargo generate-lockfile       # create Cargo.lock if missing (required before docker compose build)
cargo fmt                     # format Rust code
cargo check --workspace       # type-check all crates
cargo test -p inkstone-core   # run unit tests (no database needed)
./scripts/run-tests.sh        # run all tests (starts test Postgres automatically)
cd apps/web && npm run build  # build frontend
docker compose build          # build Docker image
```

## API

Dev auth: HTTP routes use `x-dev-user-id` and `x-dev-device-id` headers.
WebSocket uses query params `?dev_user_id=...&dev_device_id=...`.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check |
| POST | `/api/workspaces` | Create workspace |
| GET | `/api/workspaces` | List user's workspaces |
| POST | `/api/workspaces/:id/docs` | Create document (encrypted title) |
| GET | `/api/workspaces/:id/docs` | List documents in workspace |
| GET | `/api/docs/:id` | Get document metadata |
| POST | `/api/docs/:id/updates` | Store encrypted update blob (idempotent) |
| GET | `/api/docs/:id/updates?after_seq=0&limit=500` | List encrypted update blobs |
| GET | `/api/docs/:id/snapshot` | Get latest encrypted snapshot |
| POST | `/api/docs/:id/snapshot` | Upsert encrypted snapshot |
| GET | `/api/sync/docs/:id/ws` | WebSocket for real-time sync |

## Architecture

- **inkstone-core**: Shared types (IDs, crypto envelope, protocol messages, ink model, markdown parser, graph index)
- **inkstone-server**: Axum HTTP + WebSocket server, SQLx + Postgres, zero-knowledge document storage

## v0.1 security status

The current web client uses dev-only base64 plaintext placeholders. This is not encryption.
The server sync spine stores opaque blobs and is designed for client-side encryption.
Real E2EE is planned for the client crypto module.
Do not deploy dev auth in production.

## Security

- Server stores only encrypted blobs (titles, updates, snapshots, metadata)
- Client must encrypt before upload (XChaCha20-Poly1305 via WASM or WebCrypto)
- Dev auth is NOT production-safe
- Server cannot perform plaintext search
