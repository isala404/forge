//! Cluster metrics. With `otel` feature: real OpenTelemetry instruments.
//! Without: no-op stubs so call sites don't need cfg gates everywhere.

#[cfg(feature = "otel")]
use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Gauge, Histogram},
};
#[cfg(feature = "otel")]
use std::sync::OnceLock;

#[cfg(feature = "otel")]
const METER_NAME: &str = "forge.cluster";

#[cfg(feature = "otel")]
static CLUSTER_METRICS: OnceLock<ClusterMetrics> = OnceLock::new();

#[cfg(feature = "otel")]
pub struct ClusterMetrics {
    heartbeat_latency: Histogram<f64>,
    nodes_active: Gauge<i64>,
    nodes_dead: Gauge<i64>,
    leader_election_attempts: Counter<u64>,
    is_leader: Gauge<i64>,
    #[cfg(feature = "gateway")]
    notifications_processed: Counter<u64>,
    #[cfg(feature = "gateway")]
    notification_latency: Histogram<f64>,
}

#[cfg(feature = "otel")]
impl ClusterMetrics {
    fn new() -> Self {
        let meter = global::meter(METER_NAME);

        let heartbeat_latency = meter
            .f64_histogram("cluster.heartbeat.latency")
            .with_description("Heartbeat round-trip latency in seconds")
            .with_unit("s")
            .build();

        let nodes_active = meter
            .i64_gauge("cluster.nodes.active")
            .with_description("Number of active nodes in the cluster")
            .build();

        let nodes_dead = meter
            .i64_gauge("cluster.nodes.dead")
            .with_description("Number of dead nodes in the cluster")
            .build();

        let leader_election_attempts = meter
            .u64_counter("cluster.leader.election_attempts")
            .with_description("Total leader election attempts")
            .build();

        let is_leader = meter
            .i64_gauge("cluster.leader.is_leader")
            .with_description("Whether this node is the leader (1) or not (0)")
            .build();

        #[cfg(feature = "gateway")]
        let notifications_processed = meter
            .u64_counter("cluster.reactor.notifications_processed")
            .with_description("Total change notifications processed")
            .build();

        #[cfg(feature = "gateway")]
        let notification_latency = meter
            .f64_histogram("cluster.reactor.notification_latency")
            .with_description("Change notification processing latency in seconds")
            .with_unit("s")
            .build();

        Self {
            heartbeat_latency,
            nodes_active,
            nodes_dead,
            leader_election_attempts,
            is_leader,
            #[cfg(feature = "gateway")]
            notifications_processed,
            #[cfg(feature = "gateway")]
            notification_latency,
        }
    }
}

#[cfg(feature = "otel")]
fn metrics() -> &'static ClusterMetrics {
    CLUSTER_METRICS.get_or_init(ClusterMetrics::new)
}

#[cfg(feature = "otel")]
pub fn record_heartbeat_latency(latency_secs: f64) {
    metrics().heartbeat_latency.record(latency_secs, &[]);
}

#[cfg(feature = "otel")]
pub fn set_node_counts(active: i64, dead: i64) {
    metrics().nodes_active.record(active, &[]);
    metrics().nodes_dead.record(dead, &[]);
}

#[cfg(feature = "otel")]
pub fn record_leader_election_attempt(role: &str, acquired: bool) {
    metrics().leader_election_attempts.add(
        1,
        &[
            KeyValue::new("role", role.to_string()),
            KeyValue::new("acquired", acquired),
        ],
    );
}

#[cfg(feature = "otel")]
pub fn set_is_leader(role: &str, is_leader: bool) {
    metrics().is_leader.record(
        if is_leader { 1 } else { 0 },
        &[KeyValue::new("role", role.to_string())],
    );
}

#[cfg(all(feature = "otel", feature = "gateway"))]
pub fn record_notification_processed(table: &str) {
    metrics()
        .notifications_processed
        .add(1, &[KeyValue::new("table", table.to_string())]);
}

#[cfg(all(feature = "otel", feature = "gateway"))]
pub fn record_notification_latency(latency_secs: f64) {
    metrics().notification_latency.record(latency_secs, &[]);
}

#[cfg(not(feature = "otel"))]
#[inline]
pub fn record_heartbeat_latency(_latency_secs: f64) {}

#[cfg(not(feature = "otel"))]
#[inline]
pub fn set_node_counts(_active: i64, _dead: i64) {}

#[cfg(not(feature = "otel"))]
#[inline]
pub fn record_leader_election_attempt(_role: &str, _acquired: bool) {}

#[cfg(not(feature = "otel"))]
#[inline]
pub fn set_is_leader(_role: &str, _is_leader: bool) {}

#[cfg(all(not(feature = "otel"), feature = "gateway"))]
#[inline]
pub fn record_notification_processed(_table: &str) {}

#[cfg(all(not(feature = "otel"), feature = "gateway"))]
#[inline]
pub fn record_notification_latency(_latency_secs: f64) {}
