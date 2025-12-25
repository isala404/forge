# Configuration Reference

> *Complete forge.toml reference*

---

## Example Configuration

```toml
# forge.toml

[project]
name = "my-app"
version = "0.1.0"

[database]
url = "postgres://localhost/my_app"
pool_size = 50
pool_timeout = "30s"

[node]
roles = ["gateway", "function", "worker", "scheduler"]
worker_capabilities = ["general"]

[gateway]
port = 8080
max_connections = 10000

[function]
max_concurrent = 1000
timeout = "30s"

[worker]
max_concurrent_jobs = 50
job_timeout = "1h"

[cluster]
name = "production"
discovery = "postgres"

[observability]
metrics_enabled = true
logging_enabled = true
tracing_enabled = true
dashboard_enabled = true

[security]
secret_key = "${FORGE_SECRET}"
```

---

## Sections

### [project]

```toml
[project]
name = "my-app"          # Project name
version = "0.1.0"        # Version
```

### [database]

```toml
[database]
url = "postgres://..."   # Connection string (required)
pool_size = 50           # Connection pool size
pool_timeout = "30s"     # Pool checkout timeout
statement_timeout = "30s" # Query timeout

# Read replicas for scaling reads
replica_urls = [
    "postgres://user:pass@replica1:5432/myapp",
    "postgres://user:pass@replica2:5432/myapp"
]
read_from_replica = true  # Route read queries to replicas
```

### Connection Pool Isolation

To prevent connection starvation (where slow analytics queries or stuck workflows block mutations), configure separate pools:

```toml
[database.pools]
# Default pool for queries/mutations
default = { size = 30, timeout = "30s" }

# Pool for background jobs (isolated from user-facing operations)
jobs = { size = 15, timeout = "60s" }

# Pool for observability writes (if using same database)
observability = { size = 5, timeout = "5s" }

# Pool for long-running analytics
analytics = { size = 5, timeout = "300s", statement_timeout = "5m" }
```

Alternatively, use separate databases entirely:

```toml
[database]
url = "postgres://user:pass@primary-db:5432/myapp"

[jobs]
database_url = "postgres://user:pass@jobs-db:5432/jobs"
pool_size = 20

[observability]
database_url = "postgres://user:pass@observability-db:5432/observability"
pool_size = 10
```

### [node]

```toml
[node]
roles = ["gateway", "function", "worker", "scheduler"]
worker_capabilities = ["general", "media"]
```

### [gateway]

```toml
[gateway]
port = 8080
grpc_port = 9000
max_connections = 10000
request_timeout = "30s"
```

### [function]

```toml
[function]
max_concurrent = 1000
timeout = "30s"
memory_limit = "512Mi"
```

### [worker]

```toml
[worker]
max_concurrent_jobs = 50
job_timeout = "1h"
poll_interval = "100ms"
```

### [cluster]

```toml
[cluster]
name = "production"
discovery = "postgres"  # postgres, dns, kubernetes, static
heartbeat_interval = "5s"
dead_threshold = "15s"
```

### [observability]

```toml
[observability]
metrics_enabled = true
logging_enabled = true
tracing_enabled = true

[observability.logging]
level = "info"
slow_query_threshold = "100ms"

[observability.metrics]
flush_interval = "10s"
```

### [security]

```toml
[security]
secret_key = "${FORGE_SECRET}"

[security.auth]
jwt_secret = "${JWT_SECRET}"
session_ttl = "7d"
```

---

## Environment Variable Substitution

Use `${VAR}` syntax:

```toml
[database]
url = "${DATABASE_URL}"

[security]
secret_key = "${FORGE_SECRET}"
```

---

## Related Documentation

- [CLI](CLI.md) — Command reference
- [Security](SECURITY.md) — Security settings
