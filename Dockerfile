# syntax=docker/dockerfile:1
# Multi-stage build for the IONe server binary.
# IONe uses runtime sqlx queries (no compile-time query! macros), so no database
# is needed at build time. Migrations are embedded via sqlx::migrate!("./migrations")
# and run on startup against DATABASE_URL.

FROM rust:1-bookworm AS builder
WORKDIR /build
# aws-lc-sys (pulled by rustls/aws-lc-rs) needs cmake + clang; libssl-dev for lettre's native-tls.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config cmake clang libssl-dev \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release --bin ione

FROM debian:bookworm-slim
# ca-certificates for outbound HTTPS (USGS feed); libssl3 for lettre native-tls at runtime.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/target/release/ione /usr/local/bin/ione
COPY --from=builder /build/static ./static
ENV IONE_STATIC_DIR=/app/static
EXPOSE 3000
CMD ["ione"]
