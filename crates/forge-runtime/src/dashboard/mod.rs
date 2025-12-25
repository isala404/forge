mod api;
mod assets;
mod pages;

pub use api::DashboardApi;
pub use assets::DashboardAssets;
pub use pages::DashboardPages;

use axum::{routing::get, Router};
use sqlx::PgPool;

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
}

/// Create the dashboard router.
pub fn create_dashboard_router(state: DashboardState) -> Router {
    Router::new()
        // Dashboard pages
        .route("/", get(pages::index))
        .route("/metrics", get(pages::metrics))
        .route("/logs", get(pages::logs))
        .route("/traces", get(pages::traces))
        .route("/traces/:trace_id", get(pages::trace_detail))
        .route("/alerts", get(pages::alerts))
        .route("/jobs", get(pages::jobs))
        .route("/workflows", get(pages::workflows))
        .route("/cluster", get(pages::cluster))
        // Static assets
        .route("/assets/styles.css", get(assets::styles_css))
        .route("/assets/main.js", get(assets::main_js))
        .route("/assets/chart.js", get(assets::chart_js))
        .with_state(state)
}

/// Create the API router for observability data.
pub fn create_api_router(state: DashboardState) -> Router {
    Router::new()
        // Metrics API
        .route("/metrics", get(api::list_metrics))
        .route("/metrics/:name", get(api::get_metric))
        .route("/metrics/series", get(api::get_metric_series))
        // Logs API
        .route("/logs", get(api::list_logs))
        .route("/logs/search", get(api::search_logs))
        // Traces API
        .route("/traces", get(api::list_traces))
        .route("/traces/:trace_id", get(api::get_trace))
        // Alerts API
        .route("/alerts", get(api::list_alerts))
        .route("/alerts/active", get(api::get_active_alerts))
        // Jobs API
        .route("/jobs", get(api::list_jobs))
        .route("/jobs/stats", get(api::get_job_stats))
        // Cluster API
        .route("/cluster/nodes", get(api::list_nodes))
        .route("/cluster/health", get(api::get_cluster_health))
        // System API
        .route("/system/info", get(api::get_system_info))
        .route("/system/stats", get(api::get_system_stats))
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
        // Just verify the types compile
        let _: fn(PgPool, DashboardConfig) -> DashboardState =
            |pool, config| DashboardState { pool, config };
    }
}
