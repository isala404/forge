//! Data models for the Task Manager.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::enums::*;

/// A team that contains users and projects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A user in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Team membership (many-to-many between teams and users).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub id: Uuid,
    pub team_id: Uuid,
    pub user_id: Uuid,
    pub role: TeamRole,
    pub joined_at: DateTime<Utc>,
}

/// A project within a team.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub team_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: ProjectStatus,
    pub color: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A task within a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub assignee_id: Option<Uuid>,
    pub due_date: Option<DateTime<Utc>>,
    pub position: i32,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A comment on a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: Uuid,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// An attachment on a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub uploaded_by: Uuid,
    pub filename: String,
    pub file_size: i64,
    pub content_type: String,
    pub storage_key: String,
    pub created_at: DateTime<Utc>,
}

// Input types for mutations

/// Input for creating a team.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTeamInput {
    pub name: String,
    pub description: Option<String>,
}

/// Input for creating a project.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectInput {
    pub team_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
}

/// Input for creating a task.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaskInput {
    pub project_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<TaskPriority>,
    pub assignee_id: Option<Uuid>,
    pub due_date: Option<DateTime<Utc>>,
}

/// Input for updating a task.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaskInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<TaskStatus>,
    pub priority: Option<TaskPriority>,
    pub assignee_id: Option<Uuid>,
    pub due_date: Option<DateTime<Utc>>,
    pub position: Option<i32>,
}

/// Input for creating a comment.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCommentInput {
    pub task_id: Uuid,
    pub content: String,
}
