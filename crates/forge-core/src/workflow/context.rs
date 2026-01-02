use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::parallel::ParallelBuilder;
use super::step::StepStatus;
use super::suspend::{SuspendReason, WorkflowEvent};
use crate::function::AuthContext;
use crate::{ForgeError, Result};

/// Type alias for compensation handler function.
pub type CompensationHandler = Arc<
    dyn Fn(serde_json::Value) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync,
>;

/// Step state stored during execution.
#[derive(Debug, Clone)]
pub struct StepState {
    /// Step name.
    pub name: String,
    /// Step status.
    pub status: StepStatus,
    /// Serialized result (if completed).
    pub result: Option<serde_json::Value>,
    /// Error message (if failed).
    pub error: Option<String>,
    /// When the step started.
    pub started_at: Option<DateTime<Utc>>,
    /// When the step completed.
    pub completed_at: Option<DateTime<Utc>>,
}

impl StepState {
    /// Create a new pending step state.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: StepStatus::Pending,
            result: None,
            error: None,
            started_at: None,
            completed_at: None,
        }
    }

    /// Mark step as running.
    pub fn start(&mut self) {
        self.status = StepStatus::Running;
        self.started_at = Some(Utc::now());
    }

    /// Mark step as completed with result.
    pub fn complete(&mut self, result: serde_json::Value) {
        self.status = StepStatus::Completed;
        self.result = Some(result);
        self.completed_at = Some(Utc::now());
    }

    /// Mark step as failed with error.
    pub fn fail(&mut self, error: impl Into<String>) {
        self.status = StepStatus::Failed;
        self.error = Some(error.into());
        self.completed_at = Some(Utc::now());
    }

    /// Mark step as compensated.
    pub fn compensate(&mut self) {
        self.status = StepStatus::Compensated;
    }
}

/// Context available to workflow handlers.
pub struct WorkflowContext {
    /// Workflow run ID.
    pub run_id: Uuid,
    /// Workflow name.
    pub workflow_name: String,
    /// Workflow version.
    pub version: u32,
    /// When the workflow started.
    pub started_at: DateTime<Utc>,
    /// Deterministic workflow time (consistent across replays).
    workflow_time: DateTime<Utc>,
    /// Authentication context.
    pub auth: AuthContext,
    /// Database pool.
    db_pool: sqlx::PgPool,
    /// HTTP client.
    http_client: reqwest::Client,
    /// Step states (for resumption).
    step_states: Arc<RwLock<HashMap<String, StepState>>>,
    /// Completed steps in order (for compensation).
    completed_steps: Arc<RwLock<Vec<String>>>,
    /// Compensation handlers for completed steps.
    compensation_handlers: Arc<RwLock<HashMap<String, CompensationHandler>>>,
    /// Channel for signaling suspension (sent by workflow, received by executor).
    suspend_tx: Option<mpsc::Sender<SuspendReason>>,
    /// Whether this is a resumed execution.
    is_resumed: bool,
    /// Whether this execution resumed specifically from a sleep (timer expired).
    resumed_from_sleep: bool,
    /// Tenant ID for multi-tenancy.
    tenant_id: Option<Uuid>,
}

impl WorkflowContext {
    /// Create a new workflow context.
    pub fn new(
        run_id: Uuid,
        workflow_name: String,
        version: u32,
        db_pool: sqlx::PgPool,
        http_client: reqwest::Client,
    ) -> Self {
        let now = Utc::now();
        Self {
            run_id,
            workflow_name,
            version,
            started_at: now,
            workflow_time: now,
            auth: AuthContext::unauthenticated(),
            db_pool,
            http_client,
            step_states: Arc::new(RwLock::new(HashMap::new())),
            completed_steps: Arc::new(RwLock::new(Vec::new())),
            compensation_handlers: Arc::new(RwLock::new(HashMap::new())),
            suspend_tx: None,
            is_resumed: false,
            resumed_from_sleep: false,
            tenant_id: None,
        }
    }

    /// Create a resumed workflow context.
    pub fn resumed(
        run_id: Uuid,
        workflow_name: String,
        version: u32,
        started_at: DateTime<Utc>,
        db_pool: sqlx::PgPool,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            run_id,
            workflow_name,
            version,
            started_at,
            workflow_time: started_at,
            auth: AuthContext::unauthenticated(),
            db_pool,
            http_client,
            step_states: Arc::new(RwLock::new(HashMap::new())),
            completed_steps: Arc::new(RwLock::new(Vec::new())),
            compensation_handlers: Arc::new(RwLock::new(HashMap::new())),
            suspend_tx: None,
            is_resumed: true,
            resumed_from_sleep: false,
            tenant_id: None,
        }
    }

    /// Mark that this context resumed from a sleep (timer expired).
    pub fn with_resumed_from_sleep(mut self) -> Self {
        self.resumed_from_sleep = true;
        self
    }

    /// Set the suspend channel.
    pub fn with_suspend_channel(mut self, tx: mpsc::Sender<SuspendReason>) -> Self {
        self.suspend_tx = Some(tx);
        self
    }

    /// Set the tenant ID.
    pub fn with_tenant(mut self, tenant_id: Uuid) -> Self {
        self.tenant_id = Some(tenant_id);
        self
    }

    /// Get the tenant ID.
    pub fn tenant_id(&self) -> Option<Uuid> {
        self.tenant_id
    }

    /// Check if this is a resumed execution.
    pub fn is_resumed(&self) -> bool {
        self.is_resumed
    }

    /// Get the deterministic workflow time.
    pub fn workflow_time(&self) -> DateTime<Utc> {
        self.workflow_time
    }

    /// Get the database pool.
    pub fn db(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get the HTTP client.
    pub fn http(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Set authentication context.
    pub fn with_auth(mut self, auth: AuthContext) -> Self {
        self.auth = auth;
        self
    }

    /// Restore step states from persisted data.
    pub fn with_step_states(self, states: HashMap<String, StepState>) -> Self {
        let completed: Vec<String> = states
            .iter()
            .filter(|(_, s)| s.status == StepStatus::Completed)
            .map(|(name, _)| name.clone())
            .collect();

        *self.step_states.write().unwrap() = states;
        *self.completed_steps.write().unwrap() = completed;
        self
    }

    /// Get step state by name.
    pub fn get_step_state(&self, name: &str) -> Option<StepState> {
        self.step_states.read().unwrap().get(name).cloned()
    }

    /// Check if a step is already completed.
    pub fn is_step_completed(&self, name: &str) -> bool {
        self.step_states
            .read()
            .unwrap()
            .get(name)
            .map(|s| s.status == StepStatus::Completed)
            .unwrap_or(false)
    }

    /// Get the result of a completed step.
    pub fn get_step_result<T: serde::de::DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.step_states
            .read()
            .unwrap()
            .get(name)
            .and_then(|s| s.result.as_ref())
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Record step start.
    pub fn record_step_start(&self, name: &str) {
        let mut states = self.step_states.write().unwrap();
        let state = states
            .entry(name.to_string())
            .or_insert_with(|| StepState::new(name));
        state.start();
        let state_clone = state.clone();
        drop(states);

        // Persist to database in background
        let pool = self.db_pool.clone();
        let run_id = self.run_id;
        let step_name = name.to_string();
        tokio::spawn(async move {
            let step_id = Uuid::new_v4();
            if let Err(e) = sqlx::query(
                r#"
                INSERT INTO forge_workflow_steps (id, workflow_run_id, step_name, status, started_at)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (workflow_run_id, step_name) DO UPDATE SET
                    status = EXCLUDED.status,
                    started_at = COALESCE(forge_workflow_steps.started_at, EXCLUDED.started_at)
                "#,
            )
            .bind(step_id)
            .bind(run_id)
            .bind(&step_name)
            .bind(state_clone.status.as_str())
            .bind(state_clone.started_at)
            .execute(&pool)
            .await
            {
                tracing::warn!(
                    workflow_run_id = %run_id,
                    step = %step_name,
                    "Failed to persist step start: {}",
                    e
                );
            }
        });
    }

    /// Record step completion (fire-and-forget database update).
    /// Use `record_step_complete_async` if you need to ensure persistence before continuing.
    pub fn record_step_complete(&self, name: &str, result: serde_json::Value) {
        let state_clone = self.update_step_state_complete(name, result);

        // Persist to database in background
        if let Some(state) = state_clone {
            let pool = self.db_pool.clone();
            let run_id = self.run_id;
            let step_name = name.to_string();
            tokio::spawn(async move {
                Self::persist_step_complete(&pool, run_id, &step_name, &state).await;
            });
        }
    }

    /// Record step completion and wait for database persistence.
    pub async fn record_step_complete_async(&self, name: &str, result: serde_json::Value) {
        let state_clone = self.update_step_state_complete(name, result);

        // Persist to database synchronously
        if let Some(state) = state_clone {
            Self::persist_step_complete(&self.db_pool, self.run_id, name, &state).await;
        }
    }

    /// Update in-memory step state to completed.
    fn update_step_state_complete(
        &self,
        name: &str,
        result: serde_json::Value,
    ) -> Option<StepState> {
        let mut states = self.step_states.write().unwrap();
        if let Some(state) = states.get_mut(name) {
            state.complete(result.clone());
        }
        let state_clone = states.get(name).cloned();
        drop(states);

        let mut completed = self.completed_steps.write().unwrap();
        if !completed.contains(&name.to_string()) {
            completed.push(name.to_string());
        }
        drop(completed);

        state_clone
    }

    /// Persist step completion to database.
    async fn persist_step_complete(
        pool: &sqlx::PgPool,
        run_id: Uuid,
        step_name: &str,
        state: &StepState,
    ) {
        // Use UPSERT to handle race condition where persist_step_start hasn't completed yet
        if let Err(e) = sqlx::query(
            r#"
            INSERT INTO forge_workflow_steps (id, workflow_run_id, step_name, status, result, started_at, completed_at)
            VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6)
            ON CONFLICT (workflow_run_id, step_name) DO UPDATE
            SET status = $3, result = $4, completed_at = $6
            "#,
        )
        .bind(run_id)
        .bind(step_name)
        .bind(state.status.as_str())
        .bind(&state.result)
        .bind(state.started_at)
        .bind(state.completed_at)
        .execute(pool)
        .await
        {
            tracing::warn!(
                workflow_run_id = %run_id,
                step = %step_name,
                "Failed to persist step completion: {}",
                e
            );
        }
    }

    /// Record step failure.
    pub fn record_step_failure(&self, name: &str, error: impl Into<String>) {
        let error_str = error.into();
        let mut states = self.step_states.write().unwrap();
        if let Some(state) = states.get_mut(name) {
            state.fail(error_str.clone());
        }
        let state_clone = states.get(name).cloned();
        drop(states);

        // Persist to database in background
        if let Some(state) = state_clone {
            let pool = self.db_pool.clone();
            let run_id = self.run_id;
            let step_name = name.to_string();
            tokio::spawn(async move {
                if let Err(e) = sqlx::query(
                    r#"
                    UPDATE forge_workflow_steps
                    SET status = $3, error = $4, completed_at = $5
                    WHERE workflow_run_id = $1 AND step_name = $2
                    "#,
                )
                .bind(run_id)
                .bind(&step_name)
                .bind(state.status.as_str())
                .bind(&state.error)
                .bind(state.completed_at)
                .execute(&pool)
                .await
                {
                    tracing::warn!(
                        workflow_run_id = %run_id,
                        step = %step_name,
                        "Failed to persist step failure: {}",
                        e
                    );
                }
            });
        }
    }

    /// Record step compensation.
    pub fn record_step_compensated(&self, name: &str) {
        let mut states = self.step_states.write().unwrap();
        if let Some(state) = states.get_mut(name) {
            state.compensate();
        }
        let state_clone = states.get(name).cloned();
        drop(states);

        // Persist to database in background
        if let Some(state) = state_clone {
            let pool = self.db_pool.clone();
            let run_id = self.run_id;
            let step_name = name.to_string();
            tokio::spawn(async move {
                if let Err(e) = sqlx::query(
                    r#"
                    UPDATE forge_workflow_steps
                    SET status = $3
                    WHERE workflow_run_id = $1 AND step_name = $2
                    "#,
                )
                .bind(run_id)
                .bind(&step_name)
                .bind(state.status.as_str())
                .execute(&pool)
                .await
                {
                    tracing::warn!(
                        workflow_run_id = %run_id,
                        step = %step_name,
                        "Failed to persist step compensation: {}",
                        e
                    );
                }
            });
        }
    }

    /// Get completed steps in reverse order (for compensation).
    pub fn completed_steps_reversed(&self) -> Vec<String> {
        let completed = self.completed_steps.read().unwrap();
        completed.iter().rev().cloned().collect()
    }

    /// Get all step states.
    pub fn all_step_states(&self) -> HashMap<String, StepState> {
        self.step_states.read().unwrap().clone()
    }

    /// Get elapsed time since workflow started.
    pub fn elapsed(&self) -> chrono::Duration {
        Utc::now() - self.started_at
    }

    /// Register a compensation handler for a step.
    pub fn register_compensation(&self, step_name: &str, handler: CompensationHandler) {
        let mut handlers = self.compensation_handlers.write().unwrap();
        handlers.insert(step_name.to_string(), handler);
    }

    /// Get compensation handler for a step.
    pub fn get_compensation_handler(&self, step_name: &str) -> Option<CompensationHandler> {
        self.compensation_handlers
            .read()
            .unwrap()
            .get(step_name)
            .cloned()
    }

    /// Check if a step has a compensation handler.
    pub fn has_compensation(&self, step_name: &str) -> bool {
        self.compensation_handlers
            .read()
            .unwrap()
            .contains_key(step_name)
    }

    /// Run compensation for all completed steps in reverse order.
    /// Returns a list of (step_name, success) tuples.
    pub async fn run_compensation(&self) -> Vec<(String, bool)> {
        let steps = self.completed_steps_reversed();
        let mut results = Vec::new();

        for step_name in steps {
            let handler = self.get_compensation_handler(&step_name);
            let result = self
                .get_step_state(&step_name)
                .and_then(|s| s.result.clone());

            if let Some(handler) = handler {
                let step_result = result.unwrap_or(serde_json::Value::Null);
                match handler(step_result).await {
                    Ok(()) => {
                        self.record_step_compensated(&step_name);
                        results.push((step_name, true));
                    }
                    Err(e) => {
                        tracing::error!(step = %step_name, error = %e, "Compensation failed");
                        results.push((step_name, false));
                    }
                }
            } else {
                // No compensation handler, mark as compensated anyway
                self.record_step_compensated(&step_name);
                results.push((step_name, true));
            }
        }

        results
    }

    /// Get compensation handlers (for cloning to executor).
    pub fn compensation_handlers(&self) -> HashMap<String, CompensationHandler> {
        self.compensation_handlers.read().unwrap().clone()
    }

    // =========================================================================
    // DURABLE WORKFLOW API
    // =========================================================================

    /// Sleep for a duration.
    ///
    /// This suspends the workflow and persists the wake time to the database.
    /// The workflow scheduler will resume the workflow when the time arrives.
    ///
    /// # Example
    /// ```ignore
    /// // Sleep for 30 days
    /// ctx.sleep(Duration::from_secs(30 * 24 * 60 * 60)).await?;
    /// ```
    pub async fn sleep(&self, duration: Duration) -> Result<()> {
        // If we resumed from a sleep, the timer already expired - continue immediately
        if self.resumed_from_sleep {
            return Ok(());
        }

        let wake_at = Utc::now() + chrono::Duration::from_std(duration).unwrap_or_default();
        self.sleep_until(wake_at).await
    }

    /// Sleep until a specific time.
    ///
    /// If the wake time has already passed, returns immediately.
    ///
    /// # Example
    /// ```ignore
    /// use chrono::{Utc, Duration};
    /// let renewal_date = Utc::now() + Duration::days(30);
    /// ctx.sleep_until(renewal_date).await?;
    /// ```
    pub async fn sleep_until(&self, wake_at: DateTime<Utc>) -> Result<()> {
        // If we resumed from a sleep, the timer already expired - continue immediately
        if self.resumed_from_sleep {
            return Ok(());
        }

        // If wake time already passed, return immediately
        if wake_at <= Utc::now() {
            return Ok(());
        }

        // Persist the wake time to database
        self.set_wake_at(wake_at).await?;

        // Signal suspension to executor
        self.signal_suspend(SuspendReason::Sleep { wake_at })
            .await?;

        Ok(())
    }

    /// Wait for an external event with optional timeout.
    ///
    /// The workflow suspends until the event arrives or the timeout expires.
    /// Events are correlated by the workflow run ID.
    ///
    /// # Example
    /// ```ignore
    /// let payment: PaymentConfirmation = ctx.wait_for_event(
    ///     "payment_confirmed",
    ///     Some(Duration::from_secs(7 * 24 * 60 * 60)), // 7 days
    /// ).await?;
    /// ```
    pub async fn wait_for_event<T: DeserializeOwned>(
        &self,
        event_name: &str,
        timeout: Option<Duration>,
    ) -> Result<T> {
        let correlation_id = self.run_id.to_string();

        // Check if event already exists (race condition handling)
        if let Some(event) = self.try_consume_event(event_name, &correlation_id).await? {
            return serde_json::from_value(event.payload.unwrap_or_default())
                .map_err(|e| ForgeError::Deserialization(e.to_string()));
        }

        // Calculate timeout
        let timeout_at =
            timeout.map(|d| Utc::now() + chrono::Duration::from_std(d).unwrap_or_default());

        // Persist waiting state
        self.set_waiting_for_event(event_name, timeout_at).await?;

        // Signal suspension
        self.signal_suspend(SuspendReason::WaitingEvent {
            event_name: event_name.to_string(),
            timeout: timeout_at,
        })
        .await?;

        // After resume, try to consume the event
        self.try_consume_event(event_name, &correlation_id)
            .await?
            .and_then(|e| e.payload)
            .and_then(|p| serde_json::from_value(p).ok())
            .ok_or_else(|| ForgeError::Timeout(format!("Event '{}' timed out", event_name)))
    }

    /// Try to consume an event from the database.
    #[allow(clippy::type_complexity)]
    async fn try_consume_event(
        &self,
        event_name: &str,
        correlation_id: &str,
    ) -> Result<Option<WorkflowEvent>> {
        let result: Option<(
            Uuid,
            String,
            String,
            Option<serde_json::Value>,
            DateTime<Utc>,
        )> = sqlx::query_as(
            r#"
                UPDATE forge_workflow_events
                SET consumed_at = NOW(), consumed_by = $3
                WHERE id = (
                    SELECT id FROM forge_workflow_events
                    WHERE event_name = $1 AND correlation_id = $2 AND consumed_at IS NULL
                    ORDER BY created_at ASC LIMIT 1
                    FOR UPDATE SKIP LOCKED
                )
                RETURNING id, event_name, correlation_id, payload, created_at
                "#,
        )
        .bind(event_name)
        .bind(correlation_id)
        .bind(self.run_id)
        .fetch_optional(&self.db_pool)
        .await
        .map_err(|e| ForgeError::Database(e.to_string()))?;

        Ok(result.map(
            |(id, event_name, correlation_id, payload, created_at)| WorkflowEvent {
                id,
                event_name,
                correlation_id,
                payload,
                created_at,
            },
        ))
    }

    /// Persist wake time to database.
    async fn set_wake_at(&self, wake_at: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_workflow_runs
            SET status = 'waiting', suspended_at = NOW(), wake_at = $2
            WHERE id = $1
            "#,
        )
        .bind(self.run_id)
        .bind(wake_at)
        .execute(&self.db_pool)
        .await
        .map_err(|e| ForgeError::Database(e.to_string()))?;
        Ok(())
    }

    /// Persist waiting for event state to database.
    async fn set_waiting_for_event(
        &self,
        event_name: &str,
        timeout_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_workflow_runs
            SET status = 'waiting', suspended_at = NOW(), waiting_for_event = $2, event_timeout_at = $3
            WHERE id = $1
            "#,
        )
        .bind(self.run_id)
        .bind(event_name)
        .bind(timeout_at)
        .execute(&self.db_pool)
        .await
        .map_err(|e| ForgeError::Database(e.to_string()))?;
        Ok(())
    }

    /// Signal suspension to the executor.
    async fn signal_suspend(&self, reason: SuspendReason) -> Result<()> {
        if let Some(ref tx) = self.suspend_tx {
            tx.send(reason)
                .await
                .map_err(|_| ForgeError::Internal("Failed to signal suspension".into()))?;
        }
        // Return a special error that the executor catches
        Err(ForgeError::WorkflowSuspended)
    }

    // =========================================================================
    // PARALLEL EXECUTION API
    // =========================================================================

    /// Create a parallel builder for executing steps concurrently.
    ///
    /// # Example
    /// ```ignore
    /// let results = ctx.parallel()
    ///     .step("fetch_user", || async { get_user(id).await })
    ///     .step("fetch_orders", || async { get_orders(id).await })
    ///     .step_with_compensate("charge_card",
    ///         || async { charge_card(amount).await },
    ///         |charge| async move { refund(charge.id).await })
    ///     .run().await?;
    ///
    /// let user: User = results.get("fetch_user")?;
    /// let orders: Vec<Order> = results.get("fetch_orders")?;
    /// ```
    pub fn parallel(&self) -> ParallelBuilder<'_> {
        ParallelBuilder::new(self)
    }

    // =========================================================================
    // FLUENT STEP API
    // =========================================================================

    /// Create a step runner for executing a workflow step.
    ///
    /// This provides a fluent API for defining steps with retry, compensation,
    /// timeout, and optional behavior.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::time::Duration;
    ///
    /// // Simple step
    /// let data = ctx.step("fetch_data", || async {
    ///     Ok(fetch_from_api().await?)
    /// }).run().await?;
    ///
    /// // Step with retry (3 attempts, 2 second delay)
    /// ctx.step("send_email", || async {
    ///     send_verification_email(&user.email).await
    /// })
    /// .retry(3, Duration::from_secs(2))
    /// .run()
    /// .await?;
    ///
    /// // Step with compensation (rollback on later failure)
    /// let charge = ctx.step("charge_card", || async {
    ///     charge_credit_card(&card).await
    /// })
    /// .compensate(|charge_result| async move {
    ///     refund_charge(&charge_result.charge_id).await
    /// })
    /// .run()
    /// .await?;
    ///
    /// // Optional step (failure won't trigger compensation)
    /// ctx.step("notify_slack", || async {
    ///     post_to_slack("User signed up!").await
    /// })
    /// .optional()
    /// .run()
    /// .await?;
    ///
    /// // Step with timeout
    /// ctx.step("slow_operation", || async {
    ///     process_large_file().await
    /// })
    /// .timeout(Duration::from_secs(60))
    /// .run()
    /// .await?;
    /// ```
    pub fn step<T, F, Fut>(&self, name: impl Into<String>, f: F) -> super::StepRunner<'_, T>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = crate::Result<T>> + Send + 'static,
    {
        super::StepRunner::new(self, name, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_workflow_context_creation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let run_id = Uuid::new_v4();
        let ctx = WorkflowContext::new(
            run_id,
            "test_workflow".to_string(),
            1,
            pool,
            reqwest::Client::new(),
        );

        assert_eq!(ctx.run_id, run_id);
        assert_eq!(ctx.workflow_name, "test_workflow");
        assert_eq!(ctx.version, 1);
    }

    #[tokio::test]
    async fn test_step_state_tracking() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let ctx = WorkflowContext::new(
            Uuid::new_v4(),
            "test".to_string(),
            1,
            pool,
            reqwest::Client::new(),
        );

        ctx.record_step_start("step1");
        assert!(!ctx.is_step_completed("step1"));

        ctx.record_step_complete("step1", serde_json::json!({"result": "ok"}));
        assert!(ctx.is_step_completed("step1"));

        let result: Option<serde_json::Value> = ctx.get_step_result("step1");
        assert!(result.is_some());
    }

    #[test]
    fn test_step_state_transitions() {
        let mut state = StepState::new("test");
        assert_eq!(state.status, StepStatus::Pending);

        state.start();
        assert_eq!(state.status, StepStatus::Running);
        assert!(state.started_at.is_some());

        state.complete(serde_json::json!({}));
        assert_eq!(state.status, StepStatus::Completed);
        assert!(state.completed_at.is_some());
    }
}
