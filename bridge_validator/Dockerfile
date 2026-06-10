# Build from repository root with: docker build -f bridge_validator/Dockerfile -t bridge-validator .
# Requires .sqlx directory for offline mode: cargo sqlx prepare

# Build stage
# Pinned by digest so rebuilds resolve the exact base CI used (tags are mutable).
# Multi-arch index for rust:slim-bookworm (rust 1.x stable, resolved 2026-06-10;
# linux/amd64 manifest: sha256:c87db6b683e63a304c12d50f4bafc3fa71c1b10a16a66269c353e621252d6307).
FROM rust:slim-bookworm@sha256:b5f842fac1e3b4ff718a652a8e0173b62d9403ec826ef4998880b9347db30684 AS builder

WORKDIR /usr/src/app

RUN apt-get update && \
    apt-get install -y pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

COPY Cargo.lock ./

COPY bridge_validator/Cargo.toml ./
COPY bridge_validator/src ./src
COPY bridge_validator/migrations ./migrations

# Copy .sqlx for offline mode (run 'cargo sqlx prepare' to generate if missing)
COPY bridge_validator/.sqlx ./.sqlx

ENV SQLX_OFFLINE=true
RUN cargo build --release --locked

# Runtime stage
# Multi-arch index for debian:bookworm-slim (resolved 2026-06-10;
# linux/amd64 manifest: sha256:b29f74a267526ae6ea104eed6c46133b0ca70ce812525df8cd5817698f0a624a).
FROM debian:bookworm-slim@sha256:0104b334637a5f19aa9c983a91b54c89887c0984081f2068983107a6f6c21eeb

WORKDIR /app

RUN apt-get update && \
    apt-get install -y ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/app/target/release/worker /app/worker

COPY --from=builder /usr/src/app/migrations /app/migrations

ENV RUST_LOG=info

# Run the binary
CMD ["/app/worker"]
