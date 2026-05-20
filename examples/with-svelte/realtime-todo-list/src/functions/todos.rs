use forge::prelude::*;
use uuid::Uuid;

use crate::schema::Todo;

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateTodoInput {
    pub title: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateTodoInput {
    pub id: Uuid,
    pub title: Option<String>,
    pub completed: Option<bool>,
}

#[forge::query(auth = "none", tables("todos"))]
pub async fn list_todos(ctx: &QueryContext) -> Result<Vec<Todo>> {
    sqlx::query_as!(Todo, "SELECT * FROM todos ORDER BY created_at DESC")
        .fetch_all(ctx.db())
        .await
        .map_err(Into::into)
}

#[forge::mutation(auth = "none")]
pub async fn create_todo(ctx: &MutationContext, input: CreateTodoInput) -> Result<Todo> {
    if input.title.trim().is_empty() {
        return Err(ForgeError::Validation("Title cannot be empty".into()));
    }

    let title = input.title.trim().to_string();
    let mut conn = ctx.conn().await?;

    sqlx::query_as!(
        Todo,
        "INSERT INTO todos (title) VALUES ($1) RETURNING *",
        title
    )
    .fetch_one(&mut conn)
    .await
    .map_err(Into::into)
}

#[forge::mutation(auth = "none")]
pub async fn update_todo(ctx: &MutationContext, input: UpdateTodoInput) -> Result<Todo> {
    let title = input.title.as_deref();
    let mut conn = ctx.conn().await?;

    sqlx::query_as!(
        Todo,
        "UPDATE todos
         SET title = COALESCE($1, title),
             completed = COALESCE($2, completed)
         WHERE id = $3
         RETURNING *",
        title,
        input.completed,
        input.id
    )
    .fetch_optional(&mut conn)
    .await?
    .ok_or_else(|| ForgeError::NotFound("Todo not found".into()))
}

#[forge::mutation(auth = "none")]
pub async fn delete_todo(ctx: &MutationContext, id: Uuid) -> Result<bool> {
    let mut conn = ctx.conn().await?;

    let result = sqlx::query!("DELETE FROM todos WHERE id = $1", id)
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
        let db = base.isolated("todos_test").await.unwrap();
        db.run_sql(&forge::get_internal_sql()).await.unwrap();
        db.migrate(Path::new("migrations")).await.unwrap();
        db
    }

    fn query_ctx(pool: sqlx::PgPool) -> QueryContext {
        QueryContext::new(
            pool,
            AuthContext::unauthenticated(),
            RequestMetadata::default(),
        )
    }

    fn mutation_ctx(pool: sqlx::PgPool) -> MutationContext {
        MutationContext::new(
            pool,
            AuthContext::unauthenticated(),
            RequestMetadata::default(),
        )
    }

    #[tokio::test]
    async fn test_create_todo_trims_and_persists_title() {
        let db = setup_db().await;
        let ctx = mutation_ctx(db.pool().clone());

        let todo = create_todo(
            &ctx,
            CreateTodoInput {
                title: "  ship tests  ".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(todo.title, "ship tests");
        assert!(!todo.completed);
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn test_create_todo_rejects_blank_title() {
        let db = setup_db().await;
        let ctx = mutation_ctx(db.pool().clone());

        let err = create_todo(
            &ctx,
            CreateTodoInput {
                title: "   ".into(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ForgeError::Validation(_)));
        assert!(err.to_string().contains("Title cannot be empty"));
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn test_update_todo_toggles_completion_without_overwriting_title() {
        let db = setup_db().await;
        let ctx = mutation_ctx(db.pool().clone());

        let created = create_todo(
            &ctx,
            CreateTodoInput {
                title: "cover release path".into(),
            },
        )
        .await
        .unwrap();

        let updated = update_todo(
            &ctx,
            UpdateTodoInput {
                id: created.id,
                title: None,
                completed: Some(true),
            },
        )
        .await
        .unwrap();

        assert_eq!(updated.title, created.title);
        assert!(updated.completed);
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn test_update_todo_missing_id_returns_not_found() {
        let db = setup_db().await;
        let ctx = mutation_ctx(db.pool().clone());

        let err = update_todo(
            &ctx,
            UpdateTodoInput {
                id: Uuid::new_v4(),
                title: Some("missing".into()),
                completed: None,
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ForgeError::NotFound(_)));
        db.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn test_delete_todo_removes_item_from_query_results() {
        let db = setup_db().await;
        let m_ctx = mutation_ctx(db.pool().clone());

        let todo = create_todo(
            &m_ctx,
            CreateTodoInput {
                title: "delete me".into(),
            },
        )
        .await
        .unwrap();

        assert!(delete_todo(&m_ctx, todo.id).await.unwrap());

        let q_ctx = query_ctx(db.pool().clone());
        let todos = list_todos(&q_ctx).await.unwrap();
        assert!(!todos.iter().any(|item| item.id == todo.id));
        db.cleanup().await.unwrap();
    }
}
