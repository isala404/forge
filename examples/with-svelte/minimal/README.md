# minimal

Built with [FORGE](https://tryforge.dev). One binary, one database, everything else is just code.

## Development

```bash
docker compose up --build
```

Starts PostgreSQL, the Rust backend, and the SvelteKit frontend. All three.

- Frontend: http://localhost:9080
- Backend: http://localhost:9081
- PostgreSQL: localhost:5432

```bash
docker compose down          # stop everything
docker compose down -v       # stop + remove volumes
```

### Adding Features

```bash
forge add query list_items         # read data
forge add mutation create_item     # write data
forge add job process_item         # background work
forge add cron nightly_cleanup     # scheduled task
forge add workflow user_onboarding # multi-step process
```

Functions go in `src/functions/`. Types are generated for the frontend automatically.

### Useful Commands

```bash
forge generate              # regenerate TypeScript types from Rust models
forge check                 # validate config, migrations, and project health
forge migrate status        # check which migrations have run
forge migrate up            # apply pending migrations (forward-only)
```

### Running Tests

```bash
TEST_DATABASE_URL=postgres://localhost/test cargo test
```

## Production Build

```bash
cd frontend && bun install && bun run build && cd ..
cargo build --release
```

The release binary embeds the compiled frontend and the full runtime. One file to deploy. Point it at a PostgreSQL instance and it runs.

For Docker, VM, and other deployment options: [Deployment Guide](https://tryforge.dev/docs/ship/deploy)

## Project Structure

```
minimal/
├── src/
│   ├── main.rs              # Entry point
│   ├── schema/              # Data models (Rust types that generate TS types)
│   └── functions/           # Queries, mutations, jobs, crons, workflows
├── migrations/              # SQL migrations (applied on startup)
├── frontend/                # SvelteKit app
├── forge.toml               # Runtime configuration
├── docker-compose.yml       # Development environment
└── Dockerfile               # Production image
```

## Debugging

**Logs**: Set `log_level = "debug"` in `forge.toml` under `[observability]`, or run with `RUST_LOG=debug`. Queries slower than 500ms are warned automatically.

**Health check**: `GET /health` (liveness) and `GET /ready` (checks DB + realtime reactor).

**Inspect jobs and workflows** directly in PostgreSQL:

```sql
-- failed jobs
SELECT job_type, last_error, attempts FROM forge_jobs
WHERE status = 'failed' ORDER BY failed_at DESC;

-- active workflows
SELECT workflow_name, status, current_step FROM forge_workflow_runs
WHERE status IN ('created', 'running');

-- recent cron runs
SELECT cron_name, status, error FROM forge_cron_runs
ORDER BY scheduled_time DESC LIMIT 10;
```

**Realtime not updating?** Check that the SSE connection is open (network tab, `/events` endpoint) and that reactivity is enabled on the table (`SELECT forge_enable_reactivity('table_name');`). Don't call `refetch()` after mutations, the SSE pipeline handles it.

**Traces**: FORGE exports OpenTelemetry spans over HTTP. Point `otlp_endpoint` in `forge.toml` at your collector (Jaeger, Grafana, etc.).

## What You Get

This is a FORGE project. That means your single binary handles API serving, background jobs with retries and progress tracking, cron scheduling with leader election, durable workflows that survive restarts, real-time subscriptions via SSE, webhook processing, and MCP tool endpoints. All coordinated through PostgreSQL.

No Redis. No message queues. No separate worker processes.

## AI Agents

If you're using an AI coding agent, install the `forge-idiomatic-engineer` skill for Forge-aware code generation:

```bash
bunx skills add https://github.com/isala404/forge/tree/main/docs/skills/forge-idiomatic-engineer
```

[Documentation](https://tryforge.dev/docs)
