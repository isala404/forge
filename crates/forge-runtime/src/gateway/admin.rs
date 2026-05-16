//! Admin REST endpoints under `/_api/admin/*`.
//!
//! All routes here are admin-gated (require the `admin` role on the calling
//! `AuthContext`) and append a row to `forge_admin_audit` whenever they
//! perform a state-changing action. Read-only list/inspect endpoints don't
//! audit because they're hot paths for dashboards.
//!
//! Surface area (operator-facing):
//!
//! - `GET    /_api/admin/jobs[?status=&job_type=&limit=]`
//! - `GET    /_api/admin/jobs/{id}`
//! - `POST   /_api/admin/jobs/{id}/cancel        body: {reason?}`
//! - `POST   /_api/admin/jobs/{id}/retry         body: {reason?}`
//! - `POST   /_api/admin/jobs/{id}/force-abort   body: {reason?}`
//! - `GET    /_api/admin/workflows[?status=&workflow_name=&limit=]`
//! - `GET    /_api/admin/workflows/{id}`
//! - `POST   /_api/admin/workflows/{id}/cancel        body: {reason?}`
//! - `POST   /_api/admin/workflows/{id}/retry         body: {reason?}`
//! - `POST   /_api/admin/workflows/{id}/force-abort   body: {reason?}`
//! - `GET    /_api/admin/queues`
//! - `POST   /_api/admin/queues/{name}/pause     body: {reason?}`
//! - `POST   /_api/admin/queues/{name}/resume`
//! - `GET    /_api/admin/nodes`
//! - `GET    /_api/admin/leaders`

use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use forge_core::function::AuthContext;

use super::tracing::TracingState;

/// State shared by all admin handlers.
#[derive(Clone)]
pub struct AdminState {
    pub db_pool: PgPool,
}

/// Build the admin router. Returns `None` when no admin handler can do any
/// work (no job queue and no workflow executor); the caller can skip merging
/// in that case.
pub fn admin_router(state: AdminState) -> Router {
    Router::new()
        .route("/admin/jobs", get(list_jobs))
        .route("/admin/jobs/{id}", get(get_job))
        .route("/admin/jobs/{id}/cancel", post(cancel_job))
        .route("/admin/jobs/{id}/retry", post(retry_job))
        .route("/admin/jobs/{id}/force-abort", post(force_abort_job))
        .route("/admin/workflows", get(list_workflows))
        .route("/admin/workflows/{id}", get(get_workflow))
        .route("/admin/workflows/{id}/cancel", post(cancel_workflow))
        .route("/admin/workflows/{id}/retry", post(retry_workflow))
        .route(
            "/admin/workflows/{id}/force-abort",
            post(force_abort_workflow),
        )
        .route("/admin/queues", get(list_queues))
        .route("/admin/queues/{name}/pause", post(pause_queue))
        .route("/admin/queues/{name}/resume", post(resume_queue))
        .route("/admin/nodes", get(list_nodes))
        .route("/admin/leaders", get(list_leaders))
        .with_state(Arc::new(state))
}

const ADMIN_ROLE: &str = "admin";

#[derive(Serialize)]
struct AdminError {
    error: &'static str,
    message: String,
}

fn admin_err(
    status: StatusCode,
    code: &'static str,
    msg: impl Into<String>,
) -> axum::response::Response {
    (
        status,
        Json(AdminError {
            error: code,
            message: msg.into(),
        }),
    )
        .into_response()
}

#[allow(clippy::result_large_err)] // Err is an axum Response by design (returned straight to the client).
fn require_admin(auth: &AuthContext) -> Result<(), axum::response::Response> {
    if !auth.is_authenticated() {
        return Err(admin_err(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "Authentication required for admin endpoints",
        ));
    }
    if !auth.has_role(ADMIN_ROLE) {
        return Err(admin_err(
            StatusCode::FORBIDDEN,
            "forbidden",
            format!("Role '{}' required", ADMIN_ROLE),
        ));
    }
    Ok(())
}

fn actor_subject(auth: &AuthContext) -> Option<String> {
    auth.user_id().map(|u| u.to_string()).or_else(|| {
        auth.claims()
            .get("sub")
            .and_then(|v| v.as_str())
            .map(String::from)
    })
}

fn actor_roles(auth: &AuthContext) -> Vec<String> {
    auth.roles().to_vec()
}

#[derive(Default, Deserialize)]
struct ActionBody {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct JobListQuery {
    status: Option<String>,
    job_type: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Default, Deserialize)]
struct WorkflowListQuery {
    status: Option<String>,
    workflow_name: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Serialize)]
struct AuditedAction {
    action: &'static str,
    target_type: &'static str,
    target_id: String,
    accepted: bool,
}

#[allow(clippy::too_many_arguments)] // Audit captures the full request envelope; splitting it into a struct buys nothing.
async fn audit(
    pool: &PgPool,
    auth: &AuthContext,
    tracing_state: Option<&TracingState>,
    action: &str,
    target_type: &str,
    target_id: Option<String>,
    reason: Option<&str>,
    details: serde_json::Value,
) {
    let actor = actor_subject(auth);
    let roles = actor_roles(auth);
    let request_id = tracing_state.map(|s| s.request_id.clone());
    let trace_id = tracing_state.map(|s| s.trace_id.clone());
    let res = sqlx::query!(
        r#"
        INSERT INTO forge_admin_audit
            (actor_subject, actor_roles, action, target_type, target_id, reason, request_id, trace_id, details)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
        actor.as_deref(),
        &roles,
        action,
        target_type,
        target_id.as_deref(),
        reason,
        request_id.as_deref(),
        trace_id.as_deref(),
        details,
    )
    .execute(pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, action, target_type, "Failed to write admin audit record");
    }
}

// --- Jobs ---

#[derive(Serialize)]
struct JobRow {
    id: Uuid,
    job_type: String,
    status: String,
    priority: i32,
    attempts: i32,
    max_attempts: i32,
    worker_capability: Option<String>,
    owner_subject: Option<String>,
    scheduled_at: chrono::DateTime<chrono::Utc>,
    created_at: chrono::DateTime<chrono::Utc>,
    claimed_at: Option<chrono::DateTime<chrono::Utc>>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
    failed_at: Option<chrono::DateTime<chrono::Utc>>,
    cancelled_at: Option<chrono::DateTime<chrono::Utc>>,
    cancel_reason: Option<String>,
    last_error: Option<String>,
}

async fn list_jobs(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Query(filters): Query<JobListQuery>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let limit = filters.limit.unwrap_or(100).clamp(1, 1000);
    let result = sqlx::query!(
        r#"
        SELECT id, job_type, status, priority, attempts, max_attempts,
               worker_capability, owner_subject, scheduled_at, created_at,
               claimed_at, started_at, completed_at, failed_at, cancelled_at,
               cancel_reason, last_error
        FROM forge_jobs
        WHERE ($1::text IS NULL OR status = $1)
          AND ($2::text IS NULL OR job_type = $2)
        ORDER BY created_at DESC
        LIMIT $3
        "#,
        filters.status,
        filters.job_type,
        limit,
    )
    .fetch_all(&state.db_pool)
    .await;
    match result {
        Ok(rows) => {
            let items: Vec<JobRow> = rows
                .into_iter()
                .map(|r| JobRow {
                    id: r.id,
                    job_type: r.job_type,
                    status: r.status,
                    priority: r.priority,
                    attempts: r.attempts,
                    max_attempts: r.max_attempts,
                    worker_capability: r.worker_capability,
                    owner_subject: r.owner_subject,
                    scheduled_at: r.scheduled_at,
                    created_at: r.created_at,
                    claimed_at: r.claimed_at,
                    started_at: r.started_at,
                    completed_at: r.completed_at,
                    failed_at: r.failed_at,
                    cancelled_at: r.cancelled_at,
                    cancel_reason: r.cancel_reason,
                    last_error: r.last_error,
                })
                .collect();
            Json(serde_json::json!({ "items": items })).into_response()
        }
        Err(e) => admin_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            format!("Failed to list jobs: {}", e),
        ),
    }
}

async fn get_job(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let row = sqlx::query!(
        r#"
        SELECT id, job_type, input, output, status, priority, attempts, max_attempts,
               last_error, progress_percent, progress_message, worker_capability,
               worker_id, idempotency_key, owner_subject, scheduled_at, created_at,
               claimed_at, started_at, completed_at, failed_at,
               cancel_requested_at, cancelled_at, cancel_reason, last_heartbeat,
               expires_at, metadata
        FROM forge_jobs
        WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(&state.db_pool)
    .await;
    match row {
        Ok(Some(r)) => Json(serde_json::json!({
            "id": r.id,
            "job_type": r.job_type,
            "input": r.input,
            "output": r.output,
            "status": r.status,
            "priority": r.priority,
            "attempts": r.attempts,
            "max_attempts": r.max_attempts,
            "last_error": r.last_error,
            "progress_percent": r.progress_percent,
            "progress_message": r.progress_message,
            "worker_capability": r.worker_capability,
            "worker_id": r.worker_id,
            "idempotency_key": r.idempotency_key,
            "owner_subject": r.owner_subject,
            "scheduled_at": r.scheduled_at,
            "created_at": r.created_at,
            "claimed_at": r.claimed_at,
            "started_at": r.started_at,
            "completed_at": r.completed_at,
            "failed_at": r.failed_at,
            "cancel_requested_at": r.cancel_requested_at,
            "cancelled_at": r.cancelled_at,
            "cancel_reason": r.cancel_reason,
            "last_heartbeat": r.last_heartbeat,
            "expires_at": r.expires_at,
            "metadata": r.metadata,
        }))
        .into_response(),
        Ok(None) => admin_err(StatusCode::NOT_FOUND, "not_found", "Job not found"),
        Err(e) => admin_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            format!("Failed to fetch job: {}", e),
        ),
    }
}

async fn cancel_job(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing_state): Extension<TracingState>,
    Path(id): Path<Uuid>,
    body: Option<Json<ActionBody>>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let reason = body.and_then(|b| b.0.reason);
    // Match `JobQueue::request_cancel` semantics: running jobs go to
    // `cancel_requested` so the worker finalizes; non-running jobs go
    // straight to `cancelled`. Admin call ignores ownership.
    let updated_running = sqlx::query!(
        r#"
        UPDATE forge_jobs
        SET status = 'cancel_requested',
            cancel_requested_at = NOW(),
            cancel_reason = COALESCE($2, cancel_reason)
        WHERE id = $1 AND status = 'running'
        "#,
        id,
        reason,
    )
    .execute(&state.db_pool)
    .await;
    let running_affected = match updated_running {
        Ok(r) => r.rows_affected(),
        Err(e) => {
            return admin_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cancel_failed",
                format!("Failed to request job cancel: {}", e),
            );
        }
    };
    let accepted = if running_affected > 0 {
        true
    } else {
        match sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET status = 'cancelled',
                cancelled_at = NOW(),
                cancel_reason = COALESCE($2, cancel_reason)
            WHERE id = $1
              AND status NOT IN ('completed', 'failed', 'dead_letter', 'cancelled', 'cancel_requested')
            "#,
            id,
            reason,
        )
        .execute(&state.db_pool)
        .await
        {
            Ok(r) => r.rows_affected() > 0,
            Err(e) => {
                return admin_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "cancel_failed",
                    format!("Failed to cancel job: {}", e),
                );
            }
        }
    };
    audit(
        &state.db_pool,
        &auth,
        Some(&tracing_state),
        "job.cancel",
        "job",
        Some(id.to_string()),
        reason.as_deref(),
        serde_json::json!({ "accepted": accepted }),
    )
    .await;
    Json(AuditedAction {
        action: "cancel",
        target_type: "job",
        target_id: id.to_string(),
        accepted,
    })
    .into_response()
}

async fn retry_job(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing_state): Extension<TracingState>,
    Path(id): Path<Uuid>,
    body: Option<Json<ActionBody>>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let reason = body.and_then(|b| b.0.reason);
    let result = sqlx::query!(
        r#"
        UPDATE forge_jobs
        SET status = 'pending',
            attempts = 0,
            last_error = NULL,
            scheduled_at = NOW(),
            cancelled_at = NULL,
            cancel_requested_at = NULL,
            cancel_reason = NULL,
            completed_at = NULL,
            failed_at = NULL,
            worker_id = NULL,
            claimed_at = NULL,
            started_at = NULL
        WHERE id = $1
          AND status IN ('failed', 'dead_letter', 'cancelled')
        "#,
        id,
    )
    .execute(&state.db_pool)
    .await;

    let accepted = match result {
        Ok(r) => r.rows_affected() > 0,
        Err(e) => {
            return admin_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "retry_failed",
                format!("Failed to retry job: {}", e),
            );
        }
    };
    audit(
        &state.db_pool,
        &auth,
        Some(&tracing_state),
        "job.retry",
        "job",
        Some(id.to_string()),
        reason.as_deref(),
        serde_json::json!({ "accepted": accepted }),
    )
    .await;
    if accepted {
        let _ = sqlx::query!("SELECT pg_notify('forge_jobs_available', 'admin_retry')")
            .execute(&state.db_pool)
            .await;
    }
    Json(AuditedAction {
        action: "retry",
        target_type: "job",
        target_id: id.to_string(),
        accepted,
    })
    .into_response()
}

async fn force_abort_job(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing_state): Extension<TracingState>,
    Path(id): Path<Uuid>,
    body: Option<Json<ActionBody>>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let reason = body.and_then(|b| b.0.reason);
    let result = sqlx::query!(
        r#"
        UPDATE forge_jobs
        SET status = 'cancelled',
            cancelled_at = NOW(),
            cancel_requested_at = COALESCE(cancel_requested_at, NOW()),
            cancel_reason = COALESCE($2, cancel_reason, 'force_abort')
        WHERE id = $1
          AND status NOT IN ('completed', 'cancelled')
        "#,
        id,
        reason,
    )
    .execute(&state.db_pool)
    .await;
    let accepted = match result {
        Ok(r) => r.rows_affected() > 0,
        Err(e) => {
            return admin_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "force_abort_failed",
                format!("Failed to force-abort job: {}", e),
            );
        }
    };
    audit(
        &state.db_pool,
        &auth,
        Some(&tracing_state),
        "job.force_abort",
        "job",
        Some(id.to_string()),
        reason.as_deref(),
        serde_json::json!({ "accepted": accepted }),
    )
    .await;
    Json(AuditedAction {
        action: "force_abort",
        target_type: "job",
        target_id: id.to_string(),
        accepted,
    })
    .into_response()
}

// --- Workflows ---

#[derive(Serialize)]
struct WorkflowRow {
    id: Uuid,
    workflow_name: String,
    workflow_version: String,
    status: String,
    owner_subject: Option<String>,
    started_at: chrono::DateTime<chrono::Utc>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
    wake_at: Option<chrono::DateTime<chrono::Utc>>,
    waiting_for_event: Option<String>,
    cancel_requested_at: Option<chrono::DateTime<chrono::Utc>>,
    cancel_reason: Option<String>,
    error: Option<String>,
}

async fn list_workflows(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Query(filters): Query<WorkflowListQuery>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let limit = filters.limit.unwrap_or(100).clamp(1, 1000);
    let rows = sqlx::query!(
        r#"
        SELECT id, workflow_name, workflow_version, status, owner_subject,
               started_at, completed_at, wake_at, waiting_for_event,
               cancel_requested_at, cancel_reason, error
        FROM forge_workflow_runs
        WHERE ($1::text IS NULL OR status = $1)
          AND ($2::text IS NULL OR workflow_name = $2)
        ORDER BY started_at DESC
        LIMIT $3
        "#,
        filters.status,
        filters.workflow_name,
        limit,
    )
    .fetch_all(&state.db_pool)
    .await;
    match rows {
        Ok(rs) => {
            let items: Vec<WorkflowRow> = rs
                .into_iter()
                .map(|r| WorkflowRow {
                    id: r.id,
                    workflow_name: r.workflow_name,
                    workflow_version: r.workflow_version,
                    status: r.status,
                    owner_subject: r.owner_subject,
                    started_at: r.started_at,
                    completed_at: r.completed_at,
                    wake_at: r.wake_at,
                    waiting_for_event: r.waiting_for_event,
                    cancel_requested_at: r.cancel_requested_at,
                    cancel_reason: r.cancel_reason,
                    error: r.error,
                })
                .collect();
            Json(serde_json::json!({ "items": items })).into_response()
        }
        Err(e) => admin_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            format!("Failed to list workflows: {}", e),
        ),
    }
}

async fn get_workflow(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let row = sqlx::query!(
        r#"
        SELECT id, workflow_name, workflow_version, workflow_signature,
               owner_subject, input, output, status, blocking_reason,
               resolution_reason, current_step, step_results, started_at,
               completed_at, error, trace_id, suspended_at, wake_at,
               waiting_for_event, event_timeout_at, tenant_id,
               cancel_requested_at, cancel_reason, metadata
        FROM forge_workflow_runs
        WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(&state.db_pool)
    .await;

    match row {
        Ok(Some(r)) => Json(serde_json::json!({
            "id": r.id,
            "workflow_name": r.workflow_name,
            "workflow_version": r.workflow_version,
            "workflow_signature": r.workflow_signature,
            "owner_subject": r.owner_subject,
            "input": r.input,
            "output": r.output,
            "status": r.status,
            "blocking_reason": r.blocking_reason,
            "resolution_reason": r.resolution_reason,
            "current_step": r.current_step,
            "step_results": r.step_results,
            "started_at": r.started_at,
            "completed_at": r.completed_at,
            "error": r.error,
            "trace_id": r.trace_id,
            "suspended_at": r.suspended_at,
            "wake_at": r.wake_at,
            "waiting_for_event": r.waiting_for_event,
            "event_timeout_at": r.event_timeout_at,
            "tenant_id": r.tenant_id,
            "cancel_requested_at": r.cancel_requested_at,
            "cancel_reason": r.cancel_reason,
            "metadata": r.metadata,
        }))
        .into_response(),
        Ok(None) => admin_err(StatusCode::NOT_FOUND, "not_found", "Workflow not found"),
        Err(e) => admin_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            format!("Failed to fetch workflow: {}", e),
        ),
    }
}

async fn cancel_workflow(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing_state): Extension<TracingState>,
    Path(id): Path<Uuid>,
    body: Option<Json<ActionBody>>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let reason = body.and_then(|b| b.0.reason);
    let effective = reason.clone().unwrap_or_else(|| "admin cancel".to_string());
    // Sets `cancel_requested_at`; the `forge_workflow_runs_cancel_notify`
    // trigger fires a NOTIFY on `forge_workflow_wakeup` so the scheduler
    // picks the row up immediately (target: <50 ms).
    let result = sqlx::query!(
        r#"
        UPDATE forge_workflow_runs
        SET cancel_requested_at = NOW(),
            cancel_reason = $2
        WHERE id = $1
          AND status IN ('pending', 'running', 'sleeping', 'waiting')
          AND cancel_requested_at IS NULL
        "#,
        id,
        effective,
    )
    .execute(&state.db_pool)
    .await;
    let accepted = match result {
        Ok(r) => r.rows_affected() > 0,
        Err(e) => {
            return admin_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cancel_failed",
                format!("Failed to request workflow cancel: {}", e),
            );
        }
    };
    audit(
        &state.db_pool,
        &auth,
        Some(&tracing_state),
        "workflow.cancel",
        "workflow",
        Some(id.to_string()),
        reason.as_deref(),
        serde_json::json!({ "accepted": accepted }),
    )
    .await;
    Json(AuditedAction {
        action: "cancel",
        target_type: "workflow",
        target_id: id.to_string(),
        accepted,
    })
    .into_response()
}

async fn retry_workflow(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing_state): Extension<TracingState>,
    Path(id): Path<Uuid>,
    body: Option<Json<ActionBody>>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let reason = body.and_then(|b| b.0.reason);
    // Retry = re-mark the run as pending and clear terminal state so the
    // scheduler picks it back up. Compensation state and step results are
    // preserved; retrying a saga skips already-completed steps.
    let result = sqlx::query!(
        r#"
        UPDATE forge_workflow_runs
        SET status = 'pending',
            error = NULL,
            completed_at = NULL,
            cancel_requested_at = NULL,
            cancel_reason = NULL,
            suspended_at = NULL,
            wake_at = NULL,
            waiting_for_event = NULL,
            event_timeout_at = NULL
        WHERE id = $1
          AND status IN ('failed', 'cancelled_by_operator',
                         'blocked_missing_version', 'blocked_signature_mismatch',
                         'blocked_missing_handler', 'retired_unresumable',
                         'compensated')
        "#,
        id,
    )
    .execute(&state.db_pool)
    .await;
    let accepted = match result {
        Ok(r) => r.rows_affected() > 0,
        Err(e) => {
            return admin_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "retry_failed",
                format!("Failed to retry workflow: {}", e),
            );
        }
    };
    audit(
        &state.db_pool,
        &auth,
        Some(&tracing_state),
        "workflow.retry",
        "workflow",
        Some(id.to_string()),
        reason.as_deref(),
        serde_json::json!({ "accepted": accepted }),
    )
    .await;
    if accepted {
        let _ = sqlx::query!("SELECT pg_notify('forge_workflow_wakeup', 'admin_retry')")
            .execute(&state.db_pool)
            .await;
    }
    Json(AuditedAction {
        action: "retry",
        target_type: "workflow",
        target_id: id.to_string(),
        accepted,
    })
    .into_response()
}

async fn force_abort_workflow(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing_state): Extension<TracingState>,
    Path(id): Path<Uuid>,
    body: Option<Json<ActionBody>>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let reason = body.and_then(|b| b.0.reason);
    let effective = reason
        .clone()
        .unwrap_or_else(|| "force_abort by admin".to_string());
    let result = sqlx::query!(
        r#"
        UPDATE forge_workflow_runs
        SET status = 'cancelled_by_operator',
            error = COALESCE(error, $2),
            cancel_requested_at = COALESCE(cancel_requested_at, NOW()),
            cancel_reason = COALESCE($2, cancel_reason),
            completed_at = NOW()
        WHERE id = $1
          AND status NOT IN ('completed', 'cancelled_by_operator')
        "#,
        id,
        effective,
    )
    .execute(&state.db_pool)
    .await;
    let accepted = match result {
        Ok(r) => r.rows_affected() > 0,
        Err(e) => {
            return admin_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "force_abort_failed",
                format!("Failed to force-abort workflow: {}", e),
            );
        }
    };
    audit(
        &state.db_pool,
        &auth,
        Some(&tracing_state),
        "workflow.force_abort",
        "workflow",
        Some(id.to_string()),
        reason.as_deref(),
        serde_json::json!({ "accepted": accepted }),
    )
    .await;
    Json(AuditedAction {
        action: "force_abort",
        target_type: "workflow",
        target_id: id.to_string(),
        accepted,
    })
    .into_response()
}

// --- Queues ---

#[derive(Serialize)]
struct QueueRow {
    name: String,
    pending: i64,
    running: i64,
    paused: bool,
    paused_at: Option<chrono::DateTime<chrono::Utc>>,
    paused_by: Option<String>,
    reason: Option<String>,
}

async fn list_queues(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let stats = sqlx::query!(
        r#"
        SELECT
            COALESCE(worker_capability, 'default') AS "name!",
            SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END)::BIGINT AS pending,
            SUM(CASE WHEN status IN ('claimed', 'running') THEN 1 ELSE 0 END)::BIGINT AS running
        FROM forge_jobs
        WHERE status IN ('pending', 'claimed', 'running')
        GROUP BY COALESCE(worker_capability, 'default')
        "#,
    )
    .fetch_all(&state.db_pool)
    .await;
    let paused =
        sqlx::query!("SELECT queue_name, paused_at, paused_by, reason FROM forge_paused_queues",)
            .fetch_all(&state.db_pool)
            .await;

    match (stats, paused) {
        (Ok(stats), Ok(paused)) => {
            let mut items: std::collections::BTreeMap<String, QueueRow> = paused
                .into_iter()
                .map(|p| {
                    (
                        p.queue_name.clone(),
                        QueueRow {
                            name: p.queue_name,
                            pending: 0,
                            running: 0,
                            paused: true,
                            paused_at: Some(p.paused_at),
                            paused_by: p.paused_by,
                            reason: p.reason,
                        },
                    )
                })
                .collect();
            for row in stats {
                let entry = items.entry(row.name.clone()).or_insert(QueueRow {
                    name: row.name.clone(),
                    pending: 0,
                    running: 0,
                    paused: false,
                    paused_at: None,
                    paused_by: None,
                    reason: None,
                });
                entry.pending = row.pending.unwrap_or(0);
                entry.running = row.running.unwrap_or(0);
            }
            // Surface the canonical default/workflows/cron queues even when
            // their job counts are zero — operators expect them in the list.
            for canonical in ["default", "workflows", "cron"] {
                items.entry(canonical.to_string()).or_insert(QueueRow {
                    name: canonical.to_string(),
                    pending: 0,
                    running: 0,
                    paused: false,
                    paused_at: None,
                    paused_by: None,
                    reason: None,
                });
            }
            Json(serde_json::json!({ "items": items.into_values().collect::<Vec<_>>() }))
                .into_response()
        }
        (Err(e), _) | (_, Err(e)) => admin_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            format!("Failed to list queues: {}", e),
        ),
    }
}

async fn pause_queue(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing_state): Extension<TracingState>,
    Path(name): Path<String>,
    body: Option<Json<ActionBody>>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let reason = body.and_then(|b| b.0.reason);
    let actor = actor_subject(&auth);
    let result = sqlx::query!(
        r#"
        INSERT INTO forge_paused_queues (queue_name, paused_at, paused_by, reason)
        VALUES ($1, NOW(), $2, $3)
        ON CONFLICT (queue_name) DO UPDATE SET
            paused_at = EXCLUDED.paused_at,
            paused_by = EXCLUDED.paused_by,
            reason = EXCLUDED.reason
        "#,
        name,
        actor,
        reason,
    )
    .execute(&state.db_pool)
    .await;
    if let Err(e) = result {
        return admin_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "pause_failed",
            format!("Failed to pause queue: {}", e),
        );
    }
    audit(
        &state.db_pool,
        &auth,
        Some(&tracing_state),
        "queue.pause",
        "queue",
        Some(name.clone()),
        reason.as_deref(),
        serde_json::json!({}),
    )
    .await;
    Json(AuditedAction {
        action: "pause",
        target_type: "queue",
        target_id: name,
        accepted: true,
    })
    .into_response()
}

async fn resume_queue(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
    Extension(tracing_state): Extension<TracingState>,
    Path(name): Path<String>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let result = sqlx::query!(
        "DELETE FROM forge_paused_queues WHERE queue_name = $1",
        name,
    )
    .execute(&state.db_pool)
    .await;
    let accepted = match result {
        Ok(r) => r.rows_affected() > 0,
        Err(e) => {
            return admin_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "resume_failed",
                format!("Failed to resume queue: {}", e),
            );
        }
    };
    audit(
        &state.db_pool,
        &auth,
        Some(&tracing_state),
        "queue.resume",
        "queue",
        Some(name.clone()),
        None,
        serde_json::json!({ "accepted": accepted }),
    )
    .await;
    if accepted {
        let _ = sqlx::query!("SELECT pg_notify('forge_jobs_available', 'queue_resume')")
            .execute(&state.db_pool)
            .await;
    }
    Json(AuditedAction {
        action: "resume",
        target_type: "queue",
        target_id: name,
        accepted,
    })
    .into_response()
}

// --- Nodes / leaders ---

async fn list_nodes(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let rows = sqlx::query!(
        r#"
        SELECT id, hostname, ip_address, http_port, grpc_port, roles,
               worker_capabilities, status, version, current_connections,
               current_jobs, cpu_usage, memory_usage, started_at, last_heartbeat
        FROM forge_nodes
        ORDER BY started_at DESC
        "#,
    )
    .fetch_all(&state.db_pool)
    .await;
    match rows {
        Ok(rs) => Json(serde_json::json!({
            "items": rs.into_iter().map(|r| serde_json::json!({
                "id": r.id,
                "hostname": r.hostname,
                "ip_address": r.ip_address,
                "http_port": r.http_port,
                "grpc_port": r.grpc_port,
                "roles": r.roles,
                "worker_capabilities": r.worker_capabilities,
                "status": r.status,
                "version": r.version,
                "current_connections": r.current_connections,
                "current_jobs": r.current_jobs,
                "cpu_usage": r.cpu_usage,
                "memory_usage": r.memory_usage,
                "started_at": r.started_at,
                "last_heartbeat": r.last_heartbeat,
            })).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(e) => admin_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            format!("Failed to list nodes: {}", e),
        ),
    }
}

async fn list_leaders(
    State(state): State<Arc<AdminState>>,
    Extension(auth): Extension<AuthContext>,
) -> axum::response::Response {
    if let Err(r) = require_admin(&auth) {
        return r;
    }
    let rows = sqlx::query!(
        r#"
        SELECT role, node_id, acquired_at, lease_until
        FROM forge_leaders
        ORDER BY role ASC
        "#,
    )
    .fetch_all(&state.db_pool)
    .await;
    match rows {
        Ok(rs) => Json(serde_json::json!({
            "items": rs.into_iter().map(|r| serde_json::json!({
                "role": r.role,
                "node_id": r.node_id,
                "acquired_at": r.acquired_at,
                "lease_until": r.lease_until,
            })).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(e) => admin_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            format!("Failed to list leaders: {}", e),
        ),
    }
}
