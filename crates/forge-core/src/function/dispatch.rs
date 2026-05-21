use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::Result;
use crate::job::JobInfo;
use crate::workflow::WorkflowInfo;

/// Trait for dispatching jobs from function contexts.
pub trait JobDispatch: Send + Sync {
    /// Get job info by name for auth checking.
    fn get_info(&self, job_type: &str) -> Option<JobInfo>;

    /// Dispatch a job by its registered name.
    fn dispatch_by_name(
        &self,
        job_type: &str,
        args: serde_json::Value,
        owner_subject: Option<String>,
        tenant_id: Option<Uuid>,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + '_>>;

    /// Dispatch a job at a specific time by its registered name.
    ///
    /// The job will not be picked up by workers until `scheduled_at` is
    /// reached. In all other respects it behaves like [`dispatch_by_name`].
    fn dispatch_by_name_at(
        &self,
        job_type: &str,
        args: serde_json::Value,
        scheduled_at: DateTime<Utc>,
        owner_subject: Option<String>,
        tenant_id: Option<Uuid>,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + '_>>;

    /// Dispatch a job on an existing connection — typically the live
    /// transaction inside a `MutationContext`. The insert participates in
    /// the surrounding transaction, so the job only becomes visible to
    /// workers after commit and is rolled back on failure.
    fn dispatch_in_conn<'a>(
        &'a self,
        conn: &'a mut sqlx::PgConnection,
        job_type: &'a str,
        args: serde_json::Value,
        owner_subject: Option<String>,
        tenant_id: Option<Uuid>,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + 'a>>;

    /// Dispatch a job at a specific time on an existing connection.
    ///
    /// Combines the transactional safety of [`dispatch_in_conn`] with
    /// delayed scheduling. The job row is written inside the caller's
    /// transaction and workers will not pick it up until `scheduled_at`.
    fn dispatch_in_conn_at<'a>(
        &'a self,
        conn: &'a mut sqlx::PgConnection,
        job_type: &'a str,
        args: serde_json::Value,
        scheduled_at: DateTime<Utc>,
        owner_subject: Option<String>,
        tenant_id: Option<Uuid>,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + 'a>>;

    /// Request cancellation for a job.
    fn cancel(
        &self,
        job_id: Uuid,
        reason: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + '_>>;
}

/// Trait for accessing the KV store from handler contexts.
///
/// Defined in `forge-core` so all context types can hold a `dyn KvHandle`
/// without depending on `forge-runtime`. The runtime implements this trait
/// on `KvStore` and threads it into every context at construction time.
///
/// The KV store is namespaced: each handle was created with a namespace
/// prefix that scopes all keys, preventing collisions between subsystems.
///
/// # Example
///
/// ```ignore
/// use std::time::Duration;
///
/// #[forge::query]
/// pub async fn get_feature_flag(ctx: &QueryContext, flag: String) -> Result<bool> {
///     let raw = ctx.kv().get(&flag).await?;
///     Ok(raw.map(|b| b == b"true").unwrap_or(false))
/// }
///
/// #[forge::mutation]
/// pub async fn set_feature_flag(ctx: &MutationContext, flag: String, enabled: bool) -> Result<()> {
///     let value = if enabled { b"true".as_ref() } else { b"false".as_ref() };
///     ctx.kv().set(&flag, value, None).await
/// }
/// ```
pub trait KvHandle: Send + Sync + 'static {
    /// Get a value by key. Returns `None` if the key doesn't exist or is expired.
    #[allow(clippy::type_complexity)]
    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>>> + Send + 'a>>;

    /// Set a key to a value, optionally with a TTL. Overwrites any existing value.
    fn set<'a>(
        &'a self,
        key: &'a str,
        value: &'a [u8],
        ttl: Option<Duration>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

    /// Set a key only if it doesn't already exist (or is expired).
    /// Returns `true` if the value was stored, `false` if a live entry already existed.
    fn set_if_absent<'a>(
        &'a self,
        key: &'a str,
        value: &'a [u8],
        ttl: Option<Duration>,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;

    /// Delete a key. Returns `true` if the key existed.
    fn delete<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;

    /// Atomically increment a counter by `delta`.
    ///
    /// Creates the counter at 0 if it doesn't exist. Returns the new value.
    /// Expired counters are treated as non-existent (the value resets to `delta`).
    /// When `ttl` is `None`, an existing counter's TTL is preserved.
    fn increment<'a>(
        &'a self,
        key: &'a str,
        delta: i64,
        ttl: Option<Duration>,
    ) -> Pin<Box<dyn Future<Output = Result<i64>> + Send + 'a>>;
}

/// Trait for starting workflows from function contexts.
pub trait WorkflowDispatch: Send + Sync {
    /// Get workflow info by name for auth checking.
    fn get_info(&self, workflow_name: &str) -> Option<WorkflowInfo>;

    /// Start a workflow by its registered name.
    ///
    /// `trace_id` is propagated onto the run row so observability links request → workflow.
    fn start_by_name(
        &self,
        workflow_name: &str,
        input: serde_json::Value,
        owner_subject: Option<String>,
        trace_id: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + '_>>;

    /// Start a workflow on an existing connection — typically the live
    /// transaction inside a `MutationContext`. The run row and its
    /// `$workflow_resume` job are written in the same transaction so the
    /// worker only picks the run up after commit.
    fn start_in_conn<'a>(
        &'a self,
        conn: &'a mut sqlx::PgConnection,
        workflow_name: &'a str,
        input: serde_json::Value,
        owner_subject: Option<String>,
        trace_id: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + 'a>>;
}
