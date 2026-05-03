use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use sqlx::ConnectOptions;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use tokio::task::JoinHandle;
use tracing::log::LevelFilter;

use forge_core::config::{DatabaseConfig, PoolConfig};
use forge_core::error::{ForgeError, Result};

struct ReplicaEntry {
    pool: Arc<PgPool>,
    healthy: Arc<AtomicBool>,
}

/// Database connection wrapper with health-aware replica routing and workload isolation.
#[derive(Clone)]
pub struct Database {
    primary: Arc<PgPool>,
    replicas: Arc<Vec<ReplicaEntry>>,
    config: DatabaseConfig,
    replica_counter: Arc<AtomicUsize>,
    /// Isolated pool for background jobs, cron, daemons, workflows.
    jobs_pool: Option<Arc<PgPool>>,
    /// Isolated pool for observability writes.
    observability_pool: Option<Arc<PgPool>>,
    /// Isolated pool for long-running analytics queries.
    analytics_pool: Option<Arc<PgPool>>,
}

impl Database {
    /// Create a new database connection from configuration.
    pub async fn from_config(config: &DatabaseConfig) -> Result<Self> {
        Self::from_config_with_service(config, "forge").await
    }

    /// Create a new database connection with a service name for tracing.
    ///
    /// The service name is set as PostgreSQL's `application_name`, visible in
    /// `pg_stat_activity` for correlating queries to the originating service.
    pub async fn from_config_with_service(
        config: &DatabaseConfig,
        service_name: &str,
    ) -> Result<Self> {
        if config.url.is_empty() {
            return Err(ForgeError::Internal(
                "database.url cannot be empty. Provide a PostgreSQL connection URL.".into(),
            ));
        }

        // If pools.default overrides the primary pool size, use it
        let primary_size = config
            .pools
            .default
            .as_ref()
            .map(|p| p.size)
            .unwrap_or(config.pool_size);
        let primary_timeout = config
            .pools
            .default
            .as_ref()
            .map(|p| p.timeout_secs())
            .unwrap_or_else(|| config.pool_timeout_secs());

        let primary_min = config
            .pools
            .default
            .as_ref()
            .map(|p| p.min_size)
            .unwrap_or(config.min_pool_size);
        let primary_test = config
            .pools
            .default
            .as_ref()
            .map(|p| p.test_before_acquire)
            .unwrap_or(config.test_before_acquire);

        let statement_timeout = config
            .pools
            .default
            .as_ref()
            .and_then(|p| p.statement_timeout_secs())
            .unwrap_or_else(|| config.statement_timeout_secs());

        let primary = Self::create_pool_with_statement_timeout(
            &config.url,
            primary_size,
            primary_min,
            primary_timeout,
            statement_timeout,
            primary_test,
            service_name,
        )
        .await
        .map_err(|e| ForgeError::Internal(format!("Failed to connect to primary: {}", e)))?;

        let mut replicas = Vec::new();
        for replica_url in &config.replica_urls {
            let pool = Self::create_pool(
                replica_url,
                config.replica_pool_size.unwrap_or(config.pool_size / 2),
                config.pool_timeout_secs(),
                service_name,
            )
            .await
            .map_err(|e| ForgeError::Internal(format!("Failed to connect to replica: {}", e)))?;
            replicas.push(ReplicaEntry {
                pool: Arc::new(pool),
                healthy: Arc::new(AtomicBool::new(true)),
            });
        }

        let jobs_pool =
            Self::create_isolated_pool(&config.url, config.pools.jobs.as_ref(), service_name)
                .await?;
        let observability_pool = Self::create_isolated_pool(
            &config.url,
            config.pools.observability.as_ref(),
            service_name,
        )
        .await?;
        let analytics_pool =
            Self::create_isolated_pool(&config.url, config.pools.analytics.as_ref(), service_name)
                .await?;

        Ok(Self {
            primary: Arc::new(primary),
            replicas: Arc::new(replicas),
            config: config.clone(),
            replica_counter: Arc::new(AtomicUsize::new(0)),
            jobs_pool,
            observability_pool,
            analytics_pool,
        })
    }

    fn connect_options(url: &str, service_name: &str) -> sqlx::Result<PgConnectOptions> {
        let options: PgConnectOptions = url.parse()?;
        Ok(options
            .application_name(service_name)
            .log_statements(LevelFilter::Off)
            .log_slow_statements(LevelFilter::Warn, Duration::from_millis(500)))
    }

    fn connect_options_with_timeout(
        url: &str,
        service_name: &str,
        statement_timeout_secs: u64,
    ) -> sqlx::Result<PgConnectOptions> {
        let options: PgConnectOptions = url.parse()?;
        let mut opts = options
            .application_name(service_name)
            .log_statements(LevelFilter::Off)
            .log_slow_statements(LevelFilter::Warn, Duration::from_millis(500));
        if statement_timeout_secs > 0 {
            // Set PostgreSQL statement_timeout to prevent unbounded query execution
            opts = opts.options([("statement_timeout", &format!("{}s", statement_timeout_secs))]);
        }
        Ok(opts)
    }

    async fn create_pool(
        url: &str,
        size: u32,
        timeout_secs: u64,
        service_name: &str,
    ) -> sqlx::Result<PgPool> {
        Self::create_pool_with_opts(url, size, 0, timeout_secs, true, service_name).await
    }

    async fn create_pool_with_opts(
        url: &str,
        size: u32,
        min_size: u32,
        timeout_secs: u64,
        test_before_acquire: bool,
        service_name: &str,
    ) -> sqlx::Result<PgPool> {
        Self::create_pool_with_statement_timeout(
            url,
            size,
            min_size,
            timeout_secs,
            0,
            test_before_acquire,
            service_name,
        )
        .await
    }

    async fn create_pool_with_statement_timeout(
        url: &str,
        size: u32,
        min_size: u32,
        timeout_secs: u64,
        statement_timeout_secs: u64,
        test_before_acquire: bool,
        service_name: &str,
    ) -> sqlx::Result<PgPool> {
        let options = if statement_timeout_secs > 0 {
            Self::connect_options_with_timeout(url, service_name, statement_timeout_secs)?
        } else {
            Self::connect_options(url, service_name)?
        };
        PgPoolOptions::new()
            .max_connections(size)
            .min_connections(min_size)
            .acquire_timeout(Duration::from_secs(timeout_secs))
            .test_before_acquire(test_before_acquire)
            .connect_with(options)
            .await
    }

    async fn create_isolated_pool(
        url: &str,
        config: Option<&PoolConfig>,
        service_name: &str,
    ) -> Result<Option<Arc<PgPool>>> {
        let Some(cfg) = config else {
            return Ok(None);
        };
        let pool = Self::create_pool_with_opts(
            url,
            cfg.size,
            cfg.min_size,
            cfg.timeout_secs(),
            cfg.test_before_acquire,
            service_name,
        )
        .await
        .map_err(|e| ForgeError::Internal(format!("Failed to create isolated pool: {}", e)))?;
        Ok(Some(Arc::new(pool)))
    }

    /// Get the primary pool for writes.
    pub fn primary(&self) -> &PgPool {
        &self.primary
    }

    /// Get a pool for reads. Skips unhealthy replicas, falls back to primary.
    pub fn read_pool(&self) -> &PgPool {
        if !self.config.read_from_replica || self.replicas.is_empty() {
            return &self.primary;
        }

        let len = self.replicas.len();
        let start = self.replica_counter.fetch_add(1, Ordering::Relaxed) % len;

        // Try each replica starting from round-robin position
        for offset in 0..len {
            let idx = (start + offset) % len;
            if let Some(entry) = self.replicas.get(idx)
                && entry.healthy.load(Ordering::Relaxed)
            {
                return &entry.pool;
            }
        }

        // All replicas unhealthy, fall back to primary
        &self.primary
    }

    /// Pool for background jobs, cron, daemons, and workflows.
    /// Falls back to primary if no isolated pool is configured.
    pub fn jobs_pool(&self) -> &PgPool {
        self.jobs_pool.as_deref().unwrap_or(&self.primary)
    }

    /// Pool for observability writes (metrics, slow query logs).
    /// Falls back to primary if no isolated pool is configured.
    pub fn observability_pool(&self) -> &PgPool {
        self.observability_pool.as_deref().unwrap_or(&self.primary)
    }

    /// Pool for long-running analytics queries.
    /// Falls back to primary if no isolated pool is configured.
    pub fn analytics_pool(&self) -> &PgPool {
        self.analytics_pool.as_deref().unwrap_or(&self.primary)
    }

    /// Start background health monitoring for replicas. Returns None if no replicas configured.
    pub fn start_health_monitor(&self) -> Option<JoinHandle<()>> {
        if self.replicas.is_empty() {
            return None;
        }

        let replicas = Arc::clone(&self.replicas);
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            loop {
                interval.tick().await;
                for entry in replicas.iter() {
                    let ok = sqlx::query_scalar!("SELECT 1 as \"v!\"")
                        .fetch_one(entry.pool.as_ref())
                        .await
                        .is_ok();
                    let was_healthy = entry.healthy.swap(ok, Ordering::Relaxed);
                    if was_healthy && !ok {
                        tracing::warn!("Replica marked unhealthy");
                    } else if !was_healthy && ok {
                        tracing::info!("Replica recovered");
                    }
                }
            }
        });

        Some(handle)
    }

    /// Create a Database wrapper from an existing pool (for testing).
    #[cfg(test)]
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            primary: Arc::new(pool),
            replicas: Arc::new(Vec::new()),
            config: DatabaseConfig::default(),
            replica_counter: Arc::new(AtomicUsize::new(0)),
            jobs_pool: None,
            observability_pool: None,
            analytics_pool: None,
        }
    }

    /// Check database connectivity.
    pub async fn health_check(&self) -> Result<()> {
        sqlx::query_scalar!("SELECT 1 as \"v!\"")
            .fetch_one(self.primary.as_ref())
            .await
            .map_err(|e| ForgeError::Internal(format!("Health check failed: {}", e)))?;
        Ok(())
    }

    /// Close all connections gracefully.
    pub async fn close(&self) {
        self.primary.close().await;
        for entry in self.replicas.iter() {
            entry.pool.close().await;
        }
        if let Some(ref p) = self.jobs_pool {
            p.close().await;
        }
        if let Some(ref p) = self.observability_pool {
            p.close().await;
        }
        if let Some(ref p) = self.analytics_pool {
            p.close().await;
        }
    }
}

/// Type alias for the pool type.
pub type DatabasePool = PgPool;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_database_config_clone() {
        let config = DatabaseConfig::new("postgres://localhost/test");

        let cloned = config.clone();
        assert_eq!(cloned.url(), config.url());
        assert_eq!(cloned.pool_size, config.pool_size);
    }
}
