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
