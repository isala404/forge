use std::time::Duration;

use forge_core::cluster::{NodeInfo, NodeStatus};
use forge_core::{ForgeError, Result};

/// How often the background compactor sweeps long-dead node rows.
const COMPACTION_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Node registry for cluster membership.
pub struct NodeRegistry {
    pool: sqlx::PgPool,
    local_node: NodeInfo,
}

impl NodeRegistry {
    pub fn new(pool: sqlx::PgPool, local_node: NodeInfo) -> Self {
        let registry = Self { pool, local_node };
        registry.spawn_cleanup_loop();
        registry
    }

    /// Periodically delete `forge_nodes` rows that have been `dead` for
    /// more than 7 days. Pod churn would otherwise accumulate rows
    /// indefinitely. Runs every 6 h on a detached task; failures are logged
    /// and the loop continues.
    fn spawn_cleanup_loop(&self) {
        let pool = self.pool.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(COMPACTION_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // First tick fires immediately; skip it so startup doesn't compact.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                // Untyped: the parameterless DELETE produces no row data and
                // adding a `.sqlx` entry for it just for the compile-time
                // check has zero safety value. Allow lints locally.
                #[allow(clippy::disallowed_methods)]
                let res = sqlx::query(
                    "DELETE FROM forge_nodes \
                     WHERE status = 'dead' \
                       AND last_heartbeat < NOW() - INTERVAL '7 days'",
                )
                .execute(&pool)
                .await;
                match res {
                    Ok(result) => {
                        let n = result.rows_affected();
                        if n > 0 {
                            tracing::info!(rows = n, "Compacted dead forge_nodes rows");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "forge_nodes compaction failed");
                    }
                }
            }
        });
    }

    pub async fn register(&self) -> Result<()> {
        let roles: Vec<String> = self
            .local_node
            .roles
            .iter()
            .map(|r| r.as_str().to_string())
            .collect();

        sqlx::query!(
            r#"
            INSERT INTO forge_nodes (
                id, hostname, ip_address, http_port, grpc_port,
                roles, worker_capabilities, status, version, started_at, last_heartbeat
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW())
            ON CONFLICT (id) DO UPDATE SET
                hostname = EXCLUDED.hostname,
                ip_address = EXCLUDED.ip_address,
                http_port = EXCLUDED.http_port,
                grpc_port = EXCLUDED.grpc_port,
                roles = EXCLUDED.roles,
                worker_capabilities = EXCLUDED.worker_capabilities,
                status = EXCLUDED.status,
                version = EXCLUDED.version,
                last_heartbeat = NOW()
            "#,
            self.local_node.id.as_uuid(),
            &self.local_node.hostname,
            self.local_node.ip_address.to_string(),
            self.local_node.http_port as i32,
            self.local_node.grpc_port as i32,
            &roles,
            &self.local_node.worker_capabilities,
            self.local_node.status.as_str(),
            &self.local_node.version,
            self.local_node.started_at,
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(())
    }

    pub async fn set_status(&self, status: NodeStatus) -> Result<()> {
        sqlx::query!(
            r#"
            UPDATE forge_nodes
            SET status = $2
            WHERE id = $1
            "#,
            self.local_node.id.as_uuid(),
            status.as_str(),
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(())
    }

    pub async fn deregister(&self) -> Result<()> {
        sqlx::query!(
            r#"
            DELETE FROM forge_nodes WHERE id = $1
            "#,
            self.local_node.id.as_uuid(),
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(())
    }
}
