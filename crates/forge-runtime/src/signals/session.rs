//! Server-side session management for the signals pipeline.
//!
//! Sessions are created when the first event arrives for a visitor_id.
//! They are closed after `session_timeout_mins` of inactivity.

// Signals tables are owned by the runtime crate and aren't always present in
// the user's .sqlx cache; runtime queries are required.
#![allow(clippy::disallowed_methods)]

use std::sync::Arc;

use sqlx::PgPool;
use tracing::{debug, error};
use uuid::Uuid;

/// Create or update a session for the given visitor.
///
/// Returns the session ID (existing or newly created).
#[allow(clippy::too_many_arguments)]
pub async fn upsert_session(
    pool: &PgPool,
    session_id: Option<Uuid>,
    visitor_id: &str,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    page_url: Option<&str>,
    referrer: Option<&str>,
    user_agent: Option<&str>,
    client_ip: Option<&str>,
    is_bot: bool,
    event_type: &str,
    device_type: Option<&str>,
    browser: Option<&str>,
    os: Option<&str>,
) -> Option<Uuid> {
    if let Some(sid) = session_id {
        let is_page_view = event_type == "page_view";
        let is_error = event_type == "error";
        let is_rpc = event_type == "rpc_call";

        let result = sqlx::query(
            "UPDATE forge_signals_sessions SET
                last_activity_at = NOW(),
                event_count = event_count + 1,
                page_view_count = page_view_count + CASE WHEN $2 THEN 1 ELSE 0 END,
                rpc_call_count = rpc_call_count + CASE WHEN $3 THEN 1 ELSE 0 END,
                error_count = error_count + CASE WHEN $4 THEN 1 ELSE 0 END,
                exit_page = COALESCE($5, exit_page),
                user_id = COALESCE(user_id, $6),
                is_bounce = CASE WHEN page_view_count + CASE WHEN $2 THEN 1 ELSE 0 END > 1 THEN FALSE ELSE is_bounce END
            WHERE id = $1",
        )
        .bind(sid)
        .bind(is_page_view)
        .bind(is_rpc)
        .bind(is_error)
        .bind(page_url)
        .bind(user_id)
        .execute(pool)
        .await;

        match result {
            Ok(r) if r.rows_affected() > 0 => return Some(sid),
            Ok(_) => {} // Session not found, create new one below
            Err(e) => {
                error!(error = %e, "failed to update signal session");
                return Some(sid);
            }
        }
    }

    let new_id = Uuid::new_v4();
    let referrer_domain = referrer.and_then(extract_domain);

    let result = sqlx::query(
        "INSERT INTO forge_signals_sessions (
            id, visitor_id, user_id, tenant_id,
            entry_page, exit_page,
            referrer, referrer_domain,
            user_agent, client_ip,
            device_type, browser, os,
            is_bot, event_count, page_view_count, rpc_call_count, error_count
        ) VALUES ($1, $2, $3, $4, $5, $5, $6, $7, $8, $9, $10, $11, $12, $13, 1,
            CASE WHEN $14 = 'page_view' THEN 1 ELSE 0 END,
            CASE WHEN $14 = 'rpc_call' THEN 1 ELSE 0 END,
            CASE WHEN $14 = 'error' THEN 1 ELSE 0 END
        )",
    )
    .bind(new_id)
    .bind(visitor_id)
    .bind(user_id)
    .bind(tenant_id)
    .bind(page_url)
    .bind(referrer)
    .bind(referrer_domain)
    .bind(user_agent)
    .bind(client_ip)
    .bind(device_type)
    .bind(browser)
    .bind(os)
    .bind(is_bot)
    .bind(event_type)
    .execute(pool)
    .await;

    match result {
        Ok(_) => {
            debug!(session_id = %new_id, visitor_id, "created signal session");
            Some(new_id)
        }
        Err(e) => {
            error!(error = %e, "failed to create signal session");
            None
        }
    }
}

/// Close stale sessions that have been inactive longer than the timeout.
pub async fn close_stale_sessions(pool: &PgPool, timeout_mins: u32) {
    let result = sqlx::query(
        "UPDATE forge_signals_sessions SET
            ended_at = NOW(),
            duration_secs = EXTRACT(EPOCH FROM NOW() - started_at)::integer
        WHERE ended_at IS NULL
        AND last_activity_at < NOW() - ($1 || ' minutes')::interval",
    )
    .bind(timeout_mins as i32)
    .execute(pool)
    .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => {
            debug!(count = r.rows_affected(), "closed stale signal sessions");
        }
        Ok(_) => {}
        Err(e) => error!(error = %e, "failed to close stale signal sessions"),
    }
}

/// Spawn a background task that periodically closes stale sessions.
pub fn spawn_session_reaper(pool: Arc<PgPool>, timeout_mins: u32) {
    tokio::spawn(async move {
        // Delay first run to avoid DB pool contention during startup.
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            close_stale_sessions(&pool, timeout_mins).await;
        }
    });
}

/// Extract the domain from a URL string (e.g. "https://google.com/search" -> "google.com").
fn extract_domain(url: &str) -> Option<String> {
    // Strip scheme
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    // Take everything before the first /
    let domain = without_scheme.split('/').next()?;
    // Strip port
    let domain = domain.split(':').next()?;

    if domain.is_empty() {
        None
    } else {
        Some(domain.to_lowercase())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn extracts_domain_from_url() {
        assert_eq!(
            extract_domain("https://google.com/search"),
            Some("google.com".into())
        );
        assert_eq!(
            extract_domain("http://example.com:8080/path"),
            Some("example.com".into())
        );
        assert_eq!(
            extract_domain("https://Sub.Domain.COM/"),
            Some("sub.domain.com".into())
        );
    }

    #[tokio::test]
    async fn handles_edge_cases() {
        assert_eq!(extract_domain(""), None);
        assert_eq!(extract_domain("not-a-url"), Some("not-a-url".into()));
    }
}
