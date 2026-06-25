# Choosing backends: Postgres everywhere, filesystem blob

By default, Forge runs on a single Postgres database: every primitive — kv, queue, blob, auth, config, ratelimit, schedule, pubsub — is Postgres-backed. Two non-Postgres options exist today: where blob *bytes* live (a `BYTEA` column by default, or a local filesystem directory — metadata always stays in Postgres either way), and an in-process `kv` backend for state that doesn't need to survive a restart. This page shows how to select a backend two ways (`ForgeConfig` and `FORGE_*` env) and how to print a `backend_report()` for a health page.

## The default: Postgres everything

`Forge::init` with a plain `ForgeConfig` gives you Postgres for all eight primitives, with blob bytes in `forge_blobs.data` (`BYTEA`). This is the atomic choice: a blob `put` commits in the same SQL transaction machinery as the rest of your app.

```rust
use forge::{Forge, ForgeConfig};

#[tokio::main]
async fn main() -> forge::Result<()> {
    let forge = Forge::init(ForgeConfig::new("postgres://localhost/myapp")).await?;
    // blob().put / get / head / delete / list all work; bytes live in BYTEA.
    forge.blob().put("hello.txt", b"hi".to_vec().into(), Default::default()).await?;
    Ok(())
}
```

## Filesystem blob via `ForgeConfig`

When you don't want large objects inflating the WAL, store the bytes on disk. Metadata still goes to Postgres; only the object body moves. Build a `ForgeConfig` — two ways to set the backend:

```rust
use forge::{BlobBackendConfig, ForgeConfig};

// Convenience method:
let cfg = ForgeConfig::new("postgres://localhost/myapp")
    .with_filesystem_blob("/var/lib/app/blobs") // directory; created if missing
    .with_blob_signing_secret("change-me");      // optional; enables presign + blob_router

// Or the explicit enum (same result):
let cfg = ForgeConfig::new("postgres://localhost/myapp")
    .with_blob_backend(BlobBackendConfig::Filesystem {
        root: "/var/lib/app/blobs".into(),
    });
```

Pass it to `Forge::init(cfg).await?`. Every knob is a `with_*` method on `ForgeConfig`.

`BlobBackendConfig` is `#[non_exhaustive]` with two variants today: `Postgres` (the `Default`) and `Filesystem { root: PathBuf }`. It's an enum precisely so a future S3/R2/GCS backend is a non-breaking variant add, not a redesign.

## Env-based selection (portable across bindings)

`ForgeConfig::from_env()` reads `FORGE_*` variables. This is the path you want for config that's identical across the Rust, Node, and Python bindings — the same env vars drive all three with no per-language API.

```rust
use forge::{Forge, ForgeConfig};

#[tokio::main]
async fn main() -> forge::Result<()> {
    let forge = Forge::init(ForgeConfig::from_env()?).await?;
    Ok(())
}
```

The variables `from_env` reads:

| Variable | Effect |
|----------|--------|
| `FORGE_POSTGRES_URL` | **Required.** Connection string. |
| `FORGE_MAX_CONNECTIONS` | Pool size (must parse as a number, else `Config` error). |
| `FORGE_KV_NAMESPACE` | Key prefix shared by kv, ratelimit, blob. |
| `FORGE_BLOB_SIGNING_SECRET` | HMAC secret; enables presign + `blob_router`. |
| `FORGE_BLOB_BASE_URL` | URL prefix the blob router is mounted at (default `/_forge/blob`). |
| `FORGE_BLOB_BACKEND` | `postgres` (default) or `filesystem`/`fs`. |
| `FORGE_BLOB_FS_ROOT` | **Required when** `FORGE_BLOB_BACKEND=filesystem`. The blob directory. |

So a filesystem-blob deployment is:

```sh
FORGE_POSTGRES_URL=postgres://localhost/myapp
FORGE_BLOB_BACKEND=filesystem
FORGE_BLOB_FS_ROOT=/var/lib/app/blobs
```

If `FORGE_BLOB_BACKEND=filesystem` but `FORGE_BLOB_FS_ROOT` is unset, `from_env` returns a `Config` error naming the missing var. Note `from_env` does **not** read other knobs like `acquire_timeout` or the queue windows — set those in code on the returned `ForgeConfig` if you need them.

## Health page: `backend_report()`

`Forge::backend_report()` returns a `BackendReport` — a snapshot of which provider powers each primitive, whether it's durable, and any caveats. It's for logs, health pages, and debugging; ordinary request handling never needs it (the backend choice must not leak into app logic). `BackendReport` implements `Display`, so the quick version is just printing it:

```rust
println!("{}", forge.backend_report());
```

For a JSON health endpoint, walk `report.backends` (a `Vec<BackendInfo>`) yourself:

```rust
use forge::BackendReport;

fn health_json(report: &BackendReport) -> serde_json::Value {
    serde_json::json!({
        "backends": report.backends.iter().map(|b| serde_json::json!({
            "primitive": b.primitive.as_str(), // "kv", "queue", "blob", ...
            "provider": b.provider,             // "postgres" or "filesystem"
            "durable": b.durable,
            "caveats": b.caveats,               // "none" when there are none
        })).collect::<Vec<_>>()
    })
}
```

`BackendInfo` fields are `primitive: Primitive`, `provider: &'static str`, `durable: bool`, `caveats: &'static str`. With the default config, every line reads `provider=postgres`. Switch blob to filesystem and the blob line reads `provider=filesystem` with caveats `local-dir, shared-mount-for-multi-replica, put-not-atomic-with-app-sql`. The Postgres pubsub line always carries the caveat `at-most-once, non-durable` regardless of blob choice.

## Adding a backend

Second backends are added *inside* Forge, not bolted on from outside: the primitive traits (`Kv`, `Queue`, …) are **sealed** — public to call, implementable only within this crate — so a backend choice can never leak into application code. To add one you implement the primitive trait plus a `BackendLifecycle` (its `name`, `primitive`, `durable`, `caveats`, and an overridden `maintain` if it has anything to sweep), then wire it into `Forge::init`. The filesystem blob backend (`src/blob/fs.rs`) is the worked example: same `Blob` contract, different byte storage; the in-process `kv` backend (`src/kv/memory.rs`) is a second. Postgres still backs every primitive by default, and additional in-process backends are landing incrementally.

## Gotchas and contract guarantees

- **Filesystem blob trades atomicity for a smaller WAL.** With Postgres blob, a `put` is atomic with surrounding app SQL. With filesystem blob, the bytes are written to disk while metadata commits to Postgres — the `put` is *not* atomic with your app's SQL, and a multi-replica deploy needs a shared mount so every replica sees the same directory. `Forge::maintain()` runs the filesystem orphan sweep (reclaiming files whose metadata rows are gone); call `maintain` on a schedule either way.
- **Forge migrates its system database at startup.** Forge requires its own Postgres database — the *system database*, separate from your application's — and owns it entirely. `init` connects and applies the embedded `forge_*` migrations before returning. It's idempotent and safe to run concurrently across replicas: an advisory lock serializes it and checksums guard immutability. Each distinct feature database (see below) is migrated the same way.
- **`max_connections >= 2`.** The migration runner holds one connection for the advisory lock while drawing a second to run the SQL, so `max_connections == 1` would deadlock until the acquire timeout. `validate()` rejects that up front. A same-server feature bulkhead pool, which the system pool migrates rather than the pool itself, is exempt and may be size 1.
- **Misconfiguration fails at `init`, never lazily.** `validate()` checks the statically-checkable fields (empty DSN, `kv_namespace` containing `:`, empty filesystem root) and returns `ForgeError::Config`; connection and migration failures surface in `init`. Nothing fails on first primitive use.
- **Presigning is optional and independent of the blob backend.** `init` succeeds with no `blob_signing_secret`, and the full CRUD surface (`put`/`get`/`head`/`delete`/`list`) works. The secret is required only by `presign_upload`, `presign_download`, `verify_presigned`, and `blob_router()`; calling any of those without it returns `Config` at that call. `blob_router()` is feature-gated behind `blob-router` and serves presigned URLs against the Postgres backend.
- **`backend_report()` is observability, not control flow.** It exists so a health page can show what's powering each slot. Do not branch app logic on the provider name — the whole point of the seam is that swapping the blob backend changes no app code.

### Node / Python bindings

The bindings have no per-language backend-selection API by design. They construct Forge from the same `FORGE_*` environment variables `ForgeConfig::from_env` reads, so `FORGE_BLOB_BACKEND=filesystem` + `FORGE_BLOB_FS_ROOT=...` selects filesystem blob identically across Rust, Node, and Python. Rust is canonical; the env path is the portable common denominator. A binding that serves presigned URLs itself (rather than mounting the Rust `blob_router`) must enforce `verify_presigned` and set `Content-Disposition: attachment` + `X-Content-Type-Options: nosniff` on downloads, per the blob contract.
