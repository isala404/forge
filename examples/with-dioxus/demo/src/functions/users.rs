use crate::schema::{User, UserRole};
use forge::prelude::*;

/// List all users with reactive subscription support
#[forge::query(cache = "30s", auth = "none")]
pub async fn get_users(ctx: &QueryContext) -> Result<Vec<User>> {
    sqlx::query_as!(
        User,
        r#"
        SELECT
            id,
            email,
            name,
            role as "role: UserRole",
            password_hash,
            created_at,
            updated_at
        FROM users
        ORDER BY created_at DESC
        "#
    )
    .fetch_all(ctx.db())
    .await
    .map_err(Into::into)
}

/// Get single user by ID
#[forge::query(timeout = "10s", auth = "none")]
pub async fn get_user(ctx: &QueryContext, id: Uuid) -> Result<Option<User>> {
    sqlx::query_as!(
        User,
        r#"
        SELECT
            id,
            email,
            name,
            role as "role: UserRole",
            password_hash,
            created_at,
            updated_at
        FROM users
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(ctx.db())
    .await
    .map_err(Into::into)
}

/// Create a new user
#[forge::mutation(auth = "none")]
pub async fn create_user(
    ctx: &MutationContext,
    email: String,
    name: String,
    role: Option<UserRole>,
) -> Result<User> {
    let id = Uuid::new_v4();
    let now = Utc::now();
    let role = role.unwrap_or_default();

    let mut conn = ctx.conn().await.map_err(ForgeError::Database)?;
    sqlx::query_as!(
        User,
        r#"
        INSERT INTO users (id, email, name, role, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING
            id,
            email,
            name,
            role as "role: UserRole",
            password_hash,
            created_at,
            updated_at
        "#,
        id,
        &email,
        &name,
        role as UserRole,
        now,
        now
    )
    .fetch_one(&mut conn)
    .await
    .map_err(Into::into)
}

/// Update user with partial fields
#[forge::mutation(timeout = "30s", auth = "none")]
pub async fn update_user(
    ctx: &MutationContext,
    id: Uuid,
    email: Option<String>,
    name: Option<String>,
    role: Option<UserRole>,
) -> Result<User> {
    let mut conn = ctx.conn().await.map_err(ForgeError::Database)?;
    sqlx::query_as!(
        User,
        r#"
        UPDATE users
        SET
            email = COALESCE($2, email),
            name = COALESCE($3, name),
            role = COALESCE($4, role),
            updated_at = NOW()
        WHERE id = $1
        RETURNING
            id,
            email,
            name,
            role as "role: UserRole",
            password_hash,
            created_at,
            updated_at
        "#,
        id,
        email,
        name,
        role as Option<UserRole>
    )
    .fetch_one(&mut conn)
    .await
    .map_err(Into::into)
}

/// Delete user by ID
#[forge::mutation(auth = "none")]
pub async fn delete_user(ctx: &MutationContext, id: Uuid) -> Result<bool> {
    let mut conn = ctx.conn().await.map_err(ForgeError::Database)?;
    let result = sqlx::query!("DELETE FROM users WHERE id = $1", id)
        .execute(&mut conn)
        .await?;

    Ok(result.rows_affected() > 0)
}
