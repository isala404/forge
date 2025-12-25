use std::net::IpAddr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use forge_core::cluster::{NodeId, NodeInfo, NodeRole, NodeStatus};

/// Node registry for cluster membership.
pub struct NodeRegistry {
    pool: sqlx::PgPool,
    local_node: NodeInfo,
}

impl NodeRegistry {
    /// Create a new node registry.
    pub fn new(pool: sqlx::PgPool, local_node: NodeInfo) -> Self {
        Self { pool, local_node }
    }

    /// Get the local node info.
    pub fn local_node(&self) -> &NodeInfo {
        &self.local_node
    }

    /// Get the local node ID.
    pub fn local_id(&self) -> NodeId {
        self.local_node.id
    }

    /// Register the local node in the cluster.
    pub async fn register(&self) -> forge_core::Result<()> {
        let roles: Vec<&str> = self.local_node.roles.iter().map(|r| r.as_str()).collect();

        sqlx::query(
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
        )
        .bind(self.local_node.id.as_uuid())
        .bind(&self.local_node.hostname)
        .bind(self.local_node.ip_address.to_string())
        .bind(self.local_node.http_port as i32)
        .bind(self.local_node.grpc_port as i32)
        .bind(&roles)
        .bind(&self.local_node.worker_capabilities)
        .bind(self.local_node.status.as_str())
        .bind(&self.local_node.version)
        .bind(self.local_node.started_at)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Update node status.
    pub async fn set_status(&self, status: NodeStatus) -> forge_core::Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_nodes
            SET status = $2
            WHERE id = $1
            "#,
        )
        .bind(self.local_node.id.as_uuid())
        .bind(status.as_str())
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Deregister the local node from the cluster.
    pub async fn deregister(&self) -> forge_core::Result<()> {
        sqlx::query(
            r#"
            DELETE FROM forge_nodes WHERE id = $1
            "#,
        )
        .bind(self.local_node.id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Get all active nodes in the cluster.
    pub async fn get_active_nodes(&self) -> forge_core::Result<Vec<NodeInfo>> {
        self.get_nodes_by_status(NodeStatus::Active).await
    }

    /// Get nodes by status.
    pub async fn get_nodes_by_status(
        &self,
        status: NodeStatus,
    ) -> forge_core::Result<Vec<NodeInfo>> {
        let rows = sqlx::query(
            r#"
            SELECT id, hostname, ip_address, http_port, grpc_port,
                   roles, worker_capabilities, status, version,
                   started_at, last_heartbeat, current_connections,
                   current_jobs, cpu_usage, memory_usage
            FROM forge_nodes
            WHERE status = $1
            ORDER BY started_at
            "#,
        )
        .bind(status.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let mut nodes = Vec::new();
        for row in rows {
            use sqlx::Row;
            let id: Uuid = row.get("id");
            let ip_str: String = row.get("ip_address");
            let ip_address: IpAddr = ip_str
                .parse()
                .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)));
            let roles_str: Vec<String> = row.get("roles");
            let roles: Vec<NodeRole> = roles_str
                .iter()
                .filter_map(|s| NodeRole::from_str(s))
                .collect();

            nodes.push(NodeInfo {
                id: NodeId::from_uuid(id),
                hostname: row.get("hostname"),
                ip_address,
                http_port: row.get::<i32, _>("http_port") as u16,
                grpc_port: row.get::<i32, _>("grpc_port") as u16,
                roles,
                worker_capabilities: row.get("worker_capabilities"),
                status: NodeStatus::from_str(row.get("status")),
                version: row.get("version"),
                started_at: row.get("started_at"),
                last_heartbeat: row.get("last_heartbeat"),
                current_connections: row.get::<i32, _>("current_connections") as u32,
                current_jobs: row.get::<i32, _>("current_jobs") as u32,
                cpu_usage: row.get::<f32, _>("cpu_usage"),
                memory_usage: row.get::<f32, _>("memory_usage"),
            });
        }

        Ok(nodes)
    }

    /// Get a specific node by ID.
    pub async fn get_node(&self, node_id: NodeId) -> forge_core::Result<Option<NodeInfo>> {
        let row = sqlx::query(
            r#"
            SELECT id, hostname, ip_address, http_port, grpc_port,
                   roles, worker_capabilities, status, version,
                   started_at, last_heartbeat, current_connections,
                   current_jobs, cpu_usage, memory_usage
            FROM forge_nodes
            WHERE id = $1
            "#,
        )
        .bind(node_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        match row {
            Some(row) => {
                use sqlx::Row;
                let id: Uuid = row.get("id");
                let ip_str: String = row.get("ip_address");
                let ip_address: IpAddr = ip_str
                    .parse()
                    .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)));
                let roles_str: Vec<String> = row.get("roles");
                let roles: Vec<NodeRole> = roles_str
                    .iter()
                    .filter_map(|s| NodeRole::from_str(s))
                    .collect();

                Ok(Some(NodeInfo {
                    id: NodeId::from_uuid(id),
                    hostname: row.get("hostname"),
                    ip_address,
                    http_port: row.get::<i32, _>("http_port") as u16,
                    grpc_port: row.get::<i32, _>("grpc_port") as u16,
                    roles,
                    worker_capabilities: row.get("worker_capabilities"),
                    status: NodeStatus::from_str(row.get("status")),
                    version: row.get("version"),
                    started_at: row.get("started_at"),
                    last_heartbeat: row.get("last_heartbeat"),
                    current_connections: row.get::<i32, _>("current_connections") as u32,
                    current_jobs: row.get::<i32, _>("current_jobs") as u32,
                    cpu_usage: row.get::<f32, _>("cpu_usage"),
                    memory_usage: row.get::<f32, _>("memory_usage"),
                }))
            }
            None => Ok(None),
        }
    }

    /// Count nodes by status.
    pub async fn count_by_status(&self) -> forge_core::Result<NodeCounts> {
        let row = sqlx::query(
            r#"
            SELECT
                COUNT(*) FILTER (WHERE status = 'active') as active,
                COUNT(*) FILTER (WHERE status = 'draining') as draining,
                COUNT(*) FILTER (WHERE status = 'dead') as dead,
                COUNT(*) FILTER (WHERE status = 'joining') as joining,
                COUNT(*) as total
            FROM forge_nodes
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        use sqlx::Row;
        Ok(NodeCounts {
            active: row.get::<i64, _>("active") as usize,
            draining: row.get::<i64, _>("draining") as usize,
            dead: row.get::<i64, _>("dead") as usize,
            joining: row.get::<i64, _>("joining") as usize,
            total: row.get::<i64, _>("total") as usize,
        })
    }

    /// Mark stale nodes as dead.
    pub async fn mark_dead_nodes(&self, threshold: Duration) -> forge_core::Result<u64> {
        let threshold_secs = threshold.as_secs() as i64;

        let result = sqlx::query(
            r#"
            UPDATE forge_nodes
            SET status = 'dead'
            WHERE status = 'active'
              AND last_heartbeat < NOW() - make_interval(secs => $1)
            "#,
        )
        .bind(threshold_secs as f64)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(result.rows_affected())
    }

    /// Clean up old dead nodes.
    pub async fn cleanup_dead_nodes(&self, older_than: Duration) -> forge_core::Result<u64> {
        let threshold_secs = older_than.as_secs() as i64;

        let result = sqlx::query(
            r#"
            DELETE FROM forge_nodes
            WHERE status = 'dead'
              AND last_heartbeat < NOW() - make_interval(secs => $1)
            "#,
        )
        .bind(threshold_secs as f64)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(result.rows_affected())
    }
}

/// Node count statistics.
#[derive(Debug, Clone, Default)]
pub struct NodeCounts {
    /// Active nodes.
    pub active: usize,
    /// Draining nodes.
    pub draining: usize,
    /// Dead nodes.
    pub dead: usize,
    /// Joining nodes.
    pub joining: usize,
    /// Total nodes.
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_counts_default() {
        let counts = NodeCounts::default();
        assert_eq!(counts.active, 0);
        assert_eq!(counts.total, 0);
    }
}
