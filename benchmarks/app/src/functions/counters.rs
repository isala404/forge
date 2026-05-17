use forge::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::schema::Counter;

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateCounterInput {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IncrementInput {
    pub user_id: Uuid,
    pub id: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetCounterInput {
    pub user_id: Uuid,
    pub id: Uuid,
}

#[forge::mutation(public)]
pub async fn create_counter(ctx: &MutationContext, input: CreateCounterInput) -> Result<Counter> {
    let mut conn = ctx.conn().await?;

    sqlx::query_as!(
        Counter,
        "INSERT INTO counters (name) VALUES ($1) RETURNING *",
        &input.name
    )
    .fetch_one(&mut conn)
    .await
    .map_err(Into::into)
}

/// Atomic increment. This is the hot write path.
#[forge::mutation(unscoped)]
pub async fn increment(ctx: &MutationContext, input: IncrementInput) -> Result<Counter> {
    let mut conn = ctx.conn().await?;

    sqlx::query_as!(
        Counter,
        "UPDATE counters SET value = value + 1, updated_by = $2, updated_at = NOW()
         WHERE id = $1 RETURNING *",
        input.id,
        input.user_id
    )
    .fetch_optional(&mut conn)
    .await?
    .ok_or_else(|| ForgeError::NotFound("Counter not found".into()))
}

/// Single counter read. Hot read path.
#[forge::query(unscoped, tables("counters"))]
pub async fn get_counter(ctx: &QueryContext, input: GetCounterInput) -> Result<Counter> {
    sqlx::query_as!(Counter, "SELECT * FROM counters WHERE id = $1", input.id)
        .fetch_optional(ctx.db())
        .await?
        .ok_or_else(|| ForgeError::NotFound("Counter not found".into()))
}

/// Paginated counter list. Capped to 20 rows like a real endpoint.
#[forge::query(unscoped, tables("counters"))]
pub async fn list_counters(ctx: &QueryContext) -> Result<Vec<Counter>> {
    sqlx::query_as!(Counter, "SELECT * FROM counters ORDER BY name LIMIT 20")
        .fetch_all(ctx.db())
        .await
        .map_err(Into::into)
}
