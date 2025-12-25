use serde::{Deserialize, Serialize};

/// Alert severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    /// Informational alert.
    Info,
    /// Warning alert.
    Warning,
    /// Critical alert.
    Critical,
}

impl AlertSeverity {
    /// Convert from string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "info" | "informational" => Some(Self::Info),
            "warning" | "warn" => Some(Self::Warning),
            "critical" | "error" => Some(Self::Critical),
            _ => None,
        }
    }
}

impl std::fmt::Display for AlertSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Warning => write!(f, "warning"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

/// Alert status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertStatus {
    /// Alert is inactive.
    Inactive,
    /// Alert is pending (condition met, waiting for duration).
    Pending,
    /// Alert is firing.
    Firing,
    /// Alert was resolved.
    Resolved,
}

impl std::fmt::Display for AlertStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inactive => write!(f, "inactive"),
            Self::Pending => write!(f, "pending"),
            Self::Firing => write!(f, "firing"),
            Self::Resolved => write!(f, "resolved"),
        }
    }
}

/// Alert condition expression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertCondition {
    /// Condition expression (e.g., "rate(forge_http_errors[5m]) > 0.05").
    pub expression: String,
    /// Duration the condition must be true before firing.
    pub for_duration: std::time::Duration,
}

impl AlertCondition {
    /// Create a new condition.
    pub fn new(expression: impl Into<String>, for_duration: std::time::Duration) -> Self {
        Self {
            expression: expression.into(),
            for_duration,
        }
    }

    /// Create a condition that fires immediately.
    pub fn immediate(expression: impl Into<String>) -> Self {
        Self::new(expression, std::time::Duration::ZERO)
    }
}

/// Alert state tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertState {
    /// Current status.
    pub status: AlertStatus,
    /// When the condition first became true.
    pub pending_since: Option<chrono::DateTime<chrono::Utc>>,
    /// When the alert started firing.
    pub firing_since: Option<chrono::DateTime<chrono::Utc>>,
    /// When the alert was last resolved.
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Last evaluation time.
    pub last_evaluation: Option<chrono::DateTime<chrono::Utc>>,
    /// Last evaluation result.
    pub last_value: Option<f64>,
}

impl Default for AlertState {
    fn default() -> Self {
        Self {
            status: AlertStatus::Inactive,
            pending_since: None,
            firing_since: None,
            resolved_at: None,
            last_evaluation: None,
            last_value: None,
        }
    }
}

impl AlertState {
    /// Transition to pending state.
    pub fn set_pending(&mut self) {
        if self.status != AlertStatus::Pending && self.status != AlertStatus::Firing {
            self.status = AlertStatus::Pending;
            self.pending_since = Some(chrono::Utc::now());
        }
    }

    /// Transition to firing state.
    pub fn set_firing(&mut self) {
        if self.status != AlertStatus::Firing {
            self.status = AlertStatus::Firing;
            self.firing_since = Some(chrono::Utc::now());
        }
    }

    /// Transition to resolved state.
    pub fn set_resolved(&mut self) {
        if self.status == AlertStatus::Firing || self.status == AlertStatus::Pending {
            self.status = AlertStatus::Resolved;
            self.resolved_at = Some(chrono::Utc::now());
            self.pending_since = None;
            self.firing_since = None;
        }
    }

    /// Transition to inactive state.
    pub fn set_inactive(&mut self) {
        self.status = AlertStatus::Inactive;
        self.pending_since = None;
        self.firing_since = None;
    }

    /// Update after evaluation.
    pub fn update_evaluation(&mut self, value: f64) {
        self.last_evaluation = Some(chrono::Utc::now());
        self.last_value = Some(value);
    }

    /// Check if the alert should transition from pending to firing.
    pub fn should_fire(&self, for_duration: std::time::Duration) -> bool {
        if self.status != AlertStatus::Pending {
            return false;
        }

        if let Some(pending_since) = self.pending_since {
            let elapsed = chrono::Utc::now() - pending_since;
            return elapsed >= chrono::Duration::from_std(for_duration).unwrap();
        }

        false
    }
}

/// Alert definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    /// Alert name.
    pub name: String,
    /// Alert condition.
    pub condition: AlertCondition,
    /// Alert severity.
    pub severity: AlertSeverity,
    /// Notification channels.
    pub notify: Vec<String>,
    /// Alert description.
    pub description: Option<String>,
    /// Current state.
    pub state: AlertState,
}

impl Alert {
    /// Create a new alert.
    pub fn new(
        name: impl Into<String>,
        condition: AlertCondition,
        severity: AlertSeverity,
    ) -> Self {
        Self {
            name: name.into(),
            condition,
            severity,
            notify: Vec::new(),
            description: None,
            state: AlertState::default(),
        }
    }

    /// Add a notification channel.
    pub fn with_notify(mut self, channel: impl Into<String>) -> Self {
        self.notify.push(channel.into());
        self
    }

    /// Set the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Check if the alert is currently firing.
    pub fn is_firing(&self) -> bool {
        self.state.status == AlertStatus::Firing
    }

    /// Check if the alert needs notification.
    pub fn needs_notification(&self) -> bool {
        self.is_firing() && !self.notify.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alert_severity_ordering() {
        assert!(AlertSeverity::Info < AlertSeverity::Warning);
        assert!(AlertSeverity::Warning < AlertSeverity::Critical);
    }

    #[test]
    fn test_alert_condition() {
        let condition = AlertCondition::new(
            "rate(errors[5m]) > 0.05",
            std::time::Duration::from_secs(300),
        );

        assert_eq!(condition.expression, "rate(errors[5m]) > 0.05");
        assert_eq!(condition.for_duration, std::time::Duration::from_secs(300));
    }

    #[test]
    fn test_alert_state_transitions() {
        let mut state = AlertState::default();
        assert_eq!(state.status, AlertStatus::Inactive);

        state.set_pending();
        assert_eq!(state.status, AlertStatus::Pending);
        assert!(state.pending_since.is_some());

        state.set_firing();
        assert_eq!(state.status, AlertStatus::Firing);
        assert!(state.firing_since.is_some());

        state.set_resolved();
        assert_eq!(state.status, AlertStatus::Resolved);
        assert!(state.resolved_at.is_some());
    }

    #[test]
    fn test_alert_creation() {
        let alert = Alert::new(
            "high_error_rate",
            AlertCondition::new(
                "rate(errors[5m]) > 0.05",
                std::time::Duration::from_secs(300),
            ),
            AlertSeverity::Critical,
        )
        .with_notify("slack:#alerts")
        .with_description("Error rate exceeds 5%");

        assert_eq!(alert.name, "high_error_rate");
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert_eq!(alert.notify, vec!["slack:#alerts"]);
        assert!(!alert.is_firing());
    }
}
