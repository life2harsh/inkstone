# Inkstone

Self-hostable, local-first, E2EE collaborative Markdown knowledge workspace.

**Status:** v0.1 — Foundation.

## Quick Start

```bash
# 1. Start Postgres
docker compose up -d postgres

# 2. Run the server (initializes schema automatically)
cargo run -p inkstone-server
```

Server listens on `http://0.0.0.0:8080`.

## API

All non-WS routes require `x-dev-user-id: <uuid>` header in dev auth mode.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check |
| POST | `/api/workspaces` | Create workspace |
| GET | `/api/workspaces` | List user's workspaces |
| POST | `/api/workspaces/:id/docs` | Create document (encrypted title) |
| GET | `/api/workspaces/:id/docs` | List documents in workspace |
| GET | `/api/docs/:id` | Get document metadata |
| POST | `/api/docs/:id/updates` | Store encrypted update blob |
| GET | `/api/docs/:id/updates` | List update sequence numbers |
| GET | `/api/docs/:id/snapshot` | Get latest encrypted snapshot |
| POST | `/api/docs/:id/snapshot` | Upsert encrypted snapshot |
| GET | `/api/sync/docs/:id/ws` | WebSocket for real-time sync |

## Architecture

- **inkstone-core**: Shared types (IDs, crypto envelope, protocol messages, ink model, markdown parser, graph index)
- **inkstone-server**: Axum HTTP + WebSocket server, SQLx + Postgres, zero-knowledge document storage

## Security

- Server stores only encrypted blobs (titles, updates, snapshots, metadata)
- Client must encrypt before upload (XChaCha20-Poly1305 via WASM or WebCrypto)
- Dev auth is NOT production-safe
- Server cannot perform plaintext search
- See `docs/security.md` for details

## Vault → Migration

(Coming in v0.2)
