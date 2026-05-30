use crate::schema::{User, UserRole};
use forge::prelude::*;

/// List all users with reactive subscription support.
/// Reading the global user list requires an authenticated session.
#[forge::query(cache = "30s", unscoped)]
pub async fn get_users(ctx: &QueryContext) -> Result<Vec<User>> {
    let _ = ctx.user_id()?;
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

/// Get single user by ID. Requires an authenticated session.
#[forge::query(timeout = "10s", unscoped)]
pub async fn get_user(ctx: &QueryContext, id: Uuid) -> Result<Option<User>> {
    let _ = ctx.user_id()?;
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

/// Create a new user. Requires the `admin` role.
#[forge::mutation(scope = "global")]
pub async fn create_user(
    ctx: &MutationContext,
    email: String,
    name: String,
    role: Option<UserRole>,
) -> Result<User> {
    ctx.auth.require_role("admin")?;
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

/// Update user with partial fields. Requires the `admin` role.
#[forge::mutation(timeout = "30s", scope = "global")]
pub async fn update_user(
    ctx: &MutationContext,
    id: Uuid,
    email: Option<String>,
    name: Option<String>,
    role: Option<UserRole>,
) -> Result<User> {
    ctx.auth.require_role("admin")?;
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

/// Delete user by ID. Requires the `admin` role.
#[forge::mutation(scope = "global")]
pub async fn delete_user(ctx: &MutationContext, id: Uuid) -> Result<bool> {
    ctx.auth.require_role("admin")?;
    let mut conn = ctx.conn().await.map_err(ForgeError::Database)?;
    let result = sqlx::query!("DELETE FROM users WHERE id = $1", id)
        .execute(&mut conn)
        .await?;

    Ok(result.rows_affected() > 0)
}

#[cfg(all(test, feature = "testcontainers"))]
mod tests {
    use super::*;
    use forge::forge_core::function::{AuthContext, RequestMetadata};
    use forge::testing::{IsolatedTestDb, TestDatabase};
    use std::path::Path;

    async fn setup_db() -> IsolatedTestDb {
        let base = TestDatabase::from_env().await.unwrap();
        let db = base.isolated("users_test").await.unwrap();
        db.run_sql(&forge::get_internal_sql()).await.unwrap();
        db.migrate(Path::new("migrations")).await.unwrap();
        db
    }

    fn admin_auth() -> AuthContext {
        AuthContext::authenticated(Uuid::new_v4(), vec!["admin".into()], Default::default())
    }

    fn query_ctx(pool: sqlx::PgPool) -> QueryContext {
        QueryContext::new(pool, admin_auth(), RequestMetadata::default())
    }

    fn mutation_ctx(pool: sqlx::PgPool) -> MutationContext {
        MutationContext::new(pool, admin_auth(), RequestMetadata::default())
    }

    #[tokio::test]
    async fn test_create_user() {
        let db = setup_db().await;
        let ctx = mutation_ctx(db.pool().clone());

        let user = create_user(&ctx, "test@example.com".into(), "Test User".into(), None)
            .await
            .unwrap();

        assert_eq!(user.email, "test@example.com");
        assert_eq!(user.name, "Test User");
        assert_eq!(user.role, UserRole::default());
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn test_create_user_with_role() {
        let db = setup_db().await;
        let ctx = mutation_ctx(db.pool().clone());

        let user = create_user(
            &ctx,
            "admin@example.com".into(),
            "Admin".into(),
            Some(UserRole::Admin),
        )
        .await
        .unwrap();

        assert_eq!(user.role, UserRole::Admin);
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn test_create_user_requires_admin_role() {
        let db = setup_db().await;
        let ctx = MutationContext::new(
            db.pool().clone(),
            AuthContext::authenticated(Uuid::new_v4(), vec!["member".into()], Default::default()),
            RequestMetadata::default(),
        );

        let result = create_user(&ctx, "nope@example.com".into(), "No Admin".into(), None).await;

        assert!(
            matches!(result, Err(ForgeError::Forbidden(_))),
            "non-admin must be rejected with Forbidden, got {result:?}"
        );
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn test_get_users() {
        let db = setup_db().await;
        let m_ctx = mutation_ctx(db.pool().clone());

        create_user(&m_ctx, "a@test.com".into(), "User A".into(), None)
            .await
            .unwrap();
        create_user(&m_ctx, "b@test.com".into(), "User B".into(), None)
            .await
            .unwrap();

        let q_ctx = query_ctx(db.pool().clone());
        let users = get_users(&q_ctx).await.unwrap();
        assert!(users.len() >= 2);
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn test_get_user_by_id() {
        let db = setup_db().await;
        let m_ctx = mutation_ctx(db.pool().clone());

        let created = create_user(&m_ctx, "find@test.com".into(), "Find Me".into(), None)
            .await
            .unwrap();

        let q_ctx = query_ctx(db.pool().clone());
        let found = get_user(&q_ctx, created.id).await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, created.id);
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn test_get_user_not_found() {
        let db = setup_db().await;
        let ctx = query_ctx(db.pool().clone());

        let result = get_user(&ctx, Uuid::new_v4()).await.unwrap();
        assert!(result.is_none());
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn test_update_user() {
        let db = setup_db().await;
        let ctx = mutation_ctx(db.pool().clone());

        let user = create_user(&ctx, "update@test.com".into(), "Original".into(), None)
            .await
            .unwrap();

        let updated = update_user(
            &ctx,
            user.id,
            Some("new@test.com".into()),
            Some("Updated".into()),
            None,
        )
        .await
        .unwrap();

        assert_eq!(updated.email, "new@test.com");
        assert_eq!(updated.name, "Updated");
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn test_delete_user() {
        let db = setup_db().await;
        let ctx = mutation_ctx(db.pool().clone());

        let user = create_user(&ctx, "delete@test.com".into(), "Delete Me".into(), None)
            .await
            .unwrap();

        let deleted = delete_user(&ctx, user.id).await.unwrap();
        assert!(deleted);

        let q_ctx = query_ctx(db.pool().clone());
        let found = get_user(&q_ctx, user.id).await.unwrap();
        assert!(found.is_none());
        db.cleanup().await.unwrap();
    }
}
