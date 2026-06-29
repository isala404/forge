# Build context is the REPO ROOT — node-be has path deps on bindings/forge-node:
#   docker build -f examples/chatapp/node-be/Dockerfile .
#
# The forge-node native addon is compiled here with napi-rs (needs Rust); the
# repo-root .dockerignore strips committed *.node binaries, so we always rebuild it.
FROM node:22-bookworm

RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal \
 && npm install -g bun
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /src
COPY . .

WORKDIR /src/bindings/forge-node
RUN npm install && npm run build

WORKDIR /src/examples/chatapp/node-be
RUN bun install

ENV PORT=8082
ENV HOST=0.0.0.0
EXPOSE 8082
HEALTHCHECK --interval=5s --timeout=3s --retries=20 \
  CMD curl -fsS http://127.0.0.1:8082/healthz || exit 1
CMD ["node", "src/server.ts"]
