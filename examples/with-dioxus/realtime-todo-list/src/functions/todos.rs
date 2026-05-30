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

#[forge::query(tables("todos"))]
pub async fn list_todos(ctx: &QueryContext) -> Result<Vec<Todo>> {
    let user_id = ctx.user_id()?;
    sqlx::query_as!(
        Todo,
        "SELECT * FROM todos WHERE user_id = $1 ORDER BY created_at DESC",
        user_id
    )
    .fetch_all(ctx.db())
    .await
    .map_err(Into::into)
}

#[forge::mutation(scope = "global")]
pub async fn create_todo(ctx: &MutationContext, input: CreateTodoInput) -> Result<Todo> {
    if input.title.trim().is_empty() {
        return Err(ForgeError::Validation("Title cannot be empty".into()));
    }

    let user_id = ctx.user_id()?;
    let title = input.title.trim().to_string();
    let mut conn = ctx.conn().await?;

    sqlx::query_as!(
        Todo,
        "INSERT INTO todos (user_id, title) VALUES ($1, $2) RETURNING *",
        user_id,
        title
    )
    .fetch_one(&mut conn)
    .await
    .map_err(Into::into)
}

#[forge::mutation]
pub async fn update_todo(ctx: &MutationContext, input: UpdateTodoInput) -> Result<Todo> {
    let user_id = ctx.user_id()?;
    let title = input.title.as_deref();
    let mut conn = ctx.conn().await?;

    sqlx::query_as!(
        Todo,
        "UPDATE todos
         SET title = COALESCE($1, title),
             completed = COALESCE($2, completed)
         WHERE id = $3 AND user_id = $4
         RETURNING *",
        title,
        input.completed,
        input.id,
        user_id
    )
    .fetch_optional(&mut conn)
    .await?
    .ok_or_else(|| ForgeError::NotFound("Todo not found".into()))
}

#[forge::mutation]
pub async fn delete_todo(ctx: &MutationContext, id: Uuid) -> Result<bool> {
    let user_id = ctx.user_id()?;
    let mut conn = ctx.conn().await?;

    let result = sqlx::query!(
        "DELETE FROM todos WHERE id = $1 AND user_id = $2",
        id,
        user_id
    )
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
        let base = TestDatabase::from_env().await.expect("test db");
        let db = base.isolated("todos_test").await.expect("isolated db");
        db.run_sql(&forge::get_internal_sql())
            .await
            .expect("internal sql");
        db.migrate(Path::new("migrations"))
            .await
            .expect("migrations");
        db
    }

    async fn seed_user(pool: &sqlx::PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query!(
            "INSERT INTO users (id, email, name, password_hash) VALUES ($1, $2, $3, $4)",
            id,
            format!("{id}@test.local"),
            "Test User",
            "x"
        )
        .execute(pool)
        .await
        .expect("seed user");
        id
    }

    fn query_ctx(pool: sqlx::PgPool, user_id: Uuid) -> QueryContext {
        QueryContext::new(
            pool,
            AuthContext::authenticated(user_id, vec!["user".into()], Default::default()),
            RequestMetadata::default(),
        )
    }

    fn mutation_ctx(pool: sqlx::PgPool, user_id: Uuid) -> MutationContext {
        MutationContext::new(
            pool,
            AuthContext::authenticated(user_id, vec!["user".into()], Default::default()),
            RequestMetadata::default(),
        )
    }

    #[tokio::test]
    async fn create_todo_trims_and_persists_title() {
        let db = setup_db().await;
        let uid = seed_user(db.pool()).await;
        let ctx = mutation_ctx(db.pool().clone(), uid);

        let todo = create_todo(
            &ctx,
            CreateTodoInput {
                title: "  ship tests  ".into(),
            },
        )
        .await
        .expect("create");

        assert_eq!(todo.title, "ship tests");
        assert_eq!(todo.user_id, uid);
        assert!(!todo.completed);
        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn list_todos_isolates_by_user() {
        let db = setup_db().await;
        let alice = seed_user(db.pool()).await;
        let bob = seed_user(db.pool()).await;

        let alice_mut = mutation_ctx(db.pool().clone(), alice);
        let bob_mut = mutation_ctx(db.pool().clone(), bob);
        create_todo(
            &alice_mut,
            CreateTodoInput {
                title: "alice".into(),
            },
        )
        .await
        .expect("alice todo");
        create_todo(
            &bob_mut,
            CreateTodoInput {
                title: "bob".into(),
            },
        )
        .await
        .expect("bob todo");

        let alice_q = query_ctx(db.pool().clone(), alice);
        let bob_q = query_ctx(db.pool().clone(), bob);
        let alice_todos = list_todos(&alice_q).await.expect("alice list");
        let bob_todos = list_todos(&bob_q).await.expect("bob list");

        assert_eq!(alice_todos.len(), 1);
        assert_eq!(alice_todos[0].title, "alice");
        assert_eq!(bob_todos.len(), 1);
        assert_eq!(bob_todos[0].title, "bob");
        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn update_todo_blocks_other_users() {
        let db = setup_db().await;
        let alice = seed_user(db.pool()).await;
        let bob = seed_user(db.pool()).await;

        let alice_mut = mutation_ctx(db.pool().clone(), alice);
        let todo = create_todo(
            &alice_mut,
            CreateTodoInput {
                title: "hers".into(),
            },
        )
        .await
        .expect("create");

        let bob_mut = mutation_ctx(db.pool().clone(), bob);
        let err = update_todo(
            &bob_mut,
            UpdateTodoInput {
                id: todo.id,
                title: Some("stolen".into()),
                completed: None,
            },
        )
        .await
        .expect_err("bob must not update alice's todo");
        assert!(matches!(err, ForgeError::NotFound(_)));

        let deleted = delete_todo(&bob_mut, todo.id).await.expect("delete call");
        assert!(!deleted, "bob must not delete alice's todo");

        db.cleanup().await.expect("cleanup");
    }
}
