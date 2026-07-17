# Inkstone

Self-hostable, local-first collaborative Markdown knowledge workspace.

**Status:** v0.1 — Local-first Rust/WASM spine. E2EE and browser sync not wired yet.

## Prerequisites

- Rust 1.88+ (`rustup`, `cargo`)
- `wasm-pack` (for browser build)
- Docker + Docker Compose (only for server/Postgres work)

No Node.js required for the frontend.

## Quick Start (browser demo)

```bash
# Build the web WASM artifact
./scripts/build-web.sh

# Serve the static directory
cd apps/web
python -m http.server 8090
```

Open `http://localhost:8090` in a browser that supports WebAssembly and IndexedDB.

## Server (sync + collaboration)

```bash
# Start Postgres
docker compose up -d postgres

# Run the server (runs migrations on startup)
cargo run -p inkstone-server
```

Server listens on `http://0.0.0.0:8080`.

## Architecture

```
inkstone-core
  → Canonical document model (InkstoneDocument), operations, markdown parser,
    graph index, protocol messages, typed IDs.

inkstone-client
  → Pure Rust local-first state machine. Owns phase transitions, intent
    queue, inbox, confirmation pipeline. No I/O, no browser dependency.

inkstone-web
  → Rust/WASM browser host. IndexedDB adapter (storage.rs, indexed_db.rs),
    DOM renderer, event delegation. Built with wasm-pack.

inkstone-server
  → Axum HTTP + WebSocket server, SQLx + Postgres. Synchronization spine.
    Stores opaque encrypted blobs.

stonepen
  → External Rust/WASM ink engine submodule (not a workspace member).
```

## Development

```bash
cargo fmt                     # format all Rust code
cargo test -p inkstone-core   # core document model tests
cargo test -p inkstone-client # local-first state machine tests
cargo test -p inkstone-web    # storage crate unit tests
cargo check --workspace       # type-check all crates
```

## API (server)

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

## Security status

- The current local demo document is **plaintext** in browser IndexedDB.
- Browser network sync is **not wired** yet.
- Real client-side E2EE is **not implemented** yet.
- Server stores opaque blobs; encryption is expected at the client layer.
- Dev auth (`x-dev-user-id` / `x-dev-device-id`) is **not production-safe**.
- This application is **not production ready**.
