FROM rust:1.92-slim-bookworm AS dev
WORKDIR /app
RUN cargo install cargo-watch --locked
RUN apt-get update && apt-get install -y curl pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

FROM rust:1.92-slim-bookworm AS frontend-builder
WORKDIR /app/frontend
RUN rustup target add wasm32-unknown-unknown
RUN cargo install dioxus-cli --version 0.7.3 --locked
COPY frontend/Cargo.toml frontend/Dioxus.toml ./
COPY frontend/src ./src
RUN dx build --web --release

FROM rust:1.92-slim-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY migrations ./migrations
COPY forge.toml sqlx.toml build.rs ./
COPY frontend ./frontend
COPY --from=frontend-builder /app/frontend/dist ./frontend/dist

RUN cargo build --release

FROM debian:bookworm-20250203-slim AS runtime
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/forge-dioxus-minimal-template /app/forge-dioxus-minimal-template
COPY forge.toml /app/forge.toml
COPY migrations /app/migrations
EXPOSE 8080
CMD ["/app/forge-dioxus-minimal-template"]
