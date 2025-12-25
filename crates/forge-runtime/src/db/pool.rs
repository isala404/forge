use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

use forge_core::config::DatabaseConfig;
use forge_core::error::{ForgeError, Result};

/// Database connection wrapper providing connection pooling.
#[derive(Clone)]
pub struct Database {
    /// Primary connection pool.
    primary: Arc<PgPool>,

    /// Read replica pools (optional).
    replicas: Vec<Arc<PgPool>>,

    /// Configuration.
    config: DatabaseConfig,

    /// Counter for round-robin replica selection.
    replica_counter: Arc<std::sync::atomic::AtomicUsize>,
}

impl Database {
    /// Create a new database connection from configuration.
    pub async fn from_config(config: &DatabaseConfig) -> Result<Self> {
        let primary = Self::create_pool(&config.url, config.pool_size, config.pool_timeout_secs)
            .await
            .map_err(|e| ForgeError::Database(format!("Failed to connect to primary: {}", e)))?;

        let mut replicas = Vec::new();
        for replica_url in &config.replica_urls {
            let pool =
                Self::create_pool(replica_url, config.pool_size / 2, config.pool_timeout_secs)
                    .await
                    .map_err(|e| {
                        ForgeError::Database(format!("Failed to connect to replica: {}", e))
                    })?;
            replicas.push(Arc::new(pool));
        }

        Ok(Self {
            primary: Arc::new(primary),
            replicas,
            config: config.clone(),
            replica_counter: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    /// Create a connection pool with the given parameters.
    async fn create_pool(url: &str, size: u32, timeout_secs: u64) -> sqlx::Result<PgPool> {
        PgPoolOptions::new()
            .max_connections(size)
            .acquire_timeout(Duration::from_secs(timeout_secs))
            .connect(url)
            .await
    }

    /// Get the primary pool for writes.
    pub fn primary(&self) -> &PgPool {
        &self.primary
    }

    /// Get a pool for reads (uses replica if configured, otherwise primary).
    pub fn read_pool(&self) -> &PgPool {
        if self.config.read_from_replica && !self.replicas.is_empty() {
            // Round-robin replica selection
            let idx = self
                .replica_counter
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                % self.replicas.len();
            &self.replicas[idx]
        } else {
            &self.primary
        }
    }

    /// Check database connectivity.
    pub async fn health_check(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(self.primary.as_ref())
            .await
            .map_err(|e| ForgeError::Database(format!("Health check failed: {}", e)))?;
        Ok(())
    }

    /// Close all connections gracefully.
    pub async fn close(&self) {
        self.primary.close().await;
        for replica in &self.replicas {
            replica.close().await;
        }
    }
}

/// Type alias for the pool type.
pub type DatabasePool = PgPool;

#[cfg(test)]
mod tests {
    use super::*;

    // Integration tests require a real PostgreSQL connection
    // These are placeholder tests that don't require a database

    #[test]
    fn test_database_config_clone() {
        let config = DatabaseConfig {
            url: "postgres://localhost/test".to_string(),
            pool_size: 10,
            ..Default::default()
        };

        let cloned = config.clone();
        assert_eq!(cloned.url, config.url);
        assert_eq!(cloned.pool_size, config.pool_size);
    }
}
