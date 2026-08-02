# Inkstone

Self-hostable, local-first collaborative Markdown knowledge workspace.

**Status:** v0.1 - Local-first Rust/WASM spine under active development.

## Prerequisites

- Rust 1.88+ (`rustup`, `cargo`)
- `wasm-pack` (for browser build)
- Docker + Docker Compose (for full-stack deployment and server/Postgres work)

No Node.js required for the frontend.

Stonepen is an external Git submodule required by Cargo and Docker builds. After
cloning, initialize it and any nested submodules before building:

```bash
git submodule update --init --recursive
```

Alternatively, clone this repository with `git clone --recurse-submodules`.

## Quick Start (browser demo)

```bash
# Build the web WASM artifact
./scripts/build-web.sh

# Serve the static directory
cd apps/web
python -m http.server 8090
```

Open `http://localhost:8090` in a browser that supports WebAssembly and IndexedDB.

## Full-stack Docker deployment

The Compose stack builds the Rust server and fingerprinted Rust/WASM frontend,
serves the frontend through Caddy, proxies same-origin HTTP and WebSocket
requests under `/api/*`, and keeps Postgres and the server off host ports. Caddy
runs as an unprivileged user with named data and configuration volumes.

### Production

1. Create a private production environment file:

```bash
install -m 600 .env.production.example .env.production
```

2. Replace every `CHANGE_ME` value, set `SITE_ADDRESS` to the public hostname,
   and set `ALLOWED_ORIGINS` and `PUBLIC_BASE_URL` to the matching `https://`
   origin. If the database
   password has URL-reserved characters, percent-encode it in `DATABASE_URL`.
3. Point the hostname's DNS records at the host and allow inbound TCP 80/443
   and UDP 443. Caddy obtains and renews TLS certificates automatically.
4. Start or upgrade the complete stack with the documented deployment command:

```bash
docker compose --env-file .env.production up -d --build --wait
```

The legacy `pgdata` volume key is intentionally retained so upgrades reuse the
database volume created by earlier Inkstone Compose deployments.

Compose fails before startup if required database credentials, origins, cookie
policy, logging, ports, or site address are absent. Production authentication
is fail-closed: `DEV_AUTH=false`, HTTPS origins are required, and
`COOKIE_SECURE=true`. After the stack is healthy, issue a short-lived,
email-bound owner setup link from the server container:

```bash
docker compose --env-file .env.production exec server \
  inkstone-server owner-setup issue --email owner@example.com
```

Production requires `RESEND_API_KEY` and `RESEND_FROM_EMAIL`. Append `--send` to
deliver the owner link; mail is text-only and idempotent. Reissuing before use
revokes the previous link. `--expires-in-hours` accepts 1 through 168 and
defaults to 24. Invitations are email-bound token-only links. Acceptance uploads
a client-generated X25519 public key and remains unusable until an owner or admin
uploads a recipient-bound opaque workspace-key envelope. Account-password reset
does not recover the vault. Passphrase and generated recovery-key methods wrap
the account vault key on the client; the server stores only versioned opaque
envelopes, nonces, and AAD metadata.

### Local full stack

```bash
install -m 600 .env.example .env.local
docker compose --env-file .env.local up -d --build --wait
```

Open `http://localhost:8080`. The local example enables ordinary authentication
over loopback HTTP with non-secure cookies; the server rejects that cookie mode
for non-loopback origins. To stop the stack, run
`docker compose --env-file .env.local down`. Add `-v` only when you intend to
delete the database and Caddy volumes.

### Database backups

Backups are explicit rather than an always-running privileged job. The backup
script creates a compressed PostgreSQL custom-format dump under `backups/`,
sets restrictive permissions, and removes dumps older than 14 days:

```bash
ENV_FILE=.env.production ./scripts/backup-postgres.sh
BACKUP_RETENTION_DAYS=30 ENV_FILE=.env.production ./scripts/backup-postgres.sh
```

Copy backups off-host over encrypted transport and keep them in encrypted
storage because dumps include account metadata. Test restores regularly. The
restore is first validated into a temporary database while the live database
remains available. It then stops the server and atomically renames the live
database to a rollback name and the validated database to the live name. The
previous database is removed only after the server passes its database-backed
health check. A failed health check reverses the swap and retains the validated
restore for investigation. Restore requires an explicit confirmation:

```bash
CONFIRM_RESTORE=inkstone ENV_FILE=.env.production \
  ./scripts/restore-postgres.sh backups/inkstone-YYYYMMDDTHHMMSSZ-XXXXXXXX.dump
```

## Native server development

```bash
# Start the disposable test database on localhost:5433
docker compose -f docker-compose.test.yml up -d --wait

# Run the server (runs migrations on startup)
DATABASE_URL=postgres://inkstone:inkstone@localhost:5433/inkstone_test \
  DEV_AUTH=true cargo run -p inkstone-server
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

inkstone-workspace
  → Portable workspace paths, manifest format, and async storage contract.
    Platform-independent: no web-sys, no std::fs, no IndexedDB.
    Reference in-memory store for testing.
    Browser workspace migration and desktop filesystem storage are future work.

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

When `DEV_AUTH=true`, HTTP routes accept `x-dev-user-id` and
`x-dev-device-id`; WebSocket development auth uses matching query parameters.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check |
| POST | `/api/workspaces` | Create workspace |
| GET | `/api/workspaces` | List user's workspaces |
| POST | `/api/workspaces/:id/docs` | Create document (encrypted title and path projections) |
| GET | `/api/workspaces/:id/docs` | List documents in workspace |
| GET | `/api/docs/:id` | Get document metadata |
| PATCH | `/api/docs/:id` | Update encrypted path projection |
| PATCH | `/api/docs/:id/title` | Update sequence-guarded encrypted title projection |
| POST | `/api/docs/:id/updates` | Store encrypted update blob (idempotent) |
| GET | `/api/docs/:id/updates?after_seq=0&limit=500` | List encrypted update blobs |
| GET | `/api/docs/:id/snapshot` | Get latest encrypted snapshot |
| POST | `/api/docs/:id/snapshot` | Upsert encrypted snapshot |
| GET | `/api/sync/docs/:id/ws` | WebSocket for real-time sync |
| POST | `/api/auth/email-verification/request` | Request a single-use verification email |
| POST | `/api/auth/email-verification/consume` | Consume a verification token |
| POST | `/api/auth/password-reset/request` | Request account-password reset (not vault recovery) |
| POST | `/api/auth/password-reset/consume` | Consume reset token and revoke sessions |
| GET/PUT | `/api/auth/recovery-envelopes` | Read or CAS-write opaque account-vault-key envelopes |
| GET | `/api/auth/devices` | List active devices and sessions |
| DELETE | `/api/auth/devices/:id` | Revoke a device and its sessions |
| DELETE | `/api/auth/sessions/:id` | Revoke a session |
| POST | `/api/auth/sessions/revoke-others` | Revoke all other sessions |
| GET/POST | `/api/workspaces/:id/invitations` | List outgoing or create emailed invitations |
| POST | `/api/invitations/key-request` | Claim invitation with a client-generated public key |
| GET | `/api/workspaces/:id/key-requests` | List pending recipient key requests |
| POST | `/api/workspaces/:id/key-requests/:request/approve` | Upload recipient-bound opaque key envelope and activate membership |
| PATCH/DELETE | `/api/workspaces/:id/members/:user` | Change role or remove member |
| POST | `/api/workspaces/:id/leave` | Leave workspace with last-owner protection |
| POST | `/api/workspaces/:id/transfer-ownership` | Transactionally transfer ownership |

## Security status

- Local document state is plaintext in browser IndexedDB on the trusted device.
- Document paths, updates, snapshots, and workspace keys are encrypted before
  upload; the server stores opaque encrypted content.
- Account passwords are distinct from browser-only vault passphrases, so the
  server cannot derive workspace-key encryption keys.
- Password reset restores account authentication only and cannot decrypt or
  recover a vault. Recovery keys and plaintext vault/workspace keys never reach
  the server.
- Presence is relayed only as bounded versioned AEAD frames with nonce, AAD
  version, and monotonic counter metadata; it is never stored as a durable update.
- Dev auth (`x-dev-user-id` / `x-dev-device-id`) is for local development only.
- Serverless local mode is compiled out of production images; `?local=1` is
  accepted only by artifacts built with `scripts/build-web.sh --serverless-local`.
- Concurrent edits are ordered and recovered, but same-block conflict-free
  co-editing is not yet CRDT-based.
- Browser storage currently rejects a duplicate logical path when two joined
  workspaces use the same path; it fails without overwriting either document.

## Browser end-to-end tests

Local browser tests rebuild the current development WASM automatically, start
the static test server, and explicitly enter local mode with `?local=1`:

```bash
npm run test:e2e:local
```

The live sync suite creates accounts and workspaces and creates, renames, and
deletes persistent documents. Run it only against a dedicated disposable test
deployment, never against production or a deployment containing user data. It
requires two explicit opt-ins and runs only the desktop project. Setting
`PLAYWRIGHT_BASE_URL` disables Playwright's local static server:

```bash
PLAYWRIGHT_BASE_URL=https://inkstone-e2e.example.com \
INKSTONE_LIVE_E2E=1 \
INKSTONE_LIVE_E2E_DESTRUCTIVE_ACK=DELETE_TEST_DATA \
INKSTONE_E2E_OWNER_EMAIL=owner@example.test \
INKSTONE_E2E_OWNER_PASSWORD='dedicated-test-password' \
INKSTONE_E2E_VAULT_PASSPHRASE='dedicated-test-vault-passphrase' \
npm run test:e2e:sync
```

For an empty test deployment, first issue an owner setup link with the operator
command above. Existing test deployments sign in with the required owner email,
account password, and vault passphrase.
