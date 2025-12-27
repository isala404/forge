//! Mutation functions for the Task Manager.

use anyhow::Result;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::schema::*;

/// Create a new team.
pub async fn create_team(pool: &PgPool, input: CreateTeamInput) -> Result<Team> {
    let id = Uuid::new_v4();
    let now = Utc::now();
    let slug = slugify(&input.name);

    let team = sqlx::query_as!(
        Team,
        r#"
        INSERT INTO teams (id, name, slug, description, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $5)
        RETURNING id, name, slug, description, created_at, updated_at
        "#,
        id,
        input.name,
        slug,
        input.description,
        now
    )
    .fetch_one(pool)
    .await?;

    tracing::info!(team_id = %id, name = %team.name, "Team created");
    Ok(team)
}

/// Create a new project.
pub async fn create_project(pool: &PgPool, input: CreateProjectInput) -> Result<Project> {
    let id = Uuid::new_v4();
    let now = Utc::now();

    let project = sqlx::query_as!(
        Project,
        r#"
        INSERT INTO projects (id, team_id, name, description, status, color, created_at, updated_at)
        VALUES ($1, $2, $3, $4, 'active', $5, $6, $6)
        RETURNING id, team_id, name, description, status as "status: ProjectStatus", color, created_at, updated_at
        "#,
        id,
        input.team_id,
        input.name,
        input.description,
        input.color,
        now
    )
    .fetch_one(pool)
    .await?;

    tracing::info!(project_id = %id, name = %project.name, "Project created");
    Ok(project)
}

/// Create a new task.
pub async fn create_task(pool: &PgPool, user_id: Uuid, input: CreateTaskInput) -> Result<Task> {
    let id = Uuid::new_v4();
    let now = Utc::now();
    let priority = input.priority.unwrap_or_default();

    // Get max position for project
    let max_pos: Option<i32> = sqlx::query_scalar!(
        "SELECT MAX(position) FROM tasks WHERE project_id = $1",
        input.project_id
    )
    .fetch_one(pool)
    .await?;
    let position = max_pos.unwrap_or(0) + 1;

    let task = sqlx::query_as!(
        Task,
        r#"
        INSERT INTO tasks (id, project_id, title, description, status, priority, assignee_id, due_date, position, created_by, created_at, updated_at)
        VALUES ($1, $2, $3, $4, 'backlog', $5, $6, $7, $8, $9, $10, $10)
        RETURNING id, project_id, title, description,
                  status as "status: TaskStatus",
                  priority as "priority: TaskPriority",
                  assignee_id, due_date, position, created_by, created_at, updated_at
        "#,
        id,
        input.project_id,
        input.title,
        input.description,
        priority as TaskPriority,
        input.assignee_id,
        input.due_date,
        position,
        user_id,
        now
    )
    .fetch_one(pool)
    .await?;

    tracing::info!(task_id = %id, title = %task.title, "Task created");
    Ok(task)
}

/// Update a task.
pub async fn update_task(pool: &PgPool, task_id: Uuid, input: UpdateTaskInput) -> Result<Task> {
    let now = Utc::now();

    let task = sqlx::query_as!(
        Task,
        r#"
        UPDATE tasks
        SET title = COALESCE($2, title),
            description = COALESCE($3, description),
            status = COALESCE($4, status),
            priority = COALESCE($5, priority),
            assignee_id = COALESCE($6, assignee_id),
            due_date = COALESCE($7, due_date),
            position = COALESCE($8, position),
            updated_at = $9
        WHERE id = $1
        RETURNING id, project_id, title, description,
                  status as "status: TaskStatus",
                  priority as "priority: TaskPriority",
                  assignee_id, due_date, position, created_by, created_at, updated_at
        "#,
        task_id,
        input.title,
        input.description,
        input.status.map(|s| s as TaskStatus),
        input.priority.map(|p| p as TaskPriority),
        input.assignee_id,
        input.due_date,
        input.position,
        now
    )
    .fetch_one(pool)
    .await?;

    tracing::info!(task_id = %task_id, "Task updated");
    Ok(task)
}

/// Delete a task (soft delete).
pub async fn delete_task(pool: &PgPool, task_id: Uuid) -> Result<()> {
    sqlx::query!(
        "UPDATE tasks SET status = 'cancelled', updated_at = NOW() WHERE id = $1",
        task_id
    )
    .execute(pool)
    .await?;

    tracing::info!(task_id = %task_id, "Task deleted (soft)");
    Ok(())
}

/// Add a comment to a task.
pub async fn add_comment(pool: &PgPool, user_id: Uuid, input: CreateCommentInput) -> Result<Comment> {
    let id = Uuid::new_v4();
    let now = Utc::now();

    let comment = sqlx::query_as!(
        Comment,
        r#"
        INSERT INTO comments (id, task_id, author_id, content, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $5)
        RETURNING id, task_id, author_id, content, created_at, updated_at
        "#,
        id,
        input.task_id,
        user_id,
        input.content,
        now
    )
    .fetch_one(pool)
    .await?;

    tracing::info!(comment_id = %id, task_id = %input.task_id, "Comment added");
    Ok(comment)
}

/// Archive a project.
pub async fn archive_project(pool: &PgPool, project_id: Uuid) -> Result<Project> {
    let now = Utc::now();

    let project = sqlx::query_as!(
        Project,
        r#"
        UPDATE projects
        SET status = 'archived', updated_at = $2
        WHERE id = $1
        RETURNING id, team_id, name, description, status as "status: ProjectStatus", color, created_at, updated_at
        "#,
        project_id,
        now
    )
    .fetch_one(pool)
    .await?;

    tracing::info!(project_id = %project_id, "Project archived");
    Ok(project)
}

/// Convert a string to a URL-friendly slug.
fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
