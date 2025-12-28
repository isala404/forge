# FORGE Local Testing Guide

End-to-end guide to test FORGE from source.

## Prerequisites

- Rust (stable): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- Bun: `curl -fsSL https://bun.sh/install | bash`
- Docker
- macOS only: `brew install libiconv`

## Step 1: Start PostgreSQL

```bash
docker rm -f forge-dev-db 2>/dev/null; docker run -d \
  --name forge-dev-db \
  -e POSTGRES_DB=forge_dev \
  -e POSTGRES_USER=forge \
  -e POSTGRES_PASSWORD=forge \
  -p 5432:5432 \
  postgres:16-alpine
```

## Step 2: Install FORGE CLI

```bash
cd /path/to/forge
LIBRARY_PATH="/opt/homebrew/opt/libiconv/lib" cargo install --path crates/forge
```

Verify: `~/.cargo/bin/forge --version`

## Step 3: Create Demo Project

```bash
cd ~/Desktop
~/.cargo/bin/forge new my-app
cd my-app
```

## Step 4: Link to Local Source

Edit `Cargo.toml` - replace `forge = "0.1"` with:

```toml
forge = { path = "/path/to/forge/crates/forge" }
```

## Step 5: Install Frontend Dependencies

```bash
cd frontend && bun install && cd ..
```

## Step 6: Run Backend

```bash
LIBRARY_PATH="/opt/homebrew/opt/libiconv/lib" cargo run
```

Output:
```
⚒️  FORGE v0.1.0
🌐 Listening on http://127.0.0.1:8080
📊 Dashboard at http://127.0.0.1:8080/_dashboard
```

## Step 7: Run Frontend (new terminal)

```bash
cd ~/Desktop/my-app/frontend
bun dev
```

Output:
```
VITE ready
➜ Local: http://localhost:5173/
```

## Step 8: Verify

| URL | What |
|-----|------|
| http://localhost:5173 | Frontend UI |
| http://localhost:8080/_dashboard | Admin dashboard |
| http://localhost:8080/health | Health check |

Test API:
```bash
# List users
curl -X POST http://localhost:8080/rpc/get_users -H "Content-Type: application/json"

# Create user
curl -X POST http://localhost:8080/rpc/create_user -H "Content-Type: application/json" \
  -d '{"email":"test@example.com","name":"Test"}'
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `forge new <name>` | Create project |
| `forge add model <name>` | Add model |
| `forge add query <name>` | Add query |
| `forge add mutation <name>` | Add mutation |
| `forge add action <name>` | Add action |
| `forge add job <name>` | Add background job |
| `forge add cron <name>` | Add scheduled task |
| `forge add workflow <name>` | Add workflow |
| `forge generate` | Regenerate TypeScript types |
| `forge run` | Run server |

## Adding a Migration

```bash
cat > migrations/0002_create_products.sql << 'EOF'
CREATE TABLE IF NOT EXISTS products (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    price DECIMAL(10, 2) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
SELECT forge_enable_reactivity('products');
EOF
```

Restart backend to apply.

## Troubleshooting

**Database connection refused**
```bash
docker rm -f forge-dev-db 2>/dev/null; docker run -d \
  --name forge-dev-db \
  -e POSTGRES_DB=forge_dev \
  -e POSTGRES_USER=forge \
  -e POSTGRES_PASSWORD=forge \
  -p 5432:5432 \
  postgres:16-alpine
```

**libiconv linking error (macOS)**
```bash
brew install libiconv
LIBRARY_PATH="/opt/homebrew/opt/libiconv/lib" cargo build
```

**WebSocket not connecting**
- Backend must be running on port 8080
- Check browser console for errors
- Verify `http://localhost:8080/health` returns OK
