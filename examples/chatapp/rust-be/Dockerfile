# Build context is the REPO ROOT (forge/), because Cargo.toml has a path dep on the
# forge crate (`forge-rs = { path = "../../../" }`). Build with:
#   docker build -f examples/chatapp/rust-be/Dockerfile .
# The compose file in this directory sets `context: ../../..` for exactly this reason.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# The example ships its own Cargo.lock and pins `[workspace]` so it stands alone.
# Pin the output dir so the runtime stage finds the binary regardless of layout.
# forge compiles against its committed offline sqlx cache and the app uses runtime
# queries, so no database is needed at build time.
ENV CARGO_TARGET_DIR=/out
RUN cargo build --release --manifest-path examples/chatapp/rust-be/Cargo.toml

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /out/release/chatapp-rust-be /app/chatapp-rust-be
# Domain migrations live in src and bake into the binary (db::migrate runs on start),
# so nothing else needs copying.
ENV PORT=8081 BIND=0.0.0.0
EXPOSE 8081
HEALTHCHECK --interval=5s --timeout=3s --retries=20 \
  CMD curl -fsS http://127.0.0.1:8081/healthz || exit 1
CMD ["/app/chatapp-rust-be"]
