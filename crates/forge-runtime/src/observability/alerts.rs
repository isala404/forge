//! Alert storage and evaluation engine.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::Row;
use uuid::Uuid;

use forge_core::Result;

/// Alert severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

impl std::fmt::Display for AlertSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlertSeverity::Info => write!(f, "info"),
            AlertSeverity::Warning => write!(f, "warning"),
            AlertSeverity::Critical => write!(f, "critical"),
        }
    }
}

impl std::str::FromStr for AlertSeverity {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "info" => Ok(AlertSeverity::Info),
            "warning" => Ok(AlertSeverity::Warning),
            "critical" => Ok(AlertSeverity::Critical),
            _ => Err(format!("Unknown severity: {}", s)),
        }
    }
}

/// Alert condition operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertCondition {
    /// Greater than
    Gt,
    /// Greater than or equal
    Gte,
    /// Less than
    Lt,
    /// Less than or equal
    Lte,
    /// Equal
    Eq,
    /// Not equal
    Ne,
}

impl AlertCondition {
    /// Evaluate the condition.
    pub fn evaluate(&self, value: f64, threshold: f64) -> bool {
        match self {
            AlertCondition::Gt => value > threshold,
            AlertCondition::Gte => value >= threshold,
            AlertCondition::Lt => value < threshold,
            AlertCondition::Lte => value <= threshold,
            AlertCondition::Eq => (value - threshold).abs() < f64::EPSILON,
            AlertCondition::Ne => (value - threshold).abs() >= f64::EPSILON,
        }
    }
}

impl std::fmt::Display for AlertCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlertCondition::Gt => write!(f, "gt"),
            AlertCondition::Gte => write!(f, "gte"),
            AlertCondition::Lt => write!(f, "lt"),
            AlertCondition::Lte => write!(f, "lte"),
            AlertCondition::Eq => write!(f, "eq"),
            AlertCondition::Ne => write!(f, "ne"),
        }
    }
}

impl std::str::FromStr for AlertCondition {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "gt" | ">" => Ok(AlertCondition::Gt),
            "gte" | ">=" => Ok(AlertCondition::Gte),
            "lt" | "<" => Ok(AlertCondition::Lt),
            "lte" | "<=" => Ok(AlertCondition::Lte),
            "eq" | "==" => Ok(AlertCondition::Eq),
            "ne" | "!=" => Ok(AlertCondition::Ne),
            _ => Err(format!("Unknown condition: {}", s)),
        }
    }
}

/// Alert status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertStatus {
    Firing,
    Resolved,
}

impl std::fmt::Display for AlertStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlertStatus::Firing => write!(f, "firing"),
            AlertStatus::Resolved => write!(f, "resolved"),
        }
    }
}

/// Alert rule definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub metric_name: String,
    pub condition: AlertCondition,
    pub threshold: f64,
    pub duration_seconds: i32,
    pub severity: AlertSeverity,
    pub enabled: bool,
    pub labels: HashMap<String, String>,
    pub notification_channels: Vec<String>,
    pub cooldown_seconds: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AlertRule {
    /// Create a new alert rule.
    pub fn new(
        name: impl Into<String>,
        metric_name: impl Into<String>,
        condition: AlertCondition,
        threshold: f64,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            description: None,
            metric_name: metric_name.into(),
            condition,
            threshold,
            duration_seconds: 0,
            severity: AlertSeverity::Warning,
            enabled: true,
            labels: HashMap::new(),
            notification_channels: Vec::new(),
            cooldown_seconds: 300,
            created_at: now,
            updated_at: now,
        }
    }

    /// Set description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set severity.
    pub fn with_severity(mut self, severity: AlertSeverity) -> Self {
        self.severity = severity;
        self
    }

    /// Set duration (seconds that condition must be true).
    pub fn with_duration(mut self, seconds: i32) -> Self {
        self.duration_seconds = seconds;
        self
    }
}

/// A fired alert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub id: Uuid,
    pub rule_id: Uuid,
    pub rule_name: String,
    pub metric_value: f64,
    pub threshold: f64,
    pub severity: AlertSeverity,
    pub status: AlertStatus,
    pub triggered_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub acknowledged_by: Option<String>,
    pub labels: HashMap<String, String>,
    pub annotations: HashMap<String, String>,
}

impl Alert {
    /// Create a new firing alert.
    pub fn firing(rule: &AlertRule, metric_value: f64) -> Self {
        Self {
            id: Uuid::new_v4(),
            rule_id: rule.id,
            rule_name: rule.name.clone(),
            metric_value,
            threshold: rule.threshold,
            severity: rule.severity,
            status: AlertStatus::Firing,
            triggered_at: Utc::now(),
            resolved_at: None,
            acknowledged_at: None,
            acknowledged_by: None,
            labels: rule.labels.clone(),
            annotations: HashMap::new(),
        }
    }
}

/// Alert store for persistence.
pub struct AlertStore {
    pool: sqlx::PgPool,
}

impl AlertStore {
    /// Create a new alert store.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    // ==================== Alert Rules ====================

    /// Create an alert rule.
    pub async fn create_rule(&self, rule: &AlertRule) -> Result<()> {
        let labels = serde_json::to_value(&rule.labels).unwrap_or_default();

        sqlx::query(
            r#"
            INSERT INTO forge_alert_rules
            (id, name, description, metric_name, condition, threshold, duration_seconds,
             severity, enabled, labels, notification_channels, cooldown_seconds,
             created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            "#,
        )
        .bind(rule.id)
        .bind(&rule.name)
        .bind(&rule.description)
        .bind(&rule.metric_name)
        .bind(rule.condition.to_string())
        .bind(rule.threshold)
        .bind(rule.duration_seconds)
        .bind(rule.severity.to_string())
        .bind(rule.enabled)
        .bind(labels)
        .bind(&rule.notification_channels)
        .bind(rule.cooldown_seconds)
        .bind(rule.created_at)
        .bind(rule.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// List all alert rules.
    pub async fn list_rules(&self) -> Result<Vec<AlertRule>> {
        let rows = sqlx::query(
            r#"
            SELECT id, name, description, metric_name, condition, threshold,
                   duration_seconds, severity, enabled, labels, notification_channels,
                   cooldown_seconds, created_at, updated_at
            FROM forge_alert_rules
            ORDER BY name
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(parse_alert_rule_row).collect())
    }

    /// List enabled alert rules.
    pub async fn list_enabled_rules(&self) -> Result<Vec<AlertRule>> {
        let rows = sqlx::query(
            r#"
            SELECT id, name, description, metric_name, condition, threshold,
                   duration_seconds, severity, enabled, labels, notification_channels,
                   cooldown_seconds, created_at, updated_at
            FROM forge_alert_rules
            WHERE enabled = TRUE
            ORDER BY name
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(parse_alert_rule_row).collect())
    }

    /// Get a rule by ID.
    pub async fn get_rule(&self, id: Uuid) -> Result<Option<AlertRule>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, description, metric_name, condition, threshold,
                   duration_seconds, severity, enabled, labels, notification_channels,
                   cooldown_seconds, created_at, updated_at
            FROM forge_alert_rules
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(row.map(parse_alert_rule_row))
    }

    /// Update an alert rule.
    pub async fn update_rule(&self, rule: &AlertRule) -> Result<()> {
        let labels = serde_json::to_value(&rule.labels).unwrap_or_default();

        sqlx::query(
            r#"
            UPDATE forge_alert_rules
            SET name = $2, description = $3, metric_name = $4, condition = $5,
                threshold = $6, duration_seconds = $7, severity = $8, enabled = $9,
                labels = $10, notification_channels = $11, cooldown_seconds = $12,
                updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(rule.id)
        .bind(&rule.name)
        .bind(&rule.description)
        .bind(&rule.metric_name)
        .bind(rule.condition.to_string())
        .bind(rule.threshold)
        .bind(rule.duration_seconds)
        .bind(rule.severity.to_string())
        .bind(rule.enabled)
        .bind(labels)
        .bind(&rule.notification_channels)
        .bind(rule.cooldown_seconds)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Delete an alert rule.
    pub async fn delete_rule(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM forge_alert_rules WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    // ==================== Alerts ====================

    /// Create an alert.
    pub async fn create_alert(&self, alert: &Alert) -> Result<()> {
        let labels = serde_json::to_value(&alert.labels).unwrap_or_default();
        let annotations = serde_json::to_value(&alert.annotations).unwrap_or_default();

        sqlx::query(
            r#"
            INSERT INTO forge_alerts
            (id, rule_id, rule_name, metric_value, threshold, severity, status,
             triggered_at, resolved_at, acknowledged_at, acknowledged_by, labels, annotations)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            "#,
        )
        .bind(alert.id)
        .bind(alert.rule_id)
        .bind(&alert.rule_name)
        .bind(alert.metric_value)
        .bind(alert.threshold)
        .bind(alert.severity.to_string())
        .bind(alert.status.to_string())
        .bind(alert.triggered_at)
        .bind(alert.resolved_at)
        .bind(alert.acknowledged_at)
        .bind(&alert.acknowledged_by)
        .bind(labels)
        .bind(annotations)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// List active (firing) alerts.
    pub async fn list_active_alerts(&self) -> Result<Vec<Alert>> {
        let rows = sqlx::query(
            r#"
            SELECT id, rule_id, rule_name, metric_value, threshold, severity, status,
                   triggered_at, resolved_at, acknowledged_at, acknowledged_by, labels, annotations
            FROM forge_alerts
            WHERE status = 'firing'
            ORDER BY triggered_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(parse_alert_row).collect())
    }

    /// List recent alerts (both firing and resolved).
    pub async fn list_recent_alerts(&self, limit: i64) -> Result<Vec<Alert>> {
        let rows = sqlx::query(
            r#"
            SELECT id, rule_id, rule_name, metric_value, threshold, severity, status,
                   triggered_at, resolved_at, acknowledged_at, acknowledged_by, labels, annotations
            FROM forge_alerts
            ORDER BY triggered_at DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(parse_alert_row).collect())
    }

    /// Resolve an alert.
    pub async fn resolve_alert(&self, id: Uuid) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_alerts
            SET status = 'resolved', resolved_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Acknowledge an alert.
    pub async fn acknowledge_alert(&self, id: Uuid, by: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_alerts
            SET acknowledged_at = NOW(), acknowledged_by = $2
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(by)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Get last alert for a rule (to check cooldown).
    pub async fn get_last_alert_for_rule(&self, rule_id: Uuid) -> Result<Option<Alert>> {
        let row = sqlx::query(
            r#"
            SELECT id, rule_id, rule_name, metric_value, threshold, severity, status,
                   triggered_at, resolved_at, acknowledged_at, acknowledged_by, labels, annotations
            FROM forge_alerts
            WHERE rule_id = $1
            ORDER BY triggered_at DESC
            LIMIT 1
            "#,
        )
        .bind(rule_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(row.map(parse_alert_row))
    }

    /// Cleanup old resolved alerts.
    pub async fn cleanup(&self, retention: Duration) -> Result<u64> {
        let cutoff = Utc::now() - chrono::Duration::from_std(retention).unwrap_or_default();

        let result = sqlx::query(
            r#"
            DELETE FROM forge_alerts
            WHERE status = 'resolved' AND resolved_at < $1
            "#,
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(result.rows_affected())
    }
}

/// Alert evaluator that periodically checks rules against metrics.
pub struct AlertEvaluator {
    alert_store: Arc<AlertStore>,
    #[allow(dead_code)]
    metrics_store: Arc<super::MetricsStore>,
    pool: sqlx::PgPool,
    shutdown: Arc<RwLock<bool>>,
}

impl AlertEvaluator {
    /// Create a new alert evaluator.
    pub fn new(
        alert_store: Arc<AlertStore>,
        metrics_store: Arc<super::MetricsStore>,
        pool: sqlx::PgPool,
    ) -> Self {
        Self {
            alert_store,
            metrics_store,
            pool,
            shutdown: Arc::new(RwLock::new(false)),
        }
    }

    /// Start the evaluation loop.
    ///
    /// This runs in the background and evaluates all enabled rules every `interval`.
    pub async fn run(&self, interval: Duration) {
        tracing::info!("Alert evaluator started");

        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;

            if *self.shutdown.read().await {
                break;
            }

            if let Err(e) = self.evaluate_all_rules().await {
                tracing::error!("Alert evaluation error: {}", e);
            }
        }

        tracing::info!("Alert evaluator stopped");
    }

    /// Stop the evaluator.
    pub async fn stop(&self) {
        let mut shutdown = self.shutdown.write().await;
        *shutdown = true;
    }

    /// Evaluate all enabled rules.
    async fn evaluate_all_rules(&self) -> Result<()> {
        let rules = self.alert_store.list_enabled_rules().await?;

        for rule in rules {
            if let Err(e) = self.evaluate_rule(&rule).await {
                tracing::warn!("Failed to evaluate rule {}: {}", rule.name, e);
            }
        }

        Ok(())
    }

    /// Evaluate a single rule.
    async fn evaluate_rule(&self, rule: &AlertRule) -> Result<()> {
        // Get the latest metric value for this rule
        let metric_value = self
            .get_latest_metric_value(&rule.metric_name, &rule.labels)
            .await?;

        let metric_value = match metric_value {
            Some(v) => v,
            None => return Ok(()), // No metric data yet
        };

        // Evaluate the condition
        let condition_met = rule.condition.evaluate(metric_value, rule.threshold);

        // Check if there's an existing firing alert for this rule
        let existing_alert = self.alert_store.get_last_alert_for_rule(rule.id).await?;

        match (condition_met, existing_alert) {
            (true, None) => {
                // Condition met and no existing alert - create new alert
                let alert = Alert::firing(rule, metric_value);
                self.alert_store.create_alert(&alert).await?;
                tracing::warn!(
                    rule = rule.name,
                    value = metric_value,
                    threshold = rule.threshold,
                    severity = ?rule.severity,
                    "Alert triggered"
                );
            }
            (true, Some(existing)) if existing.status == AlertStatus::Resolved => {
                // Condition met again after resolution - check cooldown
                let cooldown = chrono::Duration::seconds(rule.cooldown_seconds as i64);
                let since_resolved = existing
                    .resolved_at
                    .map(|t| Utc::now() - t)
                    .unwrap_or(cooldown);

                if since_resolved >= cooldown {
                    // Cooldown passed - create new alert
                    let alert = Alert::firing(rule, metric_value);
                    self.alert_store.create_alert(&alert).await?;
                    tracing::warn!(
                        rule = rule.name,
                        value = metric_value,
                        threshold = rule.threshold,
                        "Alert re-triggered after cooldown"
                    );
                }
            }
            (false, Some(existing)) if existing.status == AlertStatus::Firing => {
                // Condition no longer met - resolve alert
                self.alert_store.resolve_alert(existing.id).await?;
                tracing::info!(rule = rule.name, value = metric_value, "Alert resolved");
            }
            _ => {
                // No action needed
            }
        }

        Ok(())
    }

    /// Get the latest metric value matching the rule's criteria.
    async fn get_latest_metric_value(
        &self,
        metric_name: &str,
        _labels: &HashMap<String, String>,
    ) -> Result<Option<f64>> {
        // Query the latest metric value
        let row: Option<(f64,)> = sqlx::query_as(
            r#"
            SELECT value
            FROM forge_metrics
            WHERE name = $1
            ORDER BY timestamp DESC
            LIMIT 1
            "#,
        )
        .bind(metric_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(row.map(|(v,)| v))
    }
}

use std::sync::Arc;
use tokio::sync::RwLock;

// Manual row parsing functions
fn parse_alert_rule_row(row: PgRow) -> AlertRule {
    let labels_json: serde_json::Value = row.get("labels");
    let labels: HashMap<String, String> = serde_json::from_value(labels_json).unwrap_or_default();
    let condition_str: String = row.get("condition");
    let severity_str: String = row.get("severity");

    AlertRule {
        id: row.get("id"),
        name: row.get("name"),
        description: row.get("description"),
        metric_name: row.get("metric_name"),
        condition: condition_str.parse().unwrap_or(AlertCondition::Gt),
        threshold: row.get("threshold"),
        duration_seconds: row.get("duration_seconds"),
        severity: severity_str.parse().unwrap_or(AlertSeverity::Warning),
        enabled: row.get("enabled"),
        labels,
        notification_channels: row.get("notification_channels"),
        cooldown_seconds: row.get("cooldown_seconds"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn parse_alert_row(row: PgRow) -> Alert {
    let labels_json: serde_json::Value = row.get("labels");
    let annotations_json: serde_json::Value = row.get("annotations");
    let labels: HashMap<String, String> = serde_json::from_value(labels_json).unwrap_or_default();
    let annotations: HashMap<String, String> =
        serde_json::from_value(annotations_json).unwrap_or_default();
    let severity_str: String = row.get("severity");
    let status_str: String = row.get("status");

    Alert {
        id: row.get("id"),
        rule_id: row.get("rule_id"),
        rule_name: row.get("rule_name"),
        metric_value: row.get("metric_value"),
        threshold: row.get("threshold"),
        severity: severity_str.parse().unwrap_or(AlertSeverity::Warning),
        status: if status_str == "firing" {
            AlertStatus::Firing
        } else {
            AlertStatus::Resolved
        },
        triggered_at: row.get("triggered_at"),
        resolved_at: row.get("resolved_at"),
        acknowledged_at: row.get("acknowledged_at"),
        acknowledged_by: row.get("acknowledged_by"),
        labels,
        annotations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alert_condition_evaluate() {
        assert!(AlertCondition::Gt.evaluate(10.0, 5.0));
        assert!(!AlertCondition::Gt.evaluate(5.0, 10.0));

        assert!(AlertCondition::Gte.evaluate(10.0, 10.0));
        assert!(AlertCondition::Gte.evaluate(10.0, 5.0));

        assert!(AlertCondition::Lt.evaluate(5.0, 10.0));
        assert!(!AlertCondition::Lt.evaluate(10.0, 5.0));

        assert!(AlertCondition::Lte.evaluate(10.0, 10.0));
        assert!(AlertCondition::Lte.evaluate(5.0, 10.0));

        assert!(AlertCondition::Eq.evaluate(10.0, 10.0));
        assert!(!AlertCondition::Eq.evaluate(10.0, 5.0));

        assert!(AlertCondition::Ne.evaluate(10.0, 5.0));
        assert!(!AlertCondition::Ne.evaluate(10.0, 10.0));
    }

    #[test]
    fn test_alert_rule_builder() {
        let rule = AlertRule::new("high_cpu", "cpu_usage_percent", AlertCondition::Gt, 90.0)
            .with_description("Alert when CPU usage exceeds 90%")
            .with_severity(AlertSeverity::Critical)
            .with_duration(60);

        assert_eq!(rule.name, "high_cpu");
        assert_eq!(rule.metric_name, "cpu_usage_percent");
        assert_eq!(rule.threshold, 90.0);
        assert_eq!(rule.severity, AlertSeverity::Critical);
        assert_eq!(rule.duration_seconds, 60);
    }

    #[test]
    fn test_alert_firing() {
        let rule = AlertRule::new("test", "metric", AlertCondition::Gt, 50.0);
        let alert = Alert::firing(&rule, 75.0);

        assert_eq!(alert.rule_name, "test");
        assert_eq!(alert.metric_value, 75.0);
        assert_eq!(alert.threshold, 50.0);
        assert_eq!(alert.status, AlertStatus::Firing);
    }
}
