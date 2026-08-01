# syntax=docker/dockerfile:1.7

FROM rust:1.97.0-slim-bookworm AS rust-base

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY migrations/ migrations/
COPY stonepen/ stonepen/
RUN test -f stonepen/Cargo.toml || (echo "Stonepen is missing; run: git submodule update --init --recursive" >&2; exit 1)

FROM rust-base AS server-builder
RUN cargo build --locked --release -p inkstone-server

FROM debian:bookworm-slim AS server

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 inkstone \
    && useradd --system --uid 10001 --gid inkstone --home-dir /nonexistent --shell /usr/sbin/nologin inkstone

COPY --from=server-builder /app/target/release/inkstone-server /usr/local/bin/inkstone-server

USER 10001:10001
WORKDIR /app
EXPOSE 8080
CMD ["inkstone-server"]

FROM rust-base AS web-builder
COPY apps/web/index.html apps/web/styles.css apps/web/
COPY scripts/build-web.sh scripts/build-web.sh
RUN rustup target add wasm32-unknown-unknown \
    && cargo install wasm-pack --version 0.13.1 --locked \
    && ./scripts/build-web.sh --release --dist --skip-checks

FROM caddy:2.10.2-alpine AS web

RUN addgroup -S -g 10001 caddy \
    && adduser -S -D -H -u 10001 -G caddy caddy \
    && chown -R caddy:caddy /data /config

COPY --chown=caddy:caddy Caddyfile /etc/caddy/Caddyfile
COPY --chown=caddy:caddy --from=web-builder /app/apps/web/dist/ /srv/

USER 10001:10001
EXPOSE 80 443 443/udp
