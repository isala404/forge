use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use forge_core::{ForgeError, Result};

/// Partition granularity.
#[derive(Debug, Clone, Copy, Default)]
pub enum PartitionGranularity {
    /// Daily partitions.
    Daily,
    /// Weekly partitions.
    #[default]
    Weekly,
    /// Monthly partitions.
    Monthly,
}

impl PartitionGranularity {
    /// Get the partition suffix format.
    pub fn suffix_format(&self) -> &'static str {
        match self {
            Self::Daily => "%Y%m%d",
            Self::Weekly => "%Y_w%W",
            Self::Monthly => "%Y%m",
        }
    }

    /// Get the next partition boundary.
    pub fn next_boundary(&self, from: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Self::Daily => {
                let next = from.date_naive().succ_opt().unwrap_or(from.date_naive());
                DateTime::from_naive_utc_and_offset(next.and_hms_opt(0, 0, 0).unwrap(), Utc)
            }
            Self::Weekly => {
                let days_until_monday = (7 - from.weekday().num_days_from_monday()) % 7;
                let next = from.date_naive()
                    + chrono::Duration::days(if days_until_monday == 0 {
                        7
                    } else {
                        days_until_monday as i64
                    });
                DateTime::from_naive_utc_and_offset(next.and_hms_opt(0, 0, 0).unwrap(), Utc)
            }
            Self::Monthly => {
                let (year, month) = if from.month() == 12 {
                    (from.year() + 1, 1)
                } else {
                    (from.year(), from.month() + 1)
                };
                let next = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
                DateTime::from_naive_utc_and_offset(next.and_hms_opt(0, 0, 0).unwrap(), Utc)
            }
        }
    }
}

/// Configuration for partition management.
#[derive(Debug, Clone)]
pub struct PartitionConfig {
    /// How far ahead to create partitions.
    pub lookahead: Duration,
    /// Partition size/granularity.
    pub granularity: PartitionGranularity,
    /// Retention periods per table.
    pub retention: HashMap<String, Duration>,
    /// How often to run maintenance.
    pub maintenance_interval: Duration,
}

impl Default for PartitionConfig {
    fn default() -> Self {
        let mut retention = HashMap::new();
        retention.insert("forge_metrics".to_string(), Duration::from_secs(30 * 86400)); // 30 days
        retention.insert("forge_logs".to_string(), Duration::from_secs(14 * 86400)); // 14 days
        retention.insert("forge_traces".to_string(), Duration::from_secs(7 * 86400)); // 7 days

        Self {
            lookahead: Duration::from_secs(4 * 7 * 86400), // 4 weeks
            granularity: PartitionGranularity::Weekly,
            retention,
            maintenance_interval: Duration::from_secs(3600), // 1 hour
        }
    }
}

/// Partition manager for observability tables.
pub struct PartitionManager {
    pool: PgPool,
    config: PartitionConfig,
}

impl PartitionManager {
    /// Create a new partition manager.
    pub fn new(pool: PgPool, config: PartitionConfig) -> Self {
        Self { pool, config }
    }

    /// Run the partition manager until shutdown.
    pub async fn run(&self, shutdown: CancellationToken) {
        let mut interval = tokio::time::interval(self.config.maintenance_interval);

        tracing::info!(
            granularity = ?self.config.granularity,
            lookahead = ?self.config.lookahead,
            "Partition manager started"
        );

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.maintain().await {
                        tracing::error!(error = %e, "Partition maintenance failed");
                    }
                }
                _ = shutdown.cancelled() => {
                    tracing::info!("Partition manager shutting down");
                    break;
                }
            }
        }
    }

    /// Run maintenance: create future partitions and drop expired ones.
    pub async fn maintain(&self) -> Result<()> {
        self.ensure_future_partitions().await?;
        self.drop_expired_partitions().await?;
        Ok(())
    }

    /// Ensure partitions exist for the lookahead period.
    async fn ensure_future_partitions(&self) -> Result<()> {
        let now = Utc::now();
        let lookahead_end =
            now + chrono::Duration::from_std(self.config.lookahead).unwrap_or_default();

        for table in self.config.retention.keys() {
            let mut boundary = now;
            while boundary < lookahead_end {
                let next = self.config.granularity.next_boundary(boundary);
                if let Err(e) = self.create_partition(table, boundary, next).await {
                    // Ignore "already exists" errors
                    if !e.to_string().contains("already exists") {
                        tracing::warn!(
                            table = %table,
                            error = %e,
                            "Failed to create partition"
                        );
                    }
                }
                boundary = next;
            }
        }

        Ok(())
    }

    /// Drop partitions older than retention period.
    async fn drop_expired_partitions(&self) -> Result<()> {
        let now = Utc::now();

        for (table, retention) in &self.config.retention {
            let cutoff = now - chrono::Duration::from_std(*retention).unwrap_or_default();

            // List partitions for this table
            let partitions: Vec<(String,)> = sqlx::query_as(
                r#"
                SELECT tablename::text FROM pg_tables
                WHERE tablename LIKE $1 || '_%'
                AND schemaname = 'public'
                "#,
            )
            .bind(table)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ForgeError::Database(e.to_string()))?;

            for (partition_name,) in partitions {
                // Try to parse the partition date from the name
                if let Some(partition_end) = self.parse_partition_end(&partition_name) {
                    if partition_end < cutoff {
                        if let Err(e) = self.drop_partition(&partition_name).await {
                            tracing::warn!(
                                partition = %partition_name,
                                error = %e,
                                "Failed to drop expired partition"
                            );
                        } else {
                            tracing::info!(
                                partition = %partition_name,
                                "Dropped expired partition"
                            );
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Create a partition for a time range.
    async fn create_partition(
        &self,
        parent_table: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<()> {
        let suffix = from
            .format(self.config.granularity.suffix_format())
            .to_string();
        let partition_name = format!("{}_{}", parent_table, suffix);

        let from_str = from.format("%Y-%m-%d %H:%M:%S").to_string();
        let to_str = to.format("%Y-%m-%d %H:%M:%S").to_string();

        // Determine the partition key column (reserved for future use)
        let _partition_column = if parent_table == "forge_traces" {
            "started_at"
        } else {
            "timestamp"
        };

        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {} PARTITION OF {} FOR VALUES FROM ('{}') TO ('{}')",
            partition_name, parent_table, from_str, to_str
        );

        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| ForgeError::Database(e.to_string()))?;

        tracing::debug!(
            partition = %partition_name,
            from = %from_str,
            to = %to_str,
            "Created partition"
        );

        Ok(())
    }

    /// Drop a partition.
    async fn drop_partition(&self, partition_name: &str) -> Result<()> {
        let sql = format!("DROP TABLE IF EXISTS {} CASCADE", partition_name);
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| ForgeError::Database(e.to_string()))?;
        Ok(())
    }

    /// Parse the end date from a partition name.
    fn parse_partition_end(&self, partition_name: &str) -> Option<DateTime<Utc>> {
        // Try to extract date from partition name like "forge_metrics_20250101"
        let parts: Vec<&str> = partition_name.rsplitn(2, '_').collect();
        if parts.len() < 1 {
            return None;
        }

        let date_str = parts[0];

        // Try parsing as YYYYMMDD
        if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y%m%d") {
            return Some(DateTime::from_naive_utc_and_offset(
                date.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ));
        }

        // Try parsing as YYYYMM
        if date_str.len() == 6 {
            if let Ok(year) = date_str[..4].parse::<i32>() {
                if let Ok(month) = date_str[4..].parse::<u32>() {
                    if let Some(date) = NaiveDate::from_ymd_opt(year, month, 1) {
                        return Some(DateTime::from_naive_utc_and_offset(
                            date.and_hms_opt(0, 0, 0).unwrap(),
                            Utc,
                        ));
                    }
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partition_config_default() {
        let config = PartitionConfig::default();
        assert_eq!(config.retention.len(), 3);
        assert!(config.retention.contains_key("forge_metrics"));
        assert!(config.retention.contains_key("forge_logs"));
        assert!(config.retention.contains_key("forge_traces"));
    }

    #[test]
    fn test_partition_granularity_next_boundary() {
        let now = Utc::now();

        let daily = PartitionGranularity::Daily;
        let next_daily = daily.next_boundary(now);
        assert!(next_daily > now);

        let monthly = PartitionGranularity::Monthly;
        let next_monthly = monthly.next_boundary(now);
        assert!(next_monthly > now);
    }
}
