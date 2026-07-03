# syntax=docker/dockerfile:1
FROM rust:1.88-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY migrations/ migrations/

RUN cargo build --release -p inkstone-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/inkstone-server /usr/local/bin/inkstone-server
COPY --from=builder /app/migrations /app/migrations

WORKDIR /app
EXPOSE 8080

CMD ["inkstone-server"]
