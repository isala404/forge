# chatapp-rust-be

The Rust GraphQL backend for the chatapp example. A pure API server (no SSR, no
templates): axum 0.8 + async-graphql 7 (code-first, with DataLoader) over the
[Forge](../../../) primitive library, sharing Forge's single Postgres pool.

It serves exactly the canonical [`schema.graphql`](../schema.graphql); parity is
enforced by a test that compares the emitted SDL to the canonical file under a
normalized comparison (sorted types + fields, collapsed whitespace, descriptions
ignored).

## Endpoints

- `POST /graphql`: queries and mutations.
- `GET  /graphql`: subscriptions over `graphql-transport-ws` (WebSocket upgrade).
- `GET  /healthz`: liveness.
- `/_forge/blob/*`: Forge's presigned blob router (where attachment URLs point).

Both GraphQL endpoints are Bearer-authenticated. The token is sent as
`Authorization: Bearer <token>` on HTTP and as `connectionParams.authorization`
(`"Bearer <token>"`) on the WS socket. A bearer is accepted if it validates as either a
Forge session (idle timeout slides on use) or an API key (`fk_…`, mapped to its
owner). `me` returns null when unauthenticated; every other authed resolver raises
`UNAUTHENTICATED`. CORS is permissive (dev).

## Architecture

This example intentionally co-locates its domain tables with Forge's system tables and
reuses `forge.pool()` to keep the demo to one Postgres database. Production apps can keep
their application database separate and use Forge only for its `forge_*` tables/primitives.

| file | responsibility |
| --- | --- |
| `src/db.rs` | the domain tables (`users`/`chats`/`chat_members`/`messages`/`receipts`) over Forge's pool, including the batched reads that back the loaders |
| `src/loaders.rs` | async-graphql `DataLoader`s: user-by-id, chat members, last message, unread, message receipts, online presence, one batched round-trip per field per request |
| `src/gql/` | the code-first schema, split by role: `types.rs` (output objects), `query.rs`, `mutation.rs`, `subscription.rs`, `helpers.rs` (auth + error mapping), `mod.rs` (schema/SDL builders + the SDL-parity test) |
| `src/context.rs` | shared app context, the realtime `Event` envelope, kv helpers (presence/unread/typing), config, DLQ gauge |
| `src/worker.rs` | in-process queue workers: `fanout` (deliver + unread, idempotent), `reap` (delete disappearing messages), `fail` (always nacks → DLQ demo) |
| `src/http.rs` | the axum router, Bearer auth at the edge (HTTP header + WS `connection_init`), CORS |
| `src/main.rs` | `Forge::init` (migrates `forge_*`), `db::migrate` (the domain tables), spawns workers + a scheduler/maintenance tick, serves axum |

Every relational GraphQL field resolves through a DataLoader, so a query selecting N
messages issues O(1) batched queries per field, not O(N).

### Forge primitive → feature

`auth` → sessions, API keys, password hashing · `blob` → attachments (presigned direct
upload + download) · `pubsub` → all realtime · `queue` → fanout + reap + the DLQ demo ·
`kv` → presence, unread counters, typing · `schedule` → disappearing-message expiry ·
`ratelimit` → login/signup (fail closed) + send (fail open) · `config` → the
`reactions_v2` flag and `max_upload_bytes`.

## Run

Forge configures itself from `forge.toml` in this directory: with no configuration it
boots an embedded Postgres (data persists in `.forge/pg`); a set `FORGE_POSTGRES_URL`
(interpolated by that file) wins when you'd rather use your own server. Forge migrates
its own `forge_*` tables on `init`; the app applies `migrations.sql` on startup
against the same database (it follows `forge.pool()`).

```sh
cargo run                     # no database needed: boots an embedded Postgres
# -> listening on http://127.0.0.1:8081

# ...or against your own server:
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/chatapp_rust \
FORGE_BLOB_SIGNING_SECRET=dev-secret-change-me \
cargo run

cargo run -- --print-schema   # emit the SDL (no database needed)
```

### Environment

`FORGE_POSTGRES_URL` and `FORGE_BLOB_SIGNING_SECRET` are referenced by `forge.toml`
rather than read by Forge directly. The Postgres URL defaults empty so embedded
mode can boot; the signing secret has a local default. The rest configure the
HTTP server and app loops.

| var | default | meaning |
| --- | --- | --- |
| `FORGE_POSTGRES_URL` | unset (embedded Postgres) | Postgres DSN (shared by Forge + the app); wins over `embedded` |
| `PORT` | `8081` | listen port |
| `BIND` | `127.0.0.1` | bind address (set `0.0.0.0` in containers) |
| `FORGE_BLOB_SIGNING_SECRET` | `dev-secret-change-me` | HMAC secret for presigned blob URLs |
| `APP_PRESENCE_TTL_SECS` | `30` | presence kv TTL; the key lapsing marks a user offline |
| `APP_DISAPPEARING_SECS` | `86400` | disappearing-message lifetime, snapshotted at toggle time |
| `APP_SCHEDULER_MS` | `30000` | scheduler + maintenance tick interval |
| `CORS_ORIGIN` | `*` | reserved; CORS is permissive in dev |

## Test

```sh
cargo test
```

`cargo test` runs the unit tests (SDL parity, error-code mapping, credential
validation) and one integration suite (`tests/integration.rs`). The integration suite
creates a uniquely-named Postgres database at
`postgres://postgres:forge@127.0.0.1:5432`, boots the real binary against it on an
ephemeral port, drives the GraphQL API over HTTP and WS through every integration scenario
(signup → session; two users see a group chat; live message over a subscription; typing
event; presence online → offline via kv TTL; attachment presign → PUT → send → download;
unread increments then clears on `markRead`; read receipt turns read; rate-limit throttles
a send burst; disappearing message vanishes; reactions flag toggles; API key
authenticates; `opsStats` reflects online + DLQ; `logoutAll` revokes other sessions),
then drops the database. No skips.

## Gates

```sh
cargo build
cargo clippy --all-targets -- -D warnings
cargo test
```

`clippy.toml` empties the repo-root ban on runtime `sqlx::query*` (this example
intentionally uses runtime queries against Forge's shared pool).
