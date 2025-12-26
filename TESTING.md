# FORGE Local Testing Guide

This guide explains how to build the FORGE CLI from source, scaffold a test application, and link it back to your local repository to verify functionality end-to-end.

## 1. Prerequisites

Ensure you have the following installed:
- **Rust** (latest stable): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Bun** (frontend runtime): `curl -fsSL https://bun.sh/install | bash`
- **Docker** (for PostgreSQL): Used for the database.
- **Git**

## 2. Build and Install the CLI

First, we need to compile the `forge` binary from the `crates/forge` directory and install it to your path so you can run the `forge` command globally.

```bash
# From the root of the forge repo
cargo install --path crates/forge
```

Verify installation:
```bash
forge --version
```

## 3. Create a Test Project

Create a new directory for your test/playground (outside the forge repo to simulate a real user environment).

```bash
cd ..
forge new demo-app
cd demo-app
```

## 4. Link Dependencies (Critical for Local Dev)

Since FORGE isn't published yet, the scaffolded project will try to download dependencies that don't exist. You must patch them to point to your local source code.

### Backend (`Cargo.toml`)

Open `demo-app/Cargo.toml` and modify the `[dependencies]` section to point to your local `forge` crate path.

*Assuming your source repo is at `../forge`:*

```toml
[dependencies]
# Replace the version number with a path
forge = { path = "../forge/crates/forge" }

# Keep other dependencies
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

### Frontend (`frontend/package.json`)

Open `demo-app/frontend/package.json` and modify the dependency to point to the local frontend library.

*Assuming your source repo is at `../../forge`:*

```json
"dependencies": {
  "@forge/svelte": "file:../../forge/frontend"
}
```

Then install frontend dependencies:

```bash
cd frontend
bun install
cd ..
```

## 5. Start the Database

Start a PostgreSQL instance for your demo app using Docker.

```bash
docker run -d \
  --name forge-demo-db \
  -e POSTGRES_DB=forge_dev \
  -e POSTGRES_USER=forge \
  -e POSTGRES_PASSWORD=forge \
  -p 5432:5432 \
  postgres:16
```

Ensure your `forge.toml` in `demo-app/` matches these credentials (the default usually does):

```toml
[database]
url = "postgres://forge:forge@localhost:5432/forge_dev"
```

## 6. Run the Application

You will need two terminal windows.

### Terminal 1: Backend

Run the Rust server. This will compile your app, apply migrations automatically, and start the server.

```bash
# Inside demo-app/
cargo run
```

*Expected output:*
```
  ⚒️  FORGE v0.1.0

  🌐 Listening on http://127.0.0.1:8080
  📊 Dashboard at http://127.0.0.1:8080/_dashboard
```

### Terminal 2: Frontend

Start the Svelte development server.

```bash
# Inside demo-app/frontend/
bun run dev
```

*Expected output:*
```
  VITE v6.0.0  ready in 200 ms

  ➜  Local:   http://localhost:5173/
```

## 7. Verify End-to-End Functionality

### 1. Access the Dashboard
Open **http://localhost:8080/_dashboard** in your browser.
- You should see the FORGE dashboard.
- Go to the **Schema** tab to see the default `User` model.
- Go to the **Migrations** tab to see that the initial migration was applied.

### 2. Modify Schema (Hot Reload Test)
Open `demo-app/src/schema/user.rs` and add a field:

```rust
#[forge::model]
pub struct User {
    #[id]
    pub id: Uuid,

    // ... existing fields ...

    // Add this:
    pub is_active: bool,
}
```

Save the file. Watch Terminal 1 (Backend).
- It should detect the change.
- It should automatically generate a migration for `is_active`.
- It should apply the migration.
- It should regenerate TypeScript types.

### 3. Check Frontend Types
Open `demo-app/frontend/src/lib/forge/types.ts`.
- You should see `isActive: boolean;` added to the `User` interface automatically.

### 4. Test the App
Open **http://localhost:5173**.
- You should see the default Svelte app running.
- It is fetching data from the backend using the generated RPC client.

## 8. Development Workflow Commands

While running the app, you can use the CLI to scaffold new features.

**Add a new API Query:**
```bash
forge add query get_active_users
```
*Check `src/functions/get_active_users.rs`.*

**Add a Background Job:**
```bash
forge add job send_welcome_email
```
*Check `src/functions/send_welcome_email_job.rs`.*

**Generate Client Code Manually:**
If types get out of sync:
```bash
# From demo-app/ root
# Note: Since you patched Cargo.toml, you might need to run via cargo if the installed CLI differs significantly from source
forge generate
```

## Troubleshooting

**"Dependency not found" errors:**
Double-check the relative paths in `Cargo.toml` and `package.json`. They must point to the *root* of the crates or package directories in your source repo.

**Database connection refused:**
Ensure the Docker container is running: `docker ps`. If the port is taken, change it in the docker run command and update `forge.toml`.

**Frontend connection error:**
Ensure `demo-app/frontend/src/routes/+layout.svelte` points to the correct backend URL (default `http://localhost:8080`).
