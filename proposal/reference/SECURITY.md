# Security

> *Authentication, authorization, and data protection*

---

## Authentication

### JWT Integration

```toml
# forge.toml
[security.auth]
provider = "jwt"
jwt_secret = "${JWT_SECRET}"
jwt_algorithm = "HS256"
```

```rust
#[forge::query]
pub async fn get_profile(ctx: &QueryContext) -> Result<User> {
    let user = ctx.auth.require_user()?;  // Errors if not authenticated
    ctx.db.get::<User>(user.id).await
}
```

### External Providers

```toml
# forge.toml
[security.auth]
provider = "oauth"

[security.auth.providers.google]
client_id = "${GOOGLE_CLIENT_ID}"
client_secret = "${GOOGLE_CLIENT_SECRET}"

[security.auth.providers.github]
client_id = "${GITHUB_CLIENT_ID}"
client_secret = "${GITHUB_CLIENT_SECRET}"
```

---

## Authorization

### Function-Level

```rust
#[forge::mutation]
#[require_auth]  // Must be logged in
pub async fn create_project(...) -> Result<Project> { }

#[forge::mutation]
#[require_role("admin")]  // Must be admin
pub async fn delete_user(...) -> Result<()> { }

#[forge::query]
#[public]  // No auth required
pub async fn get_public_stats(...) -> Result<Stats> { }
```

### Row-Level Security

```rust
#[forge::model]
#[tenant(field = "organization_id")]
pub struct Project {
    pub id: Uuid,
    pub organization_id: Uuid,  // Tenant isolation
    pub name: String,
}

// Queries automatically filtered by tenant
#[forge::query]
pub async fn get_projects(ctx: &QueryContext) -> Result<Vec<Project>> {
    // Automatically adds: WHERE organization_id = <current_org>
    ctx.db.query::<Project>().fetch_all().await
}
```

---

## Data Protection

### Encryption at Rest

```rust
#[forge::model]
pub struct User {
    pub id: Uuid,
    pub email: Email,
    
    #[encrypted]  // Encrypted in database
    pub ssn: String,
    
    #[encrypted]
    pub api_key: String,
}
```

### Field-Level Encryption

```toml
# forge.toml
[security.encryption]
key = "${ENCRYPTION_KEY}"
algorithm = "AES-256-GCM"
```

### Encryption Key Rotation

**The Problem:** Rotating encryption keys requires decrypting all data with the old key and re-encrypting with the new key. On large tables, this causes significant downtime.

**Solution: Envelope Encryption with Key Versioning**

FORGE uses envelope encryption—data is encrypted with a Data Encryption Key (DEK), and the DEK is encrypted with the Key Encryption Key (KEK). This allows key rotation without re-encrypting all data.

```toml
# forge.toml
[security.encryption]
# Current key version (used for new encryptions)
current_key_version = 2

# All keys (old keys needed for decryption during migration)
[security.encryption.keys]
1 = "${ENCRYPTION_KEY_V1}"  # Old key
2 = "${ENCRYPTION_KEY_V2}"  # Current key
```

**How it works:**
1. Encrypted fields store: `version:nonce:ciphertext`
2. Decryption uses the version to select the correct key
3. New writes always use `current_key_version`
4. Old data is migrated gradually in the background

### Key Rotation Procedure

```bash
# 1. Generate new key
forge security generate-key > new_key.txt

# 2. Add new key to config (don't remove old yet!)
export ENCRYPTION_KEY_V2=$(cat new_key.txt)

# 3. Update forge.toml
[security.encryption]
current_key_version = 2
[security.encryption.keys]
1 = "${ENCRYPTION_KEY_V1}"
2 = "${ENCRYPTION_KEY_V2}"

# 4. Deploy with new config (new writes use v2, reads work with v1 or v2)

# 5. Run background migration (re-encrypts with new key)
forge security rotate-keys --batch-size 1000 --delay 100ms

# Monitor progress
forge security rotation-status

# 6. After migration completes (days/weeks later), remove old key
```

### Zero-Downtime Migration

The migration runs as a background job with configurable throttling:

```toml
# forge.toml
[security.key_rotation]
batch_size = 1000          # Rows per batch
batch_delay = "100ms"      # Delay between batches
max_concurrent = 2         # Parallel migrations
priority = "low"           # Job priority (don't compete with user workloads)
```

```sql
-- Monitor rotation progress
SELECT
    table_name,
    count(*) FILTER (WHERE key_version = 1) as v1_count,
    count(*) FILTER (WHERE key_version = 2) as v2_count,
    round(count(*) FILTER (WHERE key_version = 2)::numeric / count(*) * 100, 2) as pct_migrated
FROM encrypted_field_metadata
GROUP BY table_name;
```

---

## Rate Limiting

```rust
#[forge::mutation]
#[rate_limit(requests = 10, per = "minute", key = "user")]
pub async fn sensitive_action(ctx: &MutationContext) -> Result<()> { }

#[forge::action]
#[rate_limit(requests = 100, per = "hour", key = "ip")]
pub async fn public_api(ctx: &ActionContext) -> Result<()> { }
```

### Rate Limiting Backend

Rate limit state must be shared across all cluster nodes. FORGE supports multiple backends:

```toml
# forge.toml
[security.rate_limiting]
# Backend options: "redis", "postgres", "memory"
backend = "redis"

[security.rate_limiting.redis]
url = "${REDIS_URL}"
key_prefix = "forge:ratelimit:"
```

| Backend | Accuracy | Throughput | Trade-offs |
|---------|----------|------------|------------|
| `redis` | Exact | High (50k+ ops/sec) | Requires Redis infrastructure |
| `postgres` | Exact | Medium (1-5k ops/sec) | Adds DB write per rate-limited request |
| `memory` | Per-node only | Very high | Inaccurate in clustered deployments |

### Redis Backend (Recommended for Production)

```toml
[security.rate_limiting]
backend = "redis"

[security.rate_limiting.redis]
url = "redis://localhost:6379"
# Use sliding window algorithm
algorithm = "sliding_window"
# Connection pool
pool_size = 10
```

Redis uses atomic Lua scripts for accurate counting:

```lua
-- Sliding window rate limit (executed atomically)
local key = KEYS[1]
local limit = tonumber(ARGV[1])
local window = tonumber(ARGV[2])
local now = tonumber(ARGV[3])

redis.call('ZREMRANGEBYSCORE', key, 0, now - window)
local count = redis.call('ZCARD', key)

if count < limit then
    redis.call('ZADD', key, now, now .. ':' .. math.random())
    redis.call('EXPIRE', key, window)
    return 1  -- Allowed
else
    return 0  -- Rate limited
end
```

### PostgreSQL Backend (No Additional Infrastructure)

```toml
[security.rate_limiting]
backend = "postgres"

# Use separate connection pool to avoid blocking user queries
pool_size = 5

# Cleanup old entries periodically
cleanup_interval = "1m"
```

**Caveat:** Each rate-limited request adds a database write. Not suitable for high-throughput APIs (>1000 req/sec).

### Memory Backend (Development Only)

```toml
[security.rate_limiting]
backend = "memory"
```

**Warning:** In-memory rate limiting is per-node. In a 3-node cluster, each user gets 3x the configured limit. Only use for development or single-node deployments.

### Hybrid Approach

For high-throughput with accuracy, use memory for coarse limiting and Redis for precise:

```rust
#[forge::action]
#[rate_limit(requests = 1000, per = "second", backend = "memory")]  // Fast local check
#[rate_limit(requests = 10000, per = "minute", backend = "redis")]  // Accurate cluster-wide
pub async fn high_volume_api(ctx: &ActionContext) -> Result<()> { }
```

---

## CORS Configuration

```toml
# forge.toml
[security.cors]
allowed_origins = ["https://myapp.com"]
allowed_methods = ["GET", "POST"]
allowed_headers = ["Content-Type", "Authorization"]
max_age = 3600
```

---

## Secrets Management

```toml
# forge.toml - Use environment variables
[security]
secret_key = "${FORGE_SECRET}"

[database]
url = "${DATABASE_URL}"
```

---

## Audit Logging

All mutations are automatically logged:

```sql
SELECT * FROM forge_events
WHERE user_id = '...'
ORDER BY timestamp DESC;
```

---

## Related Documentation

- [Configuration](CONFIGURATION.md) — Security settings
- [Schema](../core/SCHEMA.md) — Encryption attributes
