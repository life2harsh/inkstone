# syntax=docker/dockerfile:1
FROM rust:1.85-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests first for dependency caching.
COPY Cargo.toml Cargo.lock ./
COPY crates/inkstone-core/Cargo.toml crates/inkstone-core/
COPY crates/inkstone-server/Cargo.toml crates/inkstone-server/

# Create dummy main.rs and lib.rs so dependencies can be built.
RUN mkdir -p crates/inkstone-core/src crates/inkstone-server/src && \
    echo "fn main() {}" > crates/inkstone-server/src/main.rs && \
    touch crates/inkstone-core/src/lib.rs

# Build dependencies (this layer caches as long as Cargo.toml does not change).
RUN cargo build --release -p inkstone-server 2>/dev/null || true

# Now copy real source and migrations.
COPY crates/inkstone-core/src/ crates/inkstone-core/src/
COPY crates/inkstone-server/src/ crates/inkstone-server/src/
COPY migrations/ migrations/

# Touch main.rs to force recompile with real source.
RUN touch crates/inkstone-server/src/main.rs && \
    cargo build --release -p inkstone-server

# Runtime stage.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/inkstone-server /usr/local/bin/inkstone-server
COPY migrations/ /app/migrations/

WORKDIR /app
EXPOSE 8080

CMD ["inkstone-server"]
