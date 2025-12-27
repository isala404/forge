//! Enum types for the Task Manager.

use serde::{Deserialize, Serialize};
use sqlx::Type;

/// Task status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "task_status", rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task is in the backlog.
    Backlog,
    /// Task is ready to work on.
    Todo,
    /// Task is currently being worked on.
    InProgress,
    /// Task is in review.
    InReview,
    /// Task is completed.
    Done,
    /// Task is cancelled.
    Cancelled,
}

impl Default for TaskStatus {
    fn default() -> Self {
        Self::Backlog
    }
}

/// Task priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "task_priority", rename_all = "snake_case")]
pub enum TaskPriority {
    /// Low priority.
    Low,
    /// Medium priority.
    Medium,
    /// High priority.
    High,
    /// Urgent priority.
    Urgent,
}

impl Default for TaskPriority {
    fn default() -> Self {
        Self::Medium
    }
}

/// Team member role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "team_role", rename_all = "snake_case")]
pub enum TeamRole {
    /// Team owner with full permissions.
    Owner,
    /// Team admin with management permissions.
    Admin,
    /// Regular team member.
    Member,
    /// Guest with limited access.
    Guest,
}

impl Default for TeamRole {
    fn default() -> Self {
        Self::Member
    }
}

/// Project status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "project_status", rename_all = "snake_case")]
pub enum ProjectStatus {
    /// Project is active.
    Active,
    /// Project is on hold.
    OnHold,
    /// Project is completed.
    Completed,
    /// Project is archived.
    Archived,
}

impl Default for ProjectStatus {
    fn default() -> Self {
        Self::Active
    }
}
