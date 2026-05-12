# Build from repository root with: docker build -f bridge_validator/Dockerfile -t bridge-validator .
# Requires .sqlx directory for offline mode: cargo sqlx prepare

# Build stage
FROM rust:slim-bookworm AS builder

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
FROM debian:bookworm-slim

WORKDIR /app

RUN apt-get update && \
    apt-get install -y ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/app/target/release/worker /app/worker

COPY --from=builder /usr/src/app/migrations /app/migrations

ENV RUST_LOG=info

# Run the binary
CMD ["/app/worker"]
