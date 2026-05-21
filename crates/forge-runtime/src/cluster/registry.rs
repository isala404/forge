use forge_core::cluster::{NodeInfo, NodeStatus};
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
}
