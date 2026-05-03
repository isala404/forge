use std::net::IpAddr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use forge_core::cluster::{NodeId, NodeInfo, NodeRole, NodeStatus};
use forge_core::{ForgeError, Result};

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

    /// Update node status.
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

    /// Deregister the local node from the cluster.
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

    /// Get all active nodes in the cluster.
    pub async fn get_active_nodes(&self) -> Result<Vec<NodeInfo>> {
        self.get_nodes_by_status(NodeStatus::Active).await
    }

    /// Get nodes by status.
    pub async fn get_nodes_by_status(&self, status: NodeStatus) -> Result<Vec<NodeInfo>> {
        let rows = sqlx::query!(
            r#"
            SELECT id, hostname, ip_address, http_port, grpc_port,
                   roles, worker_capabilities, status, version,
                   started_at, last_heartbeat, current_connections,
                   current_jobs, cpu_usage, memory_usage
            FROM forge_nodes
            WHERE status = $1
            ORDER BY started_at
            "#,
            status.as_str(),
        )
        .fetch_all(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        rows.into_iter()
            .map(|row| {
                parse_node_fields(
                    row.id,
                    row.hostname,
                    row.ip_address,
                    row.http_port,
                    row.grpc_port,
                    row.roles,
                    row.worker_capabilities,
                    row.status,
                    row.version,
                    row.started_at,
                    row.last_heartbeat,
                    row.current_connections,
                    row.current_jobs,
                    row.cpu_usage,
                    row.memory_usage,
                )
            })
            .collect()
    }

    /// Get a specific node by ID.
    pub async fn get_node(&self, node_id: NodeId) -> Result<Option<NodeInfo>> {
        let row = sqlx::query!(
            r#"
            SELECT id, hostname, ip_address, http_port, grpc_port,
                   roles, worker_capabilities, status, version,
                   started_at, last_heartbeat, current_connections,
                   current_jobs, cpu_usage, memory_usage
            FROM forge_nodes
            WHERE id = $1
            "#,
            node_id.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        row.map(|row| {
            parse_node_fields(
                row.id,
                row.hostname,
                row.ip_address,
                row.http_port,
                row.grpc_port,
                row.roles,
                row.worker_capabilities,
                row.status,
                row.version,
                row.started_at,
                row.last_heartbeat,
                row.current_connections,
                row.current_jobs,
                row.cpu_usage,
                row.memory_usage,
            )
        })
        .transpose()
    }

    /// Count nodes by status.
    pub async fn count_by_status(&self) -> Result<NodeCounts> {
        let row = sqlx::query!(
            r#"
            SELECT
                COUNT(*) FILTER (WHERE status = 'active') as "active!",
                COUNT(*) FILTER (WHERE status = 'draining') as "draining!",
                COUNT(*) FILTER (WHERE status = 'dead') as "dead!",
                COUNT(*) FILTER (WHERE status = 'joining') as "joining!",
                COUNT(*) as "total!"
            FROM forge_nodes
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(NodeCounts {
            active: row.active as usize,
            draining: row.draining as usize,
            dead: row.dead as usize,
            joining: row.joining as usize,
            total: row.total as usize,
        })
    }

    /// Mark stale nodes as dead.
    pub async fn mark_dead_nodes(&self, threshold: Duration) -> Result<u64> {
        let threshold_secs = threshold.as_secs() as i64;

        let result = sqlx::query!(
            r#"
            UPDATE forge_nodes
            SET status = 'dead'
            WHERE status = 'active'
              AND last_heartbeat < NOW() - make_interval(secs => $1)
            "#,
            threshold_secs as f64,
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(result.rows_affected())
    }

    /// Clean up old dead nodes.
    pub async fn cleanup_dead_nodes(&self, older_than: Duration) -> Result<u64> {
        let threshold_secs = older_than.as_secs() as i64;

        let result = sqlx::query!(
            r#"
            DELETE FROM forge_nodes
            WHERE status = 'dead'
              AND last_heartbeat < NOW() - make_interval(secs => $1)
            "#,
            threshold_secs as f64,
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(result.rows_affected())
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_node_fields(
    id: Uuid,
    hostname: String,
    ip_address: String,
    http_port: i32,
    grpc_port: i32,
    roles: Vec<String>,
    worker_capabilities: Vec<String>,
    status: String,
    version: Option<String>,
    started_at: DateTime<Utc>,
    last_heartbeat: DateTime<Utc>,
    current_connections: i32,
    current_jobs: i32,
    cpu_usage: Option<f64>,
    memory_usage: Option<f64>,
) -> Result<NodeInfo> {
    let ip_addr: IpAddr = ip_address.parse().map_err(|e| {
        ForgeError::Validation(format!("invalid IP address '{}': {}", ip_address, e))
    })?;

    let node_roles: Vec<NodeRole> = roles
        .iter()
        .map(|s| {
            s.parse()
                .map_err(|e| ForgeError::Validation(format!("invalid role '{}': {}", s, e)))
        })
        .collect::<Result<Vec<_>>>()?;

    let node_status: NodeStatus = status
        .parse()
        .map_err(|e| ForgeError::Validation(format!("invalid status '{}': {}", status, e)))?;

    Ok(NodeInfo {
        id: NodeId::from_uuid(id),
        hostname,
        ip_address: ip_addr,
        http_port: http_port as u16,
        grpc_port: grpc_port as u16,
        roles: node_roles,
        worker_capabilities,
        status: node_status,
        version: version.unwrap_or_default(),
        started_at,
        last_heartbeat,
        current_connections: current_connections as u32,
        current_jobs: current_jobs as u32,
        cpu_usage: cpu_usage.unwrap_or(0.0) as f32,
        memory_usage: memory_usage.unwrap_or(0.0) as f32,
    })
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
