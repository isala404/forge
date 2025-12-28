mod api;
mod assets;
mod pages;

pub use api::DashboardApi;
pub use assets::DashboardAssets;
pub use pages::DashboardPages;

use std::sync::Arc;

use axum::{
    routing::{get, post},
    Router,
};
use sqlx::PgPool;
use tower_http::cors::{Any, CorsLayer};

use crate::cron::CronRegistry;
use crate::jobs::JobRegistry;
use crate::workflow::WorkflowRegistry;

/// Dashboard configuration.
#[derive(Debug, Clone)]
pub struct DashboardConfig {
    /// Whether the dashboard is enabled.
    pub enabled: bool,

    /// Dashboard path prefix (default: "/_dashboard").
    pub path_prefix: String,

    /// API path prefix (default: "/_api").
    pub api_prefix: String,

    /// Require authentication for dashboard access.
    pub require_auth: bool,

    /// Allowed admin user IDs (if require_auth is true).
    pub admin_users: Vec<String>,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path_prefix: "/_dashboard".to_string(),
            api_prefix: "/_api".to_string(),
            require_auth: false,
            admin_users: Vec::new(),
        }
    }
}

/// Dashboard state shared across handlers.
#[derive(Clone)]
pub struct DashboardState {
    pub pool: PgPool,
    pub config: DashboardConfig,
    pub job_registry: JobRegistry,
    pub cron_registry: Arc<CronRegistry>,
    pub workflow_registry: WorkflowRegistry,
}

/// Create the dashboard router.
pub fn create_dashboard_router(state: DashboardState) -> Router {
    Router::new()
        // Dashboard pages
        .route("/", get(pages::index))
        .route("/metrics", get(pages::metrics))
        .route("/logs", get(pages::logs))
        .route("/traces", get(pages::traces))
        .route("/traces/{trace_id}", get(pages::trace_detail))
        .route("/alerts", get(pages::alerts))
        .route("/jobs", get(pages::jobs))
        .route("/workflows", get(pages::workflows))
        .route("/crons", get(pages::crons))
        .route("/cluster", get(pages::cluster))
        // Static assets
        .route("/assets/styles.css", get(assets::styles_css))
        .route("/assets/main.js", get(assets::main_js))
        .route("/assets/chart.js", get(assets::chart_js))
        .with_state(state)
}

/// Create the API router for observability data.
pub fn create_api_router(state: DashboardState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        // Metrics API
        .route("/metrics", get(api::list_metrics))
        .route("/metrics/{name}", get(api::get_metric))
        .route("/metrics/series", get(api::get_metric_series))
        // Logs API
        .route("/logs", get(api::list_logs))
        .route("/logs/search", get(api::search_logs))
        // Traces API
        .route("/traces", get(api::list_traces))
        .route("/traces/{trace_id}", get(api::get_trace))
        // Alerts API
        .route("/alerts", get(api::list_alerts))
        .route("/alerts/active", get(api::get_active_alerts))
        .route("/alerts/{id}/acknowledge", post(api::acknowledge_alert))
        .route("/alerts/{id}/resolve", post(api::resolve_alert))
        // Alert Rules API
        .route(
            "/alerts/rules",
            get(api::list_alert_rules).post(api::create_alert_rule),
        )
        .route(
            "/alerts/rules/{id}",
            get(api::get_alert_rule)
                .put(api::update_alert_rule)
                .delete(api::delete_alert_rule),
        )
        // Jobs API
        .route("/jobs", get(api::list_jobs))
        .route("/jobs/stats", get(api::get_job_stats))
        .route("/jobs/registered", get(api::list_registered_jobs))
        .route("/jobs/{id}", get(api::get_job))
        // Workflows API
        .route("/workflows", get(api::list_workflows))
        .route("/workflows/stats", get(api::get_workflow_stats))
        .route("/workflows/registered", get(api::list_registered_workflows))
        .route("/workflows/{id}", get(api::get_workflow))
        // Crons API
        .route("/crons", get(api::list_crons))
        .route("/crons/stats", get(api::get_cron_stats))
        .route("/crons/history", get(api::get_cron_history))
        .route("/crons/registered", get(api::list_registered_crons))
        .route("/crons/{name}/trigger", post(api::trigger_cron))
        .route("/crons/{name}/pause", post(api::pause_cron))
        .route("/crons/{name}/resume", post(api::resume_cron))
        // Cluster API
        .route("/cluster/nodes", get(api::list_nodes))
        .route("/cluster/health", get(api::get_cluster_health))
        // System API
        .route("/system/info", get(api::get_system_info))
        .route("/system/stats", get(api::get_system_stats))
        .layer(cors)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dashboard_config_default() {
        let config = DashboardConfig::default();
        assert!(config.enabled);
        assert_eq!(config.path_prefix, "/_dashboard");
        assert_eq!(config.api_prefix, "/_api");
        assert!(!config.require_auth);
    }

    #[test]
    fn test_dashboard_state() {
        // Just verify the types compile - the new state requires registries
        let _ = DashboardConfig::default();
    }
}
