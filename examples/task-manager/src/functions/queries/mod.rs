//! Query functions for the Task Manager.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

use crate::schema::*;

/// Get a team by ID.
pub async fn get_team(pool: &PgPool, team_id: Uuid) -> Result<Option<Team>> {
    let team = sqlx::query_as!(
        Team,
        r#"SELECT id, name, slug, description, created_at, updated_at FROM teams WHERE id = $1"#,
        team_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(team)
}

/// Get all projects for a team.
pub async fn get_team_projects(pool: &PgPool, team_id: Uuid) -> Result<Vec<Project>> {
    let projects = sqlx::query_as!(
        Project,
        r#"
        SELECT id, team_id, name, description, status as "status: ProjectStatus",
               color, created_at, updated_at
        FROM projects
        WHERE team_id = $1
        ORDER BY created_at DESC
        "#,
        team_id
    )
    .fetch_all(pool)
    .await?;

    Ok(projects)
}

/// Get all tasks for a project with optional filtering.
pub async fn get_project_tasks(
    pool: &PgPool,
    project_id: Uuid,
    status: Option<TaskStatus>,
    limit: Option<i32>,
    offset: Option<i32>,
) -> Result<Vec<Task>> {
    let limit = limit.unwrap_or(50).min(100);
    let offset = offset.unwrap_or(0);

    let tasks = if let Some(status) = status {
        sqlx::query_as!(
            Task,
            r#"
            SELECT id, project_id, title, description,
                   status as "status: TaskStatus",
                   priority as "priority: TaskPriority",
                   assignee_id, due_date, position, created_by, created_at, updated_at
            FROM tasks
            WHERE project_id = $1 AND status = $2
            ORDER BY position ASC, created_at DESC
            LIMIT $3 OFFSET $4
            "#,
            project_id,
            status as TaskStatus,
            limit as i64,
            offset as i64
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as!(
            Task,
            r#"
            SELECT id, project_id, title, description,
                   status as "status: TaskStatus",
                   priority as "priority: TaskPriority",
                   assignee_id, due_date, position, created_by, created_at, updated_at
            FROM tasks
            WHERE project_id = $1
            ORDER BY position ASC, created_at DESC
            LIMIT $2 OFFSET $3
            "#,
            project_id,
            limit as i64,
            offset as i64
        )
        .fetch_all(pool)
        .await?
    };

    Ok(tasks)
}

/// Get a task by ID with all details.
pub async fn get_task_details(pool: &PgPool, task_id: Uuid) -> Result<Option<Task>> {
    let task = sqlx::query_as!(
        Task,
        r#"
        SELECT id, project_id, title, description,
               status as "status: TaskStatus",
               priority as "priority: TaskPriority",
               assignee_id, due_date, position, created_by, created_at, updated_at
        FROM tasks
        WHERE id = $1
        "#,
        task_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(task)
}

/// Get comments for a task.
pub async fn get_task_comments(pool: &PgPool, task_id: Uuid) -> Result<Vec<Comment>> {
    let comments = sqlx::query_as!(
        Comment,
        r#"
        SELECT id, task_id, author_id, content, created_at, updated_at
        FROM comments
        WHERE task_id = $1
        ORDER BY created_at ASC
        "#,
        task_id
    )
    .fetch_all(pool)
    .await?;

    Ok(comments)
}

/// Search tasks by title.
pub async fn search_tasks(
    pool: &PgPool,
    project_id: Uuid,
    query: &str,
    limit: Option<i32>,
) -> Result<Vec<Task>> {
    let limit = limit.unwrap_or(20).min(50);
    let pattern = format!("%{}%", query);

    let tasks = sqlx::query_as!(
        Task,
        r#"
        SELECT id, project_id, title, description,
               status as "status: TaskStatus",
               priority as "priority: TaskPriority",
               assignee_id, due_date, position, created_by, created_at, updated_at
        FROM tasks
        WHERE project_id = $1 AND title ILIKE $2
        ORDER BY created_at DESC
        LIMIT $3
        "#,
        project_id,
        pattern,
        limit as i64
    )
    .fetch_all(pool)
    .await?;

    Ok(tasks)
}
