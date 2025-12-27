//! Multi-step workflows for the Task Manager.
//!
//! Workflows are sagas with compensation logic for failures.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::schema::TeamRole;

/// Input for the user invitation workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInvitationInput {
    pub team_id: Uuid,
    pub email: String,
    pub role: TeamRole,
    pub inviter_id: Uuid,
}

/// User invitation workflow - multi-step process with compensation.
///
/// Steps:
/// 1. Check if user already exists
/// 2. Create invitation record
/// 3. Send invitation email
/// 4. Wait for acceptance (async)
/// 5. Add user to team
pub async fn user_invitation_workflow(
    pool: &PgPool,
    input: UserInvitationInput,
) -> Result<InvitationResult> {
    let invitation_id = Uuid::new_v4();
    tracing::info!(
        invitation_id = %invitation_id,
        team_id = %input.team_id,
        email = %input.email,
        "Starting user invitation workflow"
    );

    // Step 1: Check if user already exists
    tracing::debug!("Step 1: Checking existing user");
    let existing_user = sqlx::query!("SELECT id FROM users WHERE email = $1", input.email)
        .fetch_optional(pool)
        .await?;

    if let Some(user) = existing_user {
        // User exists - check if already a member
        let existing_member = sqlx::query!(
            "SELECT id FROM team_members WHERE team_id = $1 AND user_id = $2",
            input.team_id,
            user.id
        )
        .fetch_optional(pool)
        .await?;

        if existing_member.is_some() {
            return Err(anyhow::anyhow!("User is already a team member"));
        }
    }

    // Step 2: Create invitation record
    tracing::debug!("Step 2: Creating invitation record");
    sqlx::query!(
        r#"
        INSERT INTO invitations (id, team_id, email, role, inviter_id, created_at, expires_at)
        VALUES ($1, $2, $3, $4, $5, NOW(), NOW() + INTERVAL '7 days')
        "#,
        invitation_id,
        input.team_id,
        input.email,
        input.role as TeamRole,
        input.inviter_id
    )
    .execute(pool)
    .await?;

    // Step 3: Send invitation email
    tracing::debug!("Step 3: Sending invitation email");
    // In real app: dispatch email job
    // If this fails, we need to compensate by deleting the invitation

    // Step 4: Return pending result (user will accept async)
    Ok(InvitationResult {
        invitation_id,
        status: InvitationStatus::Pending,
    })
}

/// Compensation for user invitation workflow.
pub async fn compensate_user_invitation(pool: &PgPool, invitation_id: Uuid) -> Result<()> {
    tracing::info!(invitation_id = %invitation_id, "Compensating user invitation");

    // Delete the invitation record
    sqlx::query!("DELETE FROM invitations WHERE id = $1", invitation_id)
        .execute(pool)
        .await?;

    tracing::info!(invitation_id = %invitation_id, "Invitation compensated");
    Ok(())
}

/// Input for the project setup workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSetupInput {
    pub team_id: Uuid,
    pub name: String,
    pub template: Option<String>,
    pub owner_id: Uuid,
}

/// Project setup workflow - creates project with default structure.
///
/// Steps:
/// 1. Create project
/// 2. Create default task statuses
/// 3. Create sample tasks (if template)
/// 4. Add owner as project member
/// 5. Send welcome notification
pub async fn project_setup_workflow(
    pool: &PgPool,
    input: ProjectSetupInput,
) -> Result<ProjectSetupResult> {
    let project_id = Uuid::new_v4();
    tracing::info!(
        project_id = %project_id,
        team_id = %input.team_id,
        name = %input.name,
        "Starting project setup workflow"
    );

    // Step 1: Create project
    tracing::debug!("Step 1: Creating project");
    sqlx::query!(
        r#"
        INSERT INTO projects (id, team_id, name, status, created_at, updated_at)
        VALUES ($1, $2, $3, 'active', NOW(), NOW())
        "#,
        project_id,
        input.team_id,
        input.name
    )
    .execute(pool)
    .await?;

    // Step 2: Create default labels
    tracing::debug!("Step 2: Creating default labels");
    let default_labels = vec![
        ("bug", "#ef4444"),
        ("feature", "#3b82f6"),
        ("improvement", "#10b981"),
        ("documentation", "#8b5cf6"),
    ];

    for (label_name, color) in default_labels {
        sqlx::query!(
            "INSERT INTO labels (id, project_id, name, color) VALUES ($1, $2, $3, $4)",
            Uuid::new_v4(),
            project_id,
            label_name,
            color
        )
        .execute(pool)
        .await?;
    }

    // Step 3: Create sample tasks if using a template
    if let Some(template) = &input.template {
        tracing::debug!("Step 3: Creating template tasks");
        let sample_tasks = match template.as_str() {
            "kanban" => vec![
                "Set up project board",
                "Define workflow stages",
                "Invite team members",
            ],
            "scrum" => vec![
                "Create product backlog",
                "Plan first sprint",
                "Set up daily standup",
            ],
            _ => vec!["Welcome to your new project!"],
        };

        for (i, task_title) in sample_tasks.iter().enumerate() {
            sqlx::query!(
                r#"
                INSERT INTO tasks (id, project_id, title, status, priority, position, created_by, created_at, updated_at)
                VALUES ($1, $2, $3, 'todo', 'medium', $4, $5, NOW(), NOW())
                "#,
                Uuid::new_v4(),
                project_id,
                *task_title,
                (i + 1) as i32,
                input.owner_id
            )
            .execute(pool)
            .await?;
        }
    }

    // Step 4: Send welcome notification (simulated)
    tracing::debug!("Step 4: Sending welcome notification");
    // In real app: dispatch notification

    tracing::info!(project_id = %project_id, "Project setup complete");

    Ok(ProjectSetupResult {
        project_id,
        labels_created: 4,
        tasks_created: if input.template.is_some() { 3 } else { 0 },
    })
}

/// Result of user invitation workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitationResult {
    pub invitation_id: Uuid,
    pub status: InvitationStatus,
}

/// Invitation status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InvitationStatus {
    Pending,
    Accepted,
    Expired,
    Cancelled,
}

/// Result of project setup workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSetupResult {
    pub project_id: Uuid,
    pub labels_created: i32,
    pub tasks_created: i32,
}
