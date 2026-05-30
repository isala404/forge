FROM rust:1.92-slim-bookworm AS dev
WORKDIR /app
RUN cargo install cargo-watch --locked
RUN rustup target add wasm32-unknown-unknown
RUN cargo install dioxus-cli --version 0.7.3 --locked
RUN apt-get update && apt-get install -y curl pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

FROM rust:1.92-slim-bookworm AS frontend-builder
WORKDIR /app/examples/todo-dioxus/frontend
RUN rustup target add wasm32-unknown-unknown
RUN cargo install dioxus-cli --version 0.7.3 --locked
COPY examples/todo-dioxus/frontend/Cargo.toml examples/todo-dioxus/frontend/Dioxus.toml ./
COPY examples/todo-dioxus/frontend/.forge ./.forge
COPY examples/todo-dioxus/frontend/src ./src
RUN dx build --web --release

FROM rust:1.92-slim-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
COPY examples/todo-dioxus ./examples/todo-dioxus
COPY --from=frontend-builder /app/examples/todo-dioxus/frontend/dist ./examples/todo-dioxus/frontend/dist

WORKDIR /app/examples/todo-dioxus
RUN cargo build --release

FROM debian:bookworm-20250203-slim AS runtime
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/todo-dioxus /app/todo-dioxus
COPY --from=builder /app/examples/todo-dioxus/forge.toml /app/forge.toml
COPY --from=builder /app/examples/todo-dioxus/migrations /app/migrations
EXPOSE 8080
CMD ["/app/todo-dioxus"]
