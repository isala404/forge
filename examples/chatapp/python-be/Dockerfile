# Build context is the REPO ROOT — this project has path deps on bindings/forge-py:
#   docker build -f examples/chatapp/python-be/Dockerfile .
#
# Single stage: uv builds the editable forgelib wheel with maturin, which needs the Rust
# toolchain, so the runtime image carries Python + Rust. Keeps the uv venv in place.
# Migrations run on startup from app.main's lifespan (db.migrate) — no build-time DB.
FROM python:3.13-bookworm
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal \
 && pip install --no-cache-dir uv
ENV PATH="/root/.cargo/bin:${PATH}"
WORKDIR /src
COPY . .
WORKDIR /src/examples/chatapp/python-be
RUN uv sync --no-dev
ENV PORT=8083
EXPOSE 8083
HEALTHCHECK --interval=5s --timeout=3s --retries=20 \
  CMD curl -fsS http://127.0.0.1:8083/healthz || exit 1
CMD ["uv", "run", "--no-sync", "uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8083"]
