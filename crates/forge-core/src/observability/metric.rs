use std::collections::HashMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Metric kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    /// Counter that only increases.
    Counter,
    /// Gauge that can increase or decrease.
    Gauge,
    /// Histogram for distributions.
    Histogram,
    /// Summary with quantiles.
    Summary,
}

/// Error for parsing MetricKind from string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseMetricKindError(pub String);

impl std::fmt::Display for ParseMetricKindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid metric kind: {}", self.0)
    }
}

impl std::error::Error for ParseMetricKindError {}

impl FromStr for MetricKind {
    type Err = ParseMetricKindError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "counter" => Ok(Self::Counter),
            "gauge" => Ok(Self::Gauge),
            "histogram" => Ok(Self::Histogram),
            "summary" => Ok(Self::Summary),
            _ => Err(ParseMetricKindError(s.to_string())),
        }
    }
}

impl std::fmt::Display for MetricKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Counter => write!(f, "counter"),
            Self::Gauge => write!(f, "gauge"),
            Self::Histogram => write!(f, "histogram"),
            Self::Summary => write!(f, "summary"),
        }
    }
}

/// Metric labels as key-value pairs.
pub type MetricLabels = HashMap<String, String>;

/// A metric value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetricValue {
    /// Counter or gauge value.
    Value(f64),
    /// Histogram buckets with counts.
    Histogram {
        buckets: Vec<(f64, u64)>,
        count: u64,
        sum: f64,
    },
    /// Summary with quantiles.
    Summary {
        quantiles: Vec<(f64, f64)>,
        count: u64,
        sum: f64,
    },
}

impl MetricValue {
    /// Create a simple value.
    pub fn value(v: f64) -> Self {
        Self::Value(v)
    }

    /// Create a histogram.
    pub fn histogram(buckets: Vec<(f64, u64)>, count: u64, sum: f64) -> Self {
        Self::Histogram {
            buckets,
            count,
            sum,
        }
    }

    /// Create a summary.
    pub fn summary(quantiles: Vec<(f64, f64)>, count: u64, sum: f64) -> Self {
        Self::Summary {
            quantiles,
            count,
            sum,
        }
    }

    /// Get the scalar value if applicable.
    pub fn as_value(&self) -> Option<f64> {
        match self {
            Self::Value(v) => Some(*v),
            _ => None,
        }
    }
}

/// A single metric data point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metric {
    /// Metric name.
    pub name: String,
    /// Metric kind.
    pub kind: MetricKind,
    /// Metric labels.
    pub labels: MetricLabels,
    /// Metric value.
    pub value: MetricValue,
    /// Timestamp.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Description (for registration).
    pub description: Option<String>,
}

impl Metric {
    /// Create a new counter metric.
    pub fn counter(name: impl Into<String>, value: f64) -> Self {
        Self {
            name: name.into(),
            kind: MetricKind::Counter,
            labels: HashMap::new(),
            value: MetricValue::Value(value),
            timestamp: chrono::Utc::now(),
            description: None,
        }
    }

    /// Create a new gauge metric.
    pub fn gauge(name: impl Into<String>, value: f64) -> Self {
        Self {
            name: name.into(),
            kind: MetricKind::Gauge,
            labels: HashMap::new(),
            value: MetricValue::Value(value),
            timestamp: chrono::Utc::now(),
            description: None,
        }
    }

    /// Create a histogram metric.
    pub fn histogram(
        name: impl Into<String>,
        buckets: Vec<(f64, u64)>,
        count: u64,
        sum: f64,
    ) -> Self {
        Self {
            name: name.into(),
            kind: MetricKind::Histogram,
            labels: HashMap::new(),
            value: MetricValue::Histogram {
                buckets,
                count,
                sum,
            },
            timestamp: chrono::Utc::now(),
            description: None,
        }
    }

    /// Add a label.
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    /// Add multiple labels.
    pub fn with_labels(mut self, labels: MetricLabels) -> Self {
        self.labels.extend(labels);
        self
    }

    /// Set the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the timestamp.
    pub fn with_timestamp(mut self, timestamp: chrono::DateTime<chrono::Utc>) -> Self {
        self.timestamp = timestamp;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metric_kind_from_str() {
        assert_eq!("counter".parse::<MetricKind>(), Ok(MetricKind::Counter));
        assert_eq!("GAUGE".parse::<MetricKind>(), Ok(MetricKind::Gauge));
        assert_eq!("Histogram".parse::<MetricKind>(), Ok(MetricKind::Histogram));
        assert!("unknown".parse::<MetricKind>().is_err());
    }

    #[test]
    fn test_counter_metric() {
        let metric = Metric::counter("http_requests_total", 100.0)
            .with_label("method", "GET")
            .with_label("status", "200");

        assert_eq!(metric.name, "http_requests_total");
        assert_eq!(metric.kind, MetricKind::Counter);
        assert_eq!(metric.labels.get("method"), Some(&"GET".to_string()));
        assert_eq!(metric.value.as_value(), Some(100.0));
    }

    #[test]
    fn test_gauge_metric() {
        let metric = Metric::gauge("active_connections", 42.0);
        assert_eq!(metric.kind, MetricKind::Gauge);
        assert_eq!(metric.value.as_value(), Some(42.0));
    }

    #[test]
    fn test_histogram_metric() {
        let buckets = vec![(0.1, 10), (0.5, 50), (1.0, 80), (5.0, 100)];
        let metric = Metric::histogram("request_duration", buckets.clone(), 100, 45.5);

        assert_eq!(metric.kind, MetricKind::Histogram);
        if let MetricValue::Histogram {
            buckets: b,
            count,
            sum,
        } = &metric.value
        {
            assert_eq!(b, &buckets);
            assert_eq!(*count, 100);
            assert_eq!(*sum, 45.5);
        } else {
            panic!("Expected histogram value");
        }
    }
}
