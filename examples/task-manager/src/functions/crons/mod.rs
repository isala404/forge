//! Cron scheduled tasks for the Task Manager.
//!
//! Crons run on a schedule and can dispatch jobs.

use anyhow::Result;
use sqlx::PgPool;

/// Daily reminder cron - runs every day at 9 AM.
/// Schedule: "0 9 * * *"
pub async fn daily_reminder_cron(pool: &PgPool) -> Result<()> {
    tracing::info!("Running daily reminder cron");

    // Find all tasks due today
    let due_today = sqlx::query!(
        r#"
        SELECT t.id, t.title, t.assignee_id, u.email
        FROM tasks t
        JOIN users u ON t.assignee_id = u.id
        WHERE t.due_date::date = CURRENT_DATE
        AND t.status != 'done' AND t.status != 'cancelled'
        "#
    )
    .fetch_all(pool)
    .await?;

    tracing::info!("Found {} tasks due today", due_today.len());

    // In a real app, dispatch email jobs for each
    for task in due_today {
        tracing::debug!(
            task_id = %task.id,
            assignee = %task.email,
            "Would dispatch reminder email for: {}",
            task.title
        );
        // job_dispatcher.dispatch(SendEmailJob { ... }).await?;
    }

    Ok(())
}

/// Cleanup old tasks cron - runs every Sunday at midnight.
/// Schedule: "0 0 * * 0"
pub async fn cleanup_old_tasks_cron(pool: &PgPool) -> Result<()> {
    tracing::info!("Running cleanup cron");

    // Archive tasks that have been done for more than 90 days
    let result = sqlx::query!(
        r#"
        UPDATE tasks
        SET status = 'cancelled'
        WHERE status = 'done'
        AND updated_at < NOW() - INTERVAL '90 days'
        "#
    )
    .execute(pool)
    .await?;

    tracing::info!(
        archived = result.rows_affected(),
        "Archived old completed tasks"
    );

    // Clean up orphaned attachments
    let deleted = sqlx::query!(
        r#"
        DELETE FROM attachments
        WHERE task_id NOT IN (SELECT id FROM tasks)
        "#
    )
    .execute(pool)
    .await?;

    tracing::info!(
        deleted = deleted.rows_affected(),
        "Deleted orphaned attachments"
    );

    Ok(())
}

/// Weekly report cron - runs every Monday at 8 AM.
/// Schedule: "0 8 * * 1"
pub async fn weekly_report_cron(pool: &PgPool) -> Result<()> {
    tracing::info!("Running weekly report cron");

    // Get all teams
    let teams = sqlx::query!("SELECT id, name FROM teams")
        .fetch_all(pool)
        .await?;

    for team in teams {
        // Calculate metrics for the past week
        let stats = sqlx::query!(
            r#"
            SELECT
                COUNT(*) FILTER (WHERE created_at > NOW() - INTERVAL '7 days') as created,
                COUNT(*) FILTER (WHERE status = 'done' AND updated_at > NOW() - INTERVAL '7 days') as completed
            FROM tasks t
            JOIN projects p ON t.project_id = p.id
            WHERE p.team_id = $1
            "#,
            team.id
        )
        .fetch_one(pool)
        .await?;

        tracing::info!(
            team = %team.name,
            created = ?stats.created,
            completed = ?stats.completed,
            "Weekly stats calculated"
        );

        // In a real app, dispatch report generation job
        // job_dispatcher.dispatch(GenerateReportJob { ... }).await?;
    }

    Ok(())
}
